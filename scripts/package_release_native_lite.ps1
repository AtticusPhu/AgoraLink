param(
    [string]$Version = "0.0.10",
    [string]$Python = $env:PYTHON,
    [string]$PyInstaller = $env:PYINSTALLER,
    [string]$MakeNsis = $env:MAKENSIS
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Flavor = "native_lite"
$ScriptDir = Split-Path -Parent $PSCommandPath
$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $ScriptDir "..")).Path
$SpecPath = Join-Path $RepoRoot "AgoraLink.spec"
$AppPaths = Join-Path $RepoRoot "app_paths.py"
$InstallerDir = Join-Path $RepoRoot "installer"
$NsisScript = Join-Path $InstallerDir "AgoraLink_Native_Lite.nsi"
$DistDir = Join-Path $RepoRoot "dist"
$DistAppDir = Join-Path $DistDir "AgoraLink"
$AppExe = Join-Path $DistAppDir "AgoraLink.exe"
$RustMediaCrateDir = Join-Path $RepoRoot "rust-native\agoralink_media"
$RustMediaExe = Join-Path $RustMediaCrateDir "target\release\agoralink_media.exe"
$DistRustMediaExe = Join-Path $DistAppDir "_internal\tools\agoralink_media\agoralink_media.exe"
$DistFfmpegDir = Join-Path $DistAppDir "_internal\tools\ffmpeg"
$InstallerExe = Join-Path $DistDir "AgoraLink_Native_Lite_Setup_v$Version.exe"
$PortableZip = Join-Path $DistDir "AgoraLink_native_lite_v$Version.zip"
$ResolvedPyInstallerInvocation = $null

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "Version must use numeric semver format like 0.0.10: $Version"
}

function Assert-File {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required file not found: $Path"
    }
}

function Assert-Directory {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "Required directory not found: $Path"
    }
}

function Get-SourceAppVersion {
    Assert-File $AppPaths
    $content = Get-Content -LiteralPath $AppPaths -Raw
    $match = [regex]::Match($content, 'APP_VERSION\s*=\s*["'']v?([^"'']+)["'']')
    if (-not $match.Success) {
        throw "APP_VERSION not found in app_paths.py"
    }
    return $match.Groups[1].Value
}

function Assert-PackageVersionMatchesSource {
    $sourceVersion = Get-SourceAppVersion
    if ($sourceVersion -ne $Version) {
        throw "Package version $Version does not match app_paths.py APP_VERSION v$sourceVersion. Update -Version or app_paths.py before packaging."
    }
}

function Invoke-Checked {
    param(
        [string]$FilePath,
        [string[]]$Arguments,
        [string]$Step
    )
    Write-Host "==> $Step"
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Step failed with exit code $LASTEXITCODE"
    }
}

function Resolve-MakeNsis {
    param([string]$Requested)

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        if (Test-Path -LiteralPath $Requested -PathType Leaf) {
            return (Resolve-Path -LiteralPath $Requested).Path
        }
        $requestedCommand = Get-Command $Requested -ErrorAction SilentlyContinue
        if ($requestedCommand) {
            return $requestedCommand.Source
        }
        throw "makensis.exe not found from MAKENSIS value: $Requested"
    }

    $command = Get-Command "makensis.exe" -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $candidates = @()
    if ($env:ProgramFiles) {
        $candidates += Join-Path $env:ProgramFiles "NSIS\makensis.exe"
    }
    $programFilesX86 = [Environment]::GetEnvironmentVariable("ProgramFiles(x86)")
    if ($programFilesX86) {
        $candidates += Join-Path $programFilesX86 "NSIS\makensis.exe"
    }

    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    throw "makensis.exe not found. Install NSIS or set MAKENSIS to makensis.exe."
}

function Resolve-Cargo {
    $command = Get-Command "cargo.exe" -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }
    $command = Get-Command "cargo" -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }
    throw "cargo not found. Install Rust stable toolchain before packaging Rust native media."
}

function Resolve-ExecutableCandidate {
    param([string]$Candidate)
    if ([string]::IsNullOrWhiteSpace($Candidate)) {
        return ""
    }
    if (Test-Path -LiteralPath $Candidate -PathType Leaf) {
        return (Resolve-Path -LiteralPath $Candidate).Path
    }
    $command = Get-Command $Candidate -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }
    return ""
}

function Test-CommandOk {
    param(
        [string]$FilePath,
        [string[]]$Arguments
    )
    try {
        & $FilePath @Arguments *> $null
        return ($LASTEXITCODE -eq 0)
    }
    catch {
        return $false
    }
}

function Resolve-Python {
    $candidates = New-Object System.Collections.Generic.List[string]
    if (-not [string]::IsNullOrWhiteSpace($Python)) {
        $candidates.Add($Python)
    }

    $repoParent = (Resolve-Path -LiteralPath (Join-Path $RepoRoot "..")).Path
    $candidates.Add((Join-Path $repoParent ".venv\Scripts\python.exe"))
    $candidates.Add((Join-Path $repoParent "python_kivy\00\.venv\Scripts\python.exe"))
    $candidates.Add("python.exe")
    $candidates.Add("python")

    $tried = New-Object System.Collections.Generic.List[string]
    foreach ($candidate in ($candidates | Select-Object -Unique)) {
        $path = Resolve-ExecutableCandidate $candidate
        if ([string]::IsNullOrWhiteSpace($path)) {
            $tried.Add("$candidate (not found)")
            continue
        }
        if (Test-CommandOk -FilePath $path -Arguments @("-c", "import sys; print(sys.executable)")) {
            Write-Host "==> Python: $path"
            return $path
        }
        $tried.Add("$path (failed to start)")
    }

    throw "No working Python executable found. Tried:`n$($tried -join "`n")"
}

function Resolve-PyInstallerInvocation {
    if (-not [string]::IsNullOrWhiteSpace($PyInstaller)) {
        $path = Resolve-ExecutableCandidate $PyInstaller
        if ([string]::IsNullOrWhiteSpace($path)) {
            throw "PyInstaller executable not found from PYINSTALLER value: $PyInstaller"
        }
        if (-not (Test-CommandOk -FilePath $path -Arguments @("--version"))) {
            throw "PyInstaller executable exists but failed to start: $path"
        }
        Write-Host "==> PyInstaller: $path"
        return @{
            FilePath = $path
            Prefix = @()
        }
    }

    if (Test-CommandOk -FilePath $Python -Arguments @("-c", "import PyInstaller")) {
        Write-Host "==> PyInstaller: $Python -m PyInstaller"
        return @{
            FilePath = $Python
            Prefix = @("-m", "PyInstaller")
        }
    }

    foreach ($candidate in @("pyinstaller.exe", "pyinstaller")) {
        $path = Resolve-ExecutableCandidate $candidate
        if (-not [string]::IsNullOrWhiteSpace($path) -and (Test-CommandOk -FilePath $path -Arguments @("--version"))) {
            Write-Host "==> PyInstaller: $path"
            return @{
                FilePath = $path
                Prefix = @()
            }
        }
    }

    throw "PyInstaller is not available for Python: $Python. Install PyInstaller in that environment or pass -PyInstaller <pyinstaller.exe>."
}

function Invoke-PyInstaller {
    if ($null -eq $script:ResolvedPyInstallerInvocation) {
        $script:ResolvedPyInstallerInvocation = Resolve-PyInstallerInvocation
    }
    $arguments = @($script:ResolvedPyInstallerInvocation["Prefix"]) + @("--clean", "--noconfirm", $SpecPath)
    Invoke-Checked `
        -FilePath $script:ResolvedPyInstallerInvocation["FilePath"] `
        -Arguments $arguments `
        -Step "PyInstaller Native Lite"
}

function Invoke-RustMediaBuild {
    Assert-Directory $RustMediaCrateDir
    Assert-File (Join-Path $RustMediaCrateDir "Cargo.toml")

    $cargo = Resolve-Cargo
    Push-Location $RustMediaCrateDir
    try {
        Invoke-Checked `
            -FilePath $cargo `
            -Arguments @("build", "--release", "--locked", "--offline") `
            -Step "Rust native media release build"
    }
    finally {
        Pop-Location
    }

    Assert-File $RustMediaExe
}

function Invoke-PyCompile {
    $pyFiles = @(
        "app_paths.py",
        "main_kivy.py",
        "screen_runtime.py",
        "screen_control.py",
        "screen_share_presenter.py",
        "screen_capability.py"
    ) | ForEach-Object { Join-Path $RepoRoot $_ }

    foreach ($file in $pyFiles) {
        Assert-File $file
    }

    $cacheRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("agoralink_pycompile_" + [Guid]::NewGuid().ToString("N"))
    $oldCachePrefix = $env:PYTHONPYCACHEPREFIX
    try {
        $env:PYTHONPYCACHEPREFIX = $cacheRoot
        $compileArgs = @("-m", "py_compile") + $pyFiles
        Invoke-Checked -FilePath $Python -Arguments $compileArgs -Step "py_compile"
    }
    finally {
        if ($null -eq $oldCachePrefix) {
            Remove-Item Env:\PYTHONPYCACHEPREFIX -ErrorAction SilentlyContinue
        }
        else {
            $env:PYTHONPYCACHEPREFIX = $oldCachePrefix
        }
        if (Test-Path -LiteralPath $cacheRoot) {
            Remove-Item -LiteralPath $cacheRoot -Recurse -Force
        }
    }
}

function Remove-BundledFfmpeg {
    if (Test-Path -LiteralPath $DistFfmpegDir) {
        $resolved = (Resolve-Path -LiteralPath $DistFfmpegDir).Path
        $distRoot = (Resolve-Path -LiteralPath $DistAppDir).Path
        if (-not $resolved.StartsWith($distRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove path outside dist app dir: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}

function Assert-NoFfmpegBinaries {
    param([string]$Root)
    $bad = @()
    foreach ($name in @("ffmpeg.exe", "ffplay.exe", "ffprobe.exe")) {
        $bad += @(Get-ChildItem -LiteralPath $Root -Recurse -Filter $name -File -ErrorAction SilentlyContinue)
    }
    if ($bad.Count -gt 0) {
        $paths = ($bad | ForEach-Object { $_.FullName }) -join "`n"
        throw "Native Lite package contains forbidden FFmpeg binaries:`n$paths"
    }
}

function Assert-ZipNoFfmpegBinaries {
    param([string]$ZipPath)
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($ZipPath)
    try {
        $bad = @($zip.Entries | Where-Object {
            $name = [System.IO.Path]::GetFileName($_.FullName)
            $name -in @("ffmpeg.exe", "ffplay.exe", "ffprobe.exe")
        })
        if ($bad.Count -gt 0) {
            $paths = ($bad | ForEach-Object { $_.FullName }) -join "`n"
            throw "Native Lite portable zip contains forbidden FFmpeg binaries:`n$paths"
        }
    }
    finally {
        $zip.Dispose()
    }
}

function Format-ArtifactSize {
    param([string]$Path)
    $item = Get-Item -LiteralPath $Path
    $mb = [Math]::Round($item.Length / 1MB, 2)
    return "$($item.FullName) ($mb MB)"
}

Push-Location $RepoRoot
try {
    Assert-File $SpecPath
    Assert-File $AppPaths
    Assert-File $NsisScript
    Assert-File (Join-Path $RepoRoot "main_kivy.py")
    Assert-PackageVersionMatchesSource
    Write-Host "==> Native Lite version: $Version"

    $Python = Resolve-Python
    $script:ResolvedPyInstallerInvocation = Resolve-PyInstallerInvocation
    Invoke-PyCompile
    Invoke-RustMediaBuild

    if (Test-Path -LiteralPath $DistAppDir) {
        $resolvedDistApp = (Resolve-Path -LiteralPath $DistAppDir).Path
        $resolvedDist = if (Test-Path -LiteralPath $DistDir) { (Resolve-Path -LiteralPath $DistDir).Path } else { $DistDir }
        if (-not $resolvedDistApp.StartsWith($resolvedDist, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to clean path outside dist dir: $resolvedDistApp"
        }
        Remove-Item -LiteralPath $resolvedDistApp -Recurse -Force
    }

    $oldFlavor = $env:AGORALINK_PACKAGE_FLAVOR
    try {
        $env:AGORALINK_PACKAGE_FLAVOR = $Flavor
        Invoke-PyInstaller
    }
    finally {
        if ($null -eq $oldFlavor) {
            Remove-Item Env:\AGORALINK_PACKAGE_FLAVOR -ErrorAction SilentlyContinue
        }
        else {
            $env:AGORALINK_PACKAGE_FLAVOR = $oldFlavor
        }
    }

    Assert-File $AppExe
    Assert-File $DistRustMediaExe
    Remove-BundledFfmpeg
    Assert-NoFfmpegBinaries -Root $DistAppDir

    $makensisExe = Resolve-MakeNsis -Requested $MakeNsis
    Push-Location $InstallerDir
    try {
        Invoke-Checked `
            -FilePath $makensisExe `
            -Arguments @(
                "/DAPP_VERSION=$Version",
                "/DAPP_DISPLAY_NAME=AgoraLink Native Lite v$Version",
                "/DNATIVE_LITE_OUTFILE=..\dist\AgoraLink_Native_Lite_Setup_v$Version.exe",
                "AgoraLink_Native_Lite.nsi"
            ) `
            -Step "NSIS Native Lite"
    }
    finally {
        Pop-Location
    }
    Assert-File $InstallerExe

    Write-Host "==> Portable ZIP"
    if (Test-Path -LiteralPath $PortableZip) {
        Remove-Item -LiteralPath $PortableZip -Force
    }
    Compress-Archive -Path (Join-Path $DistAppDir "*") -DestinationPath $PortableZip -CompressionLevel Optimal
    Assert-File $PortableZip
    Assert-ZipNoFfmpegBinaries -ZipPath $PortableZip

    Write-Host "==> Final artifact checks"
    Assert-File $AppExe
    Assert-File $DistRustMediaExe
    Assert-NoFfmpegBinaries -Root $DistAppDir

    Write-Host "==> SHA256"
    foreach ($artifact in @($InstallerExe, $PortableZip)) {
        $hash = Get-FileHash -LiteralPath $artifact -Algorithm SHA256
        Write-Output ("{0}  {1}" -f $hash.Hash, $hash.Path)
    }

    Write-Host "==> Native Lite release package complete"
    Write-Host (Format-ArtifactSize $AppExe)
    Write-Host (Format-ArtifactSize $DistRustMediaExe)
    Write-Host (Format-ArtifactSize $InstallerExe)
    Write-Host (Format-ArtifactSize $PortableZip)
}
finally {
    Pop-Location
}

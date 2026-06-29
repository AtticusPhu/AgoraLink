param(
    [string]$Python = $env:PYTHON,
    [string]$MakeNsis = $env:MAKENSIS
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Version = "0.0.4"
$ScriptDir = Split-Path -Parent $PSCommandPath
$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $ScriptDir "..")).Path
$SpecPath = Join-Path $RepoRoot "AgoraLink.spec"
$InstallerDir = Join-Path $RepoRoot "installer"
$NsisScript = Join-Path $InstallerDir "AgoraLink.nsi"
$DistDir = Join-Path $RepoRoot "dist"
$DistAppDir = Join-Path $DistDir "AgoraLink"
$AppExe = Join-Path $DistAppDir "AgoraLink.exe"
$RustMediaCrateDir = Join-Path $RepoRoot "rust-native\agoralink_media"
$RustMediaExe = Join-Path $RustMediaCrateDir "target\release\agoralink_media.exe"
$DistRustMediaExe = Join-Path $DistAppDir "_internal\tools\agoralink_media\agoralink_media.exe"
$DistFfmpegExe = Join-Path $DistAppDir "_internal\tools\ffmpeg\bin\ffmpeg.exe"
$InstallerExe = Join-Path $DistDir "AgoraLink_Setup_v$Version.exe"
$PortableZip = Join-Path $DistDir "AgoraLink_portable_v$Version.zip"

if ([string]::IsNullOrWhiteSpace($Python)) {
    $Python = "python"
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
    $pyFiles = Get-ChildItem -LiteralPath $RepoRoot -Filter "*.py" -File |
        Sort-Object FullName
    if ($pyFiles.Count -eq 0) {
        throw "No Python source files found for py_compile."
    }

    $cacheRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("agoralink_pycompile_" + [Guid]::NewGuid().ToString("N"))
    $oldCachePrefix = $env:PYTHONPYCACHEPREFIX
    try {
        $env:PYTHONPYCACHEPREFIX = $cacheRoot
        $compileArgs = @("-m", "py_compile") + @($pyFiles.FullName)
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

Push-Location $RepoRoot
try {
    Assert-File $SpecPath
    Assert-File $NsisScript
    Assert-File (Join-Path $RepoRoot "main_kivy.py")

    Invoke-PyCompile
    Invoke-RustMediaBuild

    Invoke-Checked `
        -FilePath $Python `
        -Arguments @("-m", "PyInstaller", "--clean", "--noconfirm", $SpecPath) `
        -Step "PyInstaller"

    Assert-File $AppExe
    Assert-File $DistRustMediaExe
    Assert-File $DistFfmpegExe

    $makensisExe = Resolve-MakeNsis -Requested $MakeNsis
    Push-Location $InstallerDir
    try {
        Invoke-Checked -FilePath $makensisExe -Arguments @("AgoraLink.nsi") -Step "NSIS"
    }
    finally {
        Pop-Location
    }

    Assert-File $InstallerExe
    Assert-Directory $DistAppDir

    Write-Host "==> Portable ZIP"
    if (Test-Path -LiteralPath $PortableZip) {
        Remove-Item -LiteralPath $PortableZip -Force
    }
    Compress-Archive -Path (Join-Path $DistAppDir "*") -DestinationPath $PortableZip -CompressionLevel Optimal
    Assert-File $PortableZip
    Assert-File $DistRustMediaExe
    Assert-File $DistFfmpegExe

    Write-Host "==> SHA256"
    foreach ($artifact in @($InstallerExe, $PortableZip)) {
        $hash = Get-FileHash -LiteralPath $artifact -Algorithm SHA256
        Write-Output ("{0}  {1}" -f $hash.Hash, $hash.Path)
    }

    Write-Host "==> Release package complete"
    Write-Host "Generated artifacts are ignored by .gitignore and must not be committed."
}
finally {
    Pop-Location
}

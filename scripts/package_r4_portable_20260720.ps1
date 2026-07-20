param(
    [string]$Python = $env:PYTHON,
    [string]$PyInstaller = $env:PYINSTALLER,
    [string]$OutputRoot = "",
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Release = "R4"
$BuildDate = "2026-07-20"
$ExpectedNativeHash = "D0CEE722185FF1E294894557C008CA96B37FF8C75F909162DB7B658286C8AE9D"
$R3NativeHash = "55AA6B837D1CA2DFCF6362D8BEE3CFA5A9998DC8F769FD76A415DBE02DB44B05"
$ScriptDir = Split-Path -Parent $PSCommandPath
$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $ScriptDir "..")).Path
$SpecPath = Join-Path $RepoRoot "AgoraLink.spec"
$CrateRoot = Join-Path $RepoRoot "rust-native\agoralink_media"
$NativeExeSource = Join-Path $CrateRoot "target\release\agoralink_media.exe"
$NativePdbSource = Join-Path $CrateRoot "target\release\agoralink_media.pdb"
$OwnedRoot = Join-Path $RepoRoot "_local_artifacts"
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $OwnedRoot "R4_GUI_PORTABLE_20260720"
}
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$MarkerPath = Join-Path $OutputRoot ".agoralink_r4_portable_build_root"
$StageDir = Join-Path $OutputRoot "staging\native_runtime"
$WorkDir = Join-Path $OutputRoot "pyinstaller_work"
$PyInstallerDist = Join-Path $OutputRoot "pyinstaller_dist"
$PortableDir = Join-Path $OutputRoot "AgoraLink_R4_portable_20260720"
$PortableZip = Join-Path $OutputRoot "AgoraLink_R4_portable_20260720.zip"
$PortableZipHashFile = Join-Path $OutputRoot "AgoraLink_R4_portable_20260720.sha256.txt"
$VerifyDir = Join-Path $OutputRoot "portable_verify"
$BuildResultPath = Join-Path $OutputRoot "build_result.json"

function Assert-File {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required file not found: $Path"
    }
}

function Assert-Hash {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Expected
    )
    Assert-File $Path
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
    if ($actual -ne $Expected.ToUpperInvariant()) {
        throw "SHA-256 mismatch for $Path. Expected $Expected, got $actual"
    }
    return $actual
}

function Assert-OwnedOutputRoot {
    $owned = [IO.Path]::GetFullPath($OwnedRoot).TrimEnd('\') + '\'
    $candidate = [IO.Path]::GetFullPath($OutputRoot).TrimEnd('\') + '\'
    if (-not $candidate.StartsWith($owned, [StringComparison]::OrdinalIgnoreCase)) {
        throw "OutputRoot must stay under $OwnedRoot"
    }
}

function Initialize-OutputRoot {
    Assert-OwnedOutputRoot
    if (Test-Path -LiteralPath $OutputRoot) {
        if (-not $Force) {
            throw "OutputRoot already exists. Use a new path or pass -Force for this owned R4 build root: $OutputRoot"
        }
        if (-not (Test-Path -LiteralPath $MarkerPath -PathType Leaf)) {
            throw "Refusing to replace an unmarked directory: $OutputRoot"
        }
        $marker = (Get-Content -LiteralPath $MarkerPath -Raw).Trim()
        if ($marker -ne "AgoraLink R4 portable build root") {
            throw "Refusing to replace a directory with an unexpected ownership marker: $OutputRoot"
        }
        Remove-Item -LiteralPath $OutputRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
    Set-Content -LiteralPath $MarkerPath -Value "AgoraLink R4 portable build root" -Encoding UTF8
}

function Resolve-Executable {
    param([string]$Candidate)
    if ([string]::IsNullOrWhiteSpace($Candidate)) {
        return ""
    }
    if (Test-Path -LiteralPath $Candidate -PathType Leaf) {
        return (Resolve-Path -LiteralPath $Candidate).Path
    }
    $command = Get-Command $Candidate -ErrorAction SilentlyContinue
    return $(if ($command) { $command.Source } else { "" })
}

function Test-PythonModule {
    param([string]$Executable, [string]$Module)
    try {
        & $Executable -c "import $Module" *> $null
        return ($LASTEXITCODE -eq 0)
    }
    catch {
        return $false
    }
}

function Resolve-Python {
    $candidates = @(
        $Python,
        (Join-Path (Split-Path -Parent $RepoRoot) ".venv\Scripts\python.exe"),
        "python.exe",
        "python"
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique
    foreach ($candidate in $candidates) {
        $resolved = Resolve-Executable $candidate
        if ($resolved -and (Test-PythonModule -Executable $resolved -Module "sys")) {
            return $resolved
        }
    }
    throw "No working Python interpreter found. Pass -Python with the project-compatible Python executable."
}

function Resolve-PyInstallerCommand {
    param([string]$PythonExe)
    $explicit = Resolve-Executable $PyInstaller
    if ($explicit) {
        & $explicit --version *> $null
        if ($LASTEXITCODE -ne 0) {
            throw "PyInstaller failed to start: $explicit"
        }
        return @{ FilePath = $explicit; Prefix = @() }
    }
    if (Test-PythonModule -Executable $PythonExe -Module "PyInstaller") {
        return @{ FilePath = $PythonExe; Prefix = @("-m", "PyInstaller") }
    }
    throw "PyInstaller is unavailable for $PythonExe. Use the existing project environment or pass -PyInstaller."
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$Step
    )
    Write-Host "==> $Step"
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Step failed with exit code $LASTEXITCODE"
    }
}

function Get-AppVersion {
    $content = Get-Content -LiteralPath (Join-Path $RepoRoot "app_paths.py") -Raw
    $match = [regex]::Match($content, 'APP_VERSION\s*=\s*["'']([^"'']+)["'']')
    if (-not $match.Success) {
        throw "APP_VERSION not found in app_paths.py"
    }
    return $match.Groups[1].Value
}

function Invoke-PythonChecks {
    param([string]$PythonExe)
    $files = @(
        "main_kivy.py",
        "screen_runtime.py",
        "screen_share_presenter.py",
        "app_paths.py",
        "diagnostic_export.py"
    ) | ForEach-Object { Join-Path $RepoRoot $_ }
    $args = @("-m", "py_compile") + $files
    Invoke-Checked -FilePath $PythonExe -Arguments $args -Step "Python syntax checks"
    Invoke-Checked -FilePath $PythonExe -Arguments @("-m", "unittest", "-v", "tests.test_screen_runtime_r4") -Step "R4 GUI integration tests"
}

function Stage-NativeRuntime {
    Assert-Hash -Path $NativeExeSource -Expected $ExpectedNativeHash | Out-Null
    Assert-File $NativePdbSource
    New-Item -ItemType Directory -Path $StageDir -Force | Out-Null
    Get-ChildItem -LiteralPath $StageDir -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -in @("agoralink_media.exe", "agoralink_media.pdb") } |
        Remove-Item -Force
    Copy-Item -LiteralPath $NativeExeSource -Destination (Join-Path $StageDir "agoralink_media.exe") -Force
    Copy-Item -LiteralPath $NativePdbSource -Destination (Join-Path $StageDir "agoralink_media.pdb") -Force
    Assert-Hash -Path (Join-Path $StageDir "agoralink_media.exe") -Expected $ExpectedNativeHash | Out-Null
    Assert-File (Join-Path $StageDir "agoralink_media.pdb")
}

function Assert-PortablePolicy {
    param([Parameter(Mandatory = $true)][string]$Root)
    $rootPrefix = [IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $nativeFiles = @(Get-ChildItem -LiteralPath $Root -Recurse -File -ErrorAction Stop |
        Where-Object { $_.Name -in @("agoralink_media.exe", "agoralink_media.bin") })
    if ($nativeFiles.Count -ne 1) {
        throw "Portable must contain exactly one native media executable; found $($nativeFiles.Count)."
    }
    $nativeHash = (Get-FileHash -LiteralPath $nativeFiles[0].FullName -Algorithm SHA256).Hash.ToUpperInvariant()
    if ($nativeHash -eq $R3NativeHash) {
        throw "R3 native media executable found in portable: $($nativeFiles[0].FullName)"
    }
    if ($nativeHash -ne $ExpectedNativeHash) {
        throw "Unexpected native media executable found in portable: $nativeHash"
    }
    $forbidden = @(Get-ChildItem -LiteralPath $Root -Recurse -File -ErrorAction Stop | Where-Object {
        $relative = $_.FullName.Substring($rootPrefix.Length)
        $_.Extension -in @(".jsonl", ".dmp", ".db", ".key", ".pin") -or
        $_.Name -in @("gui_settings.json", "audio_probe.pcm") -or
        $relative -match '(^|[\\/])target[\\/]' -or
        $relative -match '(^|[\\/])_local_artifacts[\\/]'
    })
    if ($forbidden.Count -gt 0) {
        throw "Forbidden runtime/test/user files found in portable:`n$(($forbidden.FullName) -join "`n")"
    }
}

function Write-PortableMetadata {
    param([string]$GitBranch, [string]$GitCommit, [string]$PdbHash)
    $buildInfo = [ordered]@{
        release = $Release
        build_date = $BuildDate
        app_version = Get-AppVersion
        package_flavor = "native"
        git_branch = $GitBranch
        git_commit = $GitCommit
        native_exe_sha256 = $ExpectedNativeHash
        native_pdb_sha256 = $PdbHash
        default_preset = "r4_default"
        default_width = 1920
        default_height = 1080
        default_fps = 60
        default_bitrate_mbps = 22
        repair = "nack"
        adaptive_quality = "off"
        encoder = "auto"
        convert_backend = "auto"
        render_backend = "d3d11"
    }
    $buildInfo | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $PortableDir "BUILD_INFO.json") -Encoding UTF8
    Copy-Item -LiteralPath (Join-Path $RepoRoot "README.md") -Destination (Join-Path $PortableDir "README.md") -Force
    Copy-Item -LiteralPath (Join-Path $RepoRoot "CHANGELOG.md") -Destination (Join-Path $PortableDir "CHANGELOG.md") -Force
    @"
AgoraLink R4 Portable

Launch: AgoraLink.exe
Native runtime: _internal\tools\agoralink_media\agoralink_media.exe
Symbols: _internal\tools\agoralink_media\agoralink_media.pdb
Default screen preset: r4_default (1920x1080, 60 FPS, 22 Mbps, NACK, adaptive off)
Build date: 2026-07-20
"@ | Set-Content -LiteralPath (Join-Path $PortableDir "PORTABLE_README.txt") -Encoding UTF8

    $portablePrefix = [IO.Path]::GetFullPath($PortableDir).TrimEnd('\') + '\'
    $manifest = @(Get-ChildItem -LiteralPath $PortableDir -Recurse -File | Sort-Object FullName | ForEach-Object {
        [ordered]@{
            path = $_.FullName.Substring($portablePrefix.Length).Replace('\', '/')
            size_bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
        }
    })
    [ordered]@{ release = $Release; files = $manifest } |
        ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath (Join-Path $PortableDir "PORTABLE_CONTENTS.json") -Encoding UTF8

    $required = @(
        "AgoraLink.exe",
        "_internal\tools\agoralink_media\agoralink_media.exe",
        "_internal\tools\agoralink_media\agoralink_media.pdb",
        "BUILD_INFO.json",
        "PORTABLE_CONTENTS.json"
    )
    $sumLines = foreach ($relative in $required) {
        $path = Join-Path $PortableDir $relative
        Assert-File $path
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        "$hash  $($relative.Replace('\', '/'))"
    }
    $sumLines | Set-Content -LiteralPath (Join-Path $PortableDir "SHA256SUMS.txt") -Encoding ASCII
}

function Invoke-NativeSelfTest {
    param([string]$NativeExe, [string]$EvidencePrefix)
    $stdoutPath = "$EvidencePrefix.stdout.jsonl"
    $stderrPath = "$EvidencePrefix.stderr.txt"
    $process = Start-Process -FilePath $NativeExe -ArgumentList @("self-test") -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    if ($process.ExitCode -ne 0) {
        throw "Bundled native self-test failed with exit code $($process.ExitCode). See $stdoutPath and $stderrPath"
    }
    $event = Get-Content -LiteralPath $stdoutPath -Raw | ConvertFrom-Json
    if (-not $event.ok) {
        throw "Bundled native self-test did not report ok=true."
    }
}

Assert-File $SpecPath
Assert-File (Join-Path $RepoRoot "app_paths.py")
Assert-File $NativeExeSource
Assert-File $NativePdbSource
Initialize-OutputRoot

$Python = Resolve-Python
$pyInstallerCommand = Resolve-PyInstallerCommand -PythonExe $Python
$gitBranch = (& git -C $RepoRoot branch --show-current).Trim()
$gitCommit = (& git -C $RepoRoot rev-parse HEAD).Trim()
if ($gitBranch -ne "r4-default-adaptive-ladder") {
    throw "R4 portable must be built from r4-default-adaptive-ladder; current branch is $gitBranch"
}

$nativeHash = Assert-Hash -Path $NativeExeSource -Expected $ExpectedNativeHash
$pdbHash = (Get-FileHash -LiteralPath $NativePdbSource -Algorithm SHA256).Hash.ToUpperInvariant()
Stage-NativeRuntime
Invoke-PythonChecks -PythonExe $Python

$oldRuntimeDir = $env:AGORALINK_NATIVE_RUNTIME_DIR
$oldKivyHome = $env:KIVY_HOME
$oldCachePrefix = $env:PYTHONPYCACHEPREFIX
try {
    $env:AGORALINK_NATIVE_RUNTIME_DIR = $StageDir
    $env:KIVY_HOME = Join-Path $OutputRoot "kivy_home"
    $env:PYTHONPYCACHEPREFIX = Join-Path $OutputRoot "python_cache"
    New-Item -ItemType Directory -Path $env:KIVY_HOME -Force | Out-Null
    $arguments = @($pyInstallerCommand.Prefix) + @(
        "--noconfirm",
        "--distpath", $PyInstallerDist,
        "--workpath", $WorkDir,
        $SpecPath
    )
    Invoke-Checked -FilePath $pyInstallerCommand.FilePath -Arguments $arguments -Step "PyInstaller R4 portable build"
}
finally {
    $env:AGORALINK_NATIVE_RUNTIME_DIR = $oldRuntimeDir
    $env:KIVY_HOME = $oldKivyHome
    $env:PYTHONPYCACHEPREFIX = $oldCachePrefix
}

$PyDistApp = Join-Path $PyInstallerDist "AgoraLink"
Assert-File (Join-Path $PyDistApp "AgoraLink.exe")
Assert-Hash -Path (Join-Path $PyDistApp "_internal\tools\agoralink_media\agoralink_media.exe") -Expected $ExpectedNativeHash | Out-Null
Assert-File (Join-Path $PyDistApp "_internal\tools\agoralink_media\agoralink_media.pdb")
Copy-Item -LiteralPath $PyDistApp -Destination $PortableDir -Recurse
Write-PortableMetadata -GitBranch $gitBranch -GitCommit $gitCommit -PdbHash $pdbHash
Assert-PortablePolicy -Root $PortableDir

Invoke-NativeSelfTest `
    -NativeExe (Join-Path $PortableDir "_internal\tools\agoralink_media\agoralink_media.exe") `
    -EvidencePrefix (Join-Path $OutputRoot "portable_native_self_test")

Compress-Archive -LiteralPath $PortableDir -DestinationPath $PortableZip -CompressionLevel Optimal
Assert-File $PortableZip
$zipHash = (Get-FileHash -LiteralPath $PortableZip -Algorithm SHA256).Hash.ToUpperInvariant()
"$zipHash  $(Split-Path -Leaf $PortableZip)" | Set-Content -LiteralPath $PortableZipHashFile -Encoding ASCII

New-Item -ItemType Directory -Path $VerifyDir -Force | Out-Null
Expand-Archive -LiteralPath $PortableZip -DestinationPath $VerifyDir
$ExtractedPortable = Join-Path $VerifyDir (Split-Path -Leaf $PortableDir)
Assert-File (Join-Path $ExtractedPortable "AgoraLink.exe")
Assert-Hash -Path (Join-Path $ExtractedPortable "_internal\tools\agoralink_media\agoralink_media.exe") -Expected $ExpectedNativeHash | Out-Null
Assert-File (Join-Path $ExtractedPortable "_internal\tools\agoralink_media\agoralink_media.pdb")
Assert-PortablePolicy -Root $ExtractedPortable
Invoke-NativeSelfTest `
    -NativeExe (Join-Path $ExtractedPortable "_internal\tools\agoralink_media\agoralink_media.exe") `
    -EvidencePrefix (Join-Path $OutputRoot "extracted_native_self_test")

$buildResult = [ordered]@{
    release = $Release
    build_date = $BuildDate
    git_branch = $gitBranch
    git_commit = $gitCommit
    python = $Python
    native_source = $NativeExeSource
    native_exe_sha256 = $nativeHash
    native_pdb_sha256 = $pdbHash
    bundled_native = Join-Path $PortableDir "_internal\tools\agoralink_media\agoralink_media.exe"
    portable_dir = $PortableDir
    portable_zip = $PortableZip
    portable_zip_sha256 = $zipHash
    extracted_portable = $ExtractedPortable
    native_self_test = "pass"
    r3_binary_found = $false
}
$buildResult | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $BuildResultPath -Encoding UTF8
$buildResult | ConvertTo-Json -Depth 5

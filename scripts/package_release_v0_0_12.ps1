[CmdletBinding()]
param(
    [string]$Python = $env:PYTHON,
    [string]$OutputRoot = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$ProgressPreference = "SilentlyContinue"

$Version = "0.0.12"
$Release = "v$Version"
$ScriptDir = Split-Path -Parent $PSCommandPath
$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $ScriptDir "..")).Path
$OwnedRoot = Join-Path $RepoRoot "_local_artifacts"
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $OwnedRoot "V0_0_12_RELEASE"
}
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$MarkerPath = Join-Path $OutputRoot ".agoralink_v0_0_12_release_root"
$CrateRoot = Join-Path $RepoRoot "rust-native\agoralink_media"
$NativeExeSource = Join-Path $CrateRoot "target\release\agoralink_media.exe"
$NativePdbSource = Join-Path $CrateRoot "target\release\agoralink_media.pdb"
$StageDir = Join-Path $OutputRoot "staging\native_runtime"
$WorkDir = Join-Path $OutputRoot "pyinstaller_work"
$PyInstallerDist = Join-Path $OutputRoot "pyinstaller_dist"
$PortableDir = Join-Path $OutputRoot "portable_staging"
$PortableZip = Join-Path $OutputRoot "AgoraLink_v0.0.12_portable.zip"
$PortableHashFile = Join-Path $OutputRoot "AgoraLink_v0.0.12_portable.sha256.txt"
$VerifyDir = Join-Path $OutputRoot "portable_verify"
$BuildResultPath = Join-Path $OutputRoot "build_result.json"
$PrivacyScript = Join-Path $ScriptDir "Test-PortablePrivacy.ps1"

function Assert-File {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required file not found: $Path"
    }
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

function Assert-OwnedOutputRoot {
    $owned = [IO.Path]::GetFullPath($OwnedRoot).TrimEnd('\') + '\'
    $candidate = $OutputRoot.TrimEnd('\') + '\'
    if ($candidate.Equals($owned, [StringComparison]::OrdinalIgnoreCase) -or
        -not $candidate.StartsWith($owned, [StringComparison]::OrdinalIgnoreCase)) {
        throw "OutputRoot must be a child directory under $OwnedRoot"
    }
}

function Initialize-OutputRoot {
    Assert-OwnedOutputRoot
    if (Test-Path -LiteralPath $OutputRoot) {
        if (-not $Force) {
            throw "OutputRoot exists. Use another path or pass -Force: $OutputRoot"
        }
        if (-not (Test-Path -LiteralPath $MarkerPath -PathType Leaf)) {
            throw "Refusing to replace unmarked output directory: $OutputRoot"
        }
        if ((Get-Content -LiteralPath $MarkerPath -Raw).Trim() -ne "AgoraLink v0.0.12 release root") {
            throw "Unexpected release-root ownership marker: $MarkerPath"
        }
        Remove-Item -LiteralPath $OutputRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
    Set-Content -LiteralPath $MarkerPath -Value "AgoraLink v0.0.12 release root" -Encoding ASCII
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

function Test-PythonBuildEnvironment {
    param([string]$Executable)
    try {
        & $Executable -c "import importlib.util as u,sys; assert sys.version_info[:2] == (3,12); assert all(u.find_spec(name) for name in ('kivy','PyInstaller','win32api','cryptography'))" *> $null
        return ($LASTEXITCODE -eq 0)
    }
    catch {
        return $false
    }
}

function Resolve-Python {
    if (-not [string]::IsNullOrWhiteSpace($Python)) {
        $explicit = Resolve-Executable $Python
        if (-not $explicit) {
            throw "Explicit Python executable was not found: $Python"
        }
        if (-not (Test-PythonBuildEnvironment $explicit)) {
            throw "Explicit Python environment is not a usable locked Python 3.12 build environment: $explicit"
        }
        return $explicit
    }

    $candidates = @(
        (Join-Path $RepoRoot ".venv-v0.0.12\Scripts\python.exe"),
        (Join-Path (Split-Path -Parent $RepoRoot) ".venv\Scripts\python.exe"),
        "python.exe",
        "python"
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique
    foreach ($candidate in $candidates) {
        $resolved = Resolve-Executable $candidate
        if ($resolved -and (Test-PythonBuildEnvironment $resolved)) {
            return $resolved
        }
    }
    throw "No locked Python 3.12 build environment found. Run scripts/Setup-DevEnvironment.ps1 -IncludeBuildDependencies and pass its python.exe with -Python."
}

function Assert-SourceVersion {
    $content = Get-Content -LiteralPath (Join-Path $RepoRoot "app_paths.py") -Raw
    $match = [regex]::Match($content, 'APP_VERSION\s*=\s*["'']v?([^"'']+)["'']')
    if (-not $match.Success -or $match.Groups[1].Value -ne $Version) {
        throw "app_paths.APP_VERSION must be v$Version before packaging."
    }
}

function Invoke-RustBuild {
    $rustVersion = (& rustc +stable-x86_64-pc-windows-msvc --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $rustVersion -notmatch '^rustc 1\.96\.0\b') {
        throw "Rust 1.96.0 x86_64-pc-windows-msvc is required; found: $rustVersion"
    }

    $oldEncodedFlags = $env:CARGO_ENCODED_RUSTFLAGS
    $oldRustFlags = $env:RUSTFLAGS
    $unitSeparator = [char]0x1f
    $remapFlags = @(
        "--remap-path-prefix=$RepoRoot=/src/AgoraLink",
        "--remap-path-prefix=$env:USERPROFILE=/build/user"
    )
    try {
        Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
        $env:CARGO_ENCODED_RUSTFLAGS = $remapFlags -join $unitSeparator
        Push-Location $CrateRoot
        try {
            Invoke-Checked -FilePath "cargo" -Arguments @(
                "+stable-x86_64-pc-windows-msvc", "build", "--release", "--locked", "--offline"
            ) -Step "Rust native release build with path remapping"
        }
        finally {
            Pop-Location
        }
    }
    finally {
        if ($null -eq $oldEncodedFlags) {
            Remove-Item Env:\CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
        } else {
            $env:CARGO_ENCODED_RUSTFLAGS = $oldEncodedFlags
        }
        if ($null -eq $oldRustFlags) {
            Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
        } else {
            $env:RUSTFLAGS = $oldRustFlags
        }
    }
    Assert-File $NativeExeSource
    Assert-File $NativePdbSource
}

function Invoke-PythonChecks {
    param([string]$PythonExe)
    Invoke-Checked -FilePath $PythonExe -Arguments @("-m", "pip", "check") -Step "Locked dependency check"
    Invoke-Checked -FilePath $PythonExe -Arguments @("-B", "scripts/check_python_test_count.py") -Step "Python test-count guard"
    Invoke-Checked -FilePath $PythonExe -Arguments @(
        "-B", "-m", "unittest", "discover", "-s", "tests", "-p", "test_*.py", "-v"
    ) -Step "Python deterministic tests"
    Invoke-Checked -FilePath $PythonExe -Arguments @("-B", "screen_runtime.py", "--self-test") -Step "Python native runtime self-test"
}

function Invoke-NativeSelfTest {
    param([string]$NativeExe, [string]$Prefix)
    $stdout = "$Prefix.stdout.jsonl"
    $stderr = "$Prefix.stderr.txt"
    $process = Start-Process -FilePath $NativeExe -ArgumentList @("self-test") -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if ($process.ExitCode -ne 0) {
        throw "Native self-test failed with exit code $($process.ExitCode)."
    }
    $events = @(Get-Content -LiteralPath $stdout | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object { $_ | ConvertFrom-Json })
    if (-not ($events | Where-Object { $_.ok -eq $true })) {
        throw "Native self-test did not emit ok=true."
    }
}

function Write-PortableMetadata {
    param([string]$GitCommit, [string]$NativeHash, [string]$PythonExe)
    $licenseStatus = if (Test-Path -LiteralPath (Join-Path $RepoRoot "LICENSE") -PathType Leaf) {
        "present"
    } else {
        "USER_DECISION_REQUIRED"
    }
    $buildInfo = [ordered]@{
        release = $Release
        app_version = $Release
        package_flavor = "native"
        git_commit = $GitCommit
        build_date_utc = (Get-Date).ToUniversalTime().ToString("o")
        native_exe_sha256 = $NativeHash
        python_version = (& $PythonExe -c "import platform; print(platform.python_version())").Trim()
        rust_toolchain = (& rustc +stable-x86_64-pc-windows-msvc --version).Trim()
        media_pipeline = "WGC/D3D11/QSV-or-WMF/AGM1/WMF/D3D11"
        external_media_backend = $false
        symbols_in_portable = $false
        license_status = $licenseStatus
    }
    $buildInfo | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $PortableDir "BUILD_INFO.json") -Encoding UTF8
    Copy-Item -LiteralPath (Join-Path $RepoRoot "README.md") -Destination (Join-Path $PortableDir "README.md") -Force
    Copy-Item -LiteralPath (Join-Path $RepoRoot "CHANGELOG.md") -Destination (Join-Path $PortableDir "CHANGELOG.md") -Force
    $license = Join-Path $RepoRoot "LICENSE"
    if (Test-Path -LiteralPath $license -PathType Leaf) {
        Copy-Item -LiteralPath $license -Destination (Join-Path $PortableDir "LICENSE") -Force
    }

    $prefix = [IO.Path]::GetFullPath($PortableDir).TrimEnd('\') + '\'
    $manifest = @(Get-ChildItem -LiteralPath $PortableDir -Recurse -File | Sort-Object FullName | ForEach-Object {
        [ordered]@{
            path = $_.FullName.Substring($prefix.Length).Replace('\', '/')
            size_bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
        }
    })
    [ordered]@{ release = $Release; files = $manifest } | ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath (Join-Path $PortableDir "PORTABLE_CONTENTS.json") -Encoding UTF8

    $sumLines = Get-ChildItem -LiteralPath $PortableDir -Recurse -File | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($prefix.Length).Replace('\', '/')
        "{0}  {1}" -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash, $relative
    }
    $sumLines | Set-Content -LiteralPath (Join-Path $PortableDir "SHA256SUMS.txt") -Encoding ASCII
}

Assert-File (Join-Path $RepoRoot "AgoraLink.spec")
Assert-File $PrivacyScript
Assert-SourceVersion
$PythonExe = Resolve-Python
Write-Host "==> Python build environment: $PythonExe"
Initialize-OutputRoot

$gitBranch = (& git -C $RepoRoot branch --show-current).Trim()
$gitCommit = (& git -C $RepoRoot rev-parse HEAD).Trim()
Invoke-PythonChecks -PythonExe $PythonExe
Invoke-RustBuild

New-Item -ItemType Directory -Path $StageDir -Force | Out-Null
Copy-Item -LiteralPath $NativeExeSource -Destination (Join-Path $StageDir "agoralink_media.exe") -Force
$nativeHash = (Get-FileHash -LiteralPath $NativeExeSource -Algorithm SHA256).Hash

$oldRuntimeDir = $env:AGORALINK_NATIVE_RUNTIME_DIR
$oldKivyHome = $env:KIVY_HOME
$oldKivyLogLevel = $env:KIVY_LOG_LEVEL
$oldKivyNoFileLog = $env:KIVY_NO_FILELOG
$oldCachePrefix = $env:PYTHONPYCACHEPREFIX
try {
    $env:AGORALINK_NATIVE_RUNTIME_DIR = $StageDir
    $env:KIVY_HOME = Join-Path $OutputRoot "kivy_home"
    $env:KIVY_LOG_LEVEL = "warning"
    $env:KIVY_NO_FILELOG = "1"
    $env:PYTHONPYCACHEPREFIX = Join-Path $OutputRoot "python_cache"
    New-Item -ItemType Directory -Path $env:KIVY_HOME -Force | Out-Null
    Invoke-Checked -FilePath $PythonExe -Arguments @(
        "-m", "PyInstaller", "--noconfirm", "--clean", "--log-level", "WARN",
        "--distpath", $PyInstallerDist, "--workpath", $WorkDir,
        (Join-Path $RepoRoot "AgoraLink.spec")
    ) -Step "PyInstaller native-only portable build"
}
finally {
    $env:AGORALINK_NATIVE_RUNTIME_DIR = $oldRuntimeDir
    $env:KIVY_HOME = $oldKivyHome
    $env:KIVY_LOG_LEVEL = $oldKivyLogLevel
    $env:KIVY_NO_FILELOG = $oldKivyNoFileLog
    $env:PYTHONPYCACHEPREFIX = $oldCachePrefix
}

$PyDistApp = Join-Path $PyInstallerDist "AgoraLink"
Assert-File (Join-Path $PyDistApp "AgoraLink.exe")
Assert-File (Join-Path $PyDistApp "_internal\tools\agoralink_media\agoralink_media.exe")
$unusedKivyData = @(
    (Join-Path $PyDistApp "_internal\kivy\tests"),
    (Join-Path $PyDistApp "_internal\kivy_install\modules"),
    (Join-Path $PyDistApp "_internal\kivy\core\video"),
    (Join-Path $PyDistApp "_internal\kivy\lib\gstplayer")
)
foreach ($path in $unusedKivyData) {
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Recurse -Force
    }
}
Get-ChildItem -LiteralPath (Join-Path $PyDistApp "_internal\kivy") -Recurse -File -Filter "*.h" -ErrorAction SilentlyContinue |
    Remove-Item -Force
if (Get-ChildItem -LiteralPath $PyDistApp -Recurse -File | Where-Object { $_.Extension -ieq ".pdb" }) {
    throw "PyInstaller output contains PDB files."
}

Copy-Item -LiteralPath $PyDistApp -Destination $PortableDir -Recurse
Write-PortableMetadata -GitCommit $gitCommit -NativeHash $nativeHash -PythonExe $PythonExe
& $PrivacyScript -Root $PortableDir -ForbiddenPathPrefix @($env:USERPROFILE, $RepoRoot) |
    Set-Content -LiteralPath (Join-Path $OutputRoot "portable_privacy_scan.json") -Encoding UTF8
Invoke-NativeSelfTest -NativeExe (Join-Path $PortableDir "_internal\tools\agoralink_media\agoralink_media.exe") `
    -Prefix (Join-Path $OutputRoot "portable_native_self_test")

Compress-Archive -Path (Join-Path $PortableDir "*") -DestinationPath $PortableZip -CompressionLevel Optimal
$portableHash = (Get-FileHash -LiteralPath $PortableZip -Algorithm SHA256).Hash
"$portableHash  $(Split-Path -Leaf $PortableZip)" | Set-Content -LiteralPath $PortableHashFile -Encoding ASCII

New-Item -ItemType Directory -Path $VerifyDir -Force | Out-Null
Expand-Archive -LiteralPath $PortableZip -DestinationPath $VerifyDir
& $PrivacyScript -Root $VerifyDir -ForbiddenPathPrefix @($env:USERPROFILE, $RepoRoot) |
    Set-Content -LiteralPath (Join-Path $OutputRoot "extracted_portable_privacy_scan.json") -Encoding UTF8
Invoke-NativeSelfTest -NativeExe (Join-Path $VerifyDir "_internal\tools\agoralink_media\agoralink_media.exe") `
    -Prefix (Join-Path $OutputRoot "extracted_native_self_test")

$result = [ordered]@{
    type = "V0_0_12_PACKAGE_RESULT"
    release = $Release
    git_branch = $gitBranch
    git_commit = $gitCommit
    python_version = (& $PythonExe -c "import platform; print(platform.python_version())").Trim()
    python_executable_sha256 = (Get-FileHash -LiteralPath $PythonExe -Algorithm SHA256).Hash
    portable_zip = $PortableZip
    portable_size_bytes = (Get-Item -LiteralPath $PortableZip).Length
    portable_sha256 = $portableHash
    native_exe_sha256 = $nativeHash
    pdb_in_portable = $false
    external_media_files_in_portable = 0
    license = if (Test-Path -LiteralPath (Join-Path $RepoRoot "LICENSE")) { "present" } else { "USER_DECISION_REQUIRED" }
    privacy_scan = "pass"
    extracted_verification = "pass"
}
$result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $BuildResultPath -Encoding UTF8
$result | ConvertTo-Json -Depth 5

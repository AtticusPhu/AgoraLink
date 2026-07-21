[CmdletBinding()]
param(
    [string]$Python = "",
    [string]$NativeExe = "",
    [string]$EvidenceRoot = "",
    [switch]$RunRustSuites
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
$CrateRoot = Join-Path $RepoRoot "rust-native\agoralink_media"
if ([string]::IsNullOrWhiteSpace($Python)) {
    $Python = Join-Path $RepoRoot ".venv-official-v0.0.12\Scripts\python.exe"
}
if ([string]::IsNullOrWhiteSpace($NativeExe)) {
    $NativeExe = Join-Path $CrateRoot "target\release\agoralink_media.exe"
}
if ([string]::IsNullOrWhiteSpace($EvidenceRoot)) {
    $EvidenceRoot = Join-Path $RepoRoot "_local_artifacts\V0_0_12_FINAL_VALIDATION"
}

if (-not (Test-Path -LiteralPath $Python -PathType Leaf)) {
    throw "Validation Python not found: $Python"
}

$RunRoot = Join-Path $EvidenceRoot ("run_" + (Get-Date -Format "yyyyMMdd_HHmmss"))
$CommandRoot = Join-Path $RunRoot "commands"
$CacheRoot = Join-Path $RunRoot "python_cache"
$KivyRoot = Join-Path $RunRoot "kivy_home"
New-Item -ItemType Directory -Path $CommandRoot, $CacheRoot, $KivyRoot -Force | Out-Null

Import-Module (Join-Path $PSScriptRoot "ValidationRunner.psm1") -Force

$commandResults = New-Object System.Collections.Generic.List[object]
$failure = $null
$validationStart = Get-Date
$oldCache = $env:PYTHONPYCACHEPREFIX
$oldKivyHome = $env:KIVY_HOME
$oldKivyLogLevel = $env:KIVY_LOG_LEVEL
$oldKivyNoFileLog = $env:KIVY_NO_FILELOG
$env:PYTHONPYCACHEPREFIX = $CacheRoot
$env:KIVY_HOME = $KivyRoot
$env:KIVY_LOG_LEVEL = "warning"
$env:KIVY_NO_FILELOG = "1"

function Invoke-Gate {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Executable,
        [string[]]$CommandArguments = @(),
        [Parameter(Mandatory = $true)][string]$WorkingDirectory,
        [ValidateRange(1, 600)][int]$TimeoutSeconds = 600,
        [switch]$ExpectTimeout
    )

    $stdout = Join-Path $CommandRoot "$Name.stdout.log"
    $stderr = Join-Path $CommandRoot "$Name.stderr.log"
    $resultPath = Join-Path $CommandRoot "$Name.result.json"
    $combined = Join-Path $CommandRoot "$Name.combined.log"
    $invokeParams = @{
        Name = $Name
        Executable = $Executable
        CommandArguments = $CommandArguments
        WorkingDirectory = $WorkingDirectory
        StdoutPath = $stdout
        StderrPath = $stderr
        ResultPath = $resultPath
        TimeoutSeconds = $TimeoutSeconds
    }
    $result = Invoke-ValidatedCommand @invokeParams
    Merge-ValidatedCommandLogs -StdoutPath $stdout -StderrPath $stderr -CombinedPath $combined
    $commandResults.Add($result)

    if ($ExpectTimeout) {
        if (-not $result.timed_out -or $result.exit_code -ne 124) {
            throw "$Name did not produce the expected bounded timeout."
        }
    }
    elseif ($result.timed_out -or $result.exit_code -ne 0) {
        throw "$Name failed with exit_code=$($result.exit_code), timed_out=$($result.timed_out)."
    }
    return $result
}

function Get-DumpSnapshot {
    if (-not (Test-Path -LiteralPath "C:\CrashDumps")) {
        return @()
    }
    return @(
        Get-ChildItem -LiteralPath "C:\CrashDumps" -File -ErrorAction SilentlyContinue |
            ForEach-Object { $_.FullName }
    )
}

$dumpsBefore = Get-DumpSnapshot
$mediaProcessesBefore = @(
    Get-Process -Name agoralink_media, ffmpeg, ffprobe, ffplay -ErrorAction SilentlyContinue
)
$checks = [ordered]@{
    psscriptanalyzer = "NOT_CHECKED"
}

try {
    $cargoVersion = Invoke-Gate -Name "runner-cargo-version" -Executable "cargo" -CommandArguments @("--version") -WorkingDirectory $CrateRoot -TimeoutSeconds 60
    $cargoMetadata = Invoke-Gate -Name "runner-cargo-metadata" -Executable "cargo" -CommandArguments @("metadata", "--locked", "--offline", "--no-deps", "--format-version", "1") -WorkingDirectory $CrateRoot -TimeoutSeconds 120
    Get-Content -LiteralPath $cargoMetadata.stdout_path -Raw -Encoding UTF8 | ConvertFrom-Json | Out-Null

    $pythonVersion = Invoke-Gate -Name "runner-python-version" -Executable $Python -CommandArguments @("--version") -WorkingDirectory $RepoRoot -TimeoutSeconds 60
    $unicodeArgument = ([char]0x4E2D).ToString() + ([char]0x6587)
    $argumentEcho = Invoke-Gate -Name "runner-argument-quoting" -Executable $Python -CommandArguments @(
        "-c",
        "import json,sys; print(json.dumps(sys.argv[1:], ensure_ascii=False))",
        "space value",
        $unicodeArgument,
        "apostrophe'value"
    ) -WorkingDirectory $RepoRoot -TimeoutSeconds 60
    $echoedArguments = Get-Content -LiteralPath $argumentEcho.stdout_path -Raw -Encoding UTF8 | ConvertFrom-Json
    if (($echoedArguments -join "|") -ne ("space value|{0}|apostrophe'value" -f $unicodeArgument)) {
        throw "Validation runner argument quoting failed."
    }

    $powerShellExe = Join-Path $PSHOME "powershell.exe"
    if (-not (Test-Path -LiteralPath $powerShellExe)) {
        $powerShellExe = (Get-Process -Id $PID).Path
    }
    [void](Invoke-Gate -Name "runner-timeout" -Executable $powerShellExe -CommandArguments @(
        "-NoProfile", "-Command", "Start-Sleep -Seconds 5"
    ) -WorkingDirectory $RepoRoot -TimeoutSeconds 1 -ExpectTimeout)

    if ($RunRustSuites) {
        [void](Invoke-Gate -Name "rust-fmt" -Executable "cargo" -CommandArguments @("fmt", "--", "--check") -WorkingDirectory $CrateRoot)
        [void](Invoke-Gate -Name "rust-check" -Executable "cargo" -CommandArguments @("check", "--locked", "--offline", "--jobs", "1") -WorkingDirectory $CrateRoot)
        [void](Invoke-Gate -Name "rust-test-debug" -Executable "cargo" -CommandArguments @("test", "--locked", "--offline", "--jobs", "1") -WorkingDirectory $CrateRoot)
        [void](Invoke-Gate -Name "rust-test-release" -Executable "cargo" -CommandArguments @("test", "--release", "--locked", "--offline", "--jobs", "1") -WorkingDirectory $CrateRoot)
        [void](Invoke-Gate -Name "rust-clippy-debug" -Executable "cargo" -CommandArguments @("clippy", "--locked", "--offline", "--all-targets", "--all-features", "--jobs", "1") -WorkingDirectory $CrateRoot)
        [void](Invoke-Gate -Name "rust-clippy-release" -Executable "cargo" -CommandArguments @("clippy", "--release", "--locked", "--offline", "--all-targets", "--all-features", "--jobs", "1") -WorkingDirectory $CrateRoot)
        [void](Invoke-Gate -Name "rust-doc" -Executable "cargo" -CommandArguments @("doc", "--locked", "--offline", "--no-deps", "--jobs", "1") -WorkingDirectory $CrateRoot)
        [void](Invoke-Gate -Name "rust-build-release" -Executable "cargo" -CommandArguments @("build", "--release", "--locked", "--offline", "--jobs", "1") -WorkingDirectory $CrateRoot)
    }

    if (-not (Test-Path -LiteralPath $NativeExe -PathType Leaf)) {
        throw "Native release executable not found: $NativeExe"
    }
    $nativeSelfTest = Invoke-Gate -Name "native-self-test" -Executable $NativeExe -CommandArguments @("self-test") -WorkingDirectory $RepoRoot -TimeoutSeconds 120
    $nativeEvents = @(
        Get-Content -LiteralPath $nativeSelfTest.stdout_path -Encoding UTF8 |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
            ForEach-Object { $_ | ConvertFrom-Json }
    )
    $selfTestEvent = $nativeEvents | Where-Object { $_.type -eq "SELF_TEST" } |
        Select-Object -Last 1
    if ($null -eq $selfTestEvent -or $selfTestEvent.ok -ne $true) {
        throw "Native self-test did not emit a successful SELF_TEST event."
    }
    if ([string]::IsNullOrWhiteSpace([string]$selfTestEvent.version) -or $null -eq $selfTestEvent.capabilities) {
        throw "Native self-test is missing version or capabilities."
    }
    $nativeStderr = Get-Content -LiteralPath $nativeSelfTest.stderr_path -Raw -Encoding UTF8
    if ($nativeStderr -match '(?i)panic|access violation|\bERROR\b') {
        throw "Native self-test stderr contains a fatal marker."
    }
    $checks.native_self_test = "PASS"

    [void](Invoke-Gate -Name "python-pip-version" -Executable $Python -CommandArguments @("-m", "pip", "--version") -WorkingDirectory $RepoRoot -TimeoutSeconds 60)
    [void](Invoke-Gate -Name "python-pip-freeze" -Executable $Python -CommandArguments @("-m", "pip", "freeze", "--all") -WorkingDirectory $RepoRoot -TimeoutSeconds 120)
    [void](Invoke-Gate -Name "python-pip-check" -Executable $Python -CommandArguments @("-m", "pip", "check") -WorkingDirectory $RepoRoot -TimeoutSeconds 120)

    $trackedPython = @(& git -C $RepoRoot ls-files -- "*.py")
    if ($LASTEXITCODE -ne 0 -or $trackedPython.Count -eq 0) {
        throw "Unable to enumerate tracked Python sources."
    }
    $compileArguments = @("-B", "-m", "compileall", "-q") + $trackedPython
    [void](Invoke-Gate -Name "python-compileall" -Executable $Python -CommandArguments $compileArguments -WorkingDirectory $RepoRoot -TimeoutSeconds 300)
    $testCount = Invoke-Gate -Name "python-test-count" -Executable $Python -CommandArguments @("-B", "scripts/check_python_test_count.py") -WorkingDirectory $RepoRoot -TimeoutSeconds 120
    $testCountEvent = Get-Content -LiteralPath $testCount.stdout_path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ([int]$testCountEvent.count -le 0) {
        throw "Python zero-test guard did not discover tests."
    }
    $checks.python_test_count = [int]$testCountEvent.count

    $pythonTests = Invoke-Gate -Name "python-tests" -Executable $Python -CommandArguments @(
        "-B", "-m", "unittest", "discover", "-s", "tests", "-p", "test_*.py", "-v"
    ) -WorkingDirectory $RepoRoot -TimeoutSeconds 300
    $pythonTestText = (Get-Content -LiteralPath $pythonTests.stdout_path -Raw -Encoding UTF8) +
        (Get-Content -LiteralPath $pythonTests.stderr_path -Raw -Encoding UTF8)
    if ($pythonTestText -notmatch 'Ran\s+(\d+)\s+tests?' -or [int]$Matches[1] -ne [int]$testCountEvent.count) {
        throw "Python test result count does not match discovery."
    }
    $checks.python_tests = [int]$Matches[1]

    $runtimeSelfTest = Invoke-Gate -Name "screen-runtime-self-test" -Executable $Python -CommandArguments @(
        "-B", "screen_runtime.py", "--self-test"
    ) -WorkingDirectory $RepoRoot -TimeoutSeconds 180
    $runtimeEvent = Get-Content -LiteralPath $runtimeSelfTest.stdout_path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($runtimeEvent.ok -ne $true -or @($runtimeEvent.checks | Where-Object { $_ -ne $true }).Count -ne 0) {
        throw "screen_runtime self-test failed structured validation."
    }
    $checks.screen_runtime_checks = @($runtimeEvent.checks).Count

    $powerShellParse = Invoke-Gate -Name "powershell-parse" -Executable $powerShellExe -CommandArguments @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
        (Join-Path $PSScriptRoot "Test-TrackedPowerShell.ps1"),
        "-RepoRoot", $RepoRoot
    ) -WorkingDirectory $RepoRoot -TimeoutSeconds 180
    $powerShellEvent = Get-Content -LiteralPath $powerShellParse.stdout_path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($powerShellEvent.passed -ne $true -or $powerShellEvent.parse_error_count -ne 0) {
        throw "PowerShell parse validation failed."
    }
    $checks.powershell_files = [int]$powerShellEvent.files

    $scriptAnalyzerModule = Get-Module -ListAvailable -Name PSScriptAnalyzer |
        Select-Object -First 1
    if ($null -ne $scriptAnalyzerModule) {
        $scriptAnalyzerCommand = @'
$issues = @(Invoke-ScriptAnalyzer -Path $args[0] -Recurse)
$issues | ConvertTo-Json -Depth 5
if ($issues.Count -ne 0) { exit 1 }
'@
        [void](Invoke-Gate -Name "powershell-script-analyzer" -Executable $powerShellExe -CommandArguments @(
            "-NoProfile", "-Command", $scriptAnalyzerCommand, $RepoRoot
        ) -WorkingDirectory $RepoRoot -TimeoutSeconds 300)
        $checks.psscriptanalyzer = "PASS"
    }
    else {
        $checks.psscriptanalyzer = "TOOL_NOT_INSTALLED"
    }

    $ciText = Get-Content -LiteralPath (Join-Path $RepoRoot ".github\workflows\ci.yml") -Raw -Encoding UTF8
    $requiredCiCommands = @(
        "cargo fmt --check",
        "cargo check --locked",
        "cargo test --locked",
        "cargo test --release --locked",
        "cargo clippy --locked --all-targets --all-features",
        "cargo clippy --release --locked --all-targets --all-features",
        "cargo doc --locked --no-deps",
        'unittest discover -s tests -p "test_*.py"',
        "scripts/check_python_test_count.py",
        "screen_runtime.py --self-test",
        "scripts/Test-ValidationRunner.ps1 -Python python"
    )
    $missingCiCommands = @($requiredCiCommands | Where-Object { $ciText.IndexOf($_, [StringComparison]::Ordinal) -lt 0 })
    if ($missingCiCommands.Count -ne 0) {
        throw "CI is missing required commands: $($missingCiCommands -join ', ')"
    }
    $checks.ci_definition = "PASS"

    $cargoLogs = @(
        $commandResults | Where-Object {
            [IO.Path]::GetFileName($_.executable) -match '^(cargo|rustup)\.exe$'
        }
    )
    $cargoHelpFindings = New-Object System.Collections.Generic.List[string]
    foreach ($cargoResult in $cargoLogs) {
        $combinedText = (Get-Content -LiteralPath $cargoResult.stdout_path -Raw -Encoding UTF8) +
            (Get-Content -LiteralPath $cargoResult.stderr_path -Raw -Encoding UTF8)
        if ($combinedText -match '(?m)^Usage:\s+cargo|^Commands:\s*$') {
            $cargoHelpFindings.Add($cargoResult.name)
        }
    }
    if ($cargoHelpFindings.Count -ne 0) {
        throw "Cargo help contamination found in: $($cargoHelpFindings -join ', ')"
    }
    $checks.cargo_help_contamination = $false
}
catch {
    $failure = $_.Exception.Message
}
finally {
    $env:PYTHONPYCACHEPREFIX = $oldCache
    $env:KIVY_HOME = $oldKivyHome
    $env:KIVY_LOG_LEVEL = $oldKivyLogLevel
    $env:KIVY_NO_FILELOG = $oldKivyNoFileLog
}

$validationEnd = Get-Date
$dumpsAfter = Get-DumpSnapshot
$newDumps = @($dumpsAfter | Where-Object { $dumpsBefore -notcontains $_ })
$mediaProcessesAfter = @(
    Get-Process -Name agoralink_media, ffmpeg, ffprobe, ffplay -ErrorAction SilentlyContinue
)
$nativeHash = if (Test-Path -LiteralPath $NativeExe -PathType Leaf) {
    (Get-FileHash -LiteralPath $NativeExe -Algorithm SHA256).Hash
}
else {
    $null
}
$summary = [ordered]@{
    type = "V0_0_12_FINAL_LOCAL_VALIDATION"
    status = if ($null -eq $failure) { "PASS" } else { "FAIL" }
    failure = $failure
    repo_root = $RepoRoot
    run_root = $RunRoot
    start_time = $validationStart.ToString("o")
    end_time = $validationEnd.ToString("o")
    elapsed_ms = [math]::Round(($validationEnd - $validationStart).TotalMilliseconds)
    git_commit = (& git -C $RepoRoot rev-parse HEAD).Trim()
    dirty_working_tree = [bool](& git -C $RepoRoot status --porcelain)
    python = (Resolve-Path -LiteralPath $Python).Path
    native_exe = $NativeExe
    native_exe_sha256 = $nativeHash
    run_rust_suites = [bool]$RunRustSuites
    commands_executed = $commandResults.Count
    commands = $commandResults.ToArray()
    checks = $checks
    psscriptanalyzer = $checks.psscriptanalyzer
    new_crash_dumps = $newDumps.Count
    new_crash_dump_paths = $newDumps
    residual_media_processes = $mediaProcessesAfter.Count
    media_processes_before = $mediaProcessesBefore.Count
    manual_gates = [ordered]@{
        gui_graceful_stop_local = "NOT_RUN"
        gui_dual_host = "NOT_RUN"
        file_append = "NOT_RUN"
        github_ci = "NOT_RUN"
    }
    release_readiness = "BLOCKED_PENDING_MANUAL_AND_GITHUB_GATES"
}
$summaryPath = Join-Path $RunRoot "validation-summary.json"
[IO.File]::WriteAllText(
    $summaryPath,
    ($summary | ConvertTo-Json -Depth 8),
    [Text.UTF8Encoding]::new($false)
)
[IO.File]::WriteAllText(
    (Join-Path $EvidenceRoot "LATEST.txt"),
    $RunRoot,
    [Text.UTF8Encoding]::new($false)
)
$summary | ConvertTo-Json -Depth 8
if ($null -ne $failure) {
    throw $failure
}

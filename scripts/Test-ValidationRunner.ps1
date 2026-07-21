[CmdletBinding()]
param(
    [string]$Python = "python",
    [string]$EvidenceRoot = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($EvidenceRoot)) {
    $base = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
        Join-Path $RepoRoot "_local_artifacts\validation_runner_smoke"
    }
    else {
        Join-Path $env:RUNNER_TEMP "agoralink-validation-runner"
    }
    $EvidenceRoot = Join-Path $base (Get-Date -Format "yyyyMMdd_HHmmss_fff")
}
New-Item -ItemType Directory -Path $EvidenceRoot -Force | Out-Null

Import-Module (Join-Path $PSScriptRoot "ValidationRunner.psm1") -Force

function Invoke-SmokeCommand {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Executable,
        [string[]]$CommandArguments = @(),
        [ValidateRange(1, 60)][int]$TimeoutSeconds = 30
    )

    return Invoke-ValidatedCommand `
        -Name $Name `
        -Executable $Executable `
        -CommandArguments $CommandArguments `
        -WorkingDirectory $RepoRoot `
        -StdoutPath (Join-Path $EvidenceRoot "$Name.stdout.log") `
        -StderrPath (Join-Path $EvidenceRoot "$Name.stderr.log") `
        -ResultPath (Join-Path $EvidenceRoot "$Name.result.json") `
        -TimeoutSeconds $TimeoutSeconds
}

$cargo = Invoke-SmokeCommand -Name "cargo-version" -Executable "cargo" `
    -CommandArguments @("--version")
if ($cargo.exit_code -ne 0 -or $cargo.timed_out) {
    throw "cargo version smoke failed"
}
$cargoText = [IO.File]::ReadAllText($cargo.stdout_path)
if ($cargoText -notmatch '^cargo\s+\d+' -or $cargoText -match '(?m)^Usage:\s+cargo') {
    throw "cargo version output is invalid or contaminated by help text"
}

$unicodeArgument = ([char]0x4E2D).ToString() + ([char]0x6587)
$expectedArguments = @("space value", $unicodeArgument, "apostrophe'value")
$argumentEcho = Invoke-SmokeCommand -Name "argument-quoting" -Executable $Python `
    -CommandArguments @(
        "-c",
        "import json,sys; print(json.dumps(sys.argv[1:]))",
        $expectedArguments[0],
        $expectedArguments[1],
        $expectedArguments[2]
    )
if ($argumentEcho.exit_code -ne 0 -or $argumentEcho.timed_out) {
    throw "argument quoting smoke failed"
}
[string[]]$actualArguments = [IO.File]::ReadAllText($argumentEcho.stdout_path) |
    ConvertFrom-Json
if (($actualArguments -join "|") -ne ($expectedArguments -join "|")) {
    throw "argument quoting did not preserve spaces, Unicode, and apostrophes"
}

$powerShellExe = (Get-Process -Id $PID).Path
$timeout = Invoke-SmokeCommand -Name "bounded-timeout" -Executable $powerShellExe `
    -CommandArguments @("-NoProfile", "-Command", "Start-Sleep -Seconds 30") `
    -TimeoutSeconds 1
if (-not $timeout.timed_out -or $timeout.exit_code -ne 124) {
    throw "bounded timeout smoke did not return exit code 124"
}
if (Get-Process -Id $timeout.process_id -ErrorAction SilentlyContinue) {
    throw "timed-out child process is still running"
}

$resultFiles = @(Get-ChildItem -LiteralPath $EvidenceRoot -Filter "*.result.json")
foreach ($resultFile in $resultFiles) {
    Get-Content -LiteralPath $resultFile.FullName -Raw -Encoding UTF8 |
        ConvertFrom-Json | Out-Null
}
if ($resultFiles.Count -ne 3) {
    throw "expected exactly three command result files"
}

[ordered]@{
    type = "VALIDATION_RUNNER_SMOKE"
    passed = $true
    commands_executed = 3
    cargo_help_contamination = $false
    timeout_exit_code = 124
    evidence_root = [IO.Path]::GetFullPath($EvidenceRoot)
} | ConvertTo-Json -Depth 4

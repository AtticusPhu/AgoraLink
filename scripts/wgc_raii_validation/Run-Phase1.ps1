param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [int]$Iterations = 10
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'WgcRaiiHarness.psm1') -Force
$Executable = (Resolve-Path -LiteralPath $Executable).Path
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$phaseRoot = Join-Path $OutputRoot 'phase1'
New-Item -ItemType Directory -Force -Path $phaseRoot | Out-Null
$dumpBefore = @(Get-CrashDumpSnapshot)

for ($iteration = 1; $iteration -le $Iterations; $iteration++) {
    $caseId = 'P1_SELF_TEST_{0:D3}' -f $iteration
    $caseDir = Join-Path $phaseRoot $caseId
    New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $started = Get-Date
    $handle = Start-LoggedNativeProcess -Executable $Executable -Arguments @('self-test')
    $result = Complete-LoggedNativeProcess -Handle $handle -TimeoutMs 30000
    Write-Utf8NoBom (Join-Path $caseDir 'stdout.jsonl') $result.Stdout
    Write-Utf8NoBom (Join-Path $caseDir 'stderr.txt') $result.Stderr
    Write-Utf8NoBom (Join-Path $caseDir 'resolved_command.txt') ($handle.ResolvedCommand + "`r`n")
    $events = @(ConvertFrom-JsonLines $result.Stdout)
    $selfTests = @($events | Where-Object { $_.type -eq 'SELF_TEST' -and $_.ok -eq $true })
    $problems = @()
    if (-not $result.Finished) { $problems += 'process deadline exceeded' }
    if ($result.ExitCode -ne 0) { $problems += "exit code=$($result.ExitCode)" }
    if ($selfTests.Count -ne 1) { $problems += "successful SELF_TEST event count=$($selfTests.Count)" }
    $status = if ($problems.Count -eq 0) { 'PASS' } else { 'FAIL' }
    $detail = if ($problems.Count -eq 0) { 'self-test ok' } else { $problems -join '; ' }
    $record = [ordered]@{
        case_id = $caseId; phase = 1; group = 'self_test'; status = $status
        started_at = $started.ToString('o'); ended_at = (Get-Date).ToString('o')
        sender_exit_code = $result.ExitCode; receiver_exit_code = $null
        detail = $detail; case_dir = $caseDir; command = $handle.ResolvedCommand
    }
    Add-CaseRecord -OutputRoot $OutputRoot -Record $record
    Write-Utf8NoBom (Join-Path $caseDir 'case_result.json') ($record | ConvertTo-Json -Depth 6)
    if ($status -ne 'PASS') {
        Write-Utf8NoBom (Join-Path $OutputRoot 'FAILURE_REPORT.txt') "phase=1`r`ncase_id=$caseId`r`nerror=$detail`r`n"
        throw "Phase 1 stopped at ${caseId}: $detail"
    }
}

$dumpAfter = @(Get-CrashDumpSnapshot)
$newDumps = @(Get-NewCrashDumps $dumpBefore $dumpAfter)
if ($newDumps.Count -gt 0) { throw "Phase 1 produced $($newDumps.Count) new crash dump(s)" }
$summary = [ordered]@{ phase = 1; status = 'PASS'; cases = $Iterations; new_crash_dumps = 0 }
Write-Utf8NoBom (Join-Path $OutputRoot 'phase1_summary.json') ($summary | ConvertTo-Json -Depth 4)
$summary

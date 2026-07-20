param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [int]$Iterations = 100,
    [int]$DurationSec = 1,
    [int]$TargetFps = 30
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'WgcRaiiHarness.psm1') -Force
$Executable = (Resolve-Path -LiteralPath $Executable).Path
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$phaseRoot = Join-Path $OutputRoot 'phase2'
New-Item -ItemType Directory -Force -Path $phaseRoot | Out-Null
$dumpBefore = @(Get-CrashDumpSnapshot)

for ($iteration = 1; $iteration -le $Iterations; $iteration++) {
    $caseId = 'P2_CAPTURE_{0:D3}' -f $iteration
    $caseDir = Join-Path $phaseRoot $caseId
    New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $started = Get-Date
    $arguments = @('capture-probe', '--duration-sec', "$DurationSec", '--target-fps', "$TargetFps")
    $handle = Start-LoggedNativeProcess -Executable $Executable -Arguments $arguments
    $result = Complete-LoggedNativeProcess -Handle $handle -TimeoutMs (($DurationSec + 15) * 1000)
    if (-not $result.Finished) { Stop-FailedNativeProcess $handle }
    Write-Utf8NoBom (Join-Path $caseDir 'stdout.jsonl') $result.Stdout
    Write-Utf8NoBom (Join-Path $caseDir 'stderr.txt') $result.Stderr
    Write-Utf8NoBom (Join-Path $caseDir 'resolved_command.txt') ($handle.ResolvedCommand + "`r`n")
    $events = @(ConvertFrom-JsonLines $result.Stdout)
    $capture = @($events | Where-Object { $_.type -eq 'CAPTURE_STATS' })
    $problems = @()
    if (-not $result.Finished) { $problems += 'process deadline exceeded' }
    if ($result.ExitCode -ne 0) { $problems += "exit code=$($result.ExitCode)" }
    if ($capture.Count -eq 0) { $problems += 'CAPTURE_STATS missing' }
    if ($capture.Count -gt 0 -and [int64]$capture[-1].raw_frames -le 0) { $problems += 'no WGC frames captured' }
    if (([string]$result.Stderr) -match 'callback cleanup error|NATIVE_SCREEN_SHUTDOWN_FAILED|failed_cleanup|worker-ownership-retained') {
        $problems += 'cleanup failure marker in stderr'
    }
    $status = if ($problems.Count -eq 0) { 'PASS' } else { 'FAIL' }
    $detail = if ($problems.Count -eq 0) { 'WGC frames captured and process exited cleanly' } else { $problems -join '; ' }
    $record = [ordered]@{
        case_id = $caseId; phase = 2; group = 'capture_probe'; status = $status
        started_at = $started.ToString('o'); ended_at = (Get-Date).ToString('o')
        sender_exit_code = $result.ExitCode; receiver_exit_code = $null
        detail = $detail; case_dir = $caseDir; command = $handle.ResolvedCommand
    }
    Add-CaseRecord -OutputRoot $OutputRoot -Record $record
    Write-Utf8NoBom (Join-Path $caseDir 'case_result.json') ($record | ConvertTo-Json -Depth 6)
    if ($status -ne 'PASS') {
        Write-Utf8NoBom (Join-Path $OutputRoot 'FAILURE_REPORT.txt') "phase=2`r`ncase_id=$caseId`r`nerror=$detail`r`n"
        throw "Phase 2 stopped at ${caseId}: $detail"
    }
}

$dumpAfter = @(Get-CrashDumpSnapshot)
$newDumps = @(Get-NewCrashDumps $dumpBefore $dumpAfter)
if ($newDumps.Count -gt 0) { throw "Phase 2 produced $($newDumps.Count) new crash dump(s)" }
$residual = @(Get-Process agoralink_media -ErrorAction SilentlyContinue)
if ($residual.Count -gt 0) { throw "Phase 2 left $($residual.Count) agoralink_media process(es)" }
$summary = [ordered]@{ phase = 2; status = 'PASS'; cases = $Iterations; new_crash_dumps = 0; residual_processes = 0 }
Write-Utf8NoBom (Join-Path $OutputRoot 'phase2_summary.json') ($summary | ConvertTo-Json -Depth 4)
$summary

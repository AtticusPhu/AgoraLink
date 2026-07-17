param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [int]$CtrlCIterations = 20,
    [int]$WindowCloseIterations = 20,
    [int]$DurationIterations = 30,
    [int]$WgcInitCancelIterations = 20,
    [int]$SenderCrashIterations = 10,
    [int]$ReceiverCrashIterations = 10,
    [int]$BasePort = 57000
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'WgcRaiiHarness.psm1') -Force
$Executable = (Resolve-Path -LiteralPath $Executable).Path
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$phaseRoot = Join-Path $OutputRoot 'phase4'
New-Item -ItemType Directory -Force -Path $phaseRoot | Out-Null
$harnessBuild = Join-Path $OutputRoot 'native_harness'
$nativeHarness = (& (Join-Path $PSScriptRoot 'Build-NativeWindowsTestHarness.ps1') -OutputDirectory $harnessBuild).FullName
$dumpBefore = @(Get-CrashDumpSnapshot)
$caseNumber = 0
Assert-UdpPortRangeAvailable -StartPort $BasePort -Count (
    $CtrlCIterations + $WindowCloseIterations + $DurationIterations +
    $WgcInitCancelIterations + $SenderCrashIterations + $ReceiverCrashIterations
)

function New-ReceiverArgs([int]$Port, [string]$Title, [int]$DurationSec = 30) {
    return @(
        'screen-recv', '--bind', '127.0.0.1', '--port', "$Port",
        '--duration-sec', "$DurationSec", '--fps', '30', '--render-backend', 'd3d11',
        '--render-scale', 'fit', '--window-mode', 'windowed', '--audio', 'off',
        '--av-sync', 'off', '--json-interval-ms', '250', '--title', $Title
    )
}

function New-SenderArgs([int]$Port, [int]$DurationSec = 30) {
    return @(
        'screen-send', '--host', '127.0.0.1', '--port', "$Port",
        '--duration-sec', "$DurationSec", '--width', '1280', '--height', '720',
        '--fps', '30', '--bitrate-mbps', '8', '--encoder', 'auto',
        '--convert-backend', 'd3d11', '--packet-pacing', 'batch', '--repair', 'nack',
        '--adaptive-quality', 'off', '--audio', 'off'
    )
}

function Save-Result($CaseDir, $Prefix, $Result) {
    if ($null -eq $Result) { return }
    Write-Utf8NoBom (Join-Path $CaseDir "$Prefix.stdout.jsonl") ([string]$Result.Stdout)
    Write-Utf8NoBom (Join-Path $CaseDir "$Prefix.stderr.txt") ([string]$Result.Stderr)
}

function Complete-AfterFailure($Handle) {
    if ($null -eq $Handle) { return $null }
    if (-not $Handle.Process.HasExited) { Stop-FailedNativeProcess $Handle }
    return Complete-LoggedNativeProcess -Handle $Handle -TimeoutMs 5000
}

function Write-Phase4Record {
    param($CaseId, $Group, $Status, $Started, $Sender, $Receiver, $Detail, $CaseDir, $Extra)
    $record = [ordered]@{
        case_id = $CaseId; phase = 4; group = $Group; status = $Status
        started_at = $Started.ToString('o'); ended_at = (Get-Date).ToString('o')
        sender_exit_code = if ($Sender -and $Sender.Process.HasExited) { $Sender.Process.ExitCode } else { $null }
        receiver_exit_code = if ($Receiver -and $Receiver.Process.HasExited) { $Receiver.Process.ExitCode } else { $null }
        detail = $Detail; case_dir = $CaseDir; extra = $Extra
    }
    Add-CaseRecord -OutputRoot $OutputRoot -Record $record
    Write-Utf8NoBom (Join-Path $CaseDir 'case_result.json') ($record | ConvertTo-Json -Depth 10)
    if ($Status -ne 'PASS') {
        Write-Utf8NoBom (Join-Path $OutputRoot 'FAILURE_REPORT.txt') (
            "phase=4`r`ncase_id=$CaseId`r`nerror=$Detail`r`ncase_dir=$CaseDir`r`n"
        )
        throw "Phase 4 stopped at ${CaseId}: $Detail"
    }
}

function Invoke-CtrlBreakSenderCase([int]$Iteration) {
    $script:caseNumber++
    $caseId = 'P4_CTRLC_{0:D3}' -f $Iteration
    $caseDir = Join-Path $phaseRoot $caseId
    New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $port = $BasePort + $script:caseNumber
    $started = Get-Date
    $receiver = $null
    $receiverResult = $null
    $status = 'FAIL'; $detail = ''; $harnessJson = $null
    try {
        $receiverArgs = New-ReceiverArgs $port $caseId 30
        $senderArgs = New-SenderArgs $port 60
        $receiver = Start-LoggedNativeProcess -Executable $Executable -Arguments $receiverArgs
        Start-Sleep -Milliseconds 600
        if ($receiver.Process.HasExited) { throw 'receiver exited before sender startup' }
        $senderOut = Join-Path $caseDir 'sender.stdout.jsonl'
        $senderErr = Join-Path $caseDir 'sender.stderr.txt'
        $harnessArgs = @(
            'ctrl-break', '--exe', $Executable, '--stdout', $senderOut, '--stderr', $senderErr,
            '--delay-ms', '1800', '--timeout-ms', '15000', '--'
        ) + $senderArgs
        $harnessText = (& $nativeHarness @harnessArgs 2>&1 | Out-String).Trim()
        $harnessExit = $LASTEXITCODE
        Write-Utf8NoBom (Join-Path $caseDir 'control_harness.json') $harnessText
        $harnessJson = $harnessText | ConvertFrom-Json
        $receiverResult = Complete-LoggedNativeProcess -Handle $receiver -TimeoutMs 15000
        Save-Result $caseDir 'receiver' $receiverResult
        Write-Utf8NoBom (Join-Path $caseDir 'resolved_commands.txt') (
            "RECEIVER: $($receiver.ResolvedCommand)`r`nHARNESS: `"$nativeHarness`" $($harnessArgs -join ' ')`r`n"
        )
        if ($harnessExit -ne 0 -or $harnessJson.success -ne $true) { throw "CTRL_BREAK harness failed: $harnessText" }
        $senderText = Get-Content -LiteralPath $senderOut -Raw -Encoding UTF8
        $senderCheck = Test-CleanTerminal (ConvertFrom-JsonLines $senderText) ([int]$harnessJson.exit_code) @('ctrl_c')
        $receiverCheck = Test-CleanTerminal (ConvertFrom-JsonLines $receiverResult.Stdout) $receiverResult.ExitCode @('peer_closed')
        $problems = @($senderCheck.Messages) + @($receiverCheck.Messages)
        if ($senderCheck.Terminal -and [int64]$senderCheck.Terminal.stream_close_sent -lt 1) { $problems += 'sender stream_close_sent < 1' }
        if ($receiverCheck.Terminal -and [int64]$receiverCheck.Terminal.stream_close_received -lt 1) { $problems += 'receiver stream_close_received < 1' }
        if ($receiverCheck.Terminal -and [int64]$receiverCheck.Terminal.stream_close_ack_sent -lt 1) { $problems += 'receiver stream_close_ack_sent < 1' }
        if ($problems.Count -gt 0) { throw ($problems -join '; ') }
        $status = 'PASS'; $detail = 'real CTRL_BREAK_EVENT produced ctrl_c and peer_closed terminal events'
    } catch {
        $detail = $_.Exception.Message
        $receiverResult = Complete-AfterFailure $receiver
        Save-Result $caseDir 'receiver' $receiverResult
    }
    Write-Phase4Record $caseId 'ctrl_c_sender' $status $started $null $receiver $detail $caseDir $harnessJson
}

function Invoke-WindowCloseReceiverCase([int]$Iteration) {
    $script:caseNumber++
    $caseId = 'P4_WMCLOSE_{0:D3}' -f $Iteration
    $caseDir = Join-Path $phaseRoot $caseId
    New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $port = $BasePort + $script:caseNumber
    $started = Get-Date
    $receiver = $null; $sender = $null; $receiverResult = $null; $senderResult = $null
    $status = 'FAIL'; $detail = ''; $closeJson = $null
    try {
        $receiver = Start-LoggedNativeProcess -Executable $Executable -Arguments (New-ReceiverArgs $port $caseId 60)
        Start-Sleep -Milliseconds 600
        $sender = Start-LoggedNativeProcess -Executable $Executable -Arguments (New-SenderArgs $port 60)
        Start-Sleep -Milliseconds 1800
        $closeText = (& $nativeHarness wm-close --pid $receiver.Process.Id --title-contains $caseId --timeout-ms 10000 2>&1 | Out-String).Trim()
        $closeExit = $LASTEXITCODE
        Write-Utf8NoBom (Join-Path $caseDir 'window_close_harness.json') $closeText
        $closeJson = $closeText | ConvertFrom-Json
        if ($closeExit -ne 0 -or $closeJson.success -ne $true) { throw "WM_CLOSE harness failed: $closeText" }
        $receiverResult = Complete-LoggedNativeProcess -Handle $receiver -TimeoutMs 15000
        $senderResult = Complete-LoggedNativeProcess -Handle $sender -TimeoutMs 15000
        Save-Result $caseDir 'receiver' $receiverResult; Save-Result $caseDir 'sender' $senderResult
        Write-Utf8NoBom (Join-Path $caseDir 'resolved_commands.txt') (
            "RECEIVER: $($receiver.ResolvedCommand)`r`nSENDER: $($sender.ResolvedCommand)`r`n"
        )
        $receiverCheck = Test-CleanTerminal (ConvertFrom-JsonLines $receiverResult.Stdout) $receiverResult.ExitCode @('window_closed')
        $senderCheck = Test-CleanTerminal (ConvertFrom-JsonLines $senderResult.Stdout) $senderResult.ExitCode @('peer_closed')
        $problems = @($receiverCheck.Messages) + @($senderCheck.Messages)
        if ($receiverCheck.Terminal -and [int64]$receiverCheck.Terminal.stream_close_sent -lt 1) { $problems += 'receiver stream_close_sent < 1' }
        if ($receiverCheck.Terminal -and $receiverCheck.Terminal.stream_close_ack_received -ne $true) { $problems += 'receiver did not receive matching close ACK' }
        if ($problems.Count -gt 0) { throw ($problems -join '; ') }
        $status = 'PASS'; $detail = 'real WM_CLOSE produced window_closed and peer_closed terminal events'
    } catch {
        $detail = $_.Exception.Message
        $receiverResult = Complete-AfterFailure $receiver; $senderResult = Complete-AfterFailure $sender
        Save-Result $caseDir 'receiver' $receiverResult; Save-Result $caseDir 'sender' $senderResult
    }
    Write-Phase4Record $caseId 'window_close_receiver' $status $started $sender $receiver $detail $caseDir $closeJson
}

function Invoke-DurationCase([int]$Iteration) {
    $script:caseNumber++
    $caseId = 'P4_DURATION_{0:D3}' -f $Iteration
    $caseDir = Join-Path $phaseRoot $caseId; New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $port = $BasePort + $script:caseNumber; $started = Get-Date
    $receiver = $null; $sender = $null; $status = 'FAIL'; $detail = ''
    try {
        $receiver = Start-LoggedNativeProcess -Executable $Executable -Arguments (New-ReceiverArgs $port $caseId 8)
        Start-Sleep -Milliseconds 500
        $sender = Start-LoggedNativeProcess -Executable $Executable -Arguments (New-SenderArgs $port 2)
        $senderResult = Complete-LoggedNativeProcess $sender 20000
        $receiverResult = Complete-LoggedNativeProcess $receiver 20000
        Save-Result $caseDir 'sender' $senderResult; Save-Result $caseDir 'receiver' $receiverResult
        $senderCheck = Test-CleanTerminal (ConvertFrom-JsonLines $senderResult.Stdout) $senderResult.ExitCode @('duration')
        $receiverCheck = Test-CleanTerminal (ConvertFrom-JsonLines $receiverResult.Stdout) $receiverResult.ExitCode @('peer_closed', 'duration')
        $problems = @($senderCheck.Messages) + @($receiverCheck.Messages)
        if ($problems.Count -gt 0) { throw ($problems -join '; ') }
        $status = 'PASS'; $detail = 'program duration path exited cleanly'
    } catch {
        $detail = $_.Exception.Message; Stop-FailedNativeProcess $sender; Stop-FailedNativeProcess $receiver
    }
    Write-Phase4Record $caseId 'duration' $status $started $sender $receiver $detail $caseDir $null
}

function Invoke-WgcInitCancelCase([int]$Iteration) {
    $script:caseNumber++
    $caseId = 'P4_WGC_INIT_CANCEL_{0:D3}' -f $Iteration
    $caseDir = Join-Path $phaseRoot $caseId; New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $port = $BasePort + $script:caseNumber; $started = Get-Date; $status = 'FAIL'; $detail = ''; $harnessJson = $null
    try {
        $senderArgs = New-SenderArgs $port 60
        $senderOut = Join-Path $caseDir 'sender.stdout.jsonl'; $senderErr = Join-Path $caseDir 'sender.stderr.txt'
        $args = @('ctrl-break', '--exe', $Executable, '--stdout', $senderOut, '--stderr', $senderErr,
            '--delay-ms', '100', '--timeout-ms', '15000', '--') + $senderArgs
        $text = (& $nativeHarness @args 2>&1 | Out-String).Trim(); $exit = $LASTEXITCODE
        Write-Utf8NoBom (Join-Path $caseDir 'control_harness.json') $text
        $harnessJson = $text | ConvertFrom-Json
        if ($exit -ne 0 -or $harnessJson.success -ne $true) { throw "WGC init CTRL_BREAK failed: $text" }
        $check = Test-CleanTerminal (ConvertFrom-JsonLines (Get-Content $senderOut -Raw -Encoding UTF8)) ([int]$harnessJson.exit_code) @('ctrl_c')
        if (-not $check.Passed) { throw ($check.Messages -join '; ') }
        $status = 'PASS'; $detail = 'real CTRL_BREAK during startup/init converged through terminal cleanup'
    } catch { $detail = $_.Exception.Message }
    Write-Phase4Record $caseId 'wgc_init_cancel' $status $started $null $null $detail $caseDir $harnessJson
}

function Invoke-PeerCrashCase([int]$Iteration, [bool]$KillSender) {
    $script:caseNumber++
    $group = if ($KillSender) { 'SENDER_CRASH' } else { 'RECEIVER_CRASH' }
    $caseId = 'P4_{0}_{1:D3}' -f $group, $Iteration
    $caseDir = Join-Path $phaseRoot $caseId; New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $port = $BasePort + $script:caseNumber; $started = Get-Date
    $receiver = $null; $sender = $null; $status = 'FAIL'; $detail = ''
    try {
        $receiver = Start-LoggedNativeProcess -Executable $Executable -Arguments (New-ReceiverArgs $port $caseId 30)
        Start-Sleep -Milliseconds 500
        $sender = Start-LoggedNativeProcess -Executable $Executable -Arguments (New-SenderArgs $port 30)
        Start-Sleep -Milliseconds 1500
        $victim = if ($KillSender) { $sender } else { $receiver }
        $victim.Process.Kill()
        $null = $victim.Process.WaitForExit(5000)
        $survivor = if ($KillSender) { $receiver } else { $sender }
        $survivorResult = Complete-LoggedNativeProcess $survivor 15000
        $victimResult = Complete-LoggedNativeProcess $victim 5000
        if ($KillSender) { $receiverResult = $survivorResult; $senderResult = $victimResult }
        else { $senderResult = $survivorResult; $receiverResult = $victimResult }
        Save-Result $caseDir 'sender' $senderResult; Save-Result $caseDir 'receiver' $receiverResult
        $check = Test-CleanTerminal (ConvertFrom-JsonLines $survivorResult.Stdout) $survivorResult.ExitCode @('peer_timeout')
        if (-not $check.Passed) { throw ($check.Messages -join '; ') }
        $status = 'PASS'; $detail = 'forced peer crash converged through peer_timeout; force was used only for crash simulation'
    } catch {
        $detail = $_.Exception.Message; Stop-FailedNativeProcess $sender; Stop-FailedNativeProcess $receiver
    }
    Write-Phase4Record $caseId ($group.ToLowerInvariant()) $status $started $sender $receiver $detail $caseDir $null
}

for ($i = 1; $i -le $CtrlCIterations; $i++) { Invoke-CtrlBreakSenderCase $i }
for ($i = 1; $i -le $WindowCloseIterations; $i++) { Invoke-WindowCloseReceiverCase $i }
for ($i = 1; $i -le $DurationIterations; $i++) { Invoke-DurationCase $i }
for ($i = 1; $i -le $WgcInitCancelIterations; $i++) { Invoke-WgcInitCancelCase $i }
for ($i = 1; $i -le $SenderCrashIterations; $i++) { Invoke-PeerCrashCase $i $true }
for ($i = 1; $i -le $ReceiverCrashIterations; $i++) { Invoke-PeerCrashCase $i $false }

$dumpAfter = @(Get-CrashDumpSnapshot); $newDumps = @(Get-NewCrashDumps $dumpBefore $dumpAfter)
$residual = @(Get-Process -Name agoralink_media -ErrorAction SilentlyContinue | Select-Object Id, ProcessName, Path)
if ($newDumps.Count -gt 0 -or $residual.Count -gt 0) {
    throw "Phase 4 final gate failed: new_dumps=$($newDumps.Count), residual_processes=$($residual.Count)"
}
$summary = [ordered]@{
    phase = 4; status = 'PARTIAL'; executed_cases = $caseNumber
    ctrl_c_sender = $CtrlCIterations; window_close_receiver = $WindowCloseIterations
    duration = $DurationIterations; wgc_init_cancel = $WgcInitCancelIterations
    sender_crash = $SenderCrashIterations; receiver_crash = $ReceiverCrashIterations
    close_ack_first_loss = 'covered_by_deterministic_loopback_test_only'
    transition_preparation_ctrl_c = 'MANUAL_REQUIRED_no_deterministic_runtime_trigger'
    transition_window_close = 'MANUAL_REQUIRED_no_deterministic_runtime_trigger'
    new_crash_dumps = 0; residual_processes = 0
}
Write-Utf8NoBom (Join-Path $OutputRoot 'phase4_summary.json') ($summary | ConvertTo-Json -Depth 5)
$summary

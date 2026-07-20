param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [int]$CpuFitIterations = 30,
    [int]$D3D11FitIterations = 50,
    [int]$Exact720Iterations = 10,
    [int]$Exact1080Iterations = 10,
    [int]$SenderDurationSec = 3,
    [int]$ReceiverDurationSec = 8,
    [int]$BasePort = 56000
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'WgcRaiiHarness.psm1') -Force
$Executable = (Resolve-Path -LiteralPath $Executable).Path
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$phaseRoot = Join-Path $OutputRoot 'phase3'
New-Item -ItemType Directory -Force -Path $phaseRoot | Out-Null
$dumpBefore = @(Get-CrashDumpSnapshot)
$caseNumber = 0
Assert-UdpPortRangeAvailable -StartPort $BasePort -Count (
    $CpuFitIterations + $D3D11FitIterations + $Exact720Iterations + $Exact1080Iterations
)

function Invoke-Phase3Case {
    param(
        [string]$Group,
        [string]$ConvertBackend,
        [string]$RenderScale,
        [int]$Width,
        [int]$Height,
        [int]$Iteration
    )
    $script:caseNumber++
    $caseId = 'P3_{0}_{1:D3}' -f $Group.ToUpperInvariant(), $Iteration
    $caseDir = Join-Path $phaseRoot $caseId
    New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    $port = $BasePort + $script:caseNumber
    $started = Get-Date
    $receiverArgs = @(
        'screen-recv', '--bind', '127.0.0.1', '--port', "$port",
        '--duration-sec', "$ReceiverDurationSec", '--fps', '30',
        '--render-backend', 'd3d11', '--render-scale', $RenderScale,
        '--window-mode', 'windowed', '--audio', 'off', '--av-sync', 'off',
        '--adaptive-feedback-ms', '1000', '--json-interval-ms', '500', '--title', $caseId
    )
    $senderArgs = @(
        'screen-send', '--host', '127.0.0.1', '--port', "$port",
        '--duration-sec', "$SenderDurationSec", '--width', "$Width", '--height', "$Height",
        '--fps', '30', '--bitrate-mbps', '8', '--encoder', 'auto',
        '--convert-backend', $ConvertBackend, '--packet-pacing', 'batch',
        '--repair', 'nack', '--adaptive-quality', 'off', '--audio', 'off'
    )
    $receiver = $null
    $sender = $null
    $status = 'FAIL'
    $detail = ''
    try {
        $receiver = Start-LoggedNativeProcess -Executable $Executable -Arguments $receiverArgs
        Start-Sleep -Milliseconds 600
        if ($receiver.Process.HasExited) { throw 'receiver exited before sender startup' }
        $sender = Start-LoggedNativeProcess -Executable $Executable -Arguments $senderArgs
        $senderResult = Complete-LoggedNativeProcess -Handle $sender -TimeoutMs 30000
        $receiverResult = Complete-LoggedNativeProcess -Handle $receiver -TimeoutMs 30000
        if (-not $senderResult.Finished) { throw 'sender did not exit before case deadline' }
        if (-not $receiverResult.Finished) { throw 'receiver did not exit before case deadline' }
        Write-Utf8NoBom (Join-Path $caseDir 'sender.stdout.jsonl') $senderResult.Stdout
        Write-Utf8NoBom (Join-Path $caseDir 'sender.stderr.txt') $senderResult.Stderr
        Write-Utf8NoBom (Join-Path $caseDir 'receiver.stdout.jsonl') $receiverResult.Stdout
        Write-Utf8NoBom (Join-Path $caseDir 'receiver.stderr.txt') $receiverResult.Stderr
        Write-Utf8NoBom (Join-Path $caseDir 'resolved_commands.txt') (
            "RECEIVER: $($receiver.ResolvedCommand)`r`nSENDER: $($sender.ResolvedCommand)`r`n"
        )
        $senderCheck = Test-CleanTerminal (ConvertFrom-JsonLines $senderResult.Stdout) $senderResult.ExitCode @('duration')
        $receiverCheck = Test-CleanTerminal (ConvertFrom-JsonLines $receiverResult.Stdout) $receiverResult.ExitCode @('peer_closed', 'duration')
        $problems = @($senderCheck.Messages) + @($receiverCheck.Messages)
        if ($null -eq $receiverCheck.Terminal -or [int64]$receiverCheck.Terminal.frames_rendered -le 0) {
            $problems += 'receiver rendered no frames'
        }
        if ($RenderScale -eq 'exact' -and $null -ne $receiverCheck.Terminal) {
            if ([int]$receiverCheck.Terminal.render_client_width -ne $Width -or
                [int]$receiverCheck.Terminal.render_client_height -ne $Height) {
                $problems += "exact client mismatch: $($receiverCheck.Terminal.render_client_width)x$($receiverCheck.Terminal.render_client_height), expected=${Width}x${Height}"
            }
            if ($receiverCheck.Terminal.render_scaled -ne $false) {
                $problems += 'exact render unexpectedly reported render_scaled=true'
            }
        }
        if ($problems.Count -gt 0) { throw ($problems -join '; ') }
        $status = 'PASS'
        $detail = 'clean terminal events, workers joined, frames rendered'
    } catch {
        $detail = $_.Exception.Message
        Stop-FailedNativeProcess $sender
        Stop-FailedNativeProcess $receiver
        Write-Utf8NoBom (Join-Path $OutputRoot 'FAILURE_REPORT.txt') (
            "phase=3`r`ncase_id=$caseId`r`nerror=$detail`r`ncase_dir=$caseDir`r`n"
        )
    } finally {
        $ended = Get-Date
        $record = [ordered]@{
            case_id = $caseId; phase = 3; group = $Group; status = $status
            started_at = $started.ToString('o'); ended_at = $ended.ToString('o')
            sender_exit_code = if ($null -ne $sender -and $sender.Process.HasExited) { $sender.Process.ExitCode } else { $null }
            receiver_exit_code = if ($null -ne $receiver -and $receiver.Process.HasExited) { $receiver.Process.ExitCode } else { $null }
            detail = $detail; convert_backend = $ConvertBackend; render_scale = $RenderScale
            width = $Width; height = $Height; port = $port; case_dir = $caseDir
            receiver_command = if ($receiver) { $receiver.ResolvedCommand } else { '' }
            sender_command = if ($sender) { $sender.ResolvedCommand } else { '' }
        }
        Add-CaseRecord -OutputRoot $OutputRoot -Record $record
        Write-Utf8NoBom (Join-Path $caseDir 'case_result.json') ($record | ConvertTo-Json -Depth 8)
    }
    if ($status -ne 'PASS') { throw "Phase 3 stopped at ${caseId}: $detail" }
}

for ($i = 1; $i -le $CpuFitIterations; $i++) {
    Invoke-Phase3Case 'CPU_FIT' 'cpu' 'fit' 1280 720 $i
}
for ($i = 1; $i -le $D3D11FitIterations; $i++) {
    Invoke-Phase3Case 'D3D11_FIT' 'd3d11' 'fit' 1280 720 $i
}
for ($i = 1; $i -le $Exact720Iterations; $i++) {
    Invoke-Phase3Case 'EXACT_720' 'd3d11' 'exact' 1280 720 $i
}
for ($i = 1; $i -le $Exact1080Iterations; $i++) {
    Invoke-Phase3Case 'EXACT_1080' 'd3d11' 'exact' 1920 1080 $i
}

$dumpAfter = @(Get-CrashDumpSnapshot)
$newDumps = @(Get-NewCrashDumps $dumpBefore $dumpAfter)
if ($newDumps.Count -gt 0) {
    Write-Utf8NoBom (Join-Path $OutputRoot 'phase3_new_dumps.json') ($newDumps | ConvertTo-Json -Depth 4)
    throw "Phase 3 produced $($newDumps.Count) new crash dump(s)"
}
$summary = [ordered]@{
    phase = 3; status = 'PASS'; cases = $caseNumber
    cpu_fit = $CpuFitIterations; d3d11_fit = $D3D11FitIterations
    exact_1280x720 = $Exact720Iterations; exact_1920x1080 = $Exact1080Iterations
    new_crash_dumps = 0
}
Write-Utf8NoBom (Join-Path $OutputRoot 'phase3_summary.json') ($summary | ConvertTo-Json -Depth 4)
$summary

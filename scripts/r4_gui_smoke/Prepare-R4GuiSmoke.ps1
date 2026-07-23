param(
    [Parameter(Mandatory = $true)][string]$PortableDir,
    [Parameter(Mandatory = $true)][ValidateSet("sender", "receiver")][string]$Role,
    [string]$TestId = "R4_GUI_SMOKE_20260720_R1",
    [string]$EvidenceRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ExpectedNativeHash = "D0CEE722185FF1E294894557C008CA96B37FF8C75F909162DB7B658286C8AE9D"
$PortableDir = (Resolve-Path -LiteralPath $PortableDir).Path
if ([string]::IsNullOrWhiteSpace($EvidenceRoot)) {
    $EvidenceRoot = Join-Path (Get-Location).Path "r4_gui_smoke_evidence"
}
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$CaseDir = Join-Path ([IO.Path]::GetFullPath($EvidenceRoot)) ("{0}_{1}_{2}" -f $TestId, $Role, $timestamp)
if (Test-Path -LiteralPath $CaseDir) {
    throw "Evidence directory already exists: $CaseDir"
}
New-Item -ItemType Directory -Path $CaseDir -Force | Out-Null

$GuiExe = Join-Path $PortableDir "AgoraLink.exe"
$NativeExe = Join-Path $PortableDir "_internal\tools\agoralink_media\agoralink_media.exe"
foreach ($required in @($GuiExe, $NativeExe, (Join-Path $PortableDir "BUILD_INFO.json"))) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required portable file is missing: $required"
    }
}
$NativeHash = (Get-FileHash -LiteralPath $NativeExe -Algorithm SHA256).Hash.ToUpperInvariant()
if ($NativeHash -ne $ExpectedNativeHash) {
    throw "Portable native EXE is not the R4 build. Expected $ExpectedNativeHash, got $NativeHash"
}

$ProcessesBefore = @(Get-Process AgoraLink, agoralink_media -ErrorAction SilentlyContinue |
    Select-Object Id, ProcessName, Path, StartTime)
$PortsBefore = @()
try {
    $PortsBefore = @(Get-NetUDPEndpoint -ErrorAction Stop |
        Where-Object { $_.LocalPort -eq 9999 -or ($_.LocalPort -ge 55000 -and $_.LocalPort -le 55999) } |
        Select-Object LocalAddress, LocalPort, OwningProcess)
}
catch {
    $PortsBefore = @([ordered]@{ error = $_.Exception.Message })
}
$CrashDumpDir = "C:\CrashDumps"
$DumpsBefore = @()
if (Test-Path -LiteralPath $CrashDumpDir -PathType Container) {
    $DumpsBefore = @(Get-ChildItem -LiteralPath $CrashDumpDir -File -ErrorAction SilentlyContinue |
        Select-Object Name, Length, LastWriteTimeUtc)
}

$RoleSteps = if ($Role -eq "sender") {
@"
Sender (A) steps
1. Launch $GuiExe.
2. Confirm the fresh/default preset is R4 Default: 1920x1080, 60 FPS, 22 Mbps, NACK, adaptive off.
3. Select the receiver device or enter the receiver IP through the existing GUI flow.
4. Start screen sharing and run for at least 60 seconds.
5. Click the GUI Stop control. Wait five seconds, then start the same share again for at least 60 seconds.
6. During a separate file-transfer regression, use Send file again while the first transfer is active to create a second independent transfer task.
7. Record any NATIVE_SCREEN_ERROR, peer_timeout, retained worker, UI freeze, duplicate task, or missing task as FAIL.
"@
}
else {
@"
Receiver (B) steps
1. Launch $GuiExe.
2. Confirm the fresh/default preset is R4 Default: 1920x1080, 60 FPS, 22 Mbps, NACK, adaptive off.
3. Accept the screen offer through the existing GUI flow and wait for the viewer.
4. Confirm picture, QSV/D3D11 native stats, and at least 60 seconds of operation.
5. Observe sender-initiated stop, then accept the restarted session.
6. Stop from the GUI or close the viewer window; confirm the sender reports peer_closed rather than peer_timeout.
7. Record residual AgoraLink/agoralink_media processes or a new DMP as FAIL.
"@
}

$Instructions = @"
AgoraLink R4 GUI dual-host smoke
Test ID: $TestId
Role: $Role
Portable: $PortableDir
Native SHA-256: $NativeHash

$RoleSteps
After testing run:
  .\scripts\r4_gui_smoke\Collect-R4GuiSmoke.ps1 -EvidenceDir "$CaseDir" -Role $Role

This script does not modify Windows Firewall. Configure the existing product firewall rule manually if required.
"@
$Instructions | Set-Content -LiteralPath (Join-Path $CaseDir "MANUAL_STEPS.txt") -Encoding UTF8

$PrepareSummary = [ordered]@{
    test_id = $TestId
    role = $Role
    prepared_at = (Get-Date).ToUniversalTime().ToString("o")
    portable_dir = $PortableDir
    gui_exe = $GuiExe
    gui_exe_sha256 = (Get-FileHash -LiteralPath $GuiExe -Algorithm SHA256).Hash
    native_exe = $NativeExe
    native_exe_sha256 = $NativeHash
    symbols_in_portable = $false
    processes_before = $ProcessesBefore
    udp_ports_before = $PortsBefore
    crash_dump_dir = $CrashDumpDir
    crash_dumps_before = $DumpsBefore
    firewall_modified = $false
    status = "READY_FOR_USER_EXECUTION"
}
$PrepareSummary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $CaseDir "prepare_summary.json") -Encoding UTF8
$ProcessesBefore | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $CaseDir "processes_before.json") -Encoding UTF8
$PortsBefore | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $CaseDir "udp_ports_before.json") -Encoding UTF8
$DumpsBefore | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $CaseDir "crash_dumps_before.json") -Encoding UTF8

Write-Output $CaseDir

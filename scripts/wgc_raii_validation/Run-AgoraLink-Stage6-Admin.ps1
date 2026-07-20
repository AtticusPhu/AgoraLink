param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [int]$BasePort = 56000
)

$ErrorActionPreference = 'Stop'
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'MANUAL_REQUIRED: Phase 6 must run from an elevated x64 PowerShell session.'
}

$Executable = (Resolve-Path -LiteralPath $Executable).Path
if ([IO.Path]::GetFileName($Executable) -ne 'agoralink_media.exe') {
    throw 'Phase 6 target must be named agoralink_media.exe.'
}
$AppVerifier = 'C:\Windows\System32\appverif.exe'
$GFlags = "${env:ProgramFiles(x86)}\Windows Kits\10\Debuggers\x64\gflags.exe"
foreach ($tool in @($AppVerifier, $GFlags)) {
    if (-not (Test-Path -LiteralPath $tool -PathType Leaf)) { throw "Required tool not found: $tool" }
}

Import-Module (Join-Path $PSScriptRoot 'WgcRaiiHarness.psm1') -Force
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$snapshotRoot = Join-Path $OutputRoot 'configuration_snapshots'
New-Item -ItemType Directory -Force -Path $snapshotRoot | Out-Null
$targetName = 'agoralink_media.exe'
$ifeoKey = 'HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\agoralink_media.exe'
$ifeoPsPath = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\agoralink_media.exe'
$registryBackup = Join-Path $snapshotRoot 'ifeo_original.reg'

function Save-CommandSnapshot([string]$Path, [scriptblock]$Command) {
    $text = & $Command 2>&1 | Out-String
    Write-Utf8NoBom $Path $text
}

function Save-WerSnapshot([string]$Path) {
    $key = 'HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\agoralink_media.exe'
    $value = if (Test-Path -LiteralPath $key) { Get-ItemProperty -LiteralPath $key } else { $null }
    Write-Utf8NoBom $Path ($value | ConvertTo-Json -Depth 6)
}

$hadIfeoKey = Test-Path -LiteralPath $ifeoPsPath
if ($hadIfeoKey) {
    & reg.exe export $ifeoKey $registryBackup /y | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not export original IFEO configuration.' }
}

Save-CommandSnapshot (Join-Path $snapshotRoot 'appverif_before.txt') { & $AppVerifier -query '*' -for $targetName }
Save-CommandSnapshot (Join-Path $snapshotRoot 'gflags_before.txt') { & $GFlags /p }
Save-WerSnapshot (Join-Path $snapshotRoot 'wer_before.json')
Write-Utf8NoBom (Join-Path $snapshotRoot 'crash_dumps_before.json') (
    @(Get-CrashDumpSnapshot) | ConvertTo-Json -Depth 5
)

$completed = $false
try {
    # The CLI exposes the Basics components as the individual Heaps, Handles,
    # and Locks tests. Keep the exact enabled list in the snapshot.
    & $AppVerifier -enable Heaps Exceptions Handles Locks Memory TLS Leak -for $targetName
    if ($LASTEXITCODE -ne 0) { throw 'Application Verifier enable failed.' }
    & $GFlags /p /enable $targetName /full
    if ($LASTEXITCODE -ne 0) { throw 'GFlags full page heap enable failed.' }

    Save-CommandSnapshot (Join-Path $snapshotRoot 'appverif_enabled.txt') { & $AppVerifier -query '*' -for $targetName }
    Save-CommandSnapshot (Join-Path $snapshotRoot 'gflags_enabled.txt') { & $GFlags /p }

    & (Join-Path $PSScriptRoot 'Run-Phase2.ps1') `
        -Executable $Executable -OutputRoot (Join-Path $OutputRoot 'capture_probe') `
        -Iterations 20 -DurationSec 1 -TargetFps 30
    & (Join-Path $PSScriptRoot 'Run-Phase3.ps1') `
        -Executable $Executable -OutputRoot (Join-Path $OutputRoot 'loopback') `
        -CpuFitIterations 10 -D3D11FitIterations 20 `
        -Exact720Iterations 5 -Exact1080Iterations 5 -BasePort $BasePort
    & (Join-Path $PSScriptRoot 'Run-Phase4.ps1') `
        -Executable $Executable -OutputRoot (Join-Path $OutputRoot 'shutdown') `
        -CtrlCIterations 10 -WindowCloseIterations 10 -DurationIterations 10 `
        -WgcInitCancelIterations 10 -SenderCrashIterations 0 -ReceiverCrashIterations 0 `
        -BasePort ($BasePort + 100)

    Save-CommandSnapshot (Join-Path $OutputRoot 'appverif_log_export.txt') {
        & $AppVerifier -export log -for $targetName -with "To=$(Join-Path $OutputRoot 'appverif_latest.xml')"
    }
    $completed = $true
} finally {
    & $GFlags /p /disable $targetName 2>&1 | Out-File -LiteralPath (Join-Path $snapshotRoot 'gflags_disable.txt') -Encoding utf8
    & $AppVerifier -disable '*' -for $targetName 2>&1 | Out-File -LiteralPath (Join-Path $snapshotRoot 'appverif_disable.txt') -Encoding utf8

    if (Test-Path -LiteralPath $ifeoPsPath) {
        Remove-Item -LiteralPath $ifeoPsPath -Recurse -Force -ErrorAction Stop
    }
    if ($hadIfeoKey) {
        & reg.exe import $registryBackup | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Failed to restore original IFEO configuration.' }
    }
    Save-CommandSnapshot (Join-Path $snapshotRoot 'appverif_restored.txt') { & $AppVerifier -query '*' -for $targetName }
    Save-CommandSnapshot (Join-Path $snapshotRoot 'gflags_restored.txt') { & $GFlags /p }
    Save-WerSnapshot (Join-Path $snapshotRoot 'wer_after.json')
    Write-Utf8NoBom (Join-Path $snapshotRoot 'crash_dumps_after.json') (
        @(Get-CrashDumpSnapshot) | ConvertTo-Json -Depth 5
    )
}

if ($completed) {
    $summary = [ordered]@{
        phase = 6
        status = 'PASS'
        appverifier_tests = @('Heaps', 'Exceptions', 'Handles', 'Locks', 'Memory', 'TLS', 'Leak')
        full_page_heap = $true
        transition_cases = 'MANUAL_REQUIRED_requires_two_real_windows_hosts'
        configuration_restored = $true
    }
    Write-Utf8NoBom (Join-Path $OutputRoot 'phase6_summary.json') ($summary | ConvertTo-Json -Depth 6)
    $summary
}

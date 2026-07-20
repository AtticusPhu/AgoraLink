param(
    [Parameter(Mandatory = $true)][string]$EvidenceDir,
    [Parameter(Mandatory = $true)][ValidateSet("sender", "receiver")][string]$Role,
    [string]$GuiLogDir = "",
    [string]$ConfigPath = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$EvidenceDir = (Resolve-Path -LiteralPath $EvidenceDir).Path
$PreparePath = Join-Path $EvidenceDir "prepare_summary.json"
if (-not (Test-Path -LiteralPath $PreparePath -PathType Leaf)) {
    throw "prepare_summary.json is missing: $PreparePath"
}
$Prepare = Get-Content -LiteralPath $PreparePath -Raw | ConvertFrom-Json
if ([string]$Prepare.role -ne $Role) {
    throw "Role mismatch: prepared=$($Prepare.role), requested=$Role"
}

$UserDataDir = Join-Path ($env:LOCALAPPDATA) "AgoraLink"
if ([string]::IsNullOrWhiteSpace($GuiLogDir)) {
    $GuiLogDir = Join-Path $UserDataDir "debug"
}
if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    $ConfigPath = Join-Path $UserDataDir "gui_settings.json"
}
$CollectedLogs = Join-Path $EvidenceDir "logs"
New-Item -ItemType Directory -Path $CollectedLogs -Force | Out-Null
if (Test-Path -LiteralPath $GuiLogDir -PathType Container) {
    Get-ChildItem -LiteralPath $GuiLogDir -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -in @(".log", ".txt", ".jsonl") } |
        ForEach-Object {
            $relative = $_.FullName.Substring(([IO.Path]::GetFullPath($GuiLogDir).TrimEnd('\') + '\').Length)
            $destination = Join-Path $CollectedLogs $relative
            New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
            Copy-Item -LiteralPath $_.FullName -Destination $destination -Force
        }
}

function Protect-ConfigValue {
    param([object]$Value, [string]$KeyName = "")
    if ($KeyName -match '(?i)(password|passphrase|secret|private|token|pin|identity_key|peer_ip|server_ip|host|address|nickname|message|chat_text)') {
        return "[REDACTED]"
    }
    if ($null -eq $Value -or $Value -is [string] -or $Value -is [ValueType]) {
        return $Value
    }
    if ($Value -is [System.Collections.IDictionary]) {
        $safe = [ordered]@{}
        foreach ($key in $Value.Keys) {
            $safe[[string]$key] = Protect-ConfigValue -Value $Value[$key] -KeyName ([string]$key)
        }
        return $safe
    }
    if ($Value -is [System.Collections.IEnumerable]) {
        return @($Value | ForEach-Object { Protect-ConfigValue -Value $_ })
    }
    $objectSafe = [ordered]@{}
    foreach ($property in $Value.PSObject.Properties) {
        $objectSafe[$property.Name] = Protect-ConfigValue -Value $property.Value -KeyName $property.Name
    }
    return $objectSafe
}

$ConfigCollected = $false
if (Test-Path -LiteralPath $ConfigPath -PathType Leaf) {
    try {
        $rawConfig = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
        $safeConfig = Protect-ConfigValue -Value $rawConfig
        $safeConfig | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $EvidenceDir "config_snapshot_redacted.json") -Encoding UTF8
        $ConfigCollected = $true
    }
    catch {
        [ordered]@{ error = $_.Exception.Message } | ConvertTo-Json |
            Set-Content -LiteralPath (Join-Path $EvidenceDir "config_snapshot_error.json") -Encoding UTF8
    }
}

$ProcessesAfter = @(Get-Process AgoraLink, agoralink_media -ErrorAction SilentlyContinue |
    Select-Object Id, ProcessName, Path, StartTime)
$PortsAfter = @()
try {
    $PortsAfter = @(Get-NetUDPEndpoint -ErrorAction Stop |
        Where-Object { $_.LocalPort -eq 9999 -or ($_.LocalPort -ge 55000 -and $_.LocalPort -le 55999) } |
        Select-Object LocalAddress, LocalPort, OwningProcess)
}
catch {
    $PortsAfter = @([ordered]@{ error = $_.Exception.Message })
}
$DumpsAfter = @()
if (Test-Path -LiteralPath ([string]$Prepare.crash_dump_dir) -PathType Container) {
    $DumpsAfter = @(Get-ChildItem -LiteralPath ([string]$Prepare.crash_dump_dir) -File -ErrorAction SilentlyContinue |
        Select-Object Name, Length, LastWriteTimeUtc)
}
$BeforeDumpNames = @($Prepare.crash_dumps_before | ForEach-Object { [string]$_.Name })
$NewDumps = @($DumpsAfter | Where-Object { $_.Name -notin $BeforeDumpNames })

$ProcessesAfter | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $EvidenceDir "processes_after.json") -Encoding UTF8
$PortsAfter | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $EvidenceDir "udp_ports_after.json") -Encoding UTF8
$DumpsAfter | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $EvidenceDir "crash_dumps_after.json") -Encoding UTF8

$Summary = [ordered]@{
    test_id = [string]$Prepare.test_id
    role = $Role
    collected_at = (Get-Date).ToUniversalTime().ToString("o")
    portable_dir = [string]$Prepare.portable_dir
    native_exe_sha256 = [string]$Prepare.native_exe_sha256
    config_snapshot_collected = $ConfigCollected
    log_files_collected = @(Get-ChildItem -LiteralPath $CollectedLogs -Recurse -File -ErrorAction SilentlyContinue).Count
    residual_processes = $ProcessesAfter.Count
    new_crash_dumps = $NewDumps.Count
    new_crash_dump_names = @($NewDumps | ForEach-Object { $_.Name })
    result = "USER_REVIEW_REQUIRED"
    required_manual_cases = @("G1", "G2", "G3", "G4", "G5", "append_smoke")
}
$SummaryPath = Join-Path $EvidenceDir "summary.json"
$Summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $SummaryPath -Encoding UTF8

$ZipPath = Join-Path (Split-Path -Parent $EvidenceDir) ("r4_gui_smoke_{0}_{1}.zip" -f $Role, [string]$Prepare.test_id)
if (Test-Path -LiteralPath $ZipPath) {
    throw "Evidence ZIP already exists: $ZipPath"
}
Compress-Archive -LiteralPath $EvidenceDir -DestinationPath $ZipPath -CompressionLevel Optimal
Write-Output $ZipPath

[CmdletBinding()]
param(
    [string]$OutputRoot = "",
    [string]$PortableZip = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Release = "v0.0.12"
$ScriptDir = Split-Path -Parent $PSCommandPath
$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $ScriptDir "..")).Path
$OwnedRoot = Join-Path $RepoRoot "_local_artifacts"
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $OwnedRoot "V0_0_12_RELEASE"
}
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$PdbPath = Join-Path $RepoRoot "rust-native\agoralink_media\target\release\agoralink_media.pdb"
$NativeExe = Join-Path $RepoRoot "rust-native\agoralink_media\target\release\agoralink_media.exe"
if ([string]::IsNullOrWhiteSpace($PortableZip)) {
    $PortableZip = Join-Path $OutputRoot "AgoraLink_v0.0.12_portable.zip"
}
$SymbolsZip = Join-Path $OutputRoot "AgoraLink_v0.0.12_symbols.zip"
$SymbolsHashFile = Join-Path $OutputRoot "AgoraLink_v0.0.12_symbols.sha256.txt"
$StageDir = Join-Path $OutputRoot "symbols_staging"
$PrivacyReport = Join-Path $OutputRoot "symbols_privacy_scan.json"

foreach ($path in @($PdbPath, $NativeExe, $PortableZip)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required symbol input not found: $path"
    }
}

$ownedPrefix = [IO.Path]::GetFullPath($OwnedRoot).TrimEnd('\') + '\'
if (-not ($OutputRoot.TrimEnd('\') + '\').StartsWith($ownedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputRoot must stay under $OwnedRoot"
}

$bytes = [IO.File]::ReadAllBytes($PdbPath)
$ascii = [Text.Encoding]::ASCII.GetString($bytes)
$unicode = [Text.Encoding]::Unicode.GetString($bytes)
$patterns = [ordered]@{
    local_user_path = '(?i)[A-Z]:\\Users\\'
    appdata_path = '(?i)[A-Z]:\\Users\\[^\\]+\\AppData\\'
    temp_path = '(?i)[A-Z]:\\[^\x00\r\n]*(?:\\TEMP\\|\\TMP\\)'
    token_value = '(?i)token\s*[:=]\s*["''][^"'']+["'']'
    password_value = '(?i)password\s*[:=]\s*["''][^"'']+["'']'
    secret_value = '(?i)secret\s*[:=]\s*["''][^"'']+["'']'
    private_key_block = '(?i)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----'
    webhook_key_value = '(?i)webhook[ _-]?key\s*[:=]\s*["''][^"'']+["'']'
}
$findings = New-Object System.Collections.Generic.List[string]
foreach ($entry in $patterns.GetEnumerator()) {
    if ($ascii -match $entry.Value -or $unicode -match $entry.Value) {
        $findings.Add($entry.Key)
    }
}

$privacy = [ordered]@{
    type = "SYMBOLS_PRIVACY_SCAN"
    release = $Release
    pdb_sha256 = (Get-FileHash -LiteralPath $PdbPath -Algorithm SHA256).Hash
    findings = @($findings)
    publishable = ($findings.Count -eq 0)
}
$privacy | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $PrivacyReport -Encoding UTF8
if ($findings.Count -ne 0) {
    throw "PDB privacy scan failed. Keep symbols private and inspect $PrivacyReport"
}

if (Test-Path -LiteralPath $StageDir) {
    if (-not $Force) {
        throw "Symbol staging directory exists; pass -Force to replace it: $StageDir"
    }
    Remove-Item -LiteralPath $StageDir -Recurse -Force
}
New-Item -ItemType Directory -Path $StageDir -Force | Out-Null
Copy-Item -LiteralPath $PdbPath -Destination (Join-Path $StageDir "agoralink_media.pdb") -Force

$gitCommit = (& git -C $RepoRoot rev-parse HEAD).Trim()
$toolchain = (& rustc +stable-x86_64-pc-windows-msvc -vV | Out-String).Trim()
$info = [ordered]@{
    release = $Release
    tag = "v0.0.12"
    commit = $gitCommit
    native_exe_sha256 = (Get-FileHash -LiteralPath $NativeExe -Algorithm SHA256).Hash
    pdb_sha256 = (Get-FileHash -LiteralPath $PdbPath -Algorithm SHA256).Hash
    build_date_utc = (Get-Date).ToUniversalTime().ToString("o")
    toolchain = $toolchain
    linker = "MSVC link.exe (x86_64-pc-windows-msvc)"
    portable_asset_sha256 = (Get-FileHash -LiteralPath $PortableZip -Algorithm SHA256).Hash
    privacy_scan = "pass"
}
$info | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $StageDir "SYMBOLS_INFO.json") -Encoding UTF8

$sumFiles = @("agoralink_media.pdb", "SYMBOLS_INFO.json")
$sumLines = foreach ($name in $sumFiles) {
    $path = Join-Path $StageDir $name
    "{0}  {1}" -f (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash, $name
}
$sumLines | Set-Content -LiteralPath (Join-Path $StageDir "SHA256SUMS.txt") -Encoding ASCII

if (Test-Path -LiteralPath $SymbolsZip) {
    if (-not $Force) {
        throw "Symbols archive already exists: $SymbolsZip"
    }
    Remove-Item -LiteralPath $SymbolsZip -Force
}
Compress-Archive -Path (Join-Path $StageDir "*") -DestinationPath $SymbolsZip -CompressionLevel Optimal
$symbolsHash = (Get-FileHash -LiteralPath $SymbolsZip -Algorithm SHA256).Hash
"$symbolsHash  $(Split-Path -Leaf $SymbolsZip)" | Set-Content -LiteralPath $SymbolsHashFile -Encoding ASCII

[ordered]@{
    type = "SYMBOLS_PACKAGE_RESULT"
    symbols_zip = $SymbolsZip
    symbols_sha256 = $symbolsHash
    privacy_report = $PrivacyReport
} | ConvertTo-Json -Depth 4

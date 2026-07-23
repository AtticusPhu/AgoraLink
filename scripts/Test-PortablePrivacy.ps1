[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Root,
    [switch]$RequireLicense,
    [string[]]$ForbiddenPathPrefix = @($env:USERPROFILE)
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ResolvedRoot = (Resolve-Path -LiteralPath $Root -ErrorAction Stop).Path
$files = @(Get-ChildItem -LiteralPath $ResolvedRoot -Recurse -File -Force)
$rootPrefix = $ResolvedRoot.TrimEnd('\') + '\'

$required = @(
    "AgoraLink.exe",
    "_internal\tools\agoralink_media\agoralink_media.exe",
    "BUILD_INFO.json",
    "SHA256SUMS.txt",
    "README.md"
)
if ($RequireLicense) {
    $required += "LICENSE"
}

$failures = New-Object System.Collections.Generic.List[string]
foreach ($relative in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $ResolvedRoot $relative) -PathType Leaf)) {
        $failures.Add("required file missing: $relative")
    }
}

$nativeExecutables = @($files | Where-Object { $_.Name -ieq "agoralink_media.exe" })
if ($nativeExecutables.Count -ne 1) {
    $failures.Add("expected exactly one agoralink_media.exe; found $($nativeExecutables.Count)")
}

$forbiddenExtensions = @(
    ".pdb", ".dmp", ".log", ".db", ".sqlite", ".sqlite3", ".key", ".pin",
    ".p12", ".pcm", ".wav", ".h264", ".py", ".pyc", ".pyo", ".rs",
    ".toml", ".ps1", ".psm1", ".c", ".cc", ".cpp", ".h", ".hpp"
)
$forbiddenDirectoryNames = @(
    "_local_artifacts", "target", "tests", "test_data", "logs", "chat", "pins", "received"
)
$sensitiveNamePattern = '(?i)(^|[._-])(token|password|secret|private[_-]?key|pin)([._-]|$)'
$mediaNamePattern = '(?i)(ffmpeg|ffprobe|ffplay|ffpyplayer|gstplayer|gstreamer|avcodec|avformat|avutil|swscale|swresample)'

foreach ($file in $files) {
    $relative = $file.FullName.Substring($rootPrefix.Length)
    $segments = $relative -split '[\\/]'
    if ($forbiddenExtensions -contains $file.Extension.ToLowerInvariant()) {
        $failures.Add("forbidden extension: $relative")
    }
    if ($segments | Where-Object { $forbiddenDirectoryNames -contains $_.ToLowerInvariant() }) {
        $failures.Add("forbidden directory: $relative")
    }
    if ($file.Name -match $sensitiveNamePattern) {
        $failures.Add("sensitive filename: $relative")
    }
    if ($relative -match $mediaNamePattern) {
        $failures.Add("removed media backend artifact: $relative")
    }
}

$textExtensions = @(".txt", ".md", ".json", ".ini", ".cfg", ".yaml", ".yml", ".xml")
$textPatterns = [ordered]@{
    local_user_path = '(?i)[A-Z]:\\Users\\'
    appdata_path = '(?i)\\AppData\\'
    private_key_block = '(?i)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----'
    assigned_password = '(?i)password\s*[:=]\s*[^\s,}]+'
    assigned_secret = '(?i)(?:secret|token)\s*[:=]\s*[^\s,}]+'
}
foreach ($file in $files | Where-Object { $textExtensions -contains $_.Extension.ToLowerInvariant() }) {
    $relative = $file.FullName.Substring($rootPrefix.Length)
    $content = Get-Content -LiteralPath $file.FullName -Raw -Encoding UTF8
    foreach ($entry in $textPatterns.GetEnumerator()) {
        if ($content -match $entry.Value) {
            $failures.Add("$($entry.Key) found in text metadata: $relative")
        }
    }
}

# Public binaries must not embed this build's user profile or source checkout. Prebuilt
# third-party DLLs can contain their upstream build-machine PDB path; count those as
# provenance without treating another vendor's username as local user data.
$thirdPartyBuildPathFiles = New-Object System.Collections.Generic.List[string]
foreach ($file in $files | Where-Object { $_.Length -le 128MB }) {
    $bytes = [IO.File]::ReadAllBytes($file.FullName)
    $ascii = [Text.Encoding]::ASCII.GetString($bytes)
    $unicode = [Text.Encoding]::Unicode.GetString($bytes)
    $relative = $file.FullName.Substring($rootPrefix.Length)
    if ($ascii.IndexOf("C:\Users\", [StringComparison]::OrdinalIgnoreCase) -ge 0) {
        $thirdPartyBuildPathFiles.Add($relative)
    }
    foreach ($prefix in $ForbiddenPathPrefix | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }) {
        $normalized = [IO.Path]::GetFullPath($prefix).TrimEnd('\') + '\'
        if ($ascii.IndexOf($normalized, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
            $unicode.IndexOf($normalized, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            $failures.Add("local build path embedded in artifact: $relative")
            break
        }
    }
}

$result = [ordered]@{
    type = "PORTABLE_PRIVACY_SCAN"
    root = $ResolvedRoot
    files_scanned = $files.Count
    require_license = [bool]$RequireLicense
    forbidden_path_prefixes = @($ForbiddenPathPrefix)
    third_party_build_path_file_count = $thirdPartyBuildPathFiles.Count
    third_party_build_path_files = @($thirdPartyBuildPathFiles)
    failure_count = $failures.Count
    failures = @($failures)
    passed = ($failures.Count -eq 0)
}
$result | ConvertTo-Json -Depth 5
if ($failures.Count -ne 0) {
    throw "Portable privacy scan failed with $($failures.Count) finding(s)."
}

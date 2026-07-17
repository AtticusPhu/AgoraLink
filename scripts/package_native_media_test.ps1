[CmdletBinding()]
param(
    [string]$ReceiverIp = "<RECEIVER_IP>",
    [ValidateRange(1, 65535)]
    [int]$Port = 55134,
    [ValidateRange(1, 120)]
    [int]$Fps = 60,
    [ValidateRange(1, 500)]
    [int]$BitrateMbps = 50,
    [ValidateRange(0, 500)]
    [int]$PlayoutDelayMs = 250,
    [ValidateRange(0, 500)]
    [int]$AudioJitterBufferMs = 120,
    [string]$Tag = (Get-Date -Format "yyyyMMdd_HHmmss"),
    [string]$OutputRoot = ""
)

$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
$CrateRoot = Join-Path $ProjectRoot "rust-native\agoralink_media"
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $ProjectRoot "artifacts"
}

$PackageName = "agoralink_media_test_$Tag"
$PackageDir = Join-Path $OutputRoot $PackageName
$PayloadDir = Join-Path $PackageDir "payload"
$BinDir = Join-Path $PayloadDir ".bin"
$ZipPath = Join-Path $PackageDir "$PackageName.zip"

if (Test-Path $PackageDir) {
    throw "Test package directory already exists: $PackageDir"
}

function Invoke-Cargo([string[]]$Arguments) {
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

Push-Location $CrateRoot
try {
    Invoke-Cargo @("fmt", "--", "--check")
    Invoke-Cargo @("check", "--locked", "--offline", "--jobs", "1")
    Invoke-Cargo @("test", "--locked", "--offline", "--jobs", "1")
    Invoke-Cargo @("run", "--locked", "--offline", "--", "self-test")
    Invoke-Cargo @("build", "--release", "--locked", "--offline", "--jobs", "1")

    $Exe = Join-Path $CrateRoot "target\release\agoralink_media.exe"
    if (-not (Test-Path $Exe)) {
        throw "Release executable not found: $Exe"
    }

    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    Copy-Item -LiteralPath $Exe -Destination (Join-Path $BinDir "agoralink_media.exe") -Force

    $TestCommandsPath = Join-Path $PayloadDir "TEST_COMMANDS.ps1"
    @"
param(
    [ValidateSet("receiver", "sender")]
    [string]`$Role,
    [ValidateSet("preflight", "video-only", "audio-no-gate", "av-conservative")]
    [string]`$Test,
    [string]`$ReceiverIp = "$ReceiverIp"
)

`$ErrorActionPreference = "Stop"
`$Exe = Join-Path `$PSScriptRoot ".bin\agoralink_media.exe"
`$CommonReceiver = @("--bind", "0.0.0.0", "--port", "$Port", "--duration-sec", "30", "--fps", "$Fps", "--playout-delay-ms", "$PlayoutDelayMs", "--render-scale", "exact", "--window-mode", "borderless-fullscreen", "--render-backend", "d3d11", "--repair", "nack", "--nack-max-rounds", "3")
`$CommonSender = @("--host", `$ReceiverIp, "--port", "$Port", "--duration-sec", "30", "--width", "1920", "--height", "1080", "--fps", "$Fps", "--bitrate-mbps", "$BitrateMbps", "--encoder", "auto", "--convert-backend", "d3d11", "--packet-pacing", "batch", "--udp-payload-size", "1452", "--fec", "off", "--repair", "nack", "--repair-cache-ms", "3000")

if (`$Test -eq "preflight") {
    `$Hash = (Get-FileHash -LiteralPath `$Exe -Algorithm SHA256).Hash
    Write-Host "Executable SHA256: `$Hash"
    & `$Exe --help
    if (`$Role -eq "receiver") {
        `$Endpoint = Get-NetUDPEndpoint -LocalPort $Port -ErrorAction SilentlyContinue
        if (`$Endpoint) {
            Write-Host "UDP $Port is currently listening."
        } else {
            Write-Host "UDP $Port is not listening yet. Start the receiver before the sender."
        }
    }
    exit 0
}

if (`$Role -eq "receiver") {
    switch (`$Test) {
        "video-only" { & `$Exe screen-recv @CommonReceiver --audio off --av-sync off }
        "audio-no-gate" { & `$Exe screen-recv @CommonReceiver --audio on --av-sync off --audio-jitter-buffer-ms "$AudioJitterBufferMs" }
        "av-conservative" { & `$Exe screen-recv @CommonReceiver --audio on --av-sync conservative --audio-jitter-buffer-ms "$AudioJitterBufferMs" }
    }
} else {
    switch (`$Test) {
        "video-only" { & `$Exe screen-send @CommonSender --audio off }
        "audio-no-gate" { & `$Exe screen-send @CommonSender --audio system }
        "av-conservative" { & `$Exe screen-send @CommonSender --audio system }
    }
}
exit `$LASTEXITCODE
"@ | Set-Content -LiteralPath $TestCommandsPath -Encoding utf8

    $ReadmePath = Join-Path $PayloadDir "README_TEST.md"
    @"
# AgoraLink Native Media Monday Test Package

Tag: $Tag

Run the receiver before the sender. See `MONDAY_TEST_PLAN.md` in this package
for the acceptance criteria and troubleshooting matrix.

Examples:

~~~powershell
.\TEST_COMMANDS.ps1 -Role receiver -Test preflight
.\TEST_COMMANDS.ps1 -Role sender -Test preflight -ReceiverIp <B_IP>
.\TEST_COMMANDS.ps1 -Role receiver -Test video-only
.\TEST_COMMANDS.ps1 -Role sender -Test video-only -ReceiverIp <B_IP>
.\TEST_COMMANDS.ps1 -Role receiver -Test audio-no-gate
.\TEST_COMMANDS.ps1 -Role sender -Test audio-no-gate -ReceiverIp <B_IP>
.\TEST_COMMANDS.ps1 -Role receiver -Test av-conservative
.\TEST_COMMANDS.ps1 -Role sender -Test av-conservative -ReceiverIp <B_IP>
~~~

Save stdout JSON from both endpoints for every run. These commands do not prove
LAN, WASAPI, WGC, or D3D11 behavior until run on the intended test machines.
"@ | Set-Content -LiteralPath $ReadmePath -Encoding utf8

    $PlanSource = Join-Path $ProjectRoot "docs\RUST_NATIVE_AV_MONDAY_TEST_PLAN.md"
    if (-not (Test-Path $PlanSource)) {
        throw "Monday test plan not found: $PlanSource"
    }
    Copy-Item -LiteralPath $PlanSource -Destination (Join-Path $PayloadDir "MONDAY_TEST_PLAN.md") -Force

    $PayloadItems = Get-ChildItem -LiteralPath $PayloadDir -Force | Select-Object -ExpandProperty FullName
    Compress-Archive -Path $PayloadItems -DestinationPath $ZipPath -CompressionLevel Optimal

    $ExeHash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $BinDir "agoralink_media.exe")).Hash
    $ZipHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $ZipPath).Hash
    $GitCommit = (& git -C $ProjectRoot rev-parse HEAD).Trim()
    $Dirty = [bool]((& git -C $ProjectRoot status --porcelain))
    $RustVersion = (& rustc -V).Trim()
    $CliHelp = (& (Join-Path $BinDir "agoralink_media.exe") --help 2>&1 | Out-String).Trim()
    $Manifest = [ordered]@{
        build_timestamp_utc = (Get-Date).ToUniversalTime().ToString("o")
        git_commit = $GitCommit
        dirty_working_tree = $Dirty
        rust_version = $RustVersion
        exe_path = ".bin/agoralink_media.exe"
        exe_sha256 = $ExeHash
        zip_path = (Split-Path -Leaf $ZipPath)
        zip_sha256 = $ZipHash
        receiver_ip_default = $ReceiverIp
        port = $Port
        fps = $Fps
        bitrate_mbps = $BitrateMbps
        playout_delay_ms = $PlayoutDelayMs
        audio_jitter_buffer_ms = $AudioJitterBufferMs
        cli_help = $CliHelp
    }
    $Manifest | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $PackageDir "manifest.json") -Encoding utf8
    @"
$ExeHash  payload/.bin/agoralink_media.exe
$ZipHash  $PackageName.zip
"@ | Set-Content -LiteralPath (Join-Path $PackageDir "SHA256SUMS.txt") -Encoding ascii

    Write-Host "Native media test package: $PackageDir"
    Write-Host "Payload zip: $ZipPath"
    Write-Host "Release exe SHA256: $ExeHash"
    Write-Host "Payload zip SHA256: $ZipHash"
} finally {
    Pop-Location
}

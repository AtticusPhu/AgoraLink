param(
    [string]$Python = $env:PYTHON,
    [string]$PyInstaller = $env:PYINSTALLER,
    [string]$MakeNsis = $env:MAKENSIS
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $PSCommandPath
$BaseScript = Join-Path $ScriptDir "package_release_native_lite.ps1"

if (-not (Test-Path -LiteralPath $BaseScript -PathType Leaf)) {
    throw "Base Native Lite package script not found: $BaseScript"
}

& $BaseScript `
    -Version "0.0.10" `
    -Python $Python `
    -PyInstaller $PyInstaller `
    -MakeNsis $MakeNsis

if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

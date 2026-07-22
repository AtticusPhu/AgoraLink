param(
    [Parameter(Mandatory = $true)]
    [string]$Python,
    [string]$OutputDir = "_local_artifacts\V0_0_12_THEME_SHARPNESS_FIX\ab",
    [int]$Width = 1180,
    [int]$Height = 760
)

$ErrorActionPreference = "Stop"
$Project = Split-Path -Parent $PSScriptRoot
$ResolvedPython = (Resolve-Path -LiteralPath $Python).Path
$ResolvedOutput = Join-Path $Project $OutputDir
$env:PYTHONPATH = "$Project;$Project\.venv\Lib\site-packages"
$env:KIVY_HOME = Join-Path $Project "_local_artifacts\V0_0_12_THEME_SHARPNESS_FIX\kivy_home_ab"

New-Item -ItemType Directory -Force -Path $ResolvedOutput | Out-Null

& $ResolvedPython -B (Join-Path $PSScriptRoot "audit_theme_sharpness_ab.py") `
    --width $Width `
    --height $Height `
    --density 1.0 `
    --dpi 96 `
    --section network `
    --output-dir $ResolvedOutput

if ($LASTEXITCODE -ne 0) {
    throw "Pixel-Snap A/B harness failed with exit code $LASTEXITCODE"
}

Get-ChildItem -LiteralPath $ResolvedOutput -File | Select-Object Name, Length

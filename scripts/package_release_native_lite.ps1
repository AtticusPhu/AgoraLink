[CmdletBinding()]
param(
    [string]$Version = "0.0.12",
    [string]$Python = $env:PYTHON,
    [string]$PyInstaller = $env:PYINSTALLER,
    [string]$MakeNsis = $env:MAKENSIS,
    [string]$OutputRoot = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($Version -ne "0.0.12") {
    throw "This compatibility entry point only packages v0.0.12."
}
if (-not [string]::IsNullOrWhiteSpace($PyInstaller)) {
    Write-Warning "-PyInstaller is ignored; the locked Python environment supplies PyInstaller."
}
if (-not [string]::IsNullOrWhiteSpace($MakeNsis)) {
    Write-Warning "-MakeNsis is ignored; the v0.0.12 remediation gate produces the portable asset only."
}

$script = Join-Path $PSScriptRoot "package_release_v0_0_12.ps1"
$arguments = @{}
if (-not [string]::IsNullOrWhiteSpace($Python)) {
    $arguments.Python = $Python
}
if (-not [string]::IsNullOrWhiteSpace($OutputRoot)) {
    $arguments.OutputRoot = $OutputRoot
}
if ($Force) {
    $arguments.Force = $true
}
& $script @arguments
exit $LASTEXITCODE

param(
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot 'NativeWindowsTestHarness.cs'
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "Native test harness source is missing: $source"
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$output = Join-Path $OutputDirectory 'NativeWindowsTestHarness.exe'
if (Test-Path -LiteralPath $output) {
    Remove-Item -LiteralPath $output -Force
}
Add-Type -Path $source -OutputAssembly $output -OutputType ConsoleApplication
if (-not (Test-Path -LiteralPath $output -PathType Leaf)) {
    throw "Native test harness build did not produce: $output"
}
Get-Item -LiteralPath $output

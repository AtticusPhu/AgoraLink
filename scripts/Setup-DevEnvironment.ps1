[CmdletBinding()]
param(
    [string]$Python = "python",
    [string]$VenvPath = ".venv-v0.0.12",
    [switch]$IncludeBuildDependencies,
    [switch]$Recreate
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $ProjectRoot

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Executable,
        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]]$Arguments
    )

    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $Executable $($Arguments -join ' ')"
    }
}

$PythonProbe = "import ensurepip,sys; ok=sys.version_info[:2] == (3, 12) and sys.implementation.name == 'cpython'; print(sys.executable); raise SystemExit(0 if ok else 'AgoraLink requires CPython 3.12')"
Invoke-Checked $Python -c $PythonProbe

$ResolvedVenv = if ([System.IO.Path]::IsPathRooted($VenvPath)) {
    [System.IO.Path]::GetFullPath($VenvPath)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $ProjectRoot $VenvPath))
}

$ProjectPrefix = [System.IO.Path]::GetFullPath($ProjectRoot).TrimEnd('\') + '\'
if (-not $ResolvedVenv.StartsWith($ProjectPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "VenvPath must remain inside the project directory: $ResolvedVenv"
}

if ($Recreate -and (Test-Path -LiteralPath $ResolvedVenv)) {
    Remove-Item -LiteralPath $ResolvedVenv -Recurse -Force
}

$VenvPython = Join-Path $ResolvedVenv "Scripts\python.exe"
if (-not (Test-Path -LiteralPath $VenvPython)) {
    Invoke-Checked $Python -m venv $ResolvedVenv
}

Invoke-Checked $VenvPython -c "import sys; assert sys.version_info[:2] == (3, 12); print(sys.executable)"
Invoke-Checked $VenvPython -m pip install --disable-pip-version-check "pip==26.1.2"

$LockFile = if ($IncludeBuildDependencies) {
    Join-Path $ProjectRoot "build-requirements.lock"
} else {
    Join-Path $ProjectRoot "requirements.lock"
}
Invoke-Checked $VenvPython -m pip install --disable-pip-version-check --requirement $LockFile
Invoke-Checked $VenvPython -m pip check
Invoke-Checked $VenvPython -B scripts/check_python_test_count.py
Invoke-Checked $VenvPython -B -m unittest discover -s tests -p "test_*.py" -v
Invoke-Checked $VenvPython -B screen_runtime.py --self-test

Write-Host "AgoraLink development environment is ready: $ResolvedVenv"

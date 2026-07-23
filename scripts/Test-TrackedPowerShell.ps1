[CmdletBinding()]
param(
    [string]$RepoRoot = (Split-Path -Parent $PSScriptRoot)
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$resolvedRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
$trackedFiles = @(
    & git -C $resolvedRoot ls-files --cached --others --exclude-standard -- "*.ps1" "*.psm1"
)
if ($LASTEXITCODE -ne 0) {
    throw "git ls-files failed with exit code $LASTEXITCODE"
}

$failures = New-Object System.Collections.Generic.List[object]
foreach ($relativePath in $trackedFiles) {
    $tokens = $null
    $errors = $null
    $fullPath = Join-Path $resolvedRoot $relativePath
    [void][Management.Automation.Language.Parser]::ParseFile(
        $fullPath,
        [ref]$tokens,
        [ref]$errors
    )
    foreach ($parseError in $errors) {
        $failures.Add([ordered]@{
            file = $relativePath
            line = $parseError.Extent.StartLineNumber
            column = $parseError.Extent.StartColumnNumber
            message = $parseError.Message
        })
    }
}

$result = [ordered]@{
    type = "POWERSHELL_PARSE"
    files = $trackedFiles.Count
    parse_error_count = $failures.Count
    failures = $failures.ToArray()
    passed = ($failures.Count -eq 0)
}
$result | ConvertTo-Json -Depth 5
if ($failures.Count -ne 0) {
    throw "$($failures.Count) PowerShell parse error(s) found."
}

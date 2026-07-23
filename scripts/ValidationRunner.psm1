Set-StrictMode -Version Latest

function ConvertTo-NativeArgument {
    param(
        [AllowEmptyString()]
        [string]$Value
    )

    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
        return $Value
    }

    $escaped = [regex]::Replace($Value, '(\\*)"', '$1$1\"')
    $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
    return '"' + $escaped + '"'
}

function Resolve-ValidationExecutable {
    param([Parameter(Mandatory = $true)][string]$Executable)

    if (Test-Path -LiteralPath $Executable -PathType Leaf) {
        return (Resolve-Path -LiteralPath $Executable).Path
    }

    $command = Get-Command $Executable -CommandType Application -ErrorAction Stop |
        Select-Object -First 1
    return $command.Source
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [AllowEmptyString()][string]$Content
    )

    $parent = Split-Path -Parent $Path
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
}

function Invoke-ValidatedCommand {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Executable,
        [string[]]$CommandArguments = @(),
        [Parameter(Mandatory = $true)][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][string]$StdoutPath,
        [Parameter(Mandatory = $true)][string]$StderrPath,
        [Parameter(Mandatory = $true)][string]$ResultPath,
        [ValidateRange(1, 3600)][int]$TimeoutSeconds = 600
    )

    $resolvedExecutable = Resolve-ValidationExecutable -Executable $Executable
    $resolvedWorkingDirectory = (Resolve-Path -LiteralPath $WorkingDirectory).Path
    $stdoutFullPath = [IO.Path]::GetFullPath($StdoutPath)
    $stderrFullPath = [IO.Path]::GetFullPath($StderrPath)
    $resultFullPath = [IO.Path]::GetFullPath($ResultPath)
    $argumentList = @($CommandArguments)
    $commandLineArguments = ($argumentList | ForEach-Object {
        ConvertTo-NativeArgument -Value ([string]$_)
    }) -join ' '

    $start = Get-Date
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    $process = $null
    $stdout = ""
    $stderr = ""
    $exitCode = -1
    $timedOut = $false
    $processId = $null
    $startError = $null

    try {
        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $resolvedExecutable
        $startInfo.Arguments = $commandLineArguments
        $startInfo.WorkingDirectory = $resolvedWorkingDirectory
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
        $startInfo.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
        $startInfo.EnvironmentVariables["PYTHONUTF8"] = "1"
        $startInfo.EnvironmentVariables["PYTHONIOENCODING"] = "utf-8"

        $process = [Diagnostics.Process]::new()
        $process.StartInfo = $startInfo
        if (-not $process.Start()) {
            throw "Process start returned false."
        }
        $processId = $process.Id
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()

        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $timedOut = $true
            & (Join-Path $env:SystemRoot "System32\taskkill.exe") /PID $process.Id /T /F *> $null
            if (-not $process.WaitForExit(5000)) {
                $process.Kill()
                [void]$process.WaitForExit(5000)
            }
        }

        $process.WaitForExit()
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $exitCode = if ($timedOut) { 124 } else { $process.ExitCode }
    }
    catch {
        $startError = $_.Exception.Message
        $stderr = if ([string]::IsNullOrEmpty($stderr)) {
            $startError
        }
        else {
            $stderr + [Environment]::NewLine + $startError
        }
    }
    finally {
        $stopwatch.Stop()
        if ($null -ne $process) {
            $process.Dispose()
        }
    }

    Write-Utf8NoBom -Path $stdoutFullPath -Content $stdout
    Write-Utf8NoBom -Path $stderrFullPath -Content $stderr
    $end = Get-Date
    $result = [ordered]@{
        name = $Name
        executable = $resolvedExecutable
        arguments = $argumentList
        command = ('"{0}" {1}' -f $resolvedExecutable, $commandLineArguments).Trim()
        working_directory = $resolvedWorkingDirectory
        start_time = $start.ToString("o")
        end_time = $end.ToString("o")
        elapsed_ms = [math]::Round($stopwatch.Elapsed.TotalMilliseconds)
        exit_code = $exitCode
        timed_out = $timedOut
        process_id = $processId
        stdout_path = $stdoutFullPath
        stderr_path = $stderrFullPath
        start_error = $startError
    }
    Write-Utf8NoBom -Path $resultFullPath -Content (
        $result | ConvertTo-Json -Depth 5
    )
    return [pscustomobject]$result
}

function Merge-ValidatedCommandLogs {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$StdoutPath,
        [Parameter(Mandatory = $true)][string]$StderrPath,
        [Parameter(Mandatory = $true)][string]$CombinedPath
    )

    $stdout = if (Test-Path -LiteralPath $StdoutPath) {
        Get-Content -LiteralPath $StdoutPath -Raw -Encoding UTF8
    }
    else {
        ""
    }
    $stderr = if (Test-Path -LiteralPath $StderrPath) {
        Get-Content -LiteralPath $StderrPath -Raw -Encoding UTF8
    }
    else {
        ""
    }
    $newline = [Environment]::NewLine
    $combined = "===== STDOUT =====" + $newline + $stdout + $newline +
        "===== STDERR =====" + $newline + $stderr
    Write-Utf8NoBom -Path ([IO.Path]::GetFullPath($CombinedPath)) -Content $combined
}

Export-ModuleMember -Function Invoke-ValidatedCommand, Merge-ValidatedCommandLogs

Set-StrictMode -Version 2.0

function ConvertTo-NativeCommandLine {
    param([string[]]$Arguments)
    return (($Arguments | ForEach-Object {
        $value = [string]$_
        '"' + ($value -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
    }) -join ' ')
}

function Write-Utf8NoBom {
    param([string]$Path, [AllowEmptyString()][string]$Text)
    $parent = Split-Path -Parent $Path
    if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function Start-LoggedNativeProcess {
    param(
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Executable
    $psi.Arguments = ConvertTo-NativeCommandLine $Arguments
    $psi.WorkingDirectory = Split-Path -Parent $Executable
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $psi
    if (-not $process.Start()) { throw "Failed to start $Executable" }
    [pscustomobject]@{
        Process = $process
        StdoutTask = $process.StandardOutput.ReadToEndAsync()
        StderrTask = $process.StandardError.ReadToEndAsync()
        Arguments = @($Arguments)
        ResolvedCommand = ('"{0}" {1}' -f $Executable, $psi.Arguments)
    }
}

function Complete-LoggedNativeProcess {
    param(
        [Parameter(Mandatory = $true)]$Handle,
        [int]$TimeoutMs = 30000
    )
    $finished = $Handle.Process.WaitForExit($TimeoutMs)
    if ($finished) {
        $Handle.Process.WaitForExit()
    }
    [pscustomobject]@{
        Finished = $finished
        ExitCode = if ($finished) { [int]$Handle.Process.ExitCode } else { $null }
        Stdout = if ($finished) { [string]$Handle.StdoutTask.Result } else { '' }
        Stderr = if ($finished) { [string]$Handle.StderrTask.Result } else { '' }
        ProcessId = $Handle.Process.Id
        ResolvedCommand = $Handle.ResolvedCommand
    }
}

function Stop-FailedNativeProcess {
    param($Handle)
    if ($null -ne $Handle -and -not $Handle.Process.HasExited) {
        try { $Handle.Process.Kill() } catch {}
        try { $null = $Handle.Process.WaitForExit(5000) } catch {}
    }
}

function ConvertFrom-JsonLines {
    param([AllowEmptyString()][string]$Text)
    $events = @()
    foreach ($line in ($Text -split "`r?`n")) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        try { $events += ($line | ConvertFrom-Json -ErrorAction Stop) } catch {}
    }
    return @($events)
}

function Get-TerminalEvents {
    param([object[]]$Events)
    return @($Events | Where-Object {
        $_.type -in @('NATIVE_SCREEN_STOPPED', 'NATIVE_SCREEN_SHUTDOWN_FAILED')
    })
}

function Test-CleanTerminal {
    param(
        [object[]]$Events,
        [Nullable[int]]$ExitCode,
        [string[]]$AllowedReasons = @('duration', 'peer_closed')
    )
    $typedEvents = @($Events | Where-Object {
        $null -ne $_ -and $null -ne $_.PSObject.Properties['type']
    })
    $terminal = @(Get-TerminalEvents $typedEvents)
    $errors = @($typedEvents | Where-Object { $_.type -eq 'NATIVE_SCREEN_ERROR' })
    $messages = New-Object System.Collections.Generic.List[string]
    if ($terminal.Count -ne 1) { $messages.Add("terminal event count=$($terminal.Count), expected=1") }
    if ($errors.Count -ne 0) { $messages.Add("NATIVE_SCREEN_ERROR count=$($errors.Count)") }
    if ($ExitCode -ne 0) { $messages.Add("exit code=$ExitCode") }
    if ($terminal.Count -eq 1) {
        $last = $terminal[0]
        if ($last.type -ne 'NATIVE_SCREEN_STOPPED') { $messages.Add("terminal type=$($last.type)") }
        if ($AllowedReasons.Count -gt 0 -and $last.reason -notin $AllowedReasons) {
            $messages.Add("stop reason=$($last.reason)")
        }
        if ($last.worker_join_all_clean -ne $true) { $messages.Add('worker_join_all_clean is not true') }
        if ([int64]$last.retained_worker_count -ne 0) { $messages.Add("retained_worker_count=$($last.retained_worker_count)") }
    }
    [pscustomobject]@{
        Passed = $messages.Count -eq 0
        Messages = @($messages)
        Terminal = if ($terminal.Count -eq 1) { $terminal[0] } else { $null }
        ErrorEvents = @($errors)
    }
}

function Get-CrashDumpSnapshot {
    $folder = 'C:\CrashDumps'
    if (-not (Test-Path -LiteralPath $folder -PathType Container)) { return @() }
    return @(Get-ChildItem -LiteralPath $folder -File | Sort-Object FullName | ForEach-Object {
        [pscustomobject]@{
            Path = $_.FullName
            Length = $_.Length
            LastWriteTimeUtc = $_.LastWriteTimeUtc.ToString('o')
        }
    })
}

function Get-NewCrashDumps {
    param([object[]]$Before, [object[]]$After)
    $known = @{}
    foreach ($item in $Before) { $known[[string]$item.Path] = [string]$item.LastWriteTimeUtc }
    return @($After | Where-Object {
        -not $known.ContainsKey([string]$_.Path) -or
        $known[[string]$_.Path] -ne [string]$_.LastWriteTimeUtc
    })
}

function Assert-UdpPortRangeAvailable {
    param(
        [Parameter(Mandatory = $true)][int]$StartPort,
        [Parameter(Mandatory = $true)][int]$Count
    )
    if ($Count -le 0) { return }
    for ($offset = 1; $offset -le $Count; $offset++) {
        $port = $StartPort + $offset
        $socket = $null
        try {
            $socket = [Net.Sockets.UdpClient]::new()
            $socket.Client.Bind([Net.IPEndPoint]::new([Net.IPAddress]::Loopback, $port))
        } catch {
            throw "UDP test port $port is unavailable before execution: $($_.Exception.Message)"
        } finally {
            if ($null -ne $socket) { $socket.Dispose() }
        }
    }
}

function Add-CaseRecord {
    param(
        [Parameter(Mandatory = $true)][string]$OutputRoot,
        [Parameter(Mandatory = $true)]$Record
    )
    $jsonPath = Join-Path $OutputRoot 'cases.jsonl'
    $csvPath = Join-Path $OutputRoot 'cases.csv'
    $json = $Record | ConvertTo-Json -Depth 10 -Compress
    [IO.File]::AppendAllText($jsonPath, $json + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
    $flat = [pscustomobject]@{
        case_id = $Record.case_id
        phase = $Record.phase
        group = $Record.group
        status = $Record.status
        started_at = $Record.started_at
        ended_at = $Record.ended_at
        sender_exit_code = $Record.sender_exit_code
        receiver_exit_code = $Record.receiver_exit_code
        detail = $Record.detail
    }
    if (-not (Test-Path -LiteralPath $csvPath)) {
        $flat | Export-Csv -LiteralPath $csvPath -NoTypeInformation -Encoding UTF8
    } else {
        $flat | Export-Csv -LiteralPath $csvPath -NoTypeInformation -Encoding UTF8 -Append
    }
}

Export-ModuleMember -Function *

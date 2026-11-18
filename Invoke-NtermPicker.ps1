# Requires PowerShell 7.3+ (the clean block discards an interrupted request).
# Dot-source this file from your profile.
#
# A picker is asked for through the session daemon that runs this shell. The
# daemon names its pipe and the pane in the shell's environment, and passes the
# request to the window showing the pane. There is no secret to keep and no
# connection to hold: one connection is one request.
#
# With -DaemonPicker the daemon runs the picker itself: it lists and searches on
# the machine this shell is on, and the window only draws the rows. That works
# the same when the window is on another machine.
#
# Both switches are accepted for profiles that pass them. Nothing is compiled,
# so there is nothing to prepare in the background.
param([switch]$BackgroundInitialize, [switch]$FreshConnection)

# Kept for profiles that call it: no connection is held between requests.
function Disconnect-NtermPicker {
    [CmdletBinding()]
    param()
}

function Invoke-NtermPicker {
    [CmdletBinding(DefaultParameterSetName='Stream')]
    param(
        [Parameter(Mandatory, ParameterSetName='Named')][string] $Name,
        # One of the pickers the daemon has: files, directories.
        [Parameter(Mandatory, ParameterSetName='Daemon')][string] $DaemonPicker,
        [Parameter(ParameterSetName='Named')][Parameter(ParameterSetName='Daemon')][hashtable] $Parameters = @{},
        [Parameter(ValueFromPipeline, ParameterSetName='Stream')]
        [Parameter(ValueFromPipeline, ParameterSetName='Named')][AllowNull()][object] $InputObject,
        [Parameter(ParameterSetName='Stream')][hashtable] $Options = @{},
        [Parameter(ParameterSetName='Stream')][Parameter(ParameterSetName='Named')][string] $DisplayProperty,
        [Parameter(ParameterSetName='Stream')][Parameter(ParameterSetName='Named')][string] $ValueProperty,
        [switch] $PassThruResult,
        # Accepted for profiles that pass it: every request has its own connection.
        [switch] $FreshConnection,
        # The request is cancelled this long after the picker opens. It is
        # looked at between items and while waiting, not while whatever feeds
        # the pipeline is itself waiting.
        [ValidateRange(0, 86400)][int] $CancelAfterSeconds = 0
    )
    begin {
        $pipe = $null
        $result = $null
        $batch = [System.Collections.Generic.List[object]]::new()
        $sequence = 0L
        $nextId = 0L
        $originals = [System.Collections.Generic.Dictionary[string,object]]::new()
        $requestId = [guid]::NewGuid().ToString('N')
        # Piping into -Name streams the items of a configured Items picker.
        $daemonRuns = $PSCmdlet.ParameterSetName -eq 'Daemon'
        $streaming = -not $daemonRuns -and ($PSCmdlet.ParameterSetName -eq 'Stream' -or
            $MyInvocation.ExpectingInput -or $PSBoundParameters.ContainsKey('InputObject'))
        $pane = [uint32]0
        if (-not $env:NTERM_DAEMON_PIPE -or -not [uint32]::TryParse($env:NTERM_PANE, [ref]$pane)) {
            throw 'Run this function inside an nterm pane whose shell the session daemon runs.'
        }
        # The name came from the environment. Only its last part is used, and
        # on this machine, so it cannot lead to another one.
        if ($env:NTERM_DAEMON_PIPE -cnotmatch '\A\\\\\.\\pipe\\(nterm-session-[A-Za-z0-9_-]+)\z') {
            throw "Not an nterm session daemon's pipe."
        }
        $pipeName = $Matches[1]
        # The daemon's frames: a length, a kind, and for all but a control
        # message a number. A picker message is kind 5 and its number is the
        # conversation, which the daemon knows by the connection: 0 from here.
        $maxMessage = 1048576
        # What is being read: the four bytes of a length, then that many.
        $reading = @{ Buffer = [byte[]]::new(4); Offset = 0; Length = $true; Pending = $null; CancelAt = $null }

        function Send-PickerFrame([byte] $kind, [string] $json) {
            $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
            if ($bytes.Length -eq 0 -or $bytes.Length -gt $maxMessage) { throw 'Picker message exceeds 1 MiB.' }
            $head = if ($kind -eq 0) { 1 } else { 5 }
            $frame = [byte[]]::new(4 + $head + $bytes.Length)
            [System.BitConverter]::GetBytes([uint32]($head + $bytes.Length)).CopyTo($frame, 0)
            $frame[4] = $kind
            [System.Array]::Copy($bytes, 0, $frame, 4 + $head, $bytes.Length)
            if (-not $pipe.WriteAsync($frame, 0, $frame.Length).Wait(5000)) {
                throw 'Timed out sending to the nterm session daemon.'
            }
        }
        function Send-PickerMessage($message) {
            Send-PickerFrame 5 (ConvertTo-Json -InputObject $message -Depth 32 -Compress)
        }
        # The next message's JSON, or nothing when none is complete within
        # the wait. Short waits let PowerShell observe Ctrl+C.
        function Read-PickerFrame([int] $milliseconds) {
            $waited = [System.Diagnostics.Stopwatch]::StartNew()
            while ($true) {
                if ($null -eq $reading.Pending) {
                    $reading.Pending = $pipe.ReadAsync($reading.Buffer, $reading.Offset,
                        $reading.Buffer.Length - $reading.Offset)
                }
                $left = [Math]::Max(0, $milliseconds - $waited.ElapsedMilliseconds)
                try {
                    if (-not $reading.Pending.Wait([int]$left)) { return $null }
                    $count = $reading.Pending.Result
                } catch { throw 'The nterm picker connection ended.' }
                $reading.Pending = $null
                if ($count -eq 0) { throw 'The nterm session daemon closed the connection.' }
                $reading.Offset += $count
                if ($reading.Offset -lt $reading.Buffer.Length) { continue }
                $whole = $reading.Buffer
                $reading.Offset = 0
                if ($reading.Length) {
                    $size = [System.BitConverter]::ToUInt32($whole, 0)
                    if ($size -eq 0 -or $size -gt $maxMessage + 5) { throw 'Invalid frame size.' }
                    $reading.Buffer = [byte[]]::new($size)
                    $reading.Length = $false
                    continue
                }
                $reading.Buffer = [byte[]]::new(4)
                $reading.Length = $true
                $start = if ($whole[0] -eq 0) { 1 } elseif ($whole[0] -eq 5) { 5 } else { -1 }
                if ($start -lt 0 -or $whole.Length -lt $start) { continue }
                return [System.Text.UTF8Encoding]::new($false, $true).GetString($whole, $start, $whole.Length - $start)
            }
        }
        function Send-PickerCancelWhenDue {
            if ($null -ne $reading.CancelAt -and [datetime]::UtcNow -ge $reading.CancelAt) {
                $reading.CancelAt = $null
                Send-PickerMessage @{ type='cancel'; request_id=$requestId }
            }
        }
        # A message of this request, or what the daemon says in the picker's
        # place: that it will not pass the request on, or that the window
        # ended it without an answer.
        function Read-PickerMessage([string] $raw) {
            $message = ConvertFrom-Json -InputObject $raw -AsHashtable
            if ($message.type -eq 'shell_closed') { throw 'nterm ended the picker request.' }
            if ($message.type -eq 'error') {
                if ($message.code) { throw "nterm picker rejected the request ($($message.code)): $($message.message)" }
                throw "The nterm session daemon refused the request: $($message.message)"
            }
            if ($message.request_id -ne $requestId) { throw 'Picker response belongs to a different request.' }
            $message
        }
        function Receive-PickerMessage([int] $timeoutSeconds = 0) {
            $deadline = if ($timeoutSeconds) { [datetime]::UtcNow.AddSeconds($timeoutSeconds) } else { [datetime]::MaxValue }
            while ([datetime]::UtcNow -lt $deadline) {
                Send-PickerCancelWhenDue
                $raw = Read-PickerFrame 100
                if ($null -ne $raw) { return Read-PickerMessage $raw }
            }
            throw 'Timed out waiting for nterm picker response.'
        }
        function Flush-PickerItems {
            if ($batch.Count -eq 0 -or $null -ne $result) { return }
            Send-PickerMessage @{ type='items'; request_id=$requestId; sequence=$sequence; items=@($batch.ToArray()) }
            $reply = Receive-PickerMessage 15
            if ($reply.type -eq 'result') { return $reply }
            if ($reply.type -ne 'ack' -or $reply.sequence -ne $sequence) { throw 'Unexpected picker batch response.' }
            $batch.Clear()
        }
        try {
            # The daemon may learn who this is and nothing more. What is sent
            # may hold the shell's history, so it goes only to a pipe that is
            # this user's: CurrentUserOnly refuses any other.
            $pipe = [System.IO.Pipes.NamedPipeClientStream]::new('.', $pipeName,
                [System.IO.Pipes.PipeDirection]::InOut,
                [System.IO.Pipes.PipeOptions]'Asynchronous, CurrentUserOnly',
                [System.Security.Principal.TokenImpersonationLevel]::Identification,
                [System.IO.HandleInheritability]::None)
            try { $pipe.Connect(5000) }
            catch [System.TimeoutException] { throw 'The nterm session daemon is not running.' }
            # `shell` is passed through to the window; `pick` is for the daemon.
            $first = if ($daemonRuns) { 'pick' } else { 'shell' }
            Send-PickerFrame 0 ('{"type":"' + $first + '","pane":' + $pane + '}')
            $request = switch ($PSCmdlet.ParameterSetName) {
                Daemon {
                    $strings = @{}
                    foreach ($key in $Parameters.Keys) { $strings[[string]$key] = [string]$Parameters[$key] }
                    $strings
                }
                Named {
                    $strings = @{}
                    foreach ($key in $Parameters.Keys) { $strings[[string]$key] = [string]$Parameters[$key] }
                    $named = @{ kind='named'; name=$Name; params=$strings }
                    if ($streaming) { $named.stream = $true }
                    $named
                }
                Stream {
                    $streamOptions = @{}
                    foreach ($key in $Options.Keys) { $streamOptions[$key] = $Options[$key] }
                    if ($streamOptions.ContainsKey('initial_query')) {
                        $streamOptions.initial_query = [string]$streamOptions.initial_query
                    }
                    @{ kind='stream'; options=$streamOptions }
                }
            }
            if ($daemonRuns) {
                Send-PickerMessage @{ type='open'; request_id=$requestId; picker=$DaemonPicker; params=$request }
            } else {
                Send-PickerMessage @{ type='open'; request_id=$requestId; request=$request }
            }
            $reply = Receive-PickerMessage 15
            if ($reply.type -eq 'result') { $result = $reply }
            elseif ($reply.type -ne 'opened') { throw 'Unexpected picker open response.' }
            if ($CancelAfterSeconds -and $null -eq $result) {
                $reading.CancelAt = [datetime]::UtcNow.AddSeconds($CancelAfterSeconds)
            }
        } catch {
            # Closing the connection is how the daemon, and through it the
            # window, learns that this request is over.
            if ($null -ne $pipe) { $pipe.Dispose() }
            throw
        }
    }
    process {
        if ($streaming -and $null -eq $result -and $PSBoundParameters.ContainsKey('InputObject')) {
            try {
                Send-PickerCancelWhenDue
                $raw = Read-PickerFrame 0
                if ($null -ne $raw) {
                    $reply = Read-PickerMessage $raw
                    if ($reply.type -ne 'result') { throw 'Unexpected picker response during streaming.' }
                    $result = $reply
                }
                if ($null -eq $result) {
                    $id = ($nextId++).ToString()
                    $text = if ($DisplayProperty) { [string]$InputObject.$DisplayProperty } else { [string]$InputObject }
                    $value = if ($ValueProperty) { $InputObject.$ValueProperty } else { $text }
                    $originals[$id] = $InputObject
                    $batch.Add(@{ id=$id; text=$text; value=$value })
                    if ($batch.Count -ge 8) { $result = Flush-PickerItems; $sequence++ }
                }
            } catch {
                $pipe.Dispose()
                throw
            }
        }
    }
    end {
        try {
            if ($streaming -and $null -eq $result) {
                if ($batch.Count) { $result = Flush-PickerItems; $sequence++ }
                if ($null -eq $result) {
                    $last = if ($sequence -eq 0) { $null } else { $sequence - 1 }
                    Send-PickerMessage @{ type='end_items'; request_id=$requestId; last_sequence=$last }
                    $reply = Receive-PickerMessage 15
                    if ($reply.type -eq 'result') { $result = $reply }
                    elseif ($reply.type -ne 'input_complete') { throw 'Unexpected picker completion response.' }
                }
            }
            while ($null -eq $result) { $result = Receive-PickerMessage }
            if ($result.status -eq 'error') { throw "Picker request failed ($($result.code)): $($result.message)" }
            if ($PassThruResult) { $result }
            elseif ($result.status -eq 'accepted') {
                foreach ($selected in $result.selected) {
                    if ($streaming -and -not $ValueProperty -and $originals.ContainsKey([string]$selected.id)) {
                        $originals[[string]$selected.id]
                    } else { $selected.value }
                }
            }
        } finally {
            if ($null -ne $pipe) { $pipe.Dispose() }
        }
    }
    clean {
        if ($null -ne $pipe) { $pipe.Dispose() }
    }
}

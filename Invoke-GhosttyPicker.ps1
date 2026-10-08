# Requires PowerShell 7.3+ (the clean block discards interrupted connections).
# Dot-source this file from your profile. Secrets are read only from the
# current shell environment; they are never written to stdout or disk.
param([switch]$BackgroundInitialize, [switch]$FreshConnection)

$script:GhosttyPickerFreshConnection = [bool]$FreshConnection

if (-not ($ExecutionContext.SessionState.PSVariable.Get('script:GhosttyPickerInitialization'))) {
    $script:GhosttyPickerInitialization = $null
}

if (-not ($ExecutionContext.SessionState.PSVariable.Get('script:GhosttyPickerConnection'))) {
    $script:GhosttyPickerConnection = $null
}

function Disconnect-GhosttyPicker {
    [CmdletBinding()]
    param()
    if ($null -ne $script:GhosttyPickerInitialization) { Initialize-GhosttyPickerClient }
    if ($null -ne $script:GhosttyPickerConnection) {
        $script:GhosttyPickerConnection.Client.Dispose()
        $script:GhosttyPickerConnection = $null
    }
}

$script:GhosttyPickerClientSource = @'
using System;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using Microsoft.Win32.SafeHandles;

public sealed class GhosttyPickerPipeV2 : IDisposable {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern SafeFileHandle CreateFile(string name, uint access, uint share,
        IntPtr attributes, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool GetHandleInformation(SafeFileHandle handle, out uint flags);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool WaitNamedPipe(string name, uint timeout);
    readonly FileStream stream;
    readonly BlockingCollection<string> incoming = new BlockingCollection<string>(4);
    readonly object writer = new object();
    readonly CancellationTokenSource stop = new CancellationTokenSource();
    readonly Task reader;
    Timer cancellation;
    volatile Exception failure;
    string activeRequest;
    public bool IsAlive { get { return disposed == 0 && failure == null; } }
    public void BeginRequest(string id) {
        lock (writer) {
            if (activeRequest != null) throw new InvalidOperationException("Picker client is already busy");
            if (!IsAlive) throw new IOException("Picker connection is closed");
            activeRequest = id;
        }
    }
    public void EndRequest() {
        lock (writer) {
            activeRequest = null;
            if (cancellation != null) { cancellation.Dispose(); cancellation = null; }
        }
    }
    int disposed;

    public GhosttyPickerPipeV2(string endpoint) {
        if (String.IsNullOrEmpty(endpoint) || !endpoint.StartsWith("ghostty-picker-") ||
            endpoint.IndexOf('\\') >= 0 || endpoint.IndexOf('/') >= 0)
            throw new ArgumentException("Invalid Ghostty picker endpoint");
        string path = @"\\.\pipe\" + endpoint;
        SafeFileHandle handle = null;
        var deadline = DateTime.UtcNow.AddSeconds(5);
        while (true) {
            // Precise data/synchronize rights, no FILE_CREATE_PIPE_INSTANCE.
            // OVERLAPPED | SQOS_PRESENT | IDENTIFICATION | CONTEXT_TRACKING.
            handle = CreateFile(path, 0x00100003, 0, IntPtr.Zero, 3, 0x40150000, IntPtr.Zero);
            if (!handle.IsInvalid) break;
            int error = Marshal.GetLastWin32Error(); handle.Dispose();
            if ((error != 231 && error != 2) || DateTime.UtcNow >= deadline)
                throw new IOException("Cannot connect to Ghostty picker pipe (Win32 " + error + ")");
            WaitNamedPipe(path, 100); Thread.Sleep(20);
        }
        uint flags;
        if (!GetHandleInformation(handle, out flags) || (flags & 1) != 0) {
            handle.Dispose(); throw new IOException("Picker handle must not be inheritable");
        }
        try { stream = new FileStream(handle, FileAccess.ReadWrite, 1, true); }
        catch { handle.Dispose(); throw; }
        reader = Task.Run(() => {
            try {
                while (!stop.IsCancellationRequested) {
                    var header = ReadExact(4); uint size = BitConverter.ToUInt32(header, 0);
                    if (size == 0 || size > 1048576) throw new IOException("Invalid picker frame size");
                    var text = new UTF8Encoding(false, true).GetString(ReadExact((int)size));
                    incoming.Add(text, stop.Token);
                }
            } catch (Exception error) { if (!stop.IsCancellationRequested) failure = error; }
        });
    }
    byte[] ReadExact(int length) {
        var bytes = new byte[length]; int offset = 0;
        while (offset < length) {
            int count = stream.ReadAsync(bytes, offset, length-offset, stop.Token).GetAwaiter().GetResult();
            if (count == 0) throw new EndOfStreamException("Ghostty picker disconnected");
            offset += count;
        }
        return bytes;
    }
    public void Send(string json) {
        var bytes = Encoding.UTF8.GetBytes(json);
        if (bytes.Length == 0 || bytes.Length > 1048576) throw new IOException("Picker frame exceeds 1 MiB");
        lock (writer) {
            using (var timeout = CancellationTokenSource.CreateLinkedTokenSource(stop.Token)) {
                timeout.CancelAfter(5000);
                var header = BitConverter.GetBytes((uint)bytes.Length);
                stream.WriteAsync(header, 0, 4, timeout.Token).GetAwaiter().GetResult();
                stream.WriteAsync(bytes, 0, bytes.Length, timeout.Token).GetAwaiter().GetResult();
                stream.FlushAsync(timeout.Token).GetAwaiter().GetResult();
            }
        }
    }
    // Short waits let PowerShell observe Ctrl+C instead of blocking indefinitely.
    public string Next(int milliseconds) {
        string value;
        if (incoming.TryTake(out value, milliseconds)) return value;
        if (failure != null) throw new IOException("Ghostty picker connection ended", failure);
        return null;
    }
    public static string Nonce() { return Convert.ToHexString(RandomNumberGenerator.GetBytes(32)).ToLowerInvariant(); }
    public static string Proof(string token, string endpoint, string server, string client, string role) {
        using (var data = new MemoryStream()) {
            foreach (string part in new [] { "ghostty-picker/1", endpoint, server, client, role }) {
                var bytes = Encoding.UTF8.GetBytes(part);
                data.Write(BitConverter.GetBytes((uint)bytes.Length)); data.Write(bytes);
            }
            using (var mac = new HMACSHA256(Encoding.UTF8.GetBytes(token)))
                return Convert.ToHexString(mac.ComputeHash(data.ToArray())).ToLowerInvariant();
        }
    }
    public static bool Verify(string expected, string actual) {
        if (actual == null || actual.Length != 64) return false;
        try { return CryptographicOperations.FixedTimeEquals(Convert.FromHexString(expected), Convert.FromHexString(actual)); }
        catch (FormatException) { return false; }
    }
    public void CancelAfter(string requestId, int milliseconds) {
        cancellation = new Timer(_ => {
            lock (writer) {
                if (activeRequest != requestId) return;
                try { Send("{\"type\":\"cancel\",\"request_id\":\"" + requestId + "\"}"); }
                catch { Dispose(); }
            }
        }, null, milliseconds, Timeout.Infinite);
    }
    public void Dispose() {
        if (Interlocked.Exchange(ref disposed, 1) != 0) return;
        if (cancellation != null) cancellation.Dispose();
        stop.Cancel(); stream.Dispose();
        // The reader observes cancellation; no pipe handle is transferred.
    }
}
'@

# This script runs either here or in the background runspace. It returns the
# authenticated client object, which stays in this process and is never delegated.
$script:GhosttyPickerConnectScript = {
    param([string]$Source, [string]$Endpoint, [string]$Token)
    $ErrorActionPreference = 'Stop'
    if (-not ('GhosttyPickerPipeV2' -as [type])) {
        Add-Type -TypeDefinition $Source -ErrorAction Stop
    }
    if (-not $Endpoint -or -not $Token) { return }
    $client = $null
    function Receive-AuthenticationMessage {
        $deadline = [datetime]::UtcNow.AddSeconds(10)
        while ([datetime]::UtcNow -lt $deadline) {
            $raw = $client.Next(100)
            if ($null -ne $raw) {
                $message = ConvertFrom-Json -InputObject $raw -AsHashtable
                if ($message.type -eq 'error') {
                    $client.Send((ConvertTo-Json -Compress @{type='received';request_id=$message.request_id}))
                    throw "Ghostty picker rejected the connection ($($message.code)): $($message.message)"
                }
                return $message
            }
        }
        throw 'Timed out authenticating Ghostty picker connection.'
    }
    try {
        $client = [GhosttyPickerPipeV2]::new($Endpoint)
        $challenge = Receive-AuthenticationMessage
        if ($challenge.type -ne 'challenge' -or $challenge.version -ne 1 -or $challenge.nonce -notmatch '^[0-9a-f]{64}$') {
            throw 'Invalid picker authentication challenge.'
        }
        $nonce = [GhosttyPickerPipeV2]::Nonce()
        $proof = [GhosttyPickerPipeV2]::Proof($Token, $Endpoint, $challenge.nonce, $nonce, 'client')
        $client.Send((ConvertTo-Json -Compress @{type='authenticate';version=1;client_nonce=$nonce;proof=$proof}))
        $ready = Receive-AuthenticationMessage
        if ($ready.type -ne 'ready') { throw 'Picker pipe is disabled or authentication was rejected.' }
        $expected = [GhosttyPickerPipeV2]::Proof($Token, $Endpoint, $challenge.nonce, $nonce, 'server')
        if (-not [GhosttyPickerPipeV2]::Verify($expected, $ready.proof)) { throw 'Picker server authentication failed.' }
        @{
            Client=$client; Endpoint=$Endpoint; Token=$Token
            Persistent=($ready.ContainsKey('persistent') -and $ready.persistent -eq $true)
        }
        $client = $null # Ownership passes to the caller or background output.
    } finally { if ($null -ne $client) { $client.Dispose() } }
}

function Save-GhosttyPickerInitialConnection($Connection) {
    if ($null -eq $Connection) { return }
    if (-not $script:GhosttyPickerFreshConnection -and $Connection.Persistent -and $Connection.Endpoint -eq $env:GHOSTTY_PICKER_ENDPOINT -and
        $Connection.Token -eq $env:GHOSTTY_PICKER_TOKEN -and $Connection.Client.IsAlive) {
        $script:GhosttyPickerConnection = $Connection
    } else { $Connection.Client.Dispose() }
}

function Initialize-GhosttyPickerClient {
    if ($null -ne $script:GhosttyPickerInitialization) {
        $initialization = $script:GhosttyPickerInitialization
        try {
            $connections = $initialization.PowerShell.EndInvoke($initialization.Pending)
            foreach ($connection in $connections) { Save-GhosttyPickerInitialConnection $connection }
            if ($initialization.PowerShell.HadErrors) {
                Write-Verbose 'Picker background initialization did not connect; the next request will try again.'
            }
        } catch {
            Write-Verbose 'Picker background initialization did not complete; the next request will try again.'
        } finally {
            $initialization.PowerShell.Dispose()
            $script:GhosttyPickerInitialization = $null
        }
    }
    if (-not ('GhosttyPickerPipeV2' -as [type])) {
        Add-Type -TypeDefinition $script:GhosttyPickerClientSource -ErrorAction Stop
    }
}

# Loading outside Ghostty still defines the function and compiles its client.
# Startup connection errors must not break the user's PowerShell profile.
$cached = $script:GhosttyPickerConnection
$alreadyConnected = $null -ne $cached -and $cached.Endpoint -eq $env:GHOSTTY_PICKER_ENDPOINT -and
    $cached.Token -eq $env:GHOSTTY_PICKER_TOKEN -and $cached.Client.IsAlive
if (($FreshConnection -or -not $alreadyConnected) -and $null -eq $script:GhosttyPickerInitialization) {
    Disconnect-GhosttyPicker
    if ($BackgroundInitialize) {
        $loader = [PowerShell]::Create()
        try {
            if ($FreshConnection) {
                [void]$loader.AddScript('param($source); if (-not ("GhosttyPickerPipeV2" -as [type])) { Add-Type -TypeDefinition $source -ErrorAction Stop }').AddArgument($script:GhosttyPickerClientSource)
            } else {
                [void]$loader.AddScript($script:GhosttyPickerConnectScript.ToString()).AddArgument($script:GhosttyPickerClientSource).AddArgument($env:GHOSTTY_PICKER_ENDPOINT).AddArgument($env:GHOSTTY_PICKER_TOKEN)
            }
            $script:GhosttyPickerInitialization = @{PowerShell=$loader; Pending=$loader.BeginInvoke()}
        } catch { $loader.Dispose(); throw }
    } elseif ($FreshConnection) {
        Initialize-GhosttyPickerClient
    } else {
        try {
            $connection = & $script:GhosttyPickerConnectScript $script:GhosttyPickerClientSource $env:GHOSTTY_PICKER_ENDPOINT $env:GHOSTTY_PICKER_TOKEN
            Save-GhosttyPickerInitialConnection $connection
        } catch { Write-Verbose 'Picker startup connection did not complete; the next request will try again.' }
    }
}

function Invoke-GhosttyPicker {
    [CmdletBinding(DefaultParameterSetName='Stream')]
    param(
        [Parameter(Mandatory, ParameterSetName='Named')][string] $Name,
        [Parameter(ParameterSetName='Named')][hashtable] $Parameters = @{},
        [Parameter(ValueFromPipeline, ParameterSetName='Stream')][AllowNull()][object] $InputObject,
        [Parameter(ParameterSetName='Stream')][hashtable] $Options = @{},
        [Parameter(ParameterSetName='Stream')][string] $DisplayProperty,
        [Parameter(ParameterSetName='Stream')][string] $ValueProperty,
        [switch] $PassThruResult,
        [switch] $FreshConnection = $script:GhosttyPickerFreshConnection,
        [ValidateRange(0, 86400)][int] $CancelAfterSeconds = 0
    )
    begin {
        $client = $null
        $requestComplete = $false
        $persistent = $false
        $result = $null
        $batch = [System.Collections.Generic.List[object]]::new()
        $sequence = 0L
        $nextId = 0L
        $originals = [System.Collections.Generic.Dictionary[string,object]]::new()
        $requestId = [guid]::NewGuid().ToString('N')
        $endpoint = $env:GHOSTTY_PICKER_ENDPOINT
        if (-not $endpoint -or -not $env:GHOSTTY_PICKER_TOKEN) {
            throw 'Run this function inside a newly launched Ghostty shell with picker pipe support.'
        }
        function Send-PickerMessage($message) {
            $client.Send((ConvertTo-Json -InputObject $message -Depth 32 -Compress))
        }
        function Receive-PickerMessage([int] $timeoutSeconds = 0) {
            $deadline = if ($timeoutSeconds) { [datetime]::UtcNow.AddSeconds($timeoutSeconds) } else { [datetime]::MaxValue }
            while ([datetime]::UtcNow -lt $deadline) {
                $raw = $client.Next(100)
                if ($null -ne $raw) {
                    $message = ConvertFrom-Json -InputObject $raw -AsHashtable
                    if ($message.type -eq 'error') {
                        Send-PickerMessage @{ type='received'; request_id=$message.request_id }
                        throw "Ghostty picker rejected the connection ($($message.code)): $($message.message)"
                    }
                    if ($message.type -in @('opened', 'ack', 'input_complete', 'result', 'idle') -and $message.request_id -ne $requestId) {
                        throw 'Picker response belongs to a different request.'
                    }
                    if ($message.type -eq 'result') {
                        $client.EndRequest()
                        Send-PickerMessage @{ type='received'; request_id=$requestId }
                    }
                    return $message
                }
            }
            throw 'Timed out waiting for Ghostty picker response.'
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
            Initialize-GhosttyPickerClient
            if ($FreshConnection) { Disconnect-GhosttyPicker }
            $cached = $script:GhosttyPickerConnection
            if ($null -ne $cached -and $cached.Endpoint -eq $endpoint -and
                $cached.Token -eq $env:GHOSTTY_PICKER_TOKEN -and $cached.Client.IsAlive) {
                $client = $cached.Client
                $persistent = $true
            } else {
                Disconnect-GhosttyPicker
                $connection = & $script:GhosttyPickerConnectScript $script:GhosttyPickerClientSource $endpoint $env:GHOSTTY_PICKER_TOKEN
                $client = $connection.Client
                $persistent = $connection.Persistent
                if ($persistent -and -not $FreshConnection) { $script:GhosttyPickerConnection = $connection }
            }
            $client.BeginRequest($requestId)
            $request = switch ($PSCmdlet.ParameterSetName) {
                Named { $strings = @{}; foreach ($key in $Parameters.Keys) { $strings[[string]$key] = [string]$Parameters[$key] }; @{ kind='named'; name=$Name; params=$strings } }
                Stream {
                    $streamOptions = @{}
                    foreach ($key in $Options.Keys) { $streamOptions[$key] = $Options[$key] }
                    if ($streamOptions.ContainsKey('initial_query')) {
                        $streamOptions.initial_query = [string]$streamOptions.initial_query
                    }
                    @{ kind='stream'; options=$streamOptions }
                }
            }
            Send-PickerMessage @{ type='open'; request_id=$requestId; request=$request }
            $reply = Receive-PickerMessage 15
            if ($reply.type -eq 'result') { $result = $reply }
            elseif ($reply.type -ne 'opened') { throw 'Unexpected picker open response.' }
            if ($CancelAfterSeconds -and $null -eq $result) { $client.CancelAfter($requestId, $CancelAfterSeconds * 1000) }
        } catch {
            if ($null -ne $client) { $client.Dispose() }
            throw
        }
    }
    process {
        if ($PSCmdlet.ParameterSetName -eq 'Stream' -and $null -eq $result -and $PSBoundParameters.ContainsKey('InputObject')) {
            try {
                $raw = $client.Next(0)
                if ($null -ne $raw) {
                    $reply = ConvertFrom-Json -InputObject $raw -AsHashtable
                    if ($reply.type -ne 'result' -or $reply.request_id -ne $requestId) { throw 'Unexpected picker response during streaming.' }
                    $client.EndRequest()
                    Send-PickerMessage @{ type='received'; request_id=$requestId }
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
                $client.Dispose()
                throw
            }
        }
    }
    end {
        try {
            if ($PSCmdlet.ParameterSetName -eq 'Stream' -and $null -eq $result) {
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
            if ($persistent) {
                $idle = Receive-PickerMessage 10
                if ($idle.type -ne 'idle') { throw 'Picker connection did not return to idle.' }
                Send-PickerMessage @{ type='received'; request_id=$requestId }
            }
            $client.EndRequest()
            $requestComplete = $true
            if ($result.status -eq 'error') { throw "Picker request failed ($($result.code)): $($result.message)" }
            if ($PassThruResult) { $result }
            elseif ($result.status -eq 'accepted') {
                foreach ($selected in $result.selected) {
                    if ($PSCmdlet.ParameterSetName -eq 'Stream' -and -not $ValueProperty -and $originals.ContainsKey([string]$selected.id)) {
                        $originals[[string]$selected.id]
                    } else { $selected.value }
                }
            }
        } finally {
            if ($null -ne $client -and ($FreshConnection -or -not $persistent -or -not $requestComplete)) { $client.Dispose() }
        }
    }
    clean {
        if ($null -ne $client -and ($FreshConnection -or -not $persistent -or -not $requestComplete)) { $client.Dispose() }
    }
}

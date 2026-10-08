# Picker pipe, protocol version 1

The Windows egui host owns one local duplex byte-mode named pipe per pane.
The protocol, schema, and request lifecycle are independent of Windows; a
future Linux implementation can use a Unix-domain stream socket. Linux is not
implemented here.

## Enable and test

Add this to the table returned by your Ghostty Lua configuration:

```lua
picker_pipe = {
  enabled = true,
  allow_stream = true,
  named = { "files", "lines", "words" }, -- exact configured picker names
},
```

All permissions default to disabled. Named pickers require an explicit
allowlist, including builtin names such as `builtin.lines` if that is the
name used by your configuration. Only named and streamed requests are supported.
Commands and command previews must be defined in an allowlisted Lua picker;
clients cannot submit executable definitions or command previews. Lua factories
and providers in allowlisted named pickers are trusted executable configuration.

Build with `zig build egui-sample -Doptimize=ReleaseFast`.
Restart Ghostty, then dot-source the helper **inside the new shell**:

```powershell
. C:\src\ghostty2\example\rust-egui-terminal\Invoke-GhosttyPicker.ps1

# Named picker; values return to PowerShell, normal accept callbacks do not run.
Invoke-GhosttyPicker -Name files -Parameters @{ cwd = $PWD.Path }

# Stream objects; return the selected original PowerShell object.
Get-ChildItem | Invoke-GhosttyPicker -DisplayProperty Name

# Stream display/value pairs; return the selected FullName property.
Get-ChildItem | Invoke-GhosttyPicker -DisplayProperty Name -ValueProperty FullName

# Inspect the complete result, including status and selected IDs.
'first', 'second', 'third' | Invoke-GhosttyPicker -PassThruResult

# Exercise client cancellation while the picker is open.
'first', 'second' | Invoke-GhosttyPicker -CancelAfterSeconds 2 -PassThruResult
```

PowerShell 7.3+ is required. Escape cancels from the picker. Ctrl+C closes the
client connection, cancelling its picker. A closed connection cannot receive a
terminal result. The helper stops sending after an early result, but it cannot
guarantee that arbitrary upstream pipeline commands stop producing objects.
For pipeline input it retains an ID-to-original-object mapping. Use
`-ValueProperty` to return a serializable property instead of the original object.

To compile the C# pipe client and connect/authenticate asynchronously during
profile loading, use:

```powershell
. C:\src\ghostty2\example\rust-egui-terminal\Invoke-GhosttyPicker.ps1 -BackgroundInitialize
```

The function is defined immediately. The background runspace compiles the client,
opens the pipe, and authenticates without waiting for a picker call. The status
indicator changes when authentication completes. The first picker call collects
the already connected client, waiting for initialization if it is still running,
then disposes the background runspace. Compilation happens once per PowerShell
process. Without this switch, compilation and connection complete synchronously
during script load. Reloading the script preserves a healthy connection.

Loading outside Ghostty defines the helper without attempting a pipe connection.
Startup connection failures do not interrupt the profile; a later picker call
tries again and reports any failure. `Disconnect-GhosttyPicker` also collects and
closes a pending background connection.

The helper opens one authenticated connection per PowerShell session during
script loading. Selection, Escape, and timed cancellation end the request and retain
the connection. Ctrl+C, transport errors, and incomplete requests discard it.
The next call reconnects if the cached connection has ended or the endpoint/token
has changed. A submitted request is never automatically replayed.

When the pipe is enabled, the terminal status area shows `Pipe: connected` for
an authenticated client, including between requests, and `Pipe: disconnected`
otherwise. To explicitly release the pane's connection slot:

```powershell
Disconnect-GhosttyPicker
```

Example PSReadLine binding (append to your profile after dot-sourcing):

```powershell
Set-PSReadLineKeyHandler -Chord Alt+f -ScriptBlock {
    $path = Get-ChildItem -File |
        Invoke-GhosttyPicker -DisplayProperty Name -ValueProperty FullName
    if ($null -ne $path) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert("'" + $path.Replace("'", "''") + "'")
    }
}
```

Selections are returned as data; this binding quotes a path for PowerShell.
The helper does not evaluate returned strings or run acceptance callbacks.

## Wire format and authentication

Each frame is `u32 little-endian length` followed by that many UTF-8 JSON bytes.
Maximum frame size is 1 MiB. Read/write call boundaries are not frame boundaries.
Unknown fields, duplicate JSON keys (including nested objects), unsupported
versions, invalid IDs, and invalid message ordering are rejected.

The pane's shell launch environment contains:

```
GHOSTTY_PICKER_ENDPOINT=ghostty-picker-<random 256-bit identifier>
GHOSTTY_PICKER_TOKEN=<independent random 256-bit token encoded as hex>
```

The endpoint is a basename; the client connects to `\\.\pipe\<endpoint>`.
These variables are not persisted or set globally. Shell descendants inherit
them. The token is the UTF-8 hex string used as the HMAC key, not decoded bytes.

Handshake:

1. Server sends `{"type":"challenge","version":1,"nonce":"<64 hex chars>"}`.
2. Client sends `authenticate` with `version`, `client_nonce`, and `proof`.
3. Server verifies the effective client token, process ancestry, and client proof.
4. Server sends `ready` with `version`, server `proof`, capability policy, limits,
   `persistent:true` to advertise sequential connection reuse. The client
   falls back to closing after each request when this field is absent.
5. Client verifies the server proof before sending a picker request.

Each proof uses HMAC-SHA256 over five fields, each encoded as a little-endian
u32 byte length followed by UTF-8 bytes: `ghostty-picker/1`, endpoint basename,
server nonce, client nonce, and role (`client` or `server`). There are five
fields total. Different roles prevent reflection. Fresh challenges prevent
replaying authentication proofs. Proof comparisons are constant-time for valid
lengths. Tokens and proofs must not be logged.

## Requests

Only one request is active at a time per authenticated connection. Subsequent
requests reuse authentication, but effective identity, process/shell ancestry,
and current capabilities are checked again for every open. Request IDs are
nonempty strings up to 128 bytes. The pane cannot be retargeted by the request.


```json
{"type":"open","request_id":"r1","request":{"kind":"named","name":"files","params":{"cwd":"G:\\src"}}}
```

```json
{"type":"open","request_id":"r1","request":{"kind":"stream","options":{"initial_query":"","result_rows":7,"preview_rows":14,"preview_visible":false}}}
```

Stream options also accept a static `Text` or `Ansi` preview in the existing
preview schema. Command previews are rejected. Streamed items do not currently
support per-item previews or updates.

Server sends `opened` after UI creation, or a terminal error result. Another
active overlay returns `busy`. There is one pipe connection slot per pane;
another client waits at most five seconds to connect and fails if it remains
occupied. A persistent client retains that slot until it disconnects; use
`Disconnect-GhosttyPicker` before invoking a helper from a different process in
the same pane. There is no request queue or automatic request replay.

## Streaming and results

Items are append-only with unique IDs and arbitrary JSON values:

```json
{"type":"items","request_id":"r1","sequence":0,"items":[{"id":"a","text":"README.md","value":"G:\\src\\README.md"}]}
```

The server sends `ack` with the request ID and sequence after publication into
nfm. Sequence numbers start at zero, with one batch in flight. Batches contain
1–1024 items. ID length is at most 128 bytes. Combined encoded ID/text/value
size is at most 64 KiB per item. Stream input is limited to one million items
and 256 MiB of encoded item data; actual memory usage includes additional
storage, indices, and search state.

```json
{"type":"end_items","request_id":"r1","last_sequence":0}
```

An empty stream uses `last_sequence:null`. The server sends `input_complete`.
Input completion does not close the picker or connection. The picker may be
accepted before input completion. Version 1 returns a single selection:

```json
{"type":"result","request_id":"r1","status":"accepted","selected":[{"id":"a","text":"README.md","value":"G:\\src\\README.md"}],"query":"read"}
```

Named results use the selected source index as their ID and return
the source's normal accepted value. They bypass application actions, terminal
insertion, and configured acceptance callbacks.

Cancellation:

```json
{"type":"cancel","request_id":"r1"}
```

```json
{"type":"result","request_id":"r1","status":"cancelled","reason":"client"}
```

Acceptance and cancellation race through one terminal transition; the first
wins. Client disconnect, malformed messages, and pane closure cancel the owned
picker. Command reader cancellation is requested when the picker is dropped;
this cannot reverse command side effects already performed.

After any terminal result, the client sends:

```json
{"type":"received","request_id":"r1"}
```

The server waits up to five seconds for this receipt, draining outstanding input
for that request. It then drops the request state and sends:

```json
{"type":"idle","request_id":"r1"}
```

The client acknowledges `idle` with another `received` for the same request ID
before reusing the connection. An invalid/missing acknowledgment disconnects.
These receipts avoid discarding unread completion bytes in `DisconnectNamedPipe`.
Delivery is still not guaranteed across crashes, timeouts, or connection loss.

Errors are terminal results with `status:"error"`, `code`, and `message` when a
valid request is available. Authentication/security/framing failures disconnect
without echoing request content. The handshake has a five-second deadline;
UI acknowledgments have ten-second deadlines; partial payloads and
blocked writes have five-second deadlines. Waiting for user selection has no
deadline, and idle authenticated connections have no idle timeout. Cancellation
is read concurrently with response delivery. A Lua reload that revokes the active
request capability cancels that request. Disabling the pipe or exit of the pane
shell closes idle connections; pane closure stops the service.

## Security controls and remaining risks

- An explicit DACL grants precise data, attribute-read, security-read, and
  synchronization rights to the current logon SID. It excludes pipe-instance
  creation, ACL modification, and ownership modification from client rights.
- Local clients only (`PIPE_REJECT_REMOTE_CLIENTS`), randomized endpoint, initial
  pipe-instance protection, and an explicit medium/high mandatory integrity label
  denying reads and writes from lower-integrity clients.
- All server/client/event handles are non-inheritable. No pipe handles are
  intentionally duplicated, passed to children, or delegated. Helpers open and
  authenticate their own connections.
- After authentication and every request-bearing message, the server uses
  `ImpersonateNamedPipeClient`, queries the effective token, and always reverts.
  Failed impersonation/inspection rejects the connection. Failed reversion
  terminates the process rather than continuing with an unknown thread identity.
- Effective and primary client identities must match the host's user,
  authentication/logon ID, session, integrity, elevation/type, group attributes,
  and privilege attributes. Restricted tokens
  and AppContainers are rejected. Identification level or higher is required.
  The provided client requests dynamic security context tracking. Legitimate
  clients that deliberately change token groups or privileges are also rejected.
- The connecting PID comes from Windows, not JSON. The retained client process
  handle must remain alive. Ancestry must reach the recorded pane shell, whose
  original PID and Windows creation time are captured at launch. A retained
  shell handle and ancestor creation-time checks protect against PID reuse.
- Disabled by default, separately permitted request types, explicit named picker
  allowlist, bounded frames/batches/storage, and return-only acceptance.
- Clients cannot submit command definitions or command previews. Executable
  behavior is limited to trusted allowlisted Lua picker configuration.

These controls reject direct unelevated callers to an elevated host, even when
they possess a token. Ancestry is supporting evidence, not isolation from
same-user malware. A trusted elevated client can relay attacker requests or
deliberately leak authorization. Static pipe security contexts and delegated
handles are another reason not to describe effective-token inspection as proof
of the origin of every byte. Executing a script/executable/configuration that
an attacker can modify is still unsafe in an elevated host. Argument arrays
avoid implicit shell concatenation but do not make dangerous tools or arguments
safe. The pipe does not grant permission to cross privilege contexts.

Tests cover real Windows pipe exchanges using the PowerShell helper, valid and
wrong tokens, anonymous effective identity, unrelated-shell ancestry, capability
denial, streaming before completion, selection response, and client cancellation.
An actual elevated-host/unelevated-client test and independent security review
remain necessary before treating elevated operation as hardened. Such a test
must verify rejection even after deliberately copying the endpoint and token
to the lower-privilege process. GUI focus/PSReadLine behavior needs interactive
testing in the rebuilt terminal.

References: [pipe ACLs](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights),
[pipe creation](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew),
[effective client security context](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-impersonatenamedpipeclient),
[handle duplication](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle).

Pipe requests and search-result updates now wake the native window event loop
and request a redraw directly. Egui repaint requests remain as a fallback.
GPU presentation keeps its existing pacing.

To compare a new authenticated connection per request with connection reuse:

```powershell
'one', 'two' | Invoke-GhosttyPicker -FreshConnection
```

For every request to use a new connection, dot-source the helper with:

```powershell
. C:\src\ghostty2\example\rust-egui-terminal\Invoke-GhosttyPicker.ps1 -FreshConnection -BackgroundInitialize
```

This compiles the helper in the background without opening a startup connection.
Each request completes the result and idle acknowledgments, then closes its
connection. `-FreshConnection:$false` overrides the default for an individual
call. Diagnostics include `fresh_connection`; fresh requests report
`reused=false` and measure connection and authentication time separately.
The server needs no changes.

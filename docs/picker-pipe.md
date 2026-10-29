# Opening pickers from the shell

Shells open allowlisted named pickers through the authenticated local named pipe
(Windows) or Unix socket (Linux). On Linux, use `npick`, installed beside
`nterm`; see [npick on Linux](#npick-on-linux). On Windows, the supplied
helper requires PowerShell 7.3+.
Pipe access is disabled by default. Add this to your returned Lua table, restart
the application, then load the helper inside its newly launched shell:

```lua
picker_pipe = {
  enabled = true,
  named = { "files", "builtin.lines" },
  allow_stream = true,
},
```

`named` is an exact allowlist. `files` is a custom picker defined in Lua `pickers`,
such as the FileSystem example in [Custom pickers in Lua](pickers.md#custom-pickers-in-lua);
built-ins use their `builtin.` names. `allow_stream` permits candidates sent
directly from the shell.

## Open a configured picker by name

```powershell
. C:\src\nterm\Invoke-NtermPicker.ps1

$path = Invoke-NtermPicker -Name files
$line = Invoke-NtermPicker -Name builtin.lines

# For a configured Command picker such as Ripgrep in the sample Lua file:
$match = Invoke-NtermPicker -Name Ripgrep -Parameters @{ pattern = 'TODO'; cwd = $PWD.Path }
```

Allowlist `Ripgrep` as well to run that example. Named requests use the source,
preview, and factory configured in Lua. String parameters are available to
callbacks as `params`; Command sources substitute whole `{name}` arguments from
them, and `params.cwd` overrides the command's configured directory. FileSystem
roots still come from Lua, independently of the shell's working directory.

Pipe acceptance returns the selected value to the caller. It bypasses terminal
insertion, navigation, and acceptance callbacks, including for `builtin.lines`.
The shell decides how to use the result.

## Stream custom candidates from the shell

No named Lua picker is needed; enable `allow_stream` and dot-source the helper
as above. It returns the selected original object, or the property specified
with `-ValueProperty`:

```powershell
# Return the selected FileInfo object.
$file = Get-ChildItem -File | Invoke-NtermPicker -DisplayProperty Name

# Return its FullName string instead.
$path = Get-ChildItem -File |
    Invoke-NtermPicker -DisplayProperty Name -ValueProperty FullName

# Inspect status, query, and selection.
'first', 'second', 'third' | Invoke-NtermPicker -PassThruResult
```

## Stream candidates into a named picker

Piping into `-Name` streams the candidates into a configured `type = "Items"`
picker. The Lua definition keeps its presentation and preview, including
Command and Dynamic previews that a plain stream cannot use, and only the name
needs to be in `named`:

```powershell
# ProfileCompletions in nterm.example.lua previews Get-Help for each value.
$completions.CompletionMatches |
    Invoke-NtermPicker -Name ProfileCompletions -Parameters @{ commandName = 'Get-Item' } `
        -DisplayProperty ListItemText -ValueProperty CompletionText
```

Enter accepts; Escape cancels. Ctrl+C disconnects the client and cancels its
picker. Ordinary completion keeps the authenticated connection for the next
request. Use `Disconnect-NtermPicker` to release it before connecting from
another process in the same pane. Another active overlay can return `busy`;
requests are not queued. Returned paths are not automatically shell-quoted.

## npick on Linux

With `picker_pipe` enabled as above, `npick` opens pickers from the shell
running in a pane and prints the selected value:

```sh
npick files                        # a named picker from nterm.lua
git branch --format='%(refname:short)' | npick --query main
```

Without a name, each stdin line is a candidate. `--display N` and `--value N`
show and return tab-separated fields (`-d` sets the separator), and `--json`
reads `{"text": ..., "value": ...}` lines. With `--stdin NAME`, the stdin lines
fill the named Items picker instead, which keeps its Lua preview. It exits 0
with the value on stdout,
130 when cancelled, and 2 on errors. For example, bind Ctrl+T in bash to insert
a file:

```bash
__npick_file() {
  local file
  file=$(npick files) || return
  READLINE_LINE="${READLINE_LINE:0:READLINE_POINT}$file${READLINE_LINE:READLINE_POINT}"
  READLINE_POINT=$((READLINE_POINT + ${#file}))
}
[[ -n $NTERM_PICKER_ENDPOINT ]] && bind -x '"\C-t": __npick_file'
```

[npick-completion.bash](../npick-completion.bash) is the bash counterpart of the
PowerShell profile's completion picker. Source it from `~/.bashrc`
(`deploy.sh` installs it to `share/nterm/` beside the app). In
nterm panes, Ctrl+Space lists what Tab would complete for the word at
the cursor, using bash-completion when it is installed; Enter replaces that
word, quoting file names. A key binding cannot enable readline's unquoting, so
for words containing quotes, backslashes or `~` it falls back to bash's own
file, directory and `$variable` completion.

See [the protocol](#picker-pipe-protocol-version-1) below and
[Invoke-NtermPicker.ps1](../Invoke-NtermPicker.ps1) for PSReadLine bindings,
stream options, protocol limits, authentication and cancellation.
For a portable PowerShell profile with file, directory, history, text-search,
and completion bindings, see [profile.example.ps1](../profile.example.ps1).
Executable sources and command previews must be defined in allowlisted Lua
pickers; clients cannot submit executable definitions.

## Picker pipe, protocol version 1

The egui host owns one local connection endpoint per pane: a duplex byte-mode
named pipe on Windows and a Unix-domain stream socket on Linux. Both speak the
protocol below. On Linux, use the `npick` client; see
[Linux transport](#linux-transport).

### Enable and test

For a portable set of PowerShell bindings, see [profile.example.ps1](../profile.example.ps1).
Use `nterm.example.lua`, or merge these entries into your Lua configuration
(preserving any existing pickers and permissions):

```lua
picker_pipe = {
  enabled = true,
  named = { "ProfileFiles", "ProfileDirectories", "ProfileRipgrep", "ProfileHistory", "ProfileCompletions" },
},
pickers = {
  ProfileFiles = {
    type = "FileSystem", roots = { "." }, files_only = true,
    position = "Cursor", preview_visible = false,
  },
  ProfileDirectories = {
    type = "FileSystem", roots = { "." }, directories_only = true,
    position = "Cursor", preview_visible = false,
  },
  ProfileRipgrep = {
    type = "Command", position = "Cursor", success_exit_codes = { 0, 1 },
    args = { "rg", "--no-messages", "--color=never", "-i", "--vimgrep", "--", "{pattern}" },
  },
  ProfileHistory = {
    type = "File", path = "{history}", reverse = true,
    position = "Cursor", sort = "Source", line_continuation = "`", unique = true,
  },
  -- The profile streams its completions into this Items picker. For a
  -- Get-Help preview, copy ProfileCompletions' preview from nterm.example.lua.
  ProfileCompletions = {
    type = "Items", items = nterm.json_parse("[]"), position = "Cursor",
  },
},
```

Then add this to your PowerShell `$PROFILE`, replacing the path:

```powershell
. 'C:\src\nterm\profile.example.ps1' -PickerRoots @('C:\your\projects')
```

The helper is loaded relative to the example script. Omit `-PickerRoots` to use
the directory where the profile loads. Ctrl+T/Ctrl+O search files/directories
under the current directory; Alt+T/Alt+O use the configured roots. Alt+R searches
PSReadLine's saved history, Ctrl+Shift+G searches with `rg` and inserts an `nvim`
command for review, and Ctrl+Space opens PowerShell completions. Customize the
editor command and shortcuts as needed; dot-sourcing replaces those bindings.

PowerShell 7.4+ (for `-CommandWithArgs`) and PSReadLine are required. Ripgrep
is optional for text search.
The script does nothing outside an nterm pane: it returns unless
`NTERM_PICKER_ENDPOINT` and `NTERM_PICKER_TOKEN` are set, and loads
`Invoke-NtermPicker.ps1` only when `Invoke-NtermPicker` is not already defined.
File and directory bindings use nterm's builtin FileWalker through the named
FileSystem pickers, passing the selected roots and current directory; no `fd` or
custom history-reader executable is required.
History opens the ProfileHistory File picker, which reads PSReadLine's history
file directly, newest first; the profile passes its `HistorySavePath` as
`params.history`. Text search opens the ProfileRipgrep Command picker, so nterm
runs `rg` and results appear as they arrive. Completions pipe
PowerShell's matches into the ProfileCompletions Items picker (a named stream,
see [Requests](#requests)), so the Lua configuration supplies the preview and
there is no item cap below the stream limits. Command previews are optional in
`nterm.example.lua`. The profile sends no client-defined streams, so
`allow_stream` can stay `false`.
Fresh connections avoid retaining the pane's connection slot between requests;
profile initialization still connects until the first request completes. Run
`Disconnect-NtermPicker` before starting another PowerShell process in that pane.

Add this to the table returned by your nterm Lua configuration:

```lua
picker_pipe = {
  enabled = true,
  allow_stream = true,
  named = { "files", "builtin.lines", "builtin.words" }, -- exact picker names
},
```

All permissions default to disabled. Named pickers require an explicit
allowlist, including builtin names such as `builtin.lines` if that is the
name used by your configuration. Only named and streamed requests are supported.
Commands and command previews must be defined in an allowlisted Lua picker;
clients cannot submit executable definitions or command previews. Lua factories
and providers in allowlisted named pickers are trusted executable configuration.

Build with `.\build.ps1`.
Restart nterm, then dot-source the helper **inside the new shell**:

```powershell
. C:\src\nterm\Invoke-NtermPicker.ps1

# Named picker; values return to PowerShell, normal accept callbacks do not run.
Invoke-NtermPicker -Name files -Parameters @{ cwd = $PWD.Path }

# Stream objects; return the selected original PowerShell object.
Get-ChildItem | Invoke-NtermPicker -DisplayProperty Name

# Stream display/value pairs; return the selected FullName property.
Get-ChildItem | Invoke-NtermPicker -DisplayProperty Name -ValueProperty FullName

# Stream items into a configured Items picker, which keeps its Lua preview.
# Needs only the name in picker_pipe.named, not allow_stream.
Get-ChildItem | Invoke-NtermPicker -Name ProfileCompletions -DisplayProperty Name -ValueProperty FullName

# Inspect the complete result, including status and selected IDs.
'first', 'second', 'third' | Invoke-NtermPicker -PassThruResult

# Exercise client cancellation while the picker is open.
'first', 'second' | Invoke-NtermPicker -CancelAfterSeconds 2 -PassThruResult
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
. C:\src\nterm\Invoke-NtermPicker.ps1 -BackgroundInitialize
```

The function is defined immediately. The background runspace compiles the client,
opens the pipe, and authenticates without waiting for a picker call. The status
indicator changes when authentication completes. The first picker call collects
the already connected client, waiting for initialization if it is still running,
then disposes the background runspace. Compilation happens once per PowerShell
process. Without this switch, compilation and connection complete synchronously
during script load. Reloading the script preserves a healthy connection.

Loading outside nterm defines the helper without attempting a pipe connection.
Startup connection failures do not interrupt the profile; a later picker call
tries again and reports any failure. `Disconnect-NtermPicker` also collects and
closes a pending background connection.

The helper opens one authenticated connection per PowerShell session during
script loading. Selection, Escape, and timed cancellation end the request and retain
the connection. Ctrl+C, transport errors, and incomplete requests discard it.
The next call reconnects if the cached connection has ended or the endpoint/token
has changed. A submitted request is never automatically replayed.

When the pipe is enabled, the status bar shows a two-arrow pipe icon: in the
accent color while an authenticated client is connected, including between
requests, and muted otherwise. Hovering it shows the state. To explicitly
release the pane's connection slot:

```powershell
Disconnect-NtermPicker
```

Example PSReadLine binding (append to your profile after dot-sourcing):

```powershell
Set-PSReadLineKeyHandler -Chord Alt+f -ScriptBlock {
    $path = Get-ChildItem -File |
        Invoke-NtermPicker -DisplayProperty Name -ValueProperty FullName
    if ($null -ne $path) {
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert("'" + $path.Replace("'", "''") + "'")
    }
}
```

Selections are returned as data; this binding quotes a path for PowerShell.
The helper does not evaluate returned strings or run acceptance callbacks.

### Wire format and authentication

Each frame is `u32 little-endian length` followed by that many UTF-8 JSON bytes.
Maximum frame size is 1 MiB. Read/write call boundaries are not frame boundaries.
Unknown fields, duplicate JSON keys (including nested objects), unsupported
versions, invalid IDs, and invalid message ordering are rejected.

The pane's shell launch environment contains:

```
NTERM_PICKER_ENDPOINT=nterm-picker-<random 256-bit identifier>
NTERM_PICKER_TOKEN=<independent random 256-bit token encoded as hex>
```

On Windows the endpoint is a basename; the client connects to
`\\.\pipe\<endpoint>`. On Linux it is the socket's absolute path.
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
u32 byte length followed by UTF-8 bytes: `nterm-picker/1`, endpoint basename,
server nonce, client nonce, and role (`client` or `server`). There are five
fields total. Different roles prevent reflection. Fresh challenges prevent
replaying authentication proofs. Proof comparisons are constant-time for valid
lengths. Tokens and proofs must not be logged.

### Requests

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

A named request with `"stream":true` combines the two: the configured picker
supplies the presentation and preview (including Command and Dynamic previews),
and the client supplies its items in `items` frames as described below.

```json
{"type":"open","request_id":"r1","request":{"kind":"named","name":"ProfileCompletions","params":{"commandName":"Get-Item"},"stream":true}}
```

The name must be listed in `picker_pipe.named`; `allow_stream` is not required,
because the configuration, not the client, defines the picker and its preview.
The picker must be `type = "Items"`, or the request fails with
`invalid_definition`. Streamed items replace any configured `items`. Previews
receive each item's `value` when it is a string, otherwise its JSON encoding;
the result returns the client's item ID and value, as for a stream.
`Invoke-NtermPicker` sends this request when input is piped to `-Name`, and
`npick --stdin NAME` sends it with the lines of stdin.

Server sends `opened` after UI creation, or a terminal error result. Another
active overlay returns `busy`. There is one pipe connection slot per pane;
another client waits at most five seconds to connect and fails if it remains
occupied. A persistent client retains that slot until it disconnects; use
`Disconnect-NtermPicker` before invoking a helper from a different process in
the same pane. There is no request queue or automatic request replay.

### Streaming and results

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
the source's normal accepted value; a named stream returns the streamed item's
ID and value instead. They bypass application actions, terminal
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

### Linux transport

Each pane listens on `$XDG_RUNTIME_DIR/nterm/<pid>/pane-<id>.sock`, or a
`nterm-<uid>` directory under the temporary directory when no runtime
directory is set. Both directories must be owned by the user and mode `0700`;
the socket is `0600`. On startup, directories of terminals that are no longer
running are removed; closing a pane removes its socket.

The kernel reports the connecting process's user (`SO_PEERCRED`); connections
from other users are closed before the challenge. The handshake, framing,
limits, and request lifecycle are unchanged. Windows' token, integrity, and
process ancestry checks have no equivalent: on Linux, processes of the same
user can read each other's environment (including the token) and memory, so
the token is not a boundary between them.

`npick` is the client:

```sh
npick files                       # a named picker; prints the selected value
npick root_cd                     # params: npick NAME KEY=VALUE...
fd -t f | npick --query src       # stream lines from stdin
ps -eo pid,comm | npick -d ' ' --value 1
compgen -c | npick --stdin completions   # stdin into a named Items picker
```

Named requests add the caller's directory as `params.cwd` unless given. It
exits 0 with the value on stdout, 130 when cancelled, and 2 on errors.
`npick --help` lists all options.

### Security controls and remaining risks

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

Pipe requests and search-result updates wake the native window event loop
and request a redraw directly. Egui repaint requests remain as a fallback.
GPU presentation keeps its existing pacing.

To compare a new authenticated connection per request with connection reuse:

```powershell
'one', 'two' | Invoke-NtermPicker -FreshConnection
```

For every request to use a new connection, dot-source the helper with:

```powershell
. C:\src\nterm\Invoke-NtermPicker.ps1 -FreshConnection -BackgroundInitialize
```

This compiles the helper in the background without opening a startup connection.
Each request completes the result and idle acknowledgments, then closes its
connection. `-FreshConnection:$false` overrides the default for an individual
call. Diagnostics include `fresh_connection`; fresh requests report
`reused=false` and measure connection and authentication time separately.
The server needs no changes.

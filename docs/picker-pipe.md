# Opening pickers from the shell

Shells open allowlisted named pickers through the session daemon that runs
them (Windows) or through the pane's Unix socket (Linux). `npick`, installed
beside `nterm`, is the client on both; see [npick](#npick). For PowerShell
there is also a script that starts no program: `Invoke-NtermPicker.ps1`, which
requires PowerShell 7.3+.
The same pipe can start a command in a new pane: see
[Open a pane beside this one](#open-a-pane-beside-this-one).
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
picker. Each request has a connection of its own, so nothing is held between
requests. Another active overlay, or another request already open in the pane,
returns `busy`; requests are not queued. Returned paths are not automatically
shell-quoted.

## Have the session daemon run the picker

With `-DaemonPicker` the session daemon that runs the shell lists and searches,
on the machine the shell is on, and the window only draws the rows it is sent
and says what was typed and which row was chosen. It is the kind that works in
a window on another machine (`--remote`), where the pickers above are refused:
they would list that window's own files.

```powershell
# A file under the current directory; returns its full path.
Invoke-NtermPicker -DaemonPicker files -Parameters @{ cwd = $PWD.Path }

# A folder under any of several roots.
Invoke-NtermPicker -DaemonPicker directories -Parameters @{
    roots = (ConvertTo-Json -InputObject @('C:\src', 'D:\work') -Compress)
}
```

The daemon has five pickers. For all of them `query` is the text the picker
opens with, and `position = "cursor"` shows it at the pane's cursor instead of
in the middle of the pane. The window needs `picker_pipe.enabled`, and nothing
in `named`.

`files` and `directories`: `roots` is a JSON list of folders; without it `cwd`
is the one root. With one root the rows are paths from it; the value returned
is always the whole path.

`history` lists the lines of the file `path` names, the last first and each
once, joining lines that end in a backtick to the next: PSReadLine's history
file. What matches stays in that order. The value returned is the command as
it was typed, on its own lines.

```powershell
Invoke-NtermPicker -DaemonPicker history -Parameters @{
    path = (Get-PSReadLineOption).HistorySavePath
    position = 'cursor'
}
```

`items` lists what is piped in, as the window's own stream picker does, and
is called `label`. It takes up to 100,000 rows.

```powershell
Get-Process | Invoke-NtermPicker -DaemonPicker items -DisplayProperty Name `
    -Parameters @{ label = 'Processes' }
```

`ripgrep` runs `rg` once in `cwd` for `pattern`, a regular expression, and
lists each place it is found as `path:line:column:text`. What is typed in the
picker narrows those lines; it does not search again. `rg` has to be on the
`PATH` the daemon was started with. If it cannot be run, or fails having found
nothing (a pattern it cannot read, say), the picker ends with what it said.

```powershell
Invoke-NtermPicker -DaemonPicker ripgrep -Parameters @{ cwd = $PWD.Path; pattern = 'fn main' }
```

A row's preview is its file with the line it was found on in the middle,
marked; `preview` `true` opens it showing. A file of more than 256 KB is sent
as the lines round that one, so line numbers shown beside it count from the
first line sent, not the file's first.

`history` and `items` have no previews.

Each row of `files` and `directories` has a preview, hidden until Ctrl+P shows it; with `preview = "true"`
in the parameters it opens shown. A file shows its first 256 KB as text,
coloured by its name, a picture (png, jpg, jpeg, gif, bmp, webp, tif, tiff) of up to 10 MiB shows as the picture, and a folder shows what is in it. The daemon reads them
and the window lays them out. The rest of a longer file is not
shown yet.

`profile.example.ps1` binds these pickers to
Ctrl+Shift+O and Ctrl+Shift+T (under the current directory) and to Alt+Shift+O
and Alt+Shift+T (under the roots), history to Alt+Shift+R, and PowerShell's
completions, through `items`, to Ctrl+Shift+Space.

### Pickers of your own: `nterm-daemon.lua`

The daemon also runs the pickers of a file on its own machine,
`nterm-daemon.lua`. It looks for `~/nterm-daemon.lua`, then for
`nterm-daemon.lua` beside `nterm-daemon.exe`, and uses the first there is
(`nterm-daemon --lua-config FILE` names one in their place). The file is read
when a picker of it is first asked for and again whenever it has changed, so
there is nothing to restart. `nterm-daemon.example.lua` is one to start from.

A picker there is written as one in `nterm.lua` is, under `pickers`, and is
opened by its name as the daemon's own are:

```lua
-- nterm-daemon.lua, on the machine the shell is on
return { pickers = {
  Commits = { type = "Command", sort = "SourceOrder",
              args = { "git", "log", "--oneline", "-n", "500" } },
} }
```

```lua
-- nterm.lua, on the machine the window is on
Commits = { type = "Daemon", picker = "Commits",
            action = function(app, item) app:send_text("git show " .. item.value:match("^%x+")) end },
```

```powershell
Invoke-NtermPicker -DaemonPicker Commits -Parameters @{ cwd = $PWD.Path }
```

So far it runs these types, with the fields they have in `nterm.lua`:

| Type | Fields |
| --- | --- |
| `Command` | `args`, `cwd`, `success_exit_codes`, `line_continuation`, `unique` |
| `File` | `path`, `reverse`, `line_continuation`, `unique` |
| `Items` | `items`, a list of `{ text, value }` |
| `FileSystem` | `roots`, `max_depth`, `files_only`, `directories_only`, `relative` |

and of the fields every picker has: `position` (`Cursor` shows it at the
cursor), `sort`, `initial_query` and `preview_visible`.

What the picker is opened with fills in the rest. An argument of `args`, or
`path`, written as `"{name}"` is the text given under that name, as one
argument whole whatever is in it; the picker is refused if it was not given.
`cwd` is where a `Command` runs and what a relative `path` and a `FileSystem`
with no `roots` are under; a window gives the directory the shell reported.
The program a `Command` runs is always the one the file names: `args[1]`
cannot be `"{name}"`. Nothing a window or a program in a shell sends can make
the daemon run a program the file does not name.

What shows for a row is the file's to say, as in `nterm.lua`: `preview = {
type = "Dynamic", resolve = function(item, params) ... end }`. The daemon runs
`resolve` when a row is rested on, on its own machine, so `io.open` reads that
machine's files and `preview.run { args = ..., cwd = ..., timeout_ms = ... }`
and `preview.directory_items(path)` run its programs and list its folders.
`item` has `text` and `value` (and `path`, for a `FileSystem` row); `params`
is what the picker was opened with. It returns what a window's does: `{ type =
"Text", text | lines, name, extension, syntax, center_line, highlight_line }`,
`{ type = "Ansi", bytes, center_line }`, `{ type = "Image", bytes }`, `{ type
= "Command", args, cwd, output, center_line, success_exit_codes }` for a
program the daemon then runs, or nil for nothing. The window lays it out; a
`theme` is read and not used, the colours being the window's. A picker with a
preview opens showing it unless `preview_visible = false`.

A `resolve` still running when another row is rested on, or when the picker
closes, is stopped, and so is the program it waits for. One row may take 12
seconds and show 10 MiB. `nterm.json_parse` and `nterm.json_encode_pretty`
are there as in `nterm.lua`.

What is done with the row chosen is the file's to say too: `action =
function(app, item, params) ... end`. When a window opened the picker, the
daemon runs `action` on its own machine at Enter, so it may read that
machine's files and run its programs with `preview.run`. What it asks of
`app` is done to the pane the picker is over, in the order asked, once the
function has returned: `app:send_text(text)`, `app:paste(text)`,
`app:replace_prompt(text, before, after)`, `app:show_toast(title, message,
duration_ms)` or `app:show_toast { header, message, duration_ms }`, and
`app:push_picker(name, params)` (or `app:replace_picker`), which opens another
of the daemon's pickers in place of this one. The picker is over when a row
is chosen, so `app:close_picker()` changes nothing. Anything else a window's
`app` has is refused, saying so. An `action` that fails does nothing to the
pane, and the window says why. A window entry `type = "Daemon"` whose picker
has an `action` in the daemon's file needs none of its own: the window's is
not run.

A program in a shell that opens such a picker is given the row, as of any
picker, and the `action` is not run: what is done with it is the program's.

A picker may have keys of its own, written as in `nterm.lua`: `keys = { { key
= "d", mods = "CTRL", description = "...", action = function(app, item,
params) ... end } }`. The window is told the keys when the picker opens and
tells the daemon when one is pressed on a row; the daemon runs that key's
`action` on its own machine, and what it asks of `app` is done to the pane as
above. A key leaves the picker open, so it can be pressed on row after row.
The picker closes only when the function calls `app:close_picker()` or opens
another picker. A key's function that fails shows why, and the picker stays.
The rows are not read again after a key. Keys work however the picker was
opened, by a window or by a program in a shell.

A picker may be a function, as in `nterm.lua`: `Name = function(context)
return { type = ..., ... } end`. It is called each time the picker is opened,
and what it keeps is what its `preview`, `action` and `keys` see. `context`
has `line`, `line_before_cursor`, `cursor_column` and `recent_lines` (the
last ten lines printed that are not blank), which the daemon reads from the
pane's shell itself, so they are there however the picker was opened; `cwd`;
and `copy_line` and `copy_line_before_cursor`, which only a window knows and
sends when the picker was opened from its copy mode. The daemon reads a row
of the screen as a line: one that wrapped is given as the row the cursor is
on.

A `Command` picker may have `columns = { "Hash", "Date", "Subject" }`, as in
`nterm.lua`: each line the program prints is split into them at a tab (or at
`delimiter`), and the rows show lined up under those names. What is typed is
read as it is for a window's own picker of columns. The row itself is still
the line as it was printed, which is what `action`, `preview` and `keys` are
given. `columns` do not go with `line_continuation` or `unique`.

Modules load as in `nterm.lua`: `require("nterm.git")` gives the pickers of
git that are installed with nterm (`share\nterm\lua`, beside the daemon), so
`GitCommits = git.commits {}` in the daemon's file is that picker run where
the shell is.

Whether a picker of the file opens showing its preview is the file's to say
(`preview_visible`), whatever opened it.

Not yet: previews of types
other than `Dynamic` and `items` that is a
function are refused. A picker that cannot be one is refused
when it is opened, saying why, and the file's other pickers still open. The
names `files`, `directories`, `history`, `ripgrep` and `items` are the
daemon's own and cannot be given to a picker of the file.

## npick

With `picker_pipe` enabled as above, `npick` opens pickers from the shell
running in a pane and prints the selected value. On Windows the pane's shell
must be one the session daemon runs:

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
stream options, protocol limits and cancellation.
For a portable PowerShell profile with file, directory, history, text-search,
and completion bindings, see [profile.example.ps1](../profile.example.ps1).
Executable sources and command previews must be defined in allowlisted Lua
pickers; clients cannot submit executable definitions.

## Open a pane beside this one

With `allow_panes = true` in `picker_pipe`, a program in a pane can ask for
another pane beside its own:

```sh
npick --pane less -R notes.txt        # prints the new pane's id
npick --pane --focus --cwd /src htop  # takes focus; starts in /src
npick --pane                          # the configured shell
```

The pane is added to the group the asking pane is in: its workspace's tiled
panes, or its float, whether or not that is what the window is showing. It
starts in the caller's directory unless `--cwd` says, and keeps the focus
where it was unless `--focus` is given. Everything after the command is the
command's arguments; one with a space in it is passed on in quotes, and a
single argument is taken as a whole command line, quotes and all. A pane in
the session daemon closes when its command ends, so something that only
prints needs to wait: a pager, or a shell that stays.

`npick` answers once the command has started. One that cannot be started is
an error (exit status 2), and its pane is closed.

Unlike the picker requests, this lets a client choose what runs. It is a
permission of its own and is off unless set; the checks on who may connect
are the same.

The request is `{"kind":"pane","command":"less -R notes.txt","cwd":"/src","focus":false}`,
each field optional, and the result is `accepted` with the pane's id as its
value, or an error with code `capability_denied` or `pane_failed`. The pane
that asked does not need focus, and an open overlay does not make it `busy`.

## Picker pipe, protocol version 1

The picker's messages are the same on both systems; how they travel differs.
On Windows a client connects to the session daemon's pipe and the daemon
passes its messages to the window showing the pane; see
[Windows transport](#windows-transport). On Linux each pane has a Unix-domain
socket of its own; see [Linux transport](#linux-transport).

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
`NTERM_DAEMON_PIPE` is set, and loads
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

Add this to the table returned by your nterm Lua configuration:

```lua
picker_pipe = {
  enabled = true,
  allow_stream = true,
  named = { "files", "builtin.lines", "builtin.words" }, -- exact picker names
},
```

`allow_panes = true` also permits [panes](#open-a-pane-beside-this-one).
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

The helper is PowerShell alone: nothing is compiled and no program is started.
Loading it defines the function and connects to nothing, inside nterm or
outside it. Each call opens a connection, makes its one request, and closes
it. A submitted request is never automatically replayed.

`-BackgroundInitialize`, `-FreshConnection` and `Disconnect-NtermPicker` are
still accepted, for profiles written for earlier versions, and do nothing.

`-CancelAfterSeconds` is looked at between items and while the helper waits. A
command that feeds the pipeline and stalls delays it.

When the pipe is enabled, the status bar shows a two-arrow pipe icon: in the
accent color while a request is open, and muted otherwise. Hovering it shows
the state.

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

### Windows transport

The session daemon runs the pane's shell and sets in its environment:

```
NTERM_DAEMON=<the daemon's name>
NTERM_PANE=<the daemon's number for the pane>
NTERM_DAEMON_PIPE=\\.\pipe\nterm-session-<account>-<name>
```

These are not persisted or set globally. Shell descendants inherit them. None
is a secret: the pipe decides who may connect, not knowledge of its name.

A client opens the daemon's pipe, the one the windows themselves use, and
speaks the daemon's framing: a `u32 little-endian length`, then a kind byte,
then for every kind but a control message a `u32` number, then the payload.

1. The client sends a control message (kind 0) whose payload is
   `{"type":"shell","pane":<NTERM_PANE>}`.
2. The daemon finds the window showing that pane. With no such pane, or no
   window showing it, it answers with the control message
   `{"type":"error","request":null,"message":"..."}` and closes.
3. From then on each picker message below travels as the payload of a frame of
   kind 5, numbered 0, in both directions. The daemon passes these on unread.
   The client sends `open` straight after step 1, without waiting.
4. When the window has finished with the request the daemon sends the control
   message `{"type":"shell_closed","channel":0}` and closes the connection. A
   client that has its `result` need not wait for it. Without a `result` before
   it, it means the window left the pane or went away.

One connection is one request. There is no handshake, token or proof, and no
`received` or `idle` (see [Streaming and results](#streaming-and-results)):
closing the connection ends the request. A second request for a pane that has
one open returns `busy`.

A shell that the daemon does not run has no way to open a picker on Windows.

### Linux wire format and authentication

Each frame is `u32 little-endian length` followed by that many UTF-8 JSON bytes.
Maximum frame size is 1 MiB. Read/write call boundaries are not frame boundaries.
Unknown fields, duplicate JSON keys (including nested objects), unsupported
versions, invalid IDs, and invalid message ordering are rejected.

The pane's shell launch environment contains:

```
NTERM_PICKER_ENDPOINT=<the socket's absolute path>
NTERM_PICKER_TOKEN=<random 256-bit token encoded as hex>
```

These variables are not persisted or set globally. Shell descendants inherit
them. The token is the UTF-8 hex string used as the HMAC key, not decoded bytes.

Handshake:

1. Server sends `{"type":"challenge","version":1,"nonce":"<64 hex chars>"}`.
2. Client sends `authenticate` with `version`, `client_nonce`, and `proof`.
3. Server verifies the client proof.
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

Only one request is active at a time per connection. On Linux later requests
may reuse an authenticated connection, and current capabilities are checked
again for every open. Request IDs are nonempty strings up to 128 bytes.


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
active overlay returns `busy`. On Linux there is one connection slot per pane;
another client waits at most five seconds to connect and fails if it remains
occupied. There is no request queue or automatic request replay.

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

On Windows the request is over with its result, and the rest of this section
is Linux alone. After any terminal result, the client sends:

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
Delivery is still not guaranteed across crashes, timeouts, or connection loss.

Errors are terminal results with `status:"error"`, `code`, and `message` when a
valid request is available. Authentication/security/framing failures disconnect
without echoing request content. The handshake has a five-second deadline;
UI acknowledgments have ten-second deadlines; partial payloads and
blocked writes have five-second deadlines. Waiting for user selection has no
deadline, and idle authenticated connections have no idle timeout. Cancellation
is read concurrently with response delivery. On Linux a Lua reload that revokes
the active request capability cancels that request, and disabling the pipe or
exit of the pane shell closes idle connections; pane closure stops the service.
On Windows a reload refuses later requests and leaves one that is open; exit of
the pane's shell, and the window leaving the pane, end it.

### Linux transport

Each pane listens on `$XDG_RUNTIME_DIR/nterm/<pid>/pane-<id>.sock`, or a
`nterm-<uid>` directory under the temporary directory when no runtime
directory is set. Both directories must be owned by the user and mode `0700`;
the socket is `0600`. On startup, directories of terminals that are no longer
running are removed; closing a pane removes its socket.

The kernel reports the connecting process's user (`SO_PEERCRED`); connections
from other users are closed before the challenge. Processes of the same user
can read each other's environment (including the token) and memory, so the
token is not a boundary between them.

`npick` is the client, with the same options on Windows:

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

On Windows a picker request is admitted by the session daemon's pipe, as a
window is, and by the Lua policy.

- The pipe's access list grants read and write to the account alone, from
  any of its sign-ins, and its integrity label denies reads and writes from
  lower-integrity clients. Remote clients are refused
  (`PIPE_REJECT_REMOTE_CLIENTS`).
- For every connection the daemon uses `ImpersonateNamedPipeClient` and
  compares what the client is acting as, and what its process is, with the
  daemon's own account and elevation. A client of another user, or at another
  elevation, is closed before anything it sent is read. Identification level
  is enough, and is what the supplied clients ask for.
- The clients check the other end before sending, since a request can hold the
  shell's history. `npick` confirms that the process serving the connection
  has its own security context. `Invoke-NtermPicker` accepts only a local pipe
  named `nterm-session-...` and confirms that the pipe is owned by its user.
- Disabled by default, separately permitted request types, explicit named
  picker allowlist, bounded frames/batches/storage, and return-only acceptance.
  The window showing the pane applies these, not the daemon.
- Clients cannot submit command definitions or command previews. Executable
  behavior is limited to trusted allowlisted Lua picker configuration.

The daemon does not check that a client was started from the pane's shell: any
program of the user may name any pane. Such a program could already connect as
a window, read the pane and type into it, so the check would protect nothing.
For the same reason none of this isolates the user from their own programs.
Executing a script, executable or configuration that an attacker can modify is
unsafe, more so in an elevated terminal. Argument arrays avoid implicit shell
concatenation but do not make dangerous tools or arguments safe.

The daemon's tests cover a request and its answer through a real pipe, the
window leaving the pane, the shell ending, and the refusals. An elevated
daemon with an unelevated client has not been tested, nor has the owner check
in `Invoke-NtermPicker` when elevated. GUI focus and PSReadLine behavior need
interactive testing in the rebuilt terminal.

References: [pipe ACLs](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights),
[pipe creation](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew),
[effective client security context](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-impersonatenamedpipeclient).

Picker requests and search-result updates wake the native window event loop
and request a redraw directly. GPU presentation keeps its existing pacing.

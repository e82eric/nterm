# Configuration

## Where configuration is loaded from

Lua settings are loaded at startup from the first available location:

1. An explicit `--lua-config=PATH` argument.
2. `nterm.lua` in the home directory (`%USERPROFILE%` on Windows, `$HOME` on Linux).
3. `nterm.lua` beside the executable, regardless of the current working directory.

Only one file is loaded. Missing default files preserve the existing configuration.
An explicit missing file, unreadable file, or invalid configuration reports the
filename in Messages and starts with defaults for Lua settings, without falling
back to another file. A persistent configuration indicator remains until restart.
See `nterm.lua` in this repository for a starting configuration:

[nterm.example.lua](../nterm.example.lua) is an extended Windows sample with
custom picker callbacks, command previews, snippets, and Codex and Claude
command suggestion pickers. Its filesystem roots default to `.` and tools are
resolved from `PATH`. Read its header for dependencies and optional
integrations. Snippets use `GHOSTTY_SNIPPETS_FILE` or `~/ghostty-snippets.json`;
initialize the file with `{"snippets": []}`. The suggestion pickers send
terminal context to `codex exec` and `claude --print`. Run it with
`--lua-config`:

```powershell
.\out\bin\nterm.exe --lua-config=nterm.example.lua
```

## Settings

The file returns a table with optional `font.family`, `font.size` (points),
`terminal.scrollback_limit` (bytes), `terminal.command`, `colors.theme`, and
`floating.padding` settings. For example,
`floating = { padding = { top = 15, bottom = 15, left = 10, right = 10 } }`
sets each inset as a percentage of the terminal area. Omitted sides use zero;
the four sides must leave at least 5% width and height. Without this setting,
the pane keeps its original centered 80% × 70% size and 1000 × 700 point caps.
`status_bar = { font_size = 16 }` sets the size of the status bar's text, in
points, from 6 to 48; without it, 13. The bar, its chips and its icons grow
with it. `font.size` is the terminal's text, and does not change the bar.
Unknown fields
are rejected. Rust evaluates Lua once, validates the settings, and translates them
into Ghostty configuration arguments. Ghostty's normal configuration files remain
the base; Lua overrides them, and explicit command-line options override Lua.
The picker still imports Ghostty's resolved font and tracks its cell dimensions.
Use the command palette's Reload configuration action to apply supported Lua settings.

## Remotes

A workspace's shells can run in the nterm daemon of another machine. The
window reaches it by running `ssh`, so the other machine needs an ssh server
and this build of nterm, and nothing else. The shells stay there when the
window closes or the connection is lost, to be shown again.

`remotes` names the machines:

```lua
remotes = {
  vm = {
    address = "me@192.168.56.10",
    command = "pwsh.exe -NoLogo",
    daemon = "C:\\Program Files\\nterm\\nterm-daemon.exe",
    directory = "C:\\src",
    session = "work",
    new_roots = { "C:/src" },
  },
}
```

Each has a row in the workspace picker, and nothing is connected to until its
row is chosen. Choosing `vm` connects and shows its page: the sessions
waiting there, each called by its name and `@vm`, and the folders there a
workspace can be started in. If it cannot be reached, the page says why, and
the row on the first page goes on saying it. Escape goes back. A workspace is
of one place: its splits and new panes are made where it is, and
a pane cannot move to a workspace of another place. The status line says
which machine the current workspace is on, first in its label, as in
`claudecode@HOME · PWSH.EXE`: the user its daemon runs as and that
machine's own name, which the daemon says when the window connects. Those
are only shown; nothing is checked with them. Until it has said, and for a
daemon too old to say, the label starts with the remote's name. The picker's rows
say which machine of each workspace. When a connection is lost, that machine's workspaces close, the others
stay, and its row offers to connect again: its shells are still there.

`nterm --remote NAME` opens a window that starts on that machine, and
`--session NAME` then picks which of its daemons, as it does here.

- `address` is what `ssh` is given: `user@host`, or a `Host` of your ssh
  configuration. Without it, the remote's name is used.
- `command` is what a new shell there is started with. Without it, that
  machine's own: `cmd.exe` on Windows, the account's login shell on Linux.
  `terminal.command` is this machine's and is not used.
- `daemon` is where `nterm-daemon` is there. Without it, it is found by name,
  so its folder must be on that account's `PATH`: on Linux the `PATH` of an
  ssh that runs a command, which may not have `~/.local/bin`
  (`deploy-daemon.sh` installs there), so name it.
- `directory` is where a new shell there starts, in which `~` is the home
  folder there. Without it, the home directory.
- `session` is which daemon there, for a machine that runs several. Without
  it, the one called `default`.
- `new_roots` are folders there: each folder inside one is listed on the
  remote's page, found by its daemon, and starts a workspace there in it.
  Without it, `{ "~" }`: the home folder there, which its daemon reads `~` as.
  `new_depth` is how many levels inside them; without it,
  `workspaces.new_depth`.
- `clipboard_read` is `"allow"` or `"deny"`: whether a program in a shell
  there may read this machine's clipboard, as an editor's paste does (see
  [Pasting the clipboard](#pasting-the-clipboard)). Without it, `"deny"`.

A name that `remotes` does not have is taken as an address:
`nterm --remote me@host`.

The window's `ssh` asks nothing, as there is nowhere to answer. So the machine
must take a key or an agent, not a password, and its host key must already be
known: connect once by hand (`ssh me@host`) and accept it. A machine whose key
has changed is refused. Nothing is forwarded.

A remote window shows and types, and that is all the other machine has of
this one, and its clipboard if its `clipboard_read` is `"allow"`. A new
shell is given the daemon's environment and the home
directory there, not this machine's. Pickers that a program in the shell asks
for (`npick`) are refused, as they would run here; your own key bindings'
pickers still open, and are about this machine. The workspace picker lists
no folders of `workspaces.new_roots`, which are this machine's: it lists
those in the remote's home folder, and a remote's page its own `new_roots`.
A folder chosen there is the one folder the window names for a new shell on
that machine: its daemon listed it. A new pane beside one there is started
by that pane's number, and the daemon there starts it in the folder that
pane's shell reported.

When a connection is lost, each end says what it saw, in
`%LOCALAPPDATA%\nterm` on its machine. Each line starts with the time (Unix
seconds) and the process:

- `window-SESSION.log`, beside the window: each connection to a daemon made,
  refused, lost or closed, and how long it lasted. A lost one says when the
  daemon was last heard from and what `ssh` said as it ended, such as
  `client_loop: send disconnect: Connection reset`. The window's message
  shows the last lines `ssh` said.
- `relay-SESSION.log`, on the remote: each relay `ssh` started, with both
  ends of the SSH connection, which side ended it, how long it ran and how
  much went each way. A relay with no line for its end was stopped or
  crashed.
- `daemon-SESSION.log`, beside each daemon: each client, and why it went.

Each of these is kept to a size: the window's and the relay's to 1 MB, the
daemon's to 100 MB, with the file before it kept as `NAME.old.log`. They say
where and when, never what is typed or shown.

## Prompt patterns

`prompt_patterns` configures prompt detection for copy-mode command navigation,
the `yic`/`yac` text objects, and the built-in `prompts` picker.
Prompts a shell marks with OSC 133 (shell integration) are found from the marks
whether or not a pattern matches; the patterns find prompts in programs that print
no marks, such as debuggers, nested shells, and AI CLIs. The list replaces the default patterns; an empty list disables
detection. Patterns match the beginning of a logical line and include the prompt
text to exclude when splitting. They use Rust regex syntax and are compiled when
the config loads; invalid expressions report the pattern index and reject the config.
Successful configuration reloads also update the patterns.

```lua
return {
  prompt_patterns = {
    [[^PS [^>]*>\s*]],
    [[^\d+:\d+>\s*]],
    [[^›\s*]],
    [[^────────────────────*]],
  },
}
```

## Quick selects

`quick_selects` defines named quick selects over your own regexes, each bound to
a key and listed in the command palette, and `quick_select_defaults` sets the
hint alphabet and the patterns Alt+Space hints. See
[Custom quick selects](copy-mode.md#custom-quick-selects).

## Colors

The UI follows Ghostty's resolved theme colors. Optional `colors.ui` overrides
take precedence for app chrome without changing terminal colors:

```lua
colors = {
  theme = "Gruvbox Dark",
  ui = { accent = "#fabd2f", panel_background = "#1d2021" },
},
theme_colors = {
  ["Gruvbox Dark"] = { accent = "#fabd2f" },
  ["Rose Pine"] = { accent = "#ebbcba" },
},
```

Overrides accept `#RRGGBB` or `RRGGBB`. See [UI colors](ui-colors.md) for all
eighteen roles and derivation rules. Invalid overrides use configuration Messages.
Colors are derived from the resolved Ghostty theme, then global `colors.ui`
overrides are applied, followed by `theme_colors` overrides for that exact,
case-sensitive theme name. This applies at startup and when selecting a theme;
switching to another theme removes the previous theme's overrides. These
overrides affect app chrome and UI decorations, not terminal ANSI colors.

Set `colors.ui.picker_match_highlight` to color the matched characters in picker
results independently of the selected terminal search match (`match_highlight`).
It accepts `#RRGGBB`, supports `theme_colors` and manual reload, and falls back to
`match_highlight` when omitted.

Set `colors.ui.copy_cursor_background` to color the copy-mode cursor in the
terminal and NFM preview independently of `accent` and the active search match.
It also supports `theme_colors` overrides and falls back to `accent` when omitted.

## Themes

Use **Choose theme** in the command palette to search installed user and bundled
Ghostty themes in the shared fuzzy picker. Arrow keys select an entry; Enter or clicking a theme applies it. Escape
closes the picker without changing the theme. The choice updates the main and
floating terminal panes and the shared UI palette for this session only. Explicit
Lua UI overrides continue to take precedence; picker choices replace explicit
native terminal colors for the session. To save a choice, set `colors.theme` in
Lua and remove conflicting explicit native color settings, then restart. You can also bind the
`OpenThemePicker` action, or `builtin_picker = "themes"`, in `keys`.

Both open the built-in `themes` picker, which defaults to Cursor placement with
score sorting. Change its placement through `builtin_pickers`:

```lua
builtin_pickers = {
  themes = { position = "Center", sort = "Score" },
},
keys = {
  { key = "t", mods = "CTRL|SHIFT", builtin_picker = "themes" },
},
```

Add these entries to the tables in your config. The overrides also accept
`initial_query`, and the picker works in the main and floating panes.

## Key bindings

Keys accept egui names such as `f`, `Space`, and `ArrowUp`; modifiers are joined
with `|` (`CTRL`, `SHIFT`, `ALT`). Definitions and regexes are compiled
at startup; unknown action or picker names, invalid patterns, ambiguous targets,
and duplicate bindings report configuration errors. Picker bindings trigger from
terminal or copy mode, leaving modal query input unaffected.
Without `pickers`, all four named built-ins remain available.
The default bindings include the built-in actions, Ctrl+Shift+F for built-in
`lines`, and Ctrl+Shift+P for the built-in command palette. The built-in
`words` picker has no default key. There are no default custom pickers;
`picker = "lines"` requires a `lines` entry in `pickers`.

`keys` is merged over the defaults by chord. An entry on a chord that has a
default binding replaces it, entries on other chords are added, and every
other default remains. `action = "None"` removes the default on its chord, so
the key reaches the terminal application again:

```lua
keys = {
  { key = "m", mods = "CTRL|SHIFT", action = "NewWindow" }, -- replaces EnterCopyMode
  { key = "Space", mods = "ALT", action = "None" },         -- removes quick select
},
```

Set `disable_default_keys = true` to use only your `keys`, with no defaults;
with an empty `keys = {}` it disables every top-level shortcut.

The same `keys` table accepts top-level application actions. Each binding must
specify exactly one of `action`, `picker`, `builtin_picker`, or `quick_select`:

```lua
keys = {
  { key = "e", mods = "CTRL|SHIFT", action = "ToggleFloatingPane" },
  { key = "w", mods = "CTRL|SHIFT", action = "ClosePane" },
  { key = "f", mods = "CTRL|SHIFT", action = "OpenSearch" },
  { key = "Space", mods = "CTRL|SHIFT", builtin_picker = "command_palette" },
},
```

Supported action names are `NewWindow`, `ReloadConfig`, `ShowMessages`,
`OpenErrorLog`, `OpenThemePicker`, `ToggleFloatingPane`, `CreateFloatingPane`, `OpenSearch`,
`EnterCopyMode`, `ExitCopyMode`, `OpenQuickSelect`, `PreviousCommand`,
`NextCommand`, `ScrollLineUp`, and `ScrollLineDown`, plus the workspace and
layout actions listed in [Workspaces](workspaces.md#default-bindings) and the names of your Lua
`actions`. Use `picker = "name"` to open a named picker, and
`quick_select = "name"` to open a [custom quick select](copy-mode.md#custom-quick-selects). Multiple different
shortcuts may invoke the same action; the command palette displays their actual
configured bindings. Actions without bindings remain available in the palette.

A default action bound to a new chord keeps its default chord as well; remove
that with `action = "None"`. The sample configs list all of their bindings.
Unknown actions, missing or ambiguous targets, and duplicate shortcuts fail
configuration validation.

Picker, preview, search, quick-select, and copy-mode controls retain their local
bindings. Top-level bindings do not intercept modal query input. Window and
floating-pane actions remain available in ordinary overlays; the command palette
keeps its input modal. Search and quick select can still be focused or toggled
using their configured bindings while active. In query and hint overlays, bindings
without Ctrl or Alt are left to local text input. `ExitCopyMode` is available only
while copying; Escape retains its existing copy-mode behavior, including clearing
pending motions, visual selection, and search before exiting.


## Actions and toasts

Define named actions alongside `pickers` and reference them in `keys`. Actions
also appear in the command palette with their configured label and shortcuts:

```lua
actions = {
  SaveSnippet = {
    label = "Save selection as snippet",
    action = function(app)
      local selection = app:get_selection_text()
      app:show_toast("Selected text", selection, 2000)
    end,
  },
},
keys = {
  { key = "s", mods = "CTRL|SHIFT", action = "SaveSnippet" },
},
```

An action may also be defined directly as a function; its name becomes its label.
Toasts also accept an options table with an optional header:

```lua
app:show_toast({ message = "Configuration reloaded", duration_ms = 2000 })
app:show_toast({ header = "Reload failed", message = "Invalid font size", duration_ms = 5000 })
```

Omitting `header` creates a compact message-only toast. Headers use the theme's
accent color above the body; built-in errors use the error color. Both use the
terminal font at 14 px, with bold headers, additional line spacing, and a maximum
text width of 420 px. Long notifications are truncated to fit the pane, up to
seven lines; full built-in error details remain in error history. Repeating the
same notification refreshes it instead of adding another card. Escape dismisses the
visible toasts, unless a picker or search is open. The positional form
`app:show_toast(title, message, duration_ms)` also works, with its title displayed as the header; an empty title omits it.

`get_selection_text()` returns the active pane's live selection or copy-mode
visual selection, and returns an empty string if none exists. Toast duration is in milliseconds
(default 2000, allowed range 1–60000). Toasts use the picker's `Cursor` placement
within the originating pane: centered below the cursor when space permits, above
it otherwise, with the same bounds checks and bottom fallback when the cursor is unavailable,
without taking keyboard focus. Both methods are also
available in picker action callbacks. App handles expire when the callback returns.

Both samples bind **Ctrl+Shift+S** to a `SaveSnippet` action that appends the
selection to a JSON snippets file: `~/.wtd/.wt.json` in [nterm.lua](../nterm.lua),
and `GHOSTTY_SNIPPETS_FILE` or `~/ghostty-snippets.json` in
[nterm.example.lua](../nterm.example.lua), whose Snippets picker (Ctrl+Shift+B)
reads it back. The file must already exist; start it as `{"snippets": []}`. The
action keeps other JSON fields, leaves the text selected, and shows success or
errors as toasts. It encodes with `nterm.json_encode_pretty(value)` and writes
with Lua's `io.open(path, "wb")`.

## Pasting a picture into a shell on another machine

A picture copied on this machine can be pasted into a shell that runs on
another (a `--remote` window): it is sent there as a file, and the file's path
is typed at the prompt. Claude Code there takes the path as an attached image.
It needs the session daemon, and works the same in a window on this machine.
The bundled module gives the action:

```lua
local clipboard = require("nterm.clipboard")
return {
  actions = {
    PasteImage = { label = "Paste picture from clipboard", action = clipboard.paste_image() },
  },
}
```

It is in the command palette, and can be given a key. `clipboard.paste_image
{ format = function(path) ... end }` changes what is typed for the path.

The action is made of two methods of `app`, which a configuration can put
together in other ways:

```lua
-- What the clipboard holds of the kinds asked for.
app:clipboard({ "text", "image", "files" }, function(app, clip, why) end)

-- Bytes kept as a file on the machine the pane's shell is on.
app:send_file(bytes, { extension = "png" }, function(app, path, why) end)
```

- Both return at once, and call the function given on a later frame, when the
  answer is there, with a new `app` for the same pane; not at all if the pane
  has closed by then. That `app` can do what an action's can, but for the
  workspace and floating pane methods.
- `clip.text` is the text, `clip.image` a PNG file's bytes, and `clip.files`
  the full paths of files copied in Explorer. Each is there only when it was
  asked for and the clipboard holds it; an empty `clip` is not a failure.
- `path` is where the file was kept, as that machine writes paths:
  `%LOCALAPPDATA%\nterm\pasted\` there, under a name the daemon chooses. Files
  it put there are removed a week later, when another is kept; nothing else in
  that folder is touched. A file is up to 32 MiB, and `extension` up to 10
  letters and digits (`bin` without one).
- On failure the function is given `nil` and why: the clipboard could not be
  opened, the pane's shell is not the session daemon's, the daemon there is
  older than this window, the file is too large, or no answer came within 30
  seconds.

A file is sent in one piece, so on a slow connection what is typed after it
waits until it has gone.

## Pasting the clipboard

A program in a pane's shell can be given the text on the clipboard of the
machine the window is on, even when the shell runs on another machine (a
`--remote` window). This is how an editor there pastes what you copied here:

```sh
nterm-daemon clipboard paste        # the text, as it is
nterm-daemon clipboard paste --lf   # with Windows' line ends made \n
```

It asks the window showing the pane, through the session daemon that runs
the shell, so it works only in a shell the daemon runs: not in one reached
by typing `ssh` in a pane. The shell is told the daemon's program as
`NTERM_DAEMON_PROGRAM`, for when `nterm-daemon` is not on its `PATH`. It
prints the text and exits 0, or says why not and exits 1: reading is not
allowed, no window shows the pane, the clipboard could not be opened, or its
text is over a megabyte. A clipboard holding no text gives nothing.

Whether a program may is the window's configuration's, by where the shell
runs. Reloading the configuration applies it to the next request:

```lua
return {
  terminal = { clipboard_read = "allow" },              -- this machine's shells
  remotes = { vm = { clipboard_read = "allow" } },      -- the shells on vm
}
```

`terminal.clipboard_read` is `"allow"` without it, and each remote's
`"deny"`: any program running as you there could read the clipboard, without
your asking. Only a program running on the shell's machine as the daemon's
user can ask; text a program prints cannot.

For Neovim, where the shell runs:

```lua
local nterm = vim.env.NTERM_DAEMON_PROGRAM
if nterm then
  local function paste()
    local done = vim.system({ nterm, "clipboard", "paste", "--lf" }, { text = true }):wait()
    if done.code ~= 0 then
      vim.notify(vim.trim(done.stderr), vim.log.levels.WARN)
      return {}
    end
    return vim.split(done.stdout, "\n")
  end
  vim.g.clipboard = {
    name = "nterm",
    copy = { ["+"] = require("vim.ui.clipboard.osc52").copy("+"), ["*"] = require("vim.ui.clipboard.osc52").copy("+") },
    paste = { ["+"] = paste, ["*"] = paste },
  }
end
```

Copying still goes by OSC 52, which the window takes from any program.

## Messages and error history

Recoverable errors and fallback warnings are retained in a session history
(200 records, 16 KiB details per record). Repeated failures aggregate. They also
write to stderr and retain their existing toast or local picker/preview feedback.
Choose **Show messages** or **Error history** in the command palette, or click
the bar's configuration issue count, to open the messages picker. It lists
persistent configuration issues, then the history, newest first; the preview
shows the selected message in full, and Enter closes the picker. To copy a
message, focus the preview and press Ctrl+Shift+C. Reopen the picker to refresh its snapshot.

You can bind `action = "OpenErrorLog"` or configure a named picker:

```lua
pickers = {
  errors = { type = "ErrorLog", position = "Cursor" },
},
keys = {
  { key = "h", mods = "CTRL|SHIFT", picker = "errors" },
},
```

Merge these entries into your existing tables. History resets on restart.


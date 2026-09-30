# Rust egui terminal shell

This example embeds one full Ghostty terminal in an egui shell. Ghostty keeps
its OpenGL renderer and exports completed textures to egui without a CPU pixel
copy. The native C Win32 sample remains the performance baseline.

From the repository root:

```powershell
zig build egui-sample -Doptimize=ReleaseFast
.\zig-out\bin\ghostty_egui.exe --command=pwsh.exe
```

To build and deploy the Windows sample to another directory:

```powershell
.\example\rust-egui-terminal\deploy.ps1 C:\Apps\GhosttyEgui
```

The script copies `ghostty_egui.exe` and its required `ghostty.dll`. Use
`-SkipBuild` to copy an existing build. It leaves any `ghostty.lua` in the
destination untouched; the app also checks the home directory for that file.

On Linux, select the embedded runtime build explicitly:

```sh
zig build egui-sample -Dapp-runtime=none -Doptimize=ReleaseFast
./zig-out/bin/ghostty_egui --command="$SHELL"
```

The host uses WGL context sharing on Windows and explicitly selects EGL on
Linux, including X11 and Wayland. `TerminalView` owns the Ghostty surface,
input bridge separately from `ShellApp`. Its painter owns the current exported
frame and frames awaiting GPU completion. That
boundary leaves room for fixed-position overlay terminals without moving
terminal drawing into egui.

Releasing a mouse selection leaves it highlighted. Right-click copies the
selection to the system clipboard and clears the highlight; Escape clears it
without copying. The sample uses Ghostty's `right-click-action=copy`,
`copy-on-select=none`, and `selection-clear-on-typing=false` settings, and
forwards Ghostty's clipboard writes through egui. Explicit Ghostty command-line
options can override these defaults.

Lua settings are loaded at startup from the first available location:

1. An explicit `--lua-config=PATH` argument.
2. `ghostty.lua` in the home directory (`%USERPROFILE%` on Windows, `$HOME` on Linux).
3. `ghostty.lua` beside the executable, regardless of the current working directory.

Only one file is loaded. Missing default files preserve the existing configuration.
An explicit missing file, unreadable file, or invalid configuration reports the
filename and exits before opening a window, without falling back to another file.
See `ghostty.lua` in this example for a starting configuration:

```powershell
.\zig-out\bin\ghostty_egui.exe --lua-config=example/rust-egui-terminal/ghostty.lua
```

The file returns a table with optional `font.family`, `font.size` (points),
`terminal.scrollback_limit` (bytes), `terminal.command`, `colors.theme`, and
`floating.padding` settings. For example,
`floating = { padding = { top = 15, bottom = 15, left = 10, right = 10 } }`
sets each inset as a percentage of the terminal area. Omitted sides use zero;
the four sides must leave at least 5% width and height. Without this setting,
the pane keeps its original centered 80% × 70% size and 1000 × 700 point caps.
Unknown fields
are rejected. Rust evaluates Lua once, validates the settings, and translates them
into Ghostty configuration arguments. Ghostty's normal configuration files remain
the base; Lua overrides them, and explicit command-line options override Lua.
The picker still imports Ghostty's resolved font and tracks its cell dimensions.
Changes to Lua require restarting the app; live reload is not supported.

Named pickers and their shortcuts can also be defined in Lua:

```lua
pickers = {
  lines = { type = "Scrollback", position = "Bottom", enter_action = "Navigate" },
  words = { type = "Scrollback", position = "Cursor", regex = [[\S+]], enter_action = "SendInput" },
  files = { type = "FileSystem", position = "Center", files_only = true, max_depth = 0, enter_action = "SendInput" },
},
keys = {
  { key = "f", mods = "CTRL|SHIFT", picker = "lines" },
  { key = "p", mods = "CTRL|SHIFT", picker = "words" },
  { key = "o", mods = "CTRL|SHIFT", picker = "files" },
},
```

These fields belong inside the returned configuration table.
Picker names can also map to functions. The function runs when its keybinding or
named OSC request opens the picker, and returns the same table as a static picker:

```lua
pickers = {
  context = function(cursor)
    local word = cursor.line_before_cursor:match("([^%s]+)$") or ""
    return { type = "Scrollback", regex = [[\S+]],
             initial_query = word, enter_action = "Navigate" }
  end,
},
```

`cursor.line` is the visible physical terminal row, `line_before_cursor` is its
text before the cursor, and `cursor_column` is the zero-based terminal cell column.
This is terminal output, not PSReadLine's private edit buffer. `initial_query`
works for both static and function pickers. The function may choose `type`,
`position`, `enter_action`, `action`, and other type-specific fields on every invocation.
Function results use the same validation as static definitions; errors are
printed and the picker stays closed.

Every picker type accepts `position = "Top"`, `"Center"`, `"Bottom"`, or
`"Cursor"`; the default is `"Bottom"`. Cursor placement puts the query near
the live terminal cursor, or the copy cursor when opened from copy mode, and
shows the panel below it when there is room, otherwise above it. The panel
stays inside the terminal or floating pane. If the cursor is outside the
visible viewport, placement falls back to Bottom.

Set the top-level `picker_defaults = { unique = true }` to enable deduplication
by default for Scrollback and Command pickers, including function-defined
pickers. Individual definitions can override it with `unique = false` or
`unique = true`. The global default is `false`; Items and FileSystem pickers
are unaffected.

`Scrollback` regexes,
`FileSystem` options, and `Command` arguments have separate schemas: fields from another picker type
are rejected. All types support an `action` callback; `Scrollback` also supports
both `enter_action` values. Omitting `type`
defaults to `Scrollback`. Scrollback regexes
extract individual candidates from logical
lines (joining soft wraps, never separate hard lines). The default extracts whole
nonblank lines without surrounding whitespace. Extraction uses Rust's `regex`
syntax; lookaround and backreferences are unsupported. Empty matches are skipped.
Each candidate retains its exact snapshot cell range, including Unicode text.
Set `unique = true` on a Scrollback picker to keep only the bottom-most
occurrence of each extracted value. Matching is exact and case-sensitive;
the default is `false`, which retains duplicates.
`Navigate` enters copy mode at its first cell. `SendInput` returns to the live
terminal and pastes the matched text without pressing Enter, including when the
picker was opened from copy mode.

`FileSystem` supports `roots` (a list of directory paths), `max_depth`, `files_only`,
and `directories_only`. Empty or omitted roots use the application's working
directory, which may differ from the shell's current directory. Relative roots
are resolved against the application working directory. `max_depth = 1` scans
immediate children; zero or omission means unlimited depth. File and directory
filters cannot both be true. Filesystem pickers only support `SendInput`, pasting
the raw path without shell quoting or Enter. Regex extraction is scrollback-only.
An optional preview can show the selected path's contents.

`Items` takes its candidates from an OSC request's `params.items`, a JSON-encoded
array of `{ "text": "display label", "value": "text to insert" }`, or from an
`items = function() ... end` Lua provider. The provider runs each time the picker
opens, so it can read a file that changes during the session. Lua has
the standard `io` and `os` libraries and a `ghostty.json_parse(string)` helper.
It has no filesystem scan or terminal snapshot. Without an action callback,
Enter pastes the chosen value. Item lists are limited to 2,000 entries.

`Command` runs the configured `args` array without a shell and streams stdout
lines into the fuzzy picker. An argument consisting of `{pattern}` is replaced
with the OSC request's `params.pattern`; other `{name}` arguments can use other
request parameters. The request's `params.cwd` overrides optional configured
`cwd`. The command stops when the picker closes. Without an `action` callback,
Enter pastes the selected line. The sample `Ripgrep` picker shows how to use
`rg --vimgrep` and open a result in Neovim.
For commands whose output uses continued lines, set `line_continuation` to the
line-ending marker (for example, `` "`" `` for PowerShell history). Continued
lines appear as one result joined with spaces; accepting it removes the markers
and restores newlines. An unfinished group is kept when the command exits.
Set `unique = true` on a Command picker to retain only the first occurrence
of each exact accepted value in the process output. Deduplication happens after
continuation lines are joined. The default is `false`.

Every picker type accepts an optional preview. `preview = { type = "Text",
text = "{item}" }` displays literal text; `type = "Ansi"` interprets terminal
escape sequences; `preview = { type = "Command", args = { "bat", "--color=always",
"{item}" } }` runs a process and renders its output. `{item}` in preview text,
arguments, or optional `cwd` is replaced with the selected item's accepted value.
Commands run without a shell and are cancelled when selection changes or the
picker closes. Preview output is limited to 64 KiB and displayed in 12 terminal
rows. Ghostty's VT parser handles ANSI sequences in an isolated terminal; egui
draws its text, colors, and basic styles using the picker's font.
Instead of fixed `args`, a Command preview can use `command = function(item, params)`
to return `{ args = {...}, cwd = "..." }` for each selected item. Return `nil`
to leave that item's preview blank. `item.value` is the selected value;
filesystem items also provide `item.path` and `item.is_directory`. The callback
runs when the selected item changes.

The returned command table may also include `center_line`, a zero-based line
number in the command output. The initial preview centers that line, clamping
at the beginning and end of the output. Ctrl+PageUp/PageDown then scrolls from
that position. If `bat` starts its output at file line `start`, use
`center_line = matched_line - start`.

For example:

```lua
preview = {
  type = "Command",
  command = function(item)
    if item.is_directory then
      return { args = { "pwsh", "-NoProfile", "-CommandWithArgs",
        "Get-ChildItem -LiteralPath $args[0] | Format-Table -AutoSize", item.path } }
    end
    return { args = { "bat", "--color=always", "--paging=never", item.path } }
  end,
}
```

For a scrollback picker, `preview = { type = "Scrollback", rows = 12 }` instead
shows the frozen terminal rows around the selected match, retaining their
colors and text attributes and highlighting the match. The row count can be
1–12. Only the visible preview rows are read from the snapshot.
Set `preview_visible = false` on any picker definition to start with its
preview hidden. The default is `true`. While the picker is open, Ctrl+P
toggles the preview, and Ctrl+PageUp or Ctrl+PageDown scrolls it by half a page.
Unmodified arrow and page keys continue to navigate the results.

A picker may override `enter_action` with a Lua callback:

```lua
files = {
  type = "FileSystem",
  roots = { [[G:\src]] },
  files_only = true,
  action = function(app, item)
    app:paste(item.path)
  end,
},
```

`item.text` contains the candidate text. Filesystem items also have `item.path`.
`item.value` contains the text to insert (the same as `item.text` for scrollback
and filesystem items). An action callback receives `(app, item, params)`, where
`params` contains the OSC request's string parameters, or an empty table for a
keybinding.
Scrollback items have `item.range.start` and `.end`, each with zero-based `x` and
`y` cells, and `item.snapshot_generation`.

`app:paste(text)` pastes without Enter. `app:send_text(text)` sends typed input,
with line breaks translated to Enter keys. `app:navigate(item)` enters copy mode
at a scrollback item's first cell. Paths are not automatically shell-quoted.
`app:replace_prompt(text, before, after)` sends `after` Delete and `before`
Backspace key events, then pastes `text`; use the command-line cursor and length
from the shell's OSC request to replace text on both sides of the cursor.
For PowerShell completions, the sample Lua callback uses `app:send_text` to send
Backspace bytes and Delete escape sequences, then `app:paste(item.value)` to
insert the completion without submitting the command.

Callbacks run on the UI thread after the picker stops, so keep them short.
They retain Lua closure state between invocations. Actions are queued and applied
in order only after a successful return; errors discard them and are printed to
stderr. App handles expire when the callback returns. Navigation requires an
active snapshot; perform it before any action that returns to the live terminal.

Windows uses `nfm_file_system` from `G:/src/RustWin32Host/nfm-file-system`; adjust
that Cargo path on other machines. Filesystem pickers are currently Windows-only;
Linux support will use NFM when it becomes available. Filesystem scans do not
capture or read terminal snapshots, and cancellation leaves existing copy mode intact.

Shell commands can open a named picker using the local WezTerm OSC protocol:

```text
ESC ] 9001 ; StreamingPicker ; TOKEN ; {"name":"files"} ST
```

Each terminal receives a random `WEZTERM_OSC_TOKEN` environment variable.
For example, define this function in PowerShell inside the terminal:

```powershell
function Show-TerminalPicker([string] $Name = 'files') {
    $json = @{ name = $Name } | ConvertTo-Json -Compress
    [Console]::Write("$([char]27)]9001;StreamingPicker;$env:WEZTERM_OSC_TOKEN;$json$([char]27)\")
}
Show-TerminalPicker files
```

Names refer to the Lua `pickers` table and do not require a keybinding. Only the
`action` remains an alias for `name`. Requests can include a `params` object of
string values. For example, `PowerShellCompletion` in the sample `ghostty.lua`
uses `params.items` (JSON encoded), `params.replaceBefore`, and
`params.replaceAfter` to display completions and replace text around the cursor.
Filesystem pickers can use `params.roots`, a JSON-encoded array of paths, to
override their configured roots for one request. `params.query` sets the initial
fuzzy query, for example to start a history picker with the text already typed
at the shell prompt. The OSC payload is limited to
1 MiB. BEL and ST terminators are accepted. Invalid
tokens are rejected. Requests are ignored while search, quick select, or another
picker is active; copy mode may open a picker normally. Pending requests coalesce
to the latest validated request. Child processes inherit the token; it does not
restrict programs that already have access to the terminal's environment.

Keys accept egui names such as `f`, `Space`, and `ArrowUp`; modifiers are joined
with `|` (`CTRL`, `SHIFT`, `ALT`; `CMD`/`SUPER` are macOS-only). Definitions and regexes are compiled
at startup; unknown picker names, invalid patterns, duplicate bindings, and
conflicts with reserved app shortcuts report configuration errors. Bindings only
trigger from terminal or copy mode, leaving modal query input unaffected.
Without `pickers`, the built-in `lines` picker remains available. Without `keys`,
Ctrl+Shift+F opens `lines` if defined. An explicit empty `keys = {}` disables picker
shortcuts. Explicit `pickers` and `keys` replace their respective defaults.

The Rust source is divided by responsibility: `app.rs` owns the native window
and event loop, `terminal.rs` handles pane layout, `terminal/pane.rs`
coordinates the embedded surface, `ghostty_ffi.rs`
contains the C boundary, `painter.rs` presents Ghostty's OpenGL textures,
`picker.rs` and its `picker/` support modules implement the fuzzy picker,
`input.rs` translates egui input, and `search.rs` owns search state and
matching. `presentation.rs` owns presentation pacing and detects changes to
egui's drawing commands. `main.rs`
loads Lua settings through `config.rs`, collects process arguments, and starts the app.

Input updates run promptly, independently of presentation pacing. Completed
terminal frame notifications are coalesced, and the host acquires the newest
frame when a presentation is due. The terminal paint callback has a stable
identity and draws the painter's current texture. UI changes (including search
and caret blinking) are detected by comparing egui drawing commands, without
copying or comparing terminal pixels. A separate cleanup deadline polls GPU
fences and returns old textures without running egui or swapping buffers.

Search matching runs on a dedicated Rust worker thread. It reads the frozen
Ghostty snapshot one page-sized text chunk at a time. Each chunk includes the
corresponding terminal cell for every UTF-8 byte, so the worker can return cell
ranges directly without exporting or indexing the complete scrollback text.
Snapshot generations and per-query revisions prevent completed work for an
older snapshot or query from replacing current results. Closing search waits
for the worker to stop reading before releasing the snapshot.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>/</kbd> on Windows and Linux to
open the search overlay. Rust consumes this shortcut before forwarding input
to Ghostty; Ghostty search keybindings do not control the sample's search UI.
When the floating pane is visible, the shortcut searches that pane's scrollback
and draws the search bar inside it. Hiding, closing, or resizing the pane closes
its search safely.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>E</kbd> to show or hide a centered
floating shell. The first press creates it; later presses toggle the same
terminal without restarting its shell. While visible, it receives keyboard,
paste, and mouse input. Exiting that shell closes only the floating pane.
Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>W</kbd> to close the floating pane
and its shell without closing the main terminal.

Search queries, result highlighting, match selection,
and viewport movement are owned by the Rust egui application. It asks Ghostty
for a frozen copy of the complete scrollback, performs Unicode-aware,
case-insensitive literal matching over its streamed UTF-8 text, and sends
matching cell ranges back through the snapshot API. Press <kbd>Ctrl</kbd>+<kbd>R</kbd>
in the search field to toggle regex mode. Regexes are case-sensitive by default;
use `(?i)` for case-insensitive regex matching. Search is scoped to logical
lines, so patterns do not span newlines. Invalid regexes appear in the search
bar. Canonically equivalent Unicode spellings are not normalized.
Ghostty renders the selected match and other matches as ordered decoration
layers on the frozen snapshot. PTY output
continues updating the live terminal behind the snapshot. Press
<kbd>Ctrl</kbd>+<kbd>N</kbd> for the next older result,
<kbd>Ctrl</kbd>+<kbd>P</kbd> for the previous newer result, and Escape to close
the overlay and reveal the newest live state. Enter closes the search field
and enters copy mode at the selected match's first cell, preserving the frozen
snapshot. If a query is still running, Enter waits for its result; no matches
or an invalid regex leave search open. In copy mode, <kbd>n</kbd>/<kbd>N</kbd>
can continue navigating the transferred matches.
In the floating pane, Enter places its copy cursor at the selected match;
the same copy-mode motions and selections work there.

While the search field is focused, <kbd>Ctrl</kbd>+<kbd>N</kbd> selects the
next older match and <kbd>Ctrl</kbd>+<kbd>P</kbd> selects the previous newer
match.

Clipboard paste is forwarded to Ghostty when the terminal has keyboard focus.
When the search field is focused, paste remains within the search query.

Press <kbd>Alt</kbd>+<kbd>Space</kbd> in terminal or copy mode to enter quick
select. When the floating pane is visible, it selects from that pane and places
its copy cursor there. It labels visible big words using the hardcoded regex `\S+`, including
words across soft wraps. Type a label to enter copy mode at the word's first
cell. Hold Shift on the final hint letter to leave the snapshot and paste the
matched text into the live terminal without submitting it. Caps Lock alone
does not trigger paste. Labels are case-insensitive; as you type, only matching labels remain
with bold yellow labels on muted gray matches. Hint cells retain their original
background. After typing a prefix, the already typed letters disappear and the
remaining letters stay yellow. Backspace removes
a prefix character.
Escape (or Alt+Space again) returns to the previous mode. Quick select reuses
the existing copy snapshot and preserves its cursor and selection on cancel.
In copy mode, <kbd>Shift</kbd>+<kbd>Y</kbd> shows the same hints for a linewise
yank. Completing a hint copies the logical lines from the current cursor (or
visual anchor) through the hinted word's line and stays in copy mode. Escape
cancels without changing the selection. Shift on the final hint letter still
completes the yank rather than sending text to the terminal.
Page Up/Down moves through the frozen scrollback, regenerates visible labels,
and clears the typed prefix. Other scrolling is blocked while labels are displayed.
Labels are rendered by
Ghostty using generic printable-ASCII cell text overrides; the frozen terminal
text stays unchanged.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> to open the scrollback fuzzy
picker from terminal or copy mode. Nonblank logical lines stream newest first
from a frozen snapshot into `nfm-search-core`. Type to filter; Ctrl+N/P, arrows,
and Page Up/Down move through results. Enter enters copy mode at the first
nonblank cell of the selected line. Escape returns to the originating mode.
Results fill upward from the query, with the first result at the bottom.
Arrow and page keys follow that visual direction; Ctrl+N moves to the next
result in ranking order. Ctrl+P toggles the preview when configured.
The sample currently uses the local `nfm_search_core` crate
at `G:/src/RustWin32Host/nfm-search-core`; adjust its Cargo path on other machines.

The picker imports Ghostty's resolved primary regular and bold fonts and uses the current
terminal cell dimensions to size its text and rows, including zoom and DPI changes.
Matched positions use bold red text; selection uses a background highlight.
Unsupported non-SFNT font formats use egui's monospace fallback.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>M</kbd> to enter copy
mode. Ghostty freezes the visible terminal and scrollback while the live PTY
keeps running. A steady, styled cell marks the terminal's insertion point.
With a floating pane visible, copy mode freezes and controls that pane instead.
A permanent 24-point status row below the terminal shows `TERMINAL` in live
mode and a yellow `NORMAL` label in copy mode. Visual selection changes the
label and top separator to orange and shows `VISUAL`, `V-LINE`, or `V-BLOCK`.
The right side shows the copy cursor's one-based scrollback row and column.
The status row never covers terminal content or changes size when switching modes.

<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Up</kbd>/<kbd>Down</kbd> also enter copy
mode and move to the previous/next regex-matched command prompt. In copy mode,
the same shortcuts navigate commands; a count such as <kbd>3</kbd> followed
by <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Up</kbd> skips three prompts. Navigation
stops at the first or last command without wrapping.
Move the cursor with <kbd>h</kbd>/<kbd>j</kbd>/<kbd>k</kbd>/<kbd>l</kbd> or the arrow
keys. <kbd>H</kbd>/<kbd>M</kbd>/<kbd>L</kbd> move to the top, middle, or bottom
visible row. Page Up/Down move a full screen, and <kbd>Ctrl</kbd>+<kbd>U</kbd>/
<kbd>Ctrl</kbd>+<kbd>D</kbd> move half a screen. Movement scrolls the frozen
viewport when necessary. <kbd>^</kbd> and <kbd>$</kbd> move to the first and
last nonblank cell on the current row. <kbd>w</kbd>, <kbd>b</kbd>, and
<kbd>e</kbd> move through words across the frozen scrollback.
<kbd>0</kbd> moves to the start of the row; <kbd>W</kbd>, <kbd>B</kbd>, and
<kbd>E</kbd> move through whitespace-delimited words. <kbd>gg</kbd> and
<kbd>G</kbd> move to the top and bottom of scrollback. <kbd>f</kbd>/<kbd>F</kbd>
and <kbd>t</kbd>/<kbd>T</kbd> jump to or before/after a character on the
current row; <kbd>;</kbd> repeats the jump and <kbd>,</kbd> reverses it.
<kbd>%</kbd> matches nested parentheses, brackets, or braces across rows.
Use <kbd>Ctrl</kbd>+<kbd>E</kbd>/<kbd>Y</kbd> to scroll down/up one physical
row, or prefix a count to scroll multiple rows. The cursor stays on its cell
until scrolling would move it outside the padded viewport. Visual selections
keep their anchor fixed while scrolling.

Use <kbd>zt</kbd>, <kbd>zz</kbd>, or <kbd>zb</kbd> to place the copy cursor's
row at the top, center, or bottom of the viewport without moving the cursor
or changing the selection. Placement clamps at the scrollback boundaries.
Copy motions, search-match navigation, and <kbd>zt</kbd>/<kbd>zb</kbd> keep
three rows of padding above and below the cursor when possible. Short viewports
reduce the padding; <kbd>zz</kbd> keeps the cursor centered.
Prefix a motion with a count, such as <kbd>10j</kbd>, <kbd>3w</kbd>, or
<kbd>2f:</kbd>. <kbd>3gg</kbd>/<kbd>3G</kbd> go to the third snapshot row;
<kbd>50%</kbd> goes halfway through the snapshot. <kbd>ge</kbd>/<kbd>gE</kbd>
move to the end of the previous word or whitespace-delimited word.
<kbd>+</kbd> and <kbd>-</kbd> move to the first nonblank cell on the next or
previous row; <kbd>_</kbd> does so on the current row (or the counted row).
<kbd>*</kbd>/<kbd>#</kbd> search for the word under the copy cursor toward
newer/older output. <kbd>n</kbd> repeats that direction and <kbd>N</kbd>
reverses it; matches wrap around the frozen scrollback.
In visual mode, <kbd>o</kbd> swaps the cursor and selection anchor so you can
extend the other end. <kbd>O</kbd> does the same in characterwise/linewise mode;
in rectangular mode it switches to the other horizontal corner.

Press <kbd>Ctrl</kbd>+<kbd>Q</kbd> in copy mode to toggle rectangular selection.
The block covers the same inclusive columns on each physical row, including
soft-wrapped rows. Use motions to adjust it, then <kbd>y</kbd> to copy one line
per selected row and stay in copy mode. Wide characters and combining marks
are extracted by Ghostty. The yank flash keeps the rectangular shape.

Press <kbd>v</kbd> to start characterwise visual selection, then use any copy
motion to extend it. Press <kbd>V</kbd> to select complete logical lines,
including all rows of a wrapped line. Press the active visual key again or
Escape to cancel the selection; pressing the other visual key switches modes.
Press <kbd>y</kbd> to copy the selected text to the system clipboard. Linewise
copies end with a newline. Copy mode
stays open so you can navigate or select again. With no selection, <kbd>y</kbd>
waits for a yank target: <kbd>yy</kbd> copies the current logical line,
<kbd>y$</kbd> copies through the last nonblank cell of the current physical
row. <kbd>yiw</kbd>/<kbd>yaw</kbd> copy a word without or with adjacent
whitespace; <kbd>yiW</kbd>/<kbd>yaW</kbd> treat punctuation as part of the word.
The <kbd>i</kbd>/<kbd>a</kbd> text objects also accept single or double quotes,
backticks, and either side of <kbd>()</kbd>, <kbd>[]</kbd>, <kbd>{}</kbd>, or
<kbd>&lt;&gt;</kbd>. For example, <kbd>yi(</kbd> excludes the parentheses and
<kbd>ya"</kbd> includes the quotes. <kbd>y</kbd> also accepts the copy-mode
motions, including <kbd>yw</kbd>, <kbd>yb</kbd>, <kbd>yj</kbd>, <kbd>ygg</kbd>,
<kbd>yf:</kbd>, and <kbd>y;</kbd>. Counts such as <kbd>2yy</kbd>,
<kbd>2yiw</kbd>, and <kbd>y3w</kbd> are supported.
While visual mode is active, <kbd>i</kbd>/<kbd>a</kbd> select the same text
objects: <kbd>viw</kbd>/<kbd>vaw</kbd>, <kbd>viW</kbd>/<kbd>vaW</kbd>, quotes,
brackets, and <kbd>vic</kbd>/<kbd>vac</kbd> for command blocks. Counts such as
<kbd>v2iw</kbd> are supported. Selecting an object replaces the current
selection and switches to characterwise visual mode; use motions to extend it
or <kbd>y</kbd> to copy it and stay in copy mode. Repeated text objects do not
yet expand existing selections as Vim does.
<kbd>yic</kbd>/<kbd>yac</kbd> copy a command's output or the prompt and output
using prompt regexes over the frozen scrollback. The four patterns copied
from the reference WezTerm configuration are hardcoded in
`src/copy_mode.rs` for now, with Lua configuration planned for later.
Like WezTerm, a command block ends immediately before the next matching
prompt line. Text that resembles a prompt can create a false boundary.
Every successful yank briefly highlights the copied cells for 250 ms; a new
yank replaces and restarts the highlight.
Escape cancels a pending yank. Press Escape outside visual selection to return to the live
terminal.

The native C Win32 sample remains available through
`zig build win32-sample -Doptimize=ReleaseFast`.

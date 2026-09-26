# Rust egui terminal shell

This example embeds one full Ghostty terminal in an egui shell. Ghostty keeps
its OpenGL renderer and exports completed textures to egui without a CPU pixel
copy. The native C Win32 sample remains the performance baseline.

From the repository root:

```powershell
zig build egui-sample -Doptimize=ReleaseFast
.\zig-out\bin\ghostty_egui.exe --command=pwsh.exe
```

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

Lua settings are loaded at startup from the first available location:

1. An explicit `--lua-config=PATH` argument.
2. `config.lua` in the home directory (`%USERPROFILE%` on Windows, `$HOME` on Linux).
3. `config.lua` beside the executable, regardless of the current working directory.

Only one file is loaded. Missing default files preserve the existing configuration.
An explicit missing file, unreadable file, or invalid configuration reports the
filename and exits before opening a window, without falling back to another file.
See `config.lua` in this example for a starting configuration:

```powershell
.\zig-out\bin\ghostty_egui.exe --lua-config=example/rust-egui-terminal/config.lua --command=pwsh.exe
```

The file returns a table with optional `font.family`, `font.size` (points),
`terminal.scrollback_limit` (bytes), and `colors.theme` settings. Unknown fields
are rejected. Rust evaluates Lua once, validates the settings, and translates them
into Ghostty configuration arguments. Ghostty's normal configuration files remain
the base; Lua overrides them, and explicit command-line options override Lua.
The picker still imports Ghostty's resolved font and tracks its cell dimensions.
Changes to Lua require restarting the app; callbacks and live reload are not yet supported.

The Rust source is divided by responsibility: `app.rs` owns the native window
and event loop, `terminal.rs` coordinates the embedded surface, `ffi.rs`
contains the C boundary, `painter.rs` presents Ghostty's OpenGL textures,
`input.rs` translates egui input, and `search.rs` owns search state and
matching. `presentation.rs` owns presentation pacing and detects changes to
egui's drawing commands; `frame_trace.rs` owns timing diagnostics. `main.rs`
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

While the search field is focused, <kbd>Ctrl</kbd>+<kbd>N</kbd> selects the
next older match and <kbd>Ctrl</kbd>+<kbd>P</kbd> selects the previous newer
match.

Clipboard paste is forwarded to Ghostty when the terminal has keyboard focus.
When the search field is focused, paste remains within the search query.

Press <kbd>Alt</kbd>+<kbd>Space</kbd> in terminal or copy mode to enter quick
select. It labels visible big words using the hardcoded regex `\S+`, including
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
Arrow and page keys follow that visual direction; Ctrl+N/P move to the next
or previous result in ranking order.
There is no preview. The sample currently uses the local `nfm_search_core` crate
at `G:/src/RustWin32Host/nfm-search-core`; adjust its Cargo path on other machines.

The picker imports Ghostty's resolved primary regular and bold fonts and uses the current
terminal cell dimensions to size its text and rows, including zoom and DPI changes.
Matched positions use bold orange text; selection uses a background highlight.
Unsupported non-SFNT font formats use egui's monospace fallback.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>M</kbd> to enter the initial copy
mode. Ghostty freezes the visible terminal and scrollback while the live PTY
keeps running. A steady, styled cell marks the terminal's insertion point.
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

To record egui frame timing during an output-heavy command, set
`GHOSTTY_EGUI_TRACE` to a CSV path before launching the sample:

```powershell
$env:GHOSTTY_EGUI_TRACE = "$PWD\egui-frame-trace.csv"
.\zig-out\bin\ghostty_egui.exe --command=pwsh.exe
```

The trace records when completed frames reach the UI loop, whether a redraw
presented a new frame or deferred it for display-rate pacing, and the time
spent in Ghostty ticks, egui layout, egui painting, and buffer swap. Close the
sample to flush the trace.

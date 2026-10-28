# Copy mode, search, and quick select

To copy terminal output with the keyboard, press **Ctrl+Shift+M** to enter
[copy mode](#copy-mode). Move with the arrow keys, press `v` to start selecting,
move to extend the selection, and press `y` to copy it to the system clipboard.
Press Escape to return to the live terminal; if a selection or search is active,
Escape clears it first, so another press may be needed. The section below covers
all entry points, motions, selections, searches, and yank commands.

Copy mode captures an independently owned copy of the full terminal and
scrollback when it opens. Search, copy, and quick select read this fixed copy
while shell output continues updating the live terminal. Exiting removes the
decorations and returns to the live terminal. Snapshot history remains resident;
large histories require more memory and take longer to capture at mode entry.

## Copy mode

### Entering copy mode

These are the default bindings. Lua `keys` can override application shortcuts;
the modal motion and selection keys below are built into the shared copy engine.
Focus the terminal before using the terminal shortcuts. Close any picker or
query field first, or use its acceptance action to enter copy mode.

| Entry point | Result |
| --- | --- |
| Ctrl+Shift+M | Enter at the terminal cursor. The Lua action is `EnterCopyMode`. |
| Ctrl+Shift+Up / Ctrl+Shift+Down | Enter at the previous / next recognized command prompt. |
| Search: Ctrl+Shift+/, type a query, then Enter | Enter at the selected match, retaining the search results. |
| Scrollback picker: Ctrl+Shift+F, select a line, then Enter | Enter at the first nonblank cell of that line. |
| Quick select: Alt+Space, then type a displayed hint | Enter at the hinted word. Holding Shift on the final letter pastes the word into the live terminal instead. |

Scrollback picker previews have their own copy-mode focus: show the preview with
Ctrl+P or Ctrl+Space, then press Ctrl+W to focus it. Preview motions and yanks
operate on the preview document, including history beyond the displayed page.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>M</kbd> to enter copy
mode. Ghostty freezes the visible terminal and scrollback while the live PTY
keeps running. A steady, styled cell marks the terminal's insertion point.
With a floating pane visible, copy mode freezes and controls that pane instead.
A permanent 24-point status row below the terminal shows `TERMINAL` in live
mode and a yellow `COPY` label in copy mode. Visual selection changes the
label and top separator to orange and shows `VISUAL`, `V-LINE`, or `V-BLOCK`.
The right side shows the copy cursor's one-based scrollback row and column.
The status row never covers terminal content or changes size when switching modes.
Keyboard motions and selection commands control the saved output while this mode
is active. New shell output continues in the background and becomes visible when
you exit. Copying writes to the clipboard; it does not paste or run a command.

### A first copy

1. Press Ctrl+Shift+M. Check that the status row says `COPY`.
2. Use Up/Down or Page Up to reach the output you want, then Left/Right to
   position the cursor on its first character.
3. Press `v` (`VISUAL`), then move to its last character. Both endpoints are
   included. For complete logical lines, use Shift+V (`V-LINE`) instead.
4. Press `y`. The selected text goes to the system clipboard and briefly
   flashes; the selection clears and copy mode remains open.
5. Press Escape until the status row says `TERMINAL`. You can now paste the
   clipboard into the terminal or another application.

For a whole line without selecting first, move to it and type `yy`.
Uppercase commands mean holding Shift; sequences such as `yy`, `gg`, and `yiw`
mean pressing the keys in order, without Ctrl or Alt. A count is typed before
the command, for example `10j` moves down ten rows.

### Moving through output

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

### Searching from copy mode

Press `/` for forward search toward newer output or `?` for backward search
toward older output. Type a query in the search field; Ctrl+R toggles regex
matching. Literal search ignores case; regex search honors case unless you use
an inline flag such as `(?i)`. Matches can cross soft wraps within a logical
line, but cannot span separate logical lines.

Press Enter to return to copy mode at the selected match. If the search is still
running, Enter waits; an invalid query or no matches leaves the field open.
Escape cancels the field and restores the copy cursor and selection from before
the search. Use `n` to repeat the accepted search direction and `N` to reverse
it; navigation wraps through the snapshot. `*` and `#` start a search for the
word under the cursor toward newer and older output respectively.

### Selecting and copying

| Keys | Selection or copy |
| --- | --- |
| `v` | Toggle characterwise selection (`VISUAL`). |
| Shift+V (`V`) | Toggle logical-line selection (`V-LINE`), including soft wraps. |
| Ctrl+Q | Toggle rectangular selection (`V-BLOCK`) across physical rows. |
| `o` / `O` | Adjust the other end of a selection; `O` switches horizontal corners in block mode. |
| `y` with a selection | Copy the selection and stay in copy mode. |
| `yy` / `2yy` | Copy the current logical line / two logical lines. |
| `y` followed by a motion | Copy through that motion, for example `y$`, `yw`, or `y3w`. |
| `yiw` / `yaw` | Copy the word under the cursor, excluding / including adjacent whitespace. |
| `yic` / `yac` | Copy command output / its prompt and output using recognized prompt boundaries. |
| Shift+Y (`Y`) | Show hints; choose one to copy logical lines through its word's line. |
| Ctrl+B | Open the line-split picker for the current physical row. Escape returns; Enter inserts the piece into the live terminal and exits copy mode. |

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
`nfm-egui/src/copy_mode.rs`.
Like WezTerm, a command block ends immediately before the next matching
prompt line. Text that resembles a prompt can create a false boundary.
Every successful yank briefly highlights the copied cells for 250 ms; a new
yank replaces and restarts the highlight.
### Canceling and exiting

Escape first cancels a pending yank, text object, or viewport-placement sequence.
With a visual selection, it clears the selection and keeps copy mode open.
With retained search matches, it clears those matches and keeps copy mode open.
Otherwise, it exits to the live terminal. Press Escape again as needed until
the status row reads `TERMINAL`. Exiting does not stop the shell or clear the
clipboard.

For a direct exit binding, assign the `ExitCopyMode` application action in Lua
`keys`, for example `{ key = "x", mods = "CTRL|SHIFT", action = "ExitCopyMode" }`.
This action is available while copy mode is active and exits without stepping
through selection and search cancellation.

## Search

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>/</kbd> on Windows and Linux to
open the search overlay. Rust consumes this shortcut before forwarding input
to Ghostty; Ghostty search keybindings do not control nterm's search UI.
When the floating pane is visible, the shortcut searches that pane's scrollback
and draws the search bar inside it. Hiding, closing, or resizing the pane closes
its search safely.

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

## Quick select

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
The hint engine is shared with NFM. Alt+Space also enters Quick Select in a
picker's preview from results or preview focus. Selecting a hint moves the
preview copy cursor; cancellation restores the previous focus and viewport.
Terminal Shift-to-paste remains a terminal action. Preview hints support
PageUp/PageDown and Backspace with the same labels and word matching.
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

## Mouse selection

Releasing a mouse selection leaves it highlighted. Right-click copies the
selection to the system clipboard and clears the highlight; Escape clears it
without copying. nterm uses Ghostty's `right-click-action=copy`,
`copy-on-select=none`, and `selection-clear-on-typing=false` settings, and
forwards Ghostty's clipboard writes through egui. Explicit Ghostty command-line
options can override these defaults.


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

The Rust source is divided by responsibility: `app.rs` owns the native window
and event loop, `terminal.rs` coordinates the embedded surface, `ffi.rs`
contains the C boundary, `painter.rs` presents Ghostty's OpenGL textures,
`input.rs` translates egui input, and `search.rs` owns search state and
matching. `presentation.rs` owns presentation pacing and detects changes to
egui's drawing commands; `frame_trace.rs` owns timing diagnostics. `main.rs`
only collects process arguments and starts the app.

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

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> on Windows and Linux to
open the search overlay. Search queries, result highlighting, match selection,
and viewport movement are owned by the Rust egui application. It asks Ghostty
for a frozen copy of the complete scrollback, performs Unicode-aware,
case-insensitive literal matching over its streamed UTF-8 text, and sends
matching cell ranges back through the snapshot API. Press <kbd>Ctrl</kbd>+<kbd>R</kbd>
in the search field to toggle regex mode. Regexes are case-sensitive by default;
use `(?i)` for case-insensitive regex matching. Search is scoped to logical
lines, so patterns do not span newlines. Invalid regexes appear in the search
bar. Canonically equivalent Unicode spellings are not normalized.
Ghostty only renders that presentation state. PTY output
continues updating the live terminal behind the snapshot. Press Enter or use
<kbd>Ctrl</kbd>+<kbd>N</kbd> for the next older result, Shift+Enter or
<kbd>Ctrl</kbd>+<kbd>P</kbd> for the previous newer result, and Escape to close
the overlay and reveal the newest live state.

While the search field is focused, <kbd>Ctrl</kbd>+<kbd>N</kbd> selects the
next older match and <kbd>Ctrl</kbd>+<kbd>P</kbd> selects the previous newer
match.

Clipboard paste is forwarded to Ghostty when the terminal has keyboard focus.
When the search field is focused, paste remains within the search query.

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

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
current exported frame, and input bridge separately from `ShellApp`. That
boundary leaves room for fixed-position overlay terminals without moving
terminal drawing into egui.

The Rust source is divided by responsibility: `app.rs` owns the native window
and event loop, `terminal.rs` coordinates the embedded surface, `ffi.rs`
contains the C boundary, `painter.rs` presents Ghostty's OpenGL textures,
`input.rs` translates egui input, and `search.rs` owns search state and
matching. `main.rs` only collects process arguments and starts the app.

Search matching runs on a dedicated Rust worker thread over immutable,
application-owned snapshot rows. Ghostty snapshot access and highlight updates
remain on the UI thread. Snapshot generations and per-query revisions prevent
completed work for an older snapshot or query from replacing current results.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> on Windows and Linux to
open the search overlay. Search queries, result highlighting, match selection,
and viewport movement are owned by the Rust egui application. It asks Ghostty
for a frozen copy of the complete scrollback, reads its rows, performs literal
matching, and sends highlight ranges and the selected range back through the
snapshot API. Ghostty only renders that presentation state. PTY output
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

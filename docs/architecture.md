# Architecture

How nterm is put together, for working on its code.

## Source layout

The Rust source is divided by responsibility: `app.rs` owns the native window
and event loop, `terminal.rs` handles pane layout, `terminal/pane.rs`
coordinates the embedded surface, `ghostty_ffi.rs`
contains the C boundary, `painter.rs` composites Ghostty's frames into egui, `gpu.rs` and
`gpu/bridge.rs` own the shared device and run Ghostty's GPU work,
`picker.rs` and its `picker/` support modules implement the fuzzy picker,
`input.rs` translates egui input, and `search.rs` owns search state and
matching. `presentation.rs` owns presentation pacing and detects changes to
egui's drawing commands. `main.rs`
loads Lua settings through `config.rs`, collects process arguments, and starts the app.

## Windows and rendering

Windows also share one egui context, font atlas, and GPU device. Each window
has its own swapchain, while input, focus, DPI, and presentation timing remain
per window. Ghostty's terminal rendering workers record their GPU work on the
same device through a dispatch table (`gpu/bridge.rs`, driven by
`ghostty/src/renderer/Wgpu.zig`).

`TerminalView` owns the Ghostty surface and input bridge separately from
`ShellApp`. Its painter owns the current exported frame, which returns to
Ghostty's texture pool when the next frame replaces it. That
boundary leaves room for fixed-position overlay terminals without moving
terminal drawing into egui.

Input updates run promptly, independently of presentation pacing. Completed
terminal frame notifications are coalesced, and the host acquires the newest
frame when a presentation is due. The terminal paint callback has a stable
identity and draws the painter's current texture. UI changes (including search
and caret blinking) are detected by comparing egui drawing commands, without
copying or comparing terminal pixels. A separate cleanup deadline polls GPU
fences and returns old textures without running egui or swapping buffers.

## Shared NFM components

All nterm pickers now embed the shared `nfm-egui` control used by
SimpleWindowManager. It owns result rows, query input, streaming search, structured
headers, `/` completion, keybinding help, and preview drawing. Ghostty supplies
its existing Lua sources/actions, owned scrollback cell ranges, preview documents,
theme colors, and exported terminal fonts and cell metrics. Main and floating
pane bounds determine compact sizing and placement; inactive pane pickers cannot
consume input or steal focus. Existing picker definitions and named pipe requests remain
supported. Ctrl+P/Ctrl+Space toggle previews, Ctrl+Page Up/Down page previews,
Ctrl+C copies the selected row, and Ctrl+Shift+/ opens keybinding help.

The sample uses `nfm_search_core` and the other NFM crates from
[npicker](https://github.com/e82eric/npicker), at the revision pinned in
`Cargo.toml`.

The text renderer and preview-plan provider come directly from the pinned NFM
revision; no local NFM checkout or copied preview crates are required.

The picker imports Ghostty's resolved primary regular and bold fonts and uses the current
terminal cell dimensions to size its text and rows, including zoom and DPI changes.
Matched positions use bold text in the `picker_match_highlight` color (see
[UI colors](ui-colors.md)); selection uses a background highlight.
Unsupported non-SFNT font formats use egui's monospace fallback.

Copy mode uses the shared `nfm-egui::copy_mode` engine. Ghostty supplies native
terminal snapshots, rendering, clipboard operations, and
terminal-specific actions such as hint yank and line splits.

## Search

Terminal and preview search share NFM's `document_search` logical-line scanner,
pattern compiler, and match navigation. Literal matching ignores case; regex
matching honors case unless inline flags override it. Soft wraps remain part of
the same logical line, and UTF-8 matches retain their display-cell coordinates.
Both paths limit a logical line to 4 MiB and search results to 200,000 matches;
exceeding either bound reports a search error. NFM's `SearchSession` handles
copy-mode query revisions and navigation queued while a search is pending.
NFM's `SearchService` owns the reusable worker, superseded-query cancellation,
stale-result filtering, completion notifications, and worker recovery. Ghostty
supplies a guarded snapshot cursor through `SearchDocument::visit_text`, using
chunked reads rather than one native call per row, and retains terminal search
UI and snapshot lifetime management. Cancellation waits for the worker to release
the document before Ghostty invalidates the cursor lease and frees the cursor.

Scrollback picker previews also support Ctrl+W copy-mode focus. NFM reads the
original Ghostty snapshot lazily through a host-backed document adapter, so
motions, wide-cell resolution, search, and yanks can extend beyond the displayed
page. Full-history preview search runs on a cancellable worker. The preview
lease closes before its native terminal host is destroyed.

Search matching runs on a dedicated Rust worker thread. It reads the frozen
Ghostty snapshot one page-sized text chunk at a time. Each chunk includes the
corresponding terminal cell for every UTF-8 byte, so the worker can return cell
ranges directly without exporting or indexing the complete scrollback text.
Snapshot generations and per-query revisions prevent completed work for an
older snapshot or query from replacing current results. Closing search waits
for the worker to stop reading before releasing the snapshot.

## Preview documents

Text and ANSI picker previews now use NFM's complete-document byte API.
Scrollback previews use NFM's compact `styled_document::DocumentPreviewProvider`.
The source exports text and resolved style runs once, retaining hard line breaks
and joining soft wraps into logical lines. Search candidates reference byte
ranges in that same chunked text store. NFM builds one row-layout map per width,
then renders only the requested viewport as a styled grid. Selecting another
result reuses the layout; copy navigation, search, and yank use the same owned
document. No preview terminal is cloned or reflowed, and selection changes do
not acquire Ghostty's renderer mutex.

The initial document load runs on the scrollback worker before candidates are
published. Pane resize cancels an unfinished capture and closes the picker;
loaded pickers retain their document and results. Old
immutable documents can safely outlive the host. Command/Lua previews resolve an owned
command specification on a dedicated Lua worker. NFM's shared command executor runs
it with concurrent bounded stdout/stderr reads and cancellation, then retains
the output through the complete-document API. Ghostty owns the Lua resolver worker;
NFM owns ANSI parsing, document caching and scrolling.

Command picker candidates use `nfm_picker_sources::command`, backed by the same
shared command executor in streaming mode. NFM handles pipe reads, newline and
continuation records, deduplication, accepted values, cancellation, exit codes
and incremental snapshot publication. Ghostty's `picker/command.rs` only
resolves config/request parameters and connects repaint/error reporting.
Completed records appear while the command is still running. Total candidate
output is not subject to preview byte limits; individual physical lines and
joined continuation records are limited to 4 MiB, and retained stderr to 16 KiB.

## Snapshot resize

Snapshot resize is coordinated by Rust. The pane retains a single frozen
terminal: live output stays independent, while that terminal reflows in place.
Before reflow, Rust drains search readers and closes unfinished scrollback captures.
The resize API returns an owned translation map built during Ghostty's existing
reflow pass. Sorted old-row/column segments and a row-offset table let Rust
translate all retained pane cursor and selection cells with local
indexed lookups. The map is released after translation. Zig also preserves the
viewport's top-cell anchor. No original pane layout or permanent pane row table
is retained.

Rust updates copy-mode dimensions and saved search-origin state, clears prompt
caches and yank flashes, restarts search, and rebuilds quick-select hints with
an empty prefix. Loaded scrollback pickers and line splitters retain their
document, query, results, and selected item. Preview copy mode resets and the
preview wraps at its new width. Native candidate positions remain in their
capture layout; navigation, positional callbacks, and splitting a selected line
derive logical document positions and resolve them against the current snapshot
on demand. A scan counts hard line breaks and formats only the target logical
lines to map UTF-8 offsets to cells. No per-candidate position table is added.
Standalone line splitters retain their window's logical origin. An unfinished capture (including a retained line-split
parent) is cancelled and the picker closes instead of reloading.

Selections retain their exact copied text through repeated resizes. Block
selections translate one range per selected row, so their highlights can become
disjoint after reflow. Characterwise and linewise endpoints translate too.
Moving a retained selection starts ordinary characterwise selection in the
new layout; yanking an unchanged selection uses its captured text.

A scrollback preview shares immutable Rust text and style runs with its search
source. Width-specific layouts contain row boundaries and retain that content;
copy mode keeps its layout fixed while focused. A pane resize exits preview
copy mode and discards its viewport while retaining the loaded source and selection.
Preview interaction and copying have no native terminal lease to drain.

## Errors and diagnostics

Invalid startup arguments and unrecoverable UI/internal failures use the native
fatal-error path. Rust callbacks catch panics before returning across the C API.

With `GHOSTTY_LOG=stderr`, lines containing `resize diagnostic` show the grid
and cursor after reflow, Windows ConPTY notification, and cursor-position
replies. Windows finishes reflow before notifying ConPTY so a resize-triggered
cursor query receives coordinates from the updated terminal. These diagnostics
contain coordinates only, not terminal text.


Native IME composition is disabled to avoid blocking Windows composition
calls during typing. Keyboard input, clipboard paste, and Unicode display are
unaffected.

# Rust egui terminal shell

This example embeds Ghostty terminals in an egui shell. It supports OpenGL
and Vulkan, exporting completed textures to egui without a CPU pixel copy.

From the repository root:

```powershell
zig build egui-sample -Doptimize=ReleaseFast
.\zig-out\bin\ghostty_egui.exe --command=pwsh.exe
```

Build the Vulkan application with:

```powershell
zig build egui-sample -Drenderer=vulkan -Doptimize=ReleaseFast
.\zig-out\bin\ghostty_egui.exe --command=pwsh.exe
```

This selects both the Zig Vulkan renderer and Cargo's `vulkan` feature. The
terminal workers and egui-wgpu share one Vulkan device and queue across all
windows. Each window has its own swapchain; each terminal retains its own grid,
atlases, and upload buffers. Text, cursors, cell backgrounds, Kitty images,
background images, and ShaderToy custom shaders use Vulkan. Custom shaders
compile to SPIR-V; completed terminal frames use GPU texture copies and a
bounded reusable export pool. The Vulkan path creates no OpenGL contexts and
does not fall back to another backend if Vulkan is unavailable.

Windows is the tested platform. Linux uses the same wgpu Vulkan backend but
has not been validated here. macOS requires a Vulkan implementation such as
MoltenVK; this build does not automatically select Metal. The Vulkan backend
currently serves the embedded egui application, rather than the GTK/macOS apps.
Build again without `-Drenderer=vulkan` to restore the OpenGL application. Keep
the executable and its matching `ghostty.dll` together when comparing builds.

After a Windows build, run the offscreen Vulkan pixel checks with:

```powershell
$env:NFM_GHOSTTY_VT_INCLUDE_DIR = "$PWD/include"
$env:NFM_GHOSTTY_VT_LIB_DIR = "$PWD/zig-out/lib"
$env:NFM_GHOSTTY_VT_LIB_NAME = 'ghostty-vt-static'
cargo test --manifest-path example/rust-egui-terminal/Cargo.toml --features vulkan --test vulkan_render --target-dir .zig-cache/egui-target -- --nocapture
```

These checks require a working Vulkan driver and fail if Vulkan is unavailable;
they do not substitute a different graphics backend. They check packed cell
layouts, background and glyph pixels, native image dispatch, and egui composition
on the same device.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> to open another terminal in
a new window within the same process. Each window has its own shell, terminal
view, floating pane, and UI state. Windows share the native Ghostty app and its
configuration; theme and font changes apply to all windows. Closing a window
closes its terminals, and the app exits after the last window closes.

Windows also share one egui context, font atlas, and GPU painter. In the OpenGL
build, the UI context switches between each window's drawing surface, while
input, focus, DPI, and presentation timing remain per window. Ghostty's terminal
rendering workers retain their own OpenGL contexts. A hidden graphics window
keeps shared resources alive independently of whichever terminal window closes.
The Vulkan build keeps its shared device alive without a hidden graphics window.

On Windows, run `python example/rust-egui-terminal/tests/multiwindow_smoke.py`
after building to check the shortcut, multiple windows in one process, floating
panes, resizing, shell exit, and closing the original window before the others.
The check briefly opens and focuses test windows, then closes them.
For the Vulkan custom-shader regression, add
`--terminal-arg=--custom-shader=C:/src/ghostty2/example/rust-egui-terminal/tests/vulkan_passthrough.glsl`
(adjust the absolute repository path). This also covers MSVC DLL initialization
needed by the native shader compiler.
Add `--memory-report .zig-cache/shared-ui-memory.json` to record private working
set, total working set, and private bytes for one window, four windows, and four
windows with floating panes. Use `--executable <path>` to compare another build.

On the development RTX 3060 machine, ReleaseFast builds with `cmd.exe` and no
custom shaders measured the following private resident RAM (MiB). Both runs
used 1901×1039 pixels for one window and 1901×513 pixels per window for four.

| Scenario | OpenGL | Vulkan |
| --- | ---: | ---: |
| One window | 66.6 | 36.9 |
| Four windows | 152.7 | 43.8 |
| Four windows with floating panes | 255.6 | 52.4 |

These are process working-set measurements, excluding child shells and GPU
VRAM. Private committed memory was respectively 303/738/1239 MiB for OpenGL
and 412/892/1163 MiB for Vulkan. Lower resident RAM therefore does not imply
lower commitment in every scenario. Results vary with driver, window size,
terminal content, and configuration.

Copy mode uses the shared `nfm-egui::copy_mode` engine. Ghostty supplies native
terminal snapshots, rendering, search workers, clipboard operations, and
terminal-specific actions such as hint yank and line splits.

Copy mode borrows the live terminal until its first guarded mutation. At that
point it captures independently owned compressed history before applying the
mutation. Search and text extraction decode one page into temporary storage;
they do not expand the entire snapshot. Complete cursor-style-only output
does not trigger capture because the insertion cursor is hidden in copy mode.
The compressor retains native mappings, so Windows committed-memory counters
can include those mappings even when their physical pages are discarded.

For snapshot memory diagnostics, enable native logging before starting a new
process:

```powershell
$env:GHOSTTY_LOG = 'stderr'
Start-Process -FilePath .\zig-out\bin\ghostty_egui.exe `
    -ArgumentList '--command=pwsh.exe' `
    -RedirectStandardError snapshot-diagnostics.log
```

Use `Start-Process` to explicitly provide the Windows GUI application with a
stderr handle. PowerShell's `2>` redirection can leave an empty file for this
application. Startup `info:` lines should appear before entering copy mode.

Lines containing `snapshot diagnostic` report borrowed-mode entry, the first
capture trigger, and resident/compressed terminal
page counts and bytes before and after text scans. An `owned` source means a
snapshot was actually allocated. If only `live` resident bytes grow while
compressed pages decrease, scanning is restoring compressed history.
Complete cursor-style-only output is reported as `skipped_capture=cursor_style`;
the frozen presentation hides the terminal cursor, so this needs no copy.
The reported bytes measure terminal page storage, rather than total process
memory. Inspecting the
diagnostic metadata does not restore compressed pages. Clear `GHOSTTY_LOG` to
disable logging. Rebuild with `zig build egui-sample -Doptimize=ReleaseFast` to update the DLL beside
the executable; plain `zig build` updates the library output separately.

With `GHOSTTY_LOG=stderr`, lines containing `resize diagnostic` show the grid
and cursor after reflow, Windows ConPTY notification, and cursor-position
replies. Windows finishes reflow before notifying ConPTY so a resize-triggered
cursor query receives coordinates from the updated terminal. These diagnostics
contain coordinates only, not terminal text.

Windows builds compile the bundled `xterm-ghostty` terminfo database using
`tic` from Git for Windows or PATH. For another installation, pass
`-Dterminfo-compiler="C:/path/to/tic.exe"`. The app sets `TERM`, `COLORTERM`,
and an MSYS-compatible `TERMINFO` path for its shell, so Git's `less` can
recognize the terminal without a PowerShell override.

To build and deploy the Windows sample to another directory:

```powershell
.\example\rust-egui-terminal\deploy.ps1 C:\Apps\GhosttyEgui
```

Windows `zig build`, `zig build egui-sample`, and `zig build win32-sample` also
install the matched ConPTY DLL and console host into the build's `bin` directory
(normally `zig-out/bin`). Run `zig build conpty` to install just those components.
Build and deployment share the package pin in `conpty.ps1`.
The script verifies pinned SHA-256 hashes for the downloaded or cached package
before extraction, and for both extracted components before copying them.
A mismatch stops installation. The helper also requires the .NET SDK and runs
`dotnet nuget verify` before extraction or installation, requiring the pinned
Microsoft signing certificate and valid package signatures. Updating ConPTY
requires reviewing the version, hashes, and signing certificate together.

The script copies `ghostty_egui.exe`, its required `ghostty.dll`, and the matched
ConPTY `1.25.260710002-preview` DLL and console host. It downloads the pinned
Microsoft NuGet package on first use and caches it under `.zig-cache/conpty`.
Use `-SkipBuild` to copy an existing build; the pinned ConPTY components are
still deployed. Close running instances before replacing their binaries.
It leaves any `ghostty.lua` in the
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
filename in Messages and starts with defaults for Lua settings, without falling
back to another file. A persistent configuration indicator remains until restart.
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
Use the command palette's Reload configuration action to apply supported Lua settings.

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

Overrides accept `#RRGGBB` or `RRGGBB`. See [UI-COLORS.md](UI-COLORS.md) for all
sixteen roles and derivation rules. Invalid overrides use configuration Messages.
Colors are derived from the resolved Ghostty theme, then global `colors.ui`
overrides are applied, followed by `theme_colors` overrides for that exact,
case-sensitive theme name. This applies at startup and when selecting a theme;
switching to another theme removes the previous theme's overrides. These
overrides affect app chrome and UI decorations, not terminal ANSI colors.

Set `colors.ui.picker_match_highlight` to color the matched characters in picker
results independently of the selected terminal search match (`match_highlight`).
It accepts `#RRGGBB`, supports `theme_colors` and manual reload, and falls back to
`match_highlight` when omitted.

Use **Choose theme** in the command palette to search installed user and bundled
Ghostty themes in the shared fuzzy picker. Arrow keys select an entry; Enter or clicking a theme applies it. Escape
closes the picker without changing the theme. The choice updates the main and
floating terminal panes and the shared UI palette for this session only. Explicit
Lua UI overrides continue to take precedence; picker choices replace explicit
native terminal colors for the session. To save a choice, set `colors.theme` in
Lua and remove conflicting explicit native color settings, then restart. You can also bind the
`OpenThemePicker` action in `keys`.

To configure its placement, define a named theme picker instead:

```lua
pickers = {
  themes = { type = "Themes", position = "Cursor", sort = "Score" },
},
keys = {
  { key = "t", mods = "CTRL|SHIFT", picker = "themes" },
},
```

Add these entries to the tables in your config. Named theme pickers also support
`initial_query` and work in the main and floating panes. The built-in
`OpenThemePicker` action keeps its centered placement.

## Lua actions and toasts

All Ghostty pickers now embed the shared `nfm-egui` control used by
SimpleWindowManager. It owns result rows, query input, streaming search, structured
headers, `/` completion, keybinding help, and preview drawing. Ghostty supplies
its existing Lua sources/actions, owned scrollback cell ranges, preview documents,
theme colors, and exported terminal fonts and cell metrics. Main and floating
pane bounds determine compact sizing and placement; inactive pane pickers cannot
consume input or steal focus. Existing picker definitions and OSC requests remain
supported. Ctrl+P/Ctrl+Space toggle previews, Ctrl+Page Up/Down page previews,
Ctrl+C copies the selected row, and Ctrl+Shift+/ opens keybinding help.

Preview parsing uses NFM's `nfm-preview-vt` crate and the standalone VT library
produced by this Ghostty build. Zig supplies its include/library paths; direct
Cargo builds from this repository or the sample directory use `.cargo/config.toml`.
Build the native libraries first with `zig build egui-sample`. Explicit
`NFM_GHOSTTY_VT_INCLUDE_DIR`, `NFM_GHOSTTY_VT_LIB_DIR`, and
`NFM_GHOSTTY_VT_LIB_NAME` environment variables can select another VT build.

Recoverable errors and fallback warnings are retained in a session history
(200 records, 16 KiB details per record). Repeated failures aggregate. They also
write to stderr and retain their existing toast or local picker/preview feedback.
Choose **Error history** in the command palette and select an entry to view and
copy its details. **Show messages** includes history alongside persistent
configuration issues. Reopen the history picker to refresh its snapshot.

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

Search workers start on demand. A worker failure cancels the affected search;
retrying starts a replacement. Command and preview errors include command/cwd,
exit status, and bounded stderr. Command pickers accept exit 0 by default;
ripgrep also accepts exit 1 for no matches. A command picker can specify
`success_exit_codes = { 0, 1 }` to use another convention. Partial command
results remain visible with an error indicating that they may be incomplete.
Preview errors leave the picker usable; selecting another item retries naturally.

Invalid startup arguments and unrecoverable UI/internal failures use the native
fatal-error path. Rust callbacks catch panics before returning across the C API.

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
same notification refreshes it instead of adding another card. The existing positional form continues
to work, with its title displayed as the header; an empty title omits it.

`get_selection_text()` returns the active pane's live selection or copy-mode
visual selection, and returns an empty string if none exists. Toast duration is in milliseconds
(default 2000, allowed range 1–60000). Toasts use the picker's `Cursor` placement
within the originating pane: centered below the cursor when space permits, above
it otherwise, with the same bounds checks and bottom fallback when the cursor is unavailable,
without taking keyboard focus. Both methods are also
available in picker action callbacks. App handles expire when the callback returns.

The sample binds **Ctrl+Shift+S** to save the selection in `~/.wtd/.wt.json`,
matching WezTerm's shortcut and file format. It preserves other JSON fields and
leaves the text selected after saving. The file must already exist. Success and errors are
shown as toasts. `ghostty.json_encode_pretty(value)` encodes JSON. The action
writes directly using Lua's `io.open(path, "wb")`, matching the WezTerm config.

## Codex suggestions

The sample defines `CodexSuggestions` on **Ctrl+Shift+K**, matching the WezTerm
configuration. It uses the last `#` in the text before the cursor to identify
editable input, sends that row and recent terminal context to `codex exec`, and
streams ten suggestions into a cursor-positioned picker. Enter replaces the input
before the cursor with the selected suggestion; it does not execute the command.
The model and CLI arguments are configured in the Lua factory.

Picker factories receive `context.recent_lines`: up to ten recent nonblank terminal
rows, in chronological order, read from at most the last 256 snapshot rows.
Pickers use the original seven-row layout. All picker definitions accept `sort`.
The default, `sort = "Score"`, ranks results by fuzzy match score. Use
`sort = "Source"` to preserve source order while filtering. `"SourceOrder"` remains an alias for
`"Source"`. Source order means input order for Items and Command, catalog order
for CommandPalette, newest-first for Scrollback, and scan order for FileSystem.

## Splitting a scrollback result

In a scrollback picker, **Ctrl+B** opens searchable pieces of the selected
result's first physical row. **Alt+Left** returns to the original source and
query; Escape closes the picker. Enter inserts the selected piece into the
terminal, and Ctrl+C copies it. These shortcuts appear in picker help.

In copy mode, **Ctrl+B** opens the same line-split picker for the copy cursor's
current physical row. Escape returns to copy mode with the cursor preserved;
Enter inserts the selected piece and exits copy mode. This entry point reuses
the frozen copy-mode snapshot and does not scan the full scrollback.

The shared `nfm-picker-sources::line_split` extractor prioritizes fixed-width
table fields and quoted/bracketed values before whitespace tokens and path/URL
components. It supports nested brackets, escaped and doubled quotes, and
punctuation-trimmed alternatives. Table detection samples up to ten neighboring
physical rows on either side and requires consistent gutters in at least three
rows. One-cell gutters also require an aligned next field and varying occupied
width of the preceding value; short detected fields such as `0` remain selectable.
Payloads preserve grapheme cell ranges for preview highlights.
Common PowerShell/debugger prompt prefixes are excluded by pattern; this adapter
does not yet use semantic prompt ranges. Soft-wrapped rows are split individually.

## Command palette

The command palette is a named picker with `type = "CommandPalette"`. The default
configuration and sample bind it to **Ctrl+Shift+Space**. Type to filter actions,
use the arrow keys to select one, and press Enter to run it. Escape closes the
palette and preserves the terminal's current copy mode and cursor.

Choose **Reload configuration** to reread the Lua configuration without restarting.
Picker definitions, Lua actions, keybindings, UI colors, terminal and picker fonts, the configured theme, and
floating-pane padding update after validation. A failed reload reports an error
and keeps the previous configuration. Existing pickers retain their Lua callbacks
until closed; subsequently opened pickers use the new definitions. Removing a UI
color override restores the active theme's color. Explicit command-line theme
settings retain precedence, as do command-line font family and size settings.
Ghostty preserves a font size adjusted with its zoom actions until the size is reset.
Omitted font settings retain their current values. Shell command and scrollback-limit changes
require restarting the application. This action does not reload Ghostty's native
configuration files or watch for file changes.

The action is always listed, even without a shortcut. To bind it in Lua:

```lua
keys = { { key = "r", mods = "CTRL|SHIFT", action = "ReloadConfig" } }
```

Palette entries use NFM's structured store with aligned `Action` and `Shortcut`
columns. Ordinary text uses fuzzy matching; structured queries can filter or sort
columns, for example `/:Action=~floating` or `/!Ascending==Action`.

The palette lists new window, floating pane, search, copy mode, quick select,
command navigation, and named picker actions. It displays configured picker
shortcuts and includes pickers without a binding. Unavailable actions are omitted:
closing a floating pane requires an existing pane, and entering or exiting copy
mode depends on the target pane's state. Empty Items sources without a provider
and Command sources requiring unresolved OSC parameters are omitted. Lua factories
run only when their picker is selected.

The palette uses the normal picker layout and opens within the active pane.
Pane actions target that pane; choosing an action closes the picker before dispatching it. Terminal
input is blocked while the palette is open. Close another picker, search, or quick
select overlay before opening the palette.

Define it and its shortcut alongside other pickers in the returned Lua table:

```lua
pickers = {
  commands = {
    type = "CommandPalette",
    position = "Center",
    preview_visible = false,
  },
},
keys = {
  { key = "Space", mods = "CTRL|SHIFT", picker = "commands" },
},
```

Duplicate shortcuts across picker and action bindings are rejected.
Add these entries to your existing `pickers` and `keys` tables. You can choose any
name or binding and use normal `position`, `initial_query`, `preview`, and
`preview_visible` options. Previews start hidden by default for this source.
Palette pickers are excluded from the catalog. Selection always dispatches the
catalog's typed action; `enter_action` and custom Lua `action` are unsupported.
The separate top-level `command_palette` setting has been removed.

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

### Action bindings

The same `keys` table accepts top-level application actions. Each binding must
specify exactly one of `action` or `picker`:

```lua
keys = {
  { key = "e", mods = "CTRL|SHIFT", action = "ToggleFloatingPane" },
  { key = "w", mods = "CTRL|SHIFT", action = "CloseFloatingPane" },
  { key = "f", mods = "CTRL|SHIFT", action = "OpenSearch" },
  { key = "Space", mods = "CTRL|SHIFT", picker = "commands" },
},
```

Supported action names are `NewWindow`, `ToggleFloatingPane`, `CloseFloatingPane`,
`OpenSearch`, `EnterCopyMode`, `ExitCopyMode`, `OpenQuickSelect`, `PreviousCommand`,
and `NextCommand`. Use `picker = "name"` to open a named picker. Multiple different
shortcuts may invoke the same action; the command palette displays their actual
configured bindings. Actions without bindings remain available in the palette.

Omitting `keys` preserves the built-in shortcuts and default picker bindings.
Providing `keys` replaces the complete list; `keys = {}` disables all top-level
shortcuts. The sample Lua config explicitly includes the original bindings.
Unknown actions, missing or ambiguous targets, and duplicate shortcuts fail
configuration validation. Shortcuts that were previously reserved for app
actions can now be assigned to any action or picker.

Picker, preview, search, quick-select, and copy-mode controls retain their local
bindings. Top-level bindings do not intercept modal query input. Window and
floating-pane actions remain available in ordinary overlays; the command palette
keeps its input modal. Search and quick select can still be focused or toggled
using their configured bindings while active. In query and hint overlays, bindings
without Ctrl, Alt, or Cmd are left to local text input. `ExitCopyMode` is available only
while copying; Escape retains its existing copy-mode behavior, including clearing
pending motions, visual selection, and search before exiting.

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

Built-in picker presentation can be overridden separately from custom `pickers`:

```lua
builtin_pickers = {
    themes = { position = "Cursor", sort = "Score" },
    command_palette = { position = "Cursor", preview_visible = false },
    lines = { preview_visible = true },
    words = { preview_visible = false },
    line_break = {
        preview_visible = false,
        -- position = "Center",
        -- result_rows = 7,
        -- preview_rows = 12,
    },
},
```

Bind these directly with `builtin_picker`, independently of custom picker names:

```lua
keys = {
    { key = "f", mods = "CTRL|SHIFT", builtin_picker = "lines" },
    { key = "p", mods = "CTRL|SHIFT", builtin_picker = "command_palette" },
    { key = ".", mods = "CTRL|SHIFT", builtin_picker = "words" },
}
```

Themes, command palette, lines, and words always exist, even when omitted from
the config. Their overrides accept the corresponding picker options except
`type` and `regex`, which are built in. The source defaults are Themes,
CommandPalette, nonblank logical lines/Navigate, and unique `\S+` words/SendInput.
Lines preview starts visible; words preview starts hidden. Theme and command
palette pickers default to Cursor position with previews hidden. A built-in
override may also be a function of the same cursor context as custom picker
factories; its returned table is merged with defaults each time it opens.
For example, a words factory can seed `initial_query` and install a callback
to replace the word before the cursor. Existing custom `pickers` remain supported.
`picker = "words"` references a custom picker, while `builtin_picker = "words"`
references the built-in. Names beginning with `builtin.` are reserved; internal
names such as `builtin.words` also identify built-ins in the command palette.

These are partial overrides: omitted line break values use the defaults (Bottom,
seven result rows, twelve preview rows, preview visible). `line_break` applies to
Ctrl+B in copy mode and in scrollback pickers. Alt+Left restores the parent
picker's position and preview state. Ctrl+P still toggles the line break preview.
Result rows must be 1–256 and preview rows 1–12. Source and selection behavior
remain built in; unknown picker names or fields are rejected. Manual config
reload applies these settings to subsequent line break openings.
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
at startup; unknown action or picker names, invalid patterns, ambiguous targets,
and duplicate bindings report configuration errors. Picker bindings trigger from
terminal or copy mode, leaving modal query input unaffected.
Without `pickers`, the built-in `lines` and `commands` pickers remain available.
Without `keys`, built-in action bindings are retained, Ctrl+Shift+F opens `lines`
if defined, and Ctrl+Shift+Space opens the built-in `commands` palette if defined.
An explicit empty `keys = {}` disables top-level action and picker shortcuts.
Explicit `pickers` and `keys` replace their respective defaults.

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
`nfm-egui/src/copy_mode.rs` for now, with Lua configuration planned for later.
Like WezTerm, a command block ends immediately before the next matching
prompt line. Text that resembles a prompt can create a false boundary.
Every successful yank briefly highlights the copied cells for 250 ms; a new
yank replaces and restarts the highlight.
Escape cancels a pending yank. Press Escape outside visual selection to return to the live
terminal.

The native C Win32 sample remains available through
`zig build win32-sample -Doptimize=ReleaseFast`.

## Shared byte preview ownership

Text and ANSI picker previews now use NFM's complete-document byte API.
Scrollback previews use `nfm_egui::byte_preview::ByteSource::Paged`: NFM owns
centering, half-page scrolling, parsing, clipping and selected-range highlights.
Ghostty supplies only the requested styled snapshot rows at the original
terminal width. A guarded `SnapshotPreview` lease drains active reads before
Ghostty releases the frozen snapshot. Command/Lua previews resolve an owned
command specification on the Lua UI thread. NFM's shared command executor runs
it with concurrent bounded stdout/stderr reads and cancellation, then retains
the output through the complete-document API. Ghostty owns no preview worker,
ANSI parser, page cache or half-page scrolling state.

Command picker candidates use `nfm_picker_sources::command`, backed by the same
shared command executor in streaming mode. NFM handles pipe reads, newline and
continuation records, deduplication, accepted values, cancellation, exit codes
and incremental snapshot publication. Ghostty's `picker/command.rs` only
resolves config/request parameters and connects repaint/error reporting.
Completed records appear while the command is still running. Total candidate
output is not subject to preview byte limits; individual physical lines and
joined continuation records are limited to 4 MiB, and retained stderr to 16 KiB.

# Rust egui terminal shell

This example embeds Ghostty terminals in an egui shell. It renders with wgpu:
Direct3D 12 on Windows and Vulkan on Linux. Ghostty and egui share one GPU
device, so finished terminal frames are textures that egui draws directly,
without a copy. Set `WGPU_BACKEND` (for example `vulkan`) to choose another
backend. Without a hardware adapter, as in a virtual machine, it falls back
to software rendering (WARP on Windows) and reports that as a warning.

## Features

- **[Workspaces and layouts](WORKSPACES.md):** Indexed workspaces
  with SWM-style tile, monocle, deck, and grid layouts, Lua automation, and a
  clickable workspace bar. See the [workspace guide](WORKSPACES.md) and
  [example Lua configuration](workspaces.example.lua).
- **[Vim Copy Mode](#vim-copy-mode):** Press Ctrl+Shift+M to browse a frozen copy of
  terminal output with Vim-style motions, search, and character, line, or block
  selections. Press `v`, move to select text, then press `y` to copy it to the
  clipboard. Escape clears an active selection or search before exiting.
- **Search and quick select:** Search scrollback with literal or regex queries,
  filter lines in a fuzzy picker, or jump to words using keyboard hints.
- **[Pickers](#pickers):** Use built-ins or configure custom sources and previews
  in Lua; open named pickers or stream candidates from the shell.
- **Multiple windows:** Press Ctrl+Shift+N to open an independent terminal window
  in the same process, sharing configuration, fonts, and theme settings.
- **[Floating panes](#multiple-floating-panes):** Create, name, hide, restore,
  and cycle through additional shells within a window.
- **[Command palette](#command-palette):** Find application actions and pickers
  from one searchable menu.
- **[Lua customization](#lua-actions-and-toasts):** Configure keybindings,
  pickers, actions, fonts, themes, and UI colors.

## Getting started

Ghostty is built from the `ghostty` submodule, a fork that adds the embedding
APIs this app uses. Clone with `--recursive`, or run
`git submodule update --init` in an existing clone. Building needs Zig and
Cargo, plus the .NET SDK on Windows (see ConPTY below).

```powershell
.\build.ps1
.\out\bin\ghostty_egui.exe --command=pwsh.exe
```

`build.ps1` runs `zig build host-lib` in the submodule, which installs the
Ghostty libraries, resources, and ConPTY into `out/`. It then builds this crate
with Cargo and copies the executable and Lua modules beside them.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> to open another terminal in
a new window within the same process. Each window has its own shell, terminal
view, floating pane, and UI state. Windows share the native Ghostty app and its
configuration; theme and font changes apply to all windows. Closing a window
closes its terminals, and the app exits after the last window closes.

Windows also share one egui context, font atlas, and GPU device. Each window
has its own swapchain, while input, focus, DPI, and presentation timing remain
per window. Ghostty's terminal rendering workers record their GPU work on the
same device through a dispatch table (`gpu/bridge.rs`, driven by
`ghostty/src/renderer/Wgpu.zig`).

On Windows, run `python tests/multiwindow_smoke.py`
after building to check the shortcut, multiple windows in one process, floating
panes, resizing, shell exit, and closing the original window before the others.
The check briefly opens and focuses test windows, then closes them.
Add `--memory-report .zig-cache/shared-ui-memory.json` to record private working
set, total working set, and private bytes for one window, four windows, and four
windows with floating panes. Use `--executable <path>` to compare another build.

To copy terminal output with the keyboard, press **Ctrl+Shift+M** to enter
[Vim Copy Mode](#vim-copy-mode). Move with the arrow keys, press `v` to start selecting,
move to extend the selection, and press `y` to copy it to the system clipboard.
Press Escape to return to the live terminal; if a selection or search is active,
Escape clears it first, so another press may be needed. The section below covers
all entry points, motions, selections, searches, and yank commands.

Copy mode uses the shared `nfm-egui::copy_mode` engine. Ghostty supplies native
terminal snapshots, rendering, clipboard operations, and
terminal-specific actions such as hint yank and line splits.

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

Copy mode captures an independently owned copy of the full terminal and
scrollback when it opens. Search, copy, and quick select read this fixed copy
while shell output continues updating the live terminal. Exiting removes the
decorations and returns to the live terminal. Snapshot history remains resident;
large histories require more memory and take longer to capture at mode entry.
Upstream live scrollback compression remains available.

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
.\deploy.ps1 C:\Apps\GhosttyEgui
```

`build.ps1` also installs the matched ConPTY DLL and console host into
`out/bin`. Build and deployment share the package pin in the submodule's
`dist/windows/conpty.ps1`.
The script verifies pinned SHA-256 hashes for the downloaded or cached package
before extraction, and for both extracted components before copying them.
A mismatch stops installation. The helper also requires the .NET SDK and runs
`dotnet nuget verify` before extraction or installation, requiring the pinned
Microsoft signing certificate and valid package signatures. Updating ConPTY
requires reviewing the version, hashes, and signing certificate together.

The script copies `ghostty_egui.exe`, its required `ghostty.dll`, and the matched
ConPTY `1.25.260710002-preview` DLL and console host. It downloads the pinned
Microsoft NuGet package on first use and caches it under `ghostty/.zig-cache/conpty`.
Use `-SkipBuild` to copy an existing build; the pinned ConPTY components are
still deployed. Close running instances before replacing their binaries.
It leaves any `ghostty.lua` in the
destination untouched; the app also checks the home directory for that file.

On Linux, use `build.sh`:

```sh
./build.sh
./out/bin/ghostty_egui --command="$SHELL"
```

To install it with a desktop entry for application launchers (such as
awesome's menubar), and link `npick` into `~/.local/bin`, run
`deploy.sh`. It builds,
installs to `~/.local/opt/ghostty-egui` (or the folder given as its argument),
and writes `~/.local/share/applications/ghostty-egui.desktop`. Pass
`--skip-build` to install the existing build. Running instances are unaffected.

`TerminalView` owns the Ghostty surface and input bridge separately from
`ShellApp`. Its painter owns the current exported frame, which returns to
Ghostty's texture pool when the next frame replaces it. That
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
See `ghostty.lua` in this repository for a starting configuration:

[ghostty.example.lua](ghostty.example.lua) is an extended Windows sample with
custom picker callbacks, command previews, snippets, and a Codex suggestion
picker. Its filesystem roots default to `.` and tools are resolved from `PATH`.
Read its header for dependencies and optional integrations. Snippets use
`GHOSTTY_SNIPPETS_FILE` or `~/ghostty-snippets.json`; initialize the file with
`{"snippets": []}`. The Codex picker sends terminal context to `codex exec`.
Run it with `--lua-config=ghostty.example.lua`.

```powershell
.\out\bin\ghostty_egui.exe --lua-config=ghostty.lua
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

`prompt_patterns` configures prompt detection for pane copy-mode command navigation
and the line splitter. It replaces the default patterns; an empty list disables
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

Set `colors.ui.copy_cursor_background` to color the copy-mode cursor in the
terminal and NFM preview independently of `accent` and the active search match.
It also supports `theme_colors` overrides and falls back to `accent` when omitted.

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

## Multiple floating panes

The command palette includes a restore entry for each of the current workspace's
floating panes, including hidden ones. Entries always show its stable ID,
followed by its explicit name
or current terminal title, and mark the visible pane as active. Labels are
captured when the palette opens; selecting an entry restores that pane by ID.
If its shell exits while the palette is open, selection reports that the pane
has closed.

Each workspace owns its floating panes, with one visible at a time; see
[Workspaces and layouts](WORKSPACES.md). The actions below act on the current
workspace's floats. Hidden panes, and floats of other workspaces, keep their
shells running. Floats close with their workspace. The default shortcuts are:

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+E | `ToggleFloatingPane` |
| Ctrl+Alt+Shift+E | `CreateFloatingPane` |
| Ctrl+Alt+] | `NextFloatingPane` |
| Ctrl+Alt+[ | `PreviousFloatingPane` |
| Ctrl+Shift+W | `CloseFloatingPane` |

These action names can also be assigned in Lua `keys`. Toggle hides the visible
pane or restores the last focused pane, creating one if none exist. Next and
previous follow creation order, wrap around, and restore a hidden pane. With no
panes, cycling does nothing. Closing the visible pane restores the most recently
focused remaining pane; closing a hidden pane leaves the current visibility
unchanged. A shell exiting removes its pane in the same way.

Named actions and picker selection callbacks support:

```lua
actions = {
  {
    name = "ToolsPane",
    action = function(app)
      local pane = app:get_floating_pane("tools")
      if pane then
        app:restore_floating_pane(pane.id)
      else
        pane = app:create_floating_pane({
          name = "tools", command = "pwsh.exe", cwd = "C:\\src",
        })
        app:show_toast("Floating pane", "Created " .. pane.name .. " (" .. pane.id .. ")")
      end
    end,
  },
},
keys = {
  { key = "t", mods = "CTRL|ALT", action = "ToolsPane" },
},
```

`app:create_floating_pane(options)` returns `{id, name, workspace, visible, focused}` after
native creation succeeds. All options are optional; omitted command and working
directory use the normal terminal defaults. `command` is a Ghostty command
string. Names must be nonempty and unique among the workspace's floating panes.
IDs are stable, monotonically increasing within a window, and never reused.

`app:get_floating_pane(id_or_name, workspace_id?)` returns a description or
`nil`; with no target it returns the last focused floating pane. It searches the
current workspace unless given a workspace ID; to search every workspace, loop
over `app:get_workspaces()`. `app:get_floating_panes(workspace_id?)` returns a
workspace's descriptions in creation order. Descriptions are snapshots; `focused`
means the pane is the window's terminal input target, even if the OS window
itself is unfocused.

`app:restore_floating_pane(id_or_name)`, `app:toggle_floating_pane()`,
`app:next_floating_pane()`, `app:previous_floating_pane()`, and
`app:close_floating_pane(id_or_name)` change pane state immediately and act on
the current workspace's floats. `app:tile_floating_pane` and `app:float_pane`
move terminals between floating and tiled; see [WORKSPACES.md](WORKSPACES.md).
Close without a target closes the last focused pane, including when hidden.
Restore or explicit close of a missing pane raises a Lua error; lookup returns
`nil`. Creation errors are synchronous and can be handled with `pcall`.
Pane operations already performed remain in effect if a callback subsequently
fails. Queued paste/send-text commands still apply to the originating pane and
are discarded if that pane was closed by the callback. Retained app handles
expire when their callback returns.

## Lua actions and toasts

All Ghostty pickers now embed the shared `nfm-egui` control used by
SimpleWindowManager. It owns result rows, query input, streaming search, structured
headers, `/` completion, keybinding help, and preview drawing. Ghostty supplies
its existing Lua sources/actions, owned scrollback cell ranges, preview documents,
theme colors, and exported terminal fonts and cell metrics. Main and floating
pane bounds determine compact sizing and placement; inactive pane pickers cannot
consume input or steal focus. Existing picker definitions and named pipe requests remain
supported. Ctrl+P/Ctrl+Space toggle previews, Ctrl+Page Up/Down page previews,
Ctrl+C copies the selected row, and Ctrl+Shift+/ opens keybinding help.

Preview parsing uses NFM's `nfm-preview-vt` crate and the standalone VT library
produced by the Ghostty submodule. Cargo builds use `.cargo/config.toml`, which
points at `ghostty/include` and `out/lib`. Run `build.ps1` or `build.sh` once
first so those libraries exist. Explicit
`NFM_GHOSTTY_VT_INCLUDE_DIR`, `NFM_GHOSTTY_VT_LIB_DIR`, and
`NFM_GHOSTTY_VT_LIB_NAME` environment variables can select another VT build.
NFM dependencies use the Git revision pinned in `Cargo.toml`. No companion
checkout or machine-specific path override is required.

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
same notification refreshes it instead of adding another card. Escape dismisses the
visible toasts, unless a picker or search is open. The existing positional form continues
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
and Command sources requiring unresolved request parameters are omitted. Lua factories
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

## Pickers

Pickers provide a fuzzy query, selectable results, and optional previews inside
the active terminal pane. Type to filter; arrows, Ctrl+N/P, and Page Up/Down
move through results. Enter accepts and Escape cancels. Ctrl+P or Ctrl+Space
toggles the preview, Ctrl+Page Up/Down scrolls it, Ctrl+W focuses its copy mode,
and Ctrl+Shift+/ opens picker keybinding help.

### Built-in pickers

The four named built-ins are always available. With the default keybindings,
use the shortcuts below; theme selection is also available in the command palette.

| Built-in name | Purpose and acceptance behavior | Default presentation / shortcut |
| --- | --- | --- |
| `lines` | Nonblank logical scrollback lines; Enter enters Vim Copy Mode at the selected line. | Bottom, scrollback preview visible; Ctrl+Shift+F. |
| `words` | Unique whitespace-delimited scrollback words (`\S+`); Enter pastes the word without submitting it. | Bottom, preview hidden; Ctrl+Shift+P. |
| `themes` | Installed user and bundled Ghostty themes; Enter applies the selected theme for the session. | Cursor, score sorting, preview hidden; choose **Choose theme** in the palette. |
| `command_palette` | Application actions and configured pickers; Enter dispatches the selected action. | Cursor, preview hidden; Ctrl+Shift+Space. |

The line-split picker is a contextual built-in: Ctrl+B opens it for the selected
scrollback result or Vim Copy Mode cursor row. Its `line_break` settings control
presentation; it is not one of the four named `builtin_picker` binding targets.
See [Splitting a scrollback result](#splitting-a-scrollback-result) for its actions.
Error history and Messages are also available through the command palette;
[Lua actions and toasts](#lua-actions-and-toasts) explains their configuration.

Built-in picker presentation can be overridden separately from custom `pickers`:

```lua
builtin_pickers = {
    themes = { position = "Cursor", sort = "Score" },
    command_palette = { scope = "Workspace", position = "Center", preview_visible = false },
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

### Custom pickers in Lua

Add definitions to the `pickers` table returned by your
[Lua configuration](ghostty.lua), then bind them with `picker = "name"`.
Merge examples into your existing tables rather than creating duplicate fields.
Providing `keys` replaces the complete shortcut list; retain any application
and built-in picker bindings you still want.

| Source type | Candidates | Acceptance |
| --- | --- | --- |
| `Scrollback` | Frozen logical lines, or matches extracted with `regex`. | `Navigate` enters Vim Copy Mode; `SendInput` pastes text. |
| `FileSystem` | Paths under configured `roots`, with depth and file/directory filters (Windows only). | Paste the raw path without quoting or Enter. |
| `Items` | An `items` Lua provider or JSON items from a named pipe request. | Paste the selected value by default. |
| `Command` | Lines streamed from an executable's stdout. | Paste the selected line by default. |
| `Themes` | Installed themes. | Apply the selected theme. |
| `CommandPalette` | Application actions and configured pickers. | Dispatch the selected action. |
| `ErrorLog` | Session error history. | Close the picker; the preview shows the full message. |

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
specify exactly one of `action`, `picker`, or `builtin_picker`:

```lua
keys = {
  { key = "e", mods = "CTRL|SHIFT", action = "ToggleFloatingPane" },
  { key = "w", mods = "CTRL|SHIFT", action = "CloseFloatingPane" },
  { key = "f", mods = "CTRL|SHIFT", action = "OpenSearch" },
  { key = "Space", mods = "CTRL|SHIFT", builtin_picker = "command_palette" },
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
without Ctrl or Alt are left to local text input. `ExitCopyMode` is available only
while copying; Escape retains its existing copy-mode behavior, including clearing
pending motions, visual selection, and search before exiting.

### Dynamic Lua picker factories

Picker names can also map to functions. The function runs when its keybinding or
named pipe request opens the picker, and returns the same table as a static picker:

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
`context.cwd` is the shell's working directory if it reports one with OSC 7
(see [Shell working directory](#shell-working-directory)), otherwise `nil`.
This is terminal output, not PSReadLine's private edit buffer. `initial_query`
works for both static and function pickers. The function may choose `type`,
`position`, `enter_action`, `action`, and other type-specific fields on every invocation.
Function results use the same validation as static definitions; errors are
printed and the picker stays closed.

### Lua modules

`require("name")` loads `name.lua` (or `name/init.lua`) from the config file's
directory, then from the modules installed with the app in
`share/ghostty-egui/lua`. Dots become directories: `require("ghostty.git")`
loads `ghostty/git.lua`. A module beside the config replaces a bundled one of
the same name. The current directory is never searched, and native (C) modules
cannot be loaded. The config is evaluated again in each preview worker, so
modules load there too.

The app bundles `ghostty.git`, with commit and branch pickers that run git
directly and work in bash and PowerShell 7 on Linux and Windows:

```lua
local git = require("ghostty.git")
return {
  pickers = {
    -- Commits of all branches, remote branches and tags, newest first, in
    -- Hash / Date / Branch / Author / Subject columns. Enter inserts the hash.
    ["Git commits"] = git.commits(),
    -- Local and remote branches. At a prompt matching `prompt` (a Lua
    -- pattern), Enter erases the typed text and runs `git switch`; elsewhere
    -- it inserts the branch name.
    ["Git branches"] = git.branches({
      prompt = "^%[[^%]]*%][$#] ",
      require_clean = true,
    }),
  },
}
```

Both accept `position` (default `Cursor`) and `preview_visible` (default
`false`; Ctrl+P shows `git show` for a commit, or how far a branch is ahead
of or behind the current one and its recent history). For a remote branch,
`git switch` uses the local branch of the same name, creating one that tracks
the remote if needed. `require_clean = true` runs
`git diff --stat --exit-code HEAD && git switch …`, which lists changed tracked
files and does not switch while there are any. Branch names come from remotes
and can contain characters a shell interprets, so they are quoted for the
shell: `quote` defaults to `git.quote.powershell` on Windows and
`git.quote.posix` elsewhere; pass the other to match the shell you run.

The module has more pickers, configured the same way:

| Function | Rows | Enter at the prompt | Ctrl+P |
| --- | --- | --- | --- |
| `git.status()` | `git status --short` lines, paths relative to the shell's directory | Inserts the path, quoted | Staged and unstaged diffs, or an untracked file's text |
| `git.cherry_pick()` | Branches | Opens the picker named by `commits` (default `"Git cherry-pick commits"`) | Ahead/behind and history |
| `git.cherry_pick_commits()` | That branch's commits not in the current one, without merges or commits already applied | Types `git cherry-pick <hash>` | `git show` |
| `git.stashes()` | Ref / Date / Message | Types `git stash apply <ref>` (keeps the stash) | The stash's patch |
| `git.reflog()` | Hash / Entry / Date / Action, including commits no branch points to | Inserts the hash | `git show` |
| `git.file_history()` | Tracked files under the shell's directory; the word at the cursor is the initial query | Opens the picker named by `commits` (default `"Git file history commits"`) | The file's last 15 commits |
| `git.file_history_commits()` | Commits that changed that file, following renames | Inserts the hash | That commit's change to the file |
| `git.fixups()` | Commits on the current branch not on its upstream (or on `base`) | Types `git commit --fixup <hash>` | `git show` |
| `git.tags()` | Tag / Date / Subject, newest first | Inserts the tag, quoted | `git show` |

Typed commands are not run; press Enter yourself, or pass `run = true`.
Away from a prompt (without `prompt`, or e.g. in vim) Enter inserts the hash,
ref, path or name unquoted. `git.cherry_pick_commits` takes the branch from the
`range` parameter that `git.cherry_pick` passes with `app:replace_picker`, and
`git.file_history_commits` takes the `path` parameter from `git.file_history`,
so configure each pair, under the name given to `commits`; opened on its own
the second picker reports a missing parameter. `git.branches` runs its switch command unless `run = false`.

### Picker sources and common options

Every picker accepts `scope = "Pane"` or `"Workspace"`. Pane scope is the default;
command palettes default to workspace scope. Workspace pickers draw across the
workspace content while their terminal context and result destination stay with
the originating pane. Cursor anchors remain pane-relative. Switching workspaces
or focusing another pane cancels a workspace picker. See the
[workspace guide](WORKSPACES.md) for configuration examples.

Every picker type accepts `position = "Top"`, `"Center"`, `"Bottom"`, `"FullPane"`, or
`"Cursor"`; the default is `"Bottom"`. Cursor placement puts the query near
the live terminal cursor, or the copy cursor when opened from copy mode, and
shows the panel below it when there is room, otherwise above it. The panel
stays inside the terminal or floating pane. If the cursor is outside the
visible viewport, placement falls back to Bottom.

Ctrl+Shift+F toggles full-pane placement for any picker. The query
and selected result stay unchanged; preview copy mode resets and content rewraps.
Press it again to restore the configured placement. A picker configured as
FullPane temporarily switches to Center. Custom picker bindings can override
the shortcut.

`FullPane` fills the configured scope: the originating terminal or floating pane,
or the workspace content with `scope = "Workspace"`. Set `result_rows` (1–256,
default 7) to reserve result rows; the preview uses all remaining complete rows
after the query, optional header, and padding. Resizing recalculates the preview
height. `preview_rows` is ignored in this mode. Hiding the preview leaves the
remaining area blank, and very small panes reduce result rows to keep the query
accessible. Named definitions and picker factories support `result_rows` in
every position.

```lua
Details = {
  type = "Items",
  items = { { text = "Example", value = "example" } },
  position = "FullPane",
  result_rows = 7,
  preview_visible = true,
  preview = { type = "Text", text = "{item}", wrap = true },
}
```

PowerShell stream requests use the same placement and row settings:

```powershell
'Example' | Invoke-GhosttyPicker -Options @{
    position = 'FullPane'
    result_rows = 7
    preview_visible = $true
    preview = @{ type = 'Text'; text = '{item}'; wrap = $true }
}
```

Set the top-level `picker_defaults = { unique = true }` to enable deduplication
by default for Scrollback and Command pickers, including function-defined
pickers. Individual definitions can override it with `unique = false` or
`unique = true`. The global default is `false`; Items and FileSystem pickers
are unaffected.

`Scrollback` regexes,
`FileSystem` options, and `Command` arguments have separate schemas: fields from another picker type
are rejected. Candidate sources support an `action` callback; CommandPalette
dispatches its built-in actions instead. `Scrollback` also supports
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

`Items` takes its candidates from a named pipe request's `params.items`, a JSON-encoded
array of `{ "text": "display label", "value": "text to insert" }`, or from an
`items = function() ... end` Lua provider. The provider runs each time the picker
opens, so it can read a file that changes during the session. Lua has
the standard `io` and `os` libraries and a `ghostty.json_parse(string)` helper.
It has no filesystem scan or terminal snapshot. Without an action callback,
Enter pastes the chosen value. Item lists are limited to 2,000 entries.

`Command` runs the configured `args` array without a shell and streams stdout
lines into the fuzzy picker. An argument consisting of `{pattern}` is replaced
with the named pipe request's `params.pattern`; other `{name}` arguments can use other
request parameters. The request's `params.cwd` overrides optional configured
`cwd`; without either, the command runs in the shell's working directory when
known (see below), otherwise the application's. The command stops when the picker closes. Without an `action` callback,
Enter pastes the selected line. The sample `Ripgrep` picker shows how to use
`rg --vimgrep` and open a result in Neovim.
For commands whose output uses continued lines, set `line_continuation` to the
line-ending marker (for example, `` "`" `` for PowerShell history). Continued
lines appear as one result joined with spaces; accepting it removes the markers
and restores newlines. An unfinished group is kept when the command exits.
Set `columns = { "Name", ... }` on a Command picker to show its output as a
structured table: each line is split on `delimiter` (default tab) into those
columns, with a header row, aligned cells and the same column queries as the
command palette (`/:Branch=~main`). Missing fields are empty and extra
delimiters stay in the last column. The accepted value, and `{item}` in
previews, is still the whole output line. `columns` cannot be combined with
`line_continuation` or `unique`.
Set `unique = true` on a Command picker to retain only the first occurrence
of each exact accepted value in the process output. Deduplication happens after
continuation lines are joined. The default is `false`.

#### Shell working directory

Pickers opened with a key binding run commands and previews in the shell's
current directory when the shell reports it with OSC 7. The application then
sets `params.cwd` to that directory, unless a pipe request supplied `cwd` or a
`Command` definition configures its own `cwd`. Factories also receive it as
`context.cwd`. Ghostty accepts `file://` URLs and `kitty-shell-cwd://` URLs
from the local host only. For bash, report it before each prompt:

```bash
__report_pwd() { printf '\e]7;kitty-shell-cwd://%s%s\a' "$HOSTNAME" "$PWD"; }
PROMPT_COMMAND="__report_pwd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
```

Without OSC 7, commands run in the application's working directory.

### Preview configuration

Native IME composition is disabled in this host to avoid blocking Windows
composition calls during typing. Normal keyboard input, clipboard paste, and
Unicode display remain supported.

Use a Dynamic preview when Lua should choose the provider and external tools:

```lua
preview = {
  type = "Dynamic",
  max_bytes = 10 * 1024 * 1024,
  resolve = function(item, params)
    local extension = (item.value:match("%.([^./\\]+)$") or ""):lower()
    if extension == "png" then
      return {
        type = "Command", output = "Image", cwd = params.cwd,
        args = { "ffmpeg.exe", "-loglevel", "error", "-i", item.value,
          "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "pipe:1" },
      }
    end
    local path = item.path or item.value
    -- Resolve relative paths against params.cwd as appropriate.
    local file = assert(io.open(path, "rb"))
    local contents = file:read(1024 * 1024) or ""
    file:close()
    return { type = "Text", text = contents, name = path,
      syntax = "Auto", theme = "gruvbox-dark" }
  end,
}
```

The Dynamic resolver runs on a pool of dedicated Lua threads once per selection, returning
owned data. Each thread has a separate Lua state that evaluates the same configuration
source immediately when configuration loading/reload creates the worker, retaining
captured helpers and settings. Initialization runs in the background without waiting
for the first preview request. Initialization errors are printed to stderr and
reported by subsequent preview requests. Keep configuration evaluation
free of side effects: it runs in the UI state and each preview state. Reload creates
new workers from the new source; existing previews retain their original configuration.
Factory preview workers initialize when the factory is resolved, once its runtime
picker context is available, and before receiving selection requests.
NFM handles rendering, VT parsing, image decoding, stale results, and repainting.
It receives `item.text`, `item.value`, and `params`; filesystem items also expose
`item.path`. Return `nil` for a blank preview. Command plans accept `args`,
`cwd`, `output` (`Ansi` by default, or `Text`/`Image`), optional `center_line`
for Ansi, and `success_exit_codes` (default `{0}`). Command working directories
fall back to `params.cwd`; arguments are passed literally, without implicit placeholders.
Returned plans cannot contain another Lua callback or Dynamic preview.

Within Dynamic resolvers, use Lua's `io.open(path, "rb")`, `file:read`, and
`file:close` to read file bytes. The sample bounds reads to 10 MiB and checks
for NUL bytes or invalid UTF-8 with `utf8.len` to classify binary content.
Lua samples the first 4 KiB before loading text; binary previews use `file:seek("end")`
to report the full size without reading the rest. A split UTF-8 character at the
sample boundary is completed before classification. Oversized text is truncated
within the 10 MiB byte budget, with a warning appended at the bottom and no split
UTF-8 character. Lua owns path resolution,
syntax choice, and binary metadata formatting.
Standard Lua has no directory iterator. The sample uses
`preview.directory_items(path)`, a Rust-backed helper available only inside a
Dynamic preview resolver. It returns entries sorted by name, each containing
`name` and `is_directory`, or `nil` for a regular file. Missing paths and IO errors
raise Lua errors. Hidden entries are included; symlinks are not followed when
classifying entries. Lua formats the listing, appending `/` for directories.
Enumeration checks cancellation between entries and bounds returned names by
the preview byte limit and 100,000 entries. It runs on the Lua preview pool,
without launching a subprocess. Native filesystem calls can still block one lane.
No Rust-backed `preview.read_file`, `read_lines`, or `is_directory` API is provided.

`preview.run {args={"ffmpeg.exe", ...}, cwd=params.cwd, timeout_ms=10000}` runs
literal arguments and returns `{stdout=bytes, stderr=bytes, exit_code=number}`.
Check `exit_code` before using output. Both streams are bounded; cancellation,
timeout, and excessive output raise a Lua error. The default timeout is five seconds,
with a maximum of two minutes.

Cancellation releases the Lua resolver while process creation or cleanup is
still pending. The background job retains responsibility for terminating and
reaping the direct child. At most four command jobs run concurrently across
preview workers; waiting for a slot also observes cancellation and timeout.
Each picker also has four independent Lua preview contexts, initialized on
load/reload. A blocking native Lua file read occupies one context while newer
requests can use another. Reads already in the OS finish naturally; canceled
results are discarded. If all four contexts are blocked, newer requests wait.
Resolver closures retain separate state in each context: do not rely on mutable
Lua state being shared between selection requests.
The file-preview sample probes files with `io.open` before attempting a directory
listing. Directory listing now uses the native helper without a command launch.

Return `{type="Ansi", bytes=result.stdout}` to preserve ANSI colors and text
copy/search after running a command in Lua. Optional `center_line` follows the
same coordinate convention as ANSI Command plans.
Return `{type="Image", bytes=result.stdout}` for
encoded image data; binary strings require no base64 encoding.

Selection changes cancel queued work, Lua instruction loops, and helper commands.
Ordinary blocking `io`/`os.execute` calls still run off the UI thread but cannot be
interrupted by the Lua hook. Use `preview.run` for commands; cancellation terminates
the direct child, so invoke FFmpeg directly rather than through a shell that spawns it.
File-system calls can also wait for the operating system before observing cancellation.
Preview state is separate from mutable globals used by UI actions.

Dynamic resolvers can also supply text directly, without a filesystem reader:

```lua
return {
  type = "Text",
  lines = { "fn main() {", "    println!(\"hello\");", "}" },
  syntax = "Rust", theme = "gruvbox-dark",
}
```

Text plans require exactly one of `text` (a string) or `lines` (a list). They also
accept `name` and `extension` as Auto syntax hints, `center_line`, and `highlight_line`.
For example, `{type="Text", text=contents, extension="cs", syntax="Auto"}`
selects C# from NFM's bundled definitions. A leading dot is optional. Filename-specific
rules take precedence over the extension; explicit syntax choices take precedence
over both. Unknown extensions fall back to filename/first-line detection, then plain text.
The sample checks the initial file bytes for binary content before selecting syntax,
returning binary metadata with `syntax="PlainText"` and no extension hint.
Lua supplies an owned snapshot. Rust hosts can supply a `TextSource` worker
reader instead. `nfm-preview-text` owns rendering and never opens files.
Ghostty's Lua API does not accept File preview plans; Lua resolvers supply
Text or Image data instead. NFM's filesystem adapter remains unused by Ghostty.

Dynamic limits inherit `picker_defaults.preview_max_bytes` and
`preview_max_scrollback_lines`. Images preserve their aspect ratio and have no
text copy mode; PNG, JPEG, GIF, BMP, WebP, and TIFF output is decoded with an
8192-pixel dimension limit and a 128 MiB RGBA-pixel limit. Paging/repainting reuse
the current result. Errors appear in the preview. Pipe clients must use an
allowlisted named picker for Dynamic previews; inline stream previews remain
restricted to Text/Ansi.

`ghostty.example.lua` demonstrates host-owned image/video extension routing and
configurable FFmpeg/FFprobe paths. SimpleWindowManager's Rust configuration uses
`file_preview_resolver`, a worker closure returning the same typed `PreviewPlan`;
its `file_preview::MediaPreviewSettings` helper owns media routing and video probes.

Every picker type accepts an optional preview. `preview = { type = "Text",
text = "{item}" }` displays literal text; `type = "Ansi"` interprets terminal
escape sequences; `preview = { type = "Command", args = { "bat", "--color=always",
"{item}" } }` runs a process and renders its output. `{item}` in preview text,
arguments, or optional `cwd` is replaced with the selected item's accepted value.
For literal `Text` previews, set `wrap = true` inside the preview object to wrap
at the pane width by character: `preview = { type = "Text", text = "{item}", wrap = true }`.
Omitting `wrap` or setting it to `false` clips long lines. Copying preserves the
original line breaks, including with wide and combining Unicode characters.
PowerShell uses the same option through
`Invoke-GhosttyPicker -Options @{ preview_visible = $true; preview = @{ type = 'Text'; text = 'A long line'; wrap = $true } }`.
This option applies to literal `Text` previews; ANSI, command, dynamic, and
scrollback previews retain their existing layout behavior.
Commands run without a shell and are cancelled when selection changes or the
picker closes. Command preview output is limited to 10 MiB and displayed in 12 terminal
rows. Ghostty's VT parser handles ANSI sequences in an isolated terminal; egui
draws its text, colors, and basic styles using the picker's font.
Instead of fixed `args`, use a `Dynamic` preview with `resolve = function(item, params)`
to return `{ type = "Command", output = "Ansi", args = {...}, cwd = "..." }`.
Return `nil` to leave that item's preview blank. `item.value` is the selected value;
filesystem items also provide `item.path`. The resolver runs on the Lua worker
when the selected item changes. Legacy `Command.command` and `File.file` callbacks
are rejected during configuration loading; all Lua preview callbacks use `Dynamic.resolve`.

Command preview limits can be configured globally:

Set `cursor_scroll_padding = 2` in the top-level Lua table to control the rows
kept above and below the cursor in terminal copy/search mode and picker previews.
The default is two rows. Zero disables the margin; short viewports reduce it
automatically. Configuration reload applies the value to existing panes and
picker previews.

```lua
picker_defaults = {
  preview_max_bytes = 10 * 1024 * 1024,
  preview_max_scrollback_lines = 8192,
},
```

Override either limit in a picker's preview table:

```lua
preview = {
  type = "Dynamic",
  max_bytes = 20 * 1024 * 1024,
  max_scrollback_lines = 16384,
  resolve = function(item, params)
    return { type = "Command", output = "Ansi",
      args = { "bat", "--color=always", "--paging=never", item.value } }
  end,
},
```

Both values must be positive integers. Omitted values inherit the picker defaults;
the built-in defaults are 10 MiB and 8192 physical scrollback lines. `max_bytes`
caps captured command output, not terminal grid memory. The parser's byte bound
follows it, allowing additional bytes for CRLF normalization. Scrollback pruning
occurs at native page boundaries and can remove early rows when the line limit
is reached; choose enough lines to retain the context used by `center_line`.

The returned command table may also include `center_line`, a zero-based line
number in the command output. The initial preview centers that line, clamping
at the beginning and end of the output. Ctrl+PageUp/PageDown then scrolls from
that position. If `bat` starts its output at file line `start`, use
`center_line = matched_line - start`.

For example:

```lua
preview = {
  type = "Dynamic",
  resolve = function(item, params)
    return { type = "Command", output = "Ansi", cwd = params.cwd,
      args = { "bat", "--color=always", "--paging=never", item.value } }
  end,
}
```

For a scrollback picker, `preview = { type = "Scrollback", rows = 12 }` instead
shows the frozen terminal rows around the selected match, retaining their
colors and text attributes and highlighting the match. The row count can be
1–12. The source loads text and style runs once; selection changes render only the
visible rows from the shared Rust document.
Set `preview_visible = false` on any picker definition to start with its
preview hidden. The default is `true`. While the picker is open, Ctrl+P
toggles the preview, and Ctrl+PageUp or Ctrl+PageDown scrolls it by half a page.

### Custom selection callbacks

A picker can define local Lua keybindings in `keys`. They take precedence over
the picker's default shortcuts and receive `(app, item, params)`, just like an
acceptance callback. The selected item is captured before the callback runs.
Callbacks leave the picker open unless they call `app:close_picker()`:

```lua
builtin_pickers = {
  lines = {
    keys = {
      { key = "Enter", mods = "CTRL", description = "Paste selected line", action = function(app, item)
        app:close_picker()
        app:paste(item.value)
      end },
    },
  },
}
```

Here Enter still navigates to the line, while Ctrl+Enter closes the picker and
pastes its text without submitting it. The optional `description` appears in
picker help; it defaults to "Run picker Lua action". Local bindings support the same key and
modifier names as application bindings, with a Lua function as `action`. They also
work in named picker definitions and tables returned by picker factories.
They run only with a current selection; repeated keydown events do not invoke
them again. Local callbacks are disabled for pipe-owned picker requests, which
return their selection to the requesting process.

Use `key = "DoubleClick"` to override double-click acceptance on a result row:

```lua
{ key = "DoubleClick", description = "Paste clicked line", action = function(app, item)
  app:close_picker()
  app:paste(item.value)
end },
```

The callback receives the clicked item and leaves the picker open until explicitly
closed. `DoubleClick` does not accept modifiers. On pickers that normally accept a
single click, installing this override makes a single click select only, allowing
the double-click callback to run. Without an override, mouse acceptance is unchanged.

Enter has a default implementation that closes the picker and runs its existing
`enter_action` or acceptance callback. Double-click has the same default behavior.
Overriding either trigger replaces that behavior with your Lua function, which
must call `app:close_picker()` if it should close. There is no configurable
`Accept` binding action.

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
`params` contains the named pipe request's string parameters, or an empty table for a
keybinding.
Scrollback items have `item.range.start` and `.end`, each with zero-based `x` and
`y` cells, and `item.snapshot_generation`.

`app:paste(text)` pastes without Enter. `app:send_text(text)` sends typed input,
with line breaks translated to Enter keys. `app:navigate(item)` enters copy mode
at a scrollback item's first cell. Paths are not automatically shell-quoted.
`app:replace_picker(name, params)` switches to a named picker from an acceptance
callback or picker-local key. `params` is an optional table of string values,
using the same request parameters as named pipe requests (including `query`,
`items`, and `roots`). The destination installs its own source, previews,
callbacks, and local keys; it replaces the current picker without a back stack.
For example, an acceptance callback or local key action can use:

```lua
action = function(app, item)
  app:replace_picker("Details", { selected = item.value, query = item.text })
end
```

No `app:close_picker()` is needed before replacement. Compatible controls retain
their displayed rows while new results load; changes to sorting or click
acceptance create a fresh control. Picker factories resolve normally, with
request parameters available to destination callbacks and preview resolvers.
`app:replace_prompt(text, before, after)` sends `after` Delete and `before`
Backspace key events, then pastes `text`; use the command-line cursor and length
from the shell's named pipe request to replace text on both sides of the cursor.
For PowerShell completions, the sample Lua callback uses `app:send_text` to send
Backspace bytes and Delete escape sequences, then `app:paste(item.value)` to
insert the completion without submitting the command.

Callbacks run on the UI thread after the picker stops, so keep them short.
They retain Lua closure state between invocations. Actions are queued and applied
in order only after a successful return; errors discard them and are printed to
stderr. App handles expire when the callback returns. Navigation requires an
active snapshot; perform it before any action that returns to the live terminal.

Windows uses `nfm_file_system` from [npicker](https://github.com/e82eric/npicker),
pinned to `b0a21d3f1d0c8f9e6c4f68f7dcf68eb126d1c1f7`. Filesystem pickers are currently Windows-only;
Linux support will use NFM when it becomes available. Filesystem scans do not
capture or read terminal snapshots, and cancellation leaves existing copy mode intact.

### Opening pickers from the shell over a named pipe

Shells open allowlisted named pickers through the authenticated local named pipe
(Windows) or Unix socket (Linux). On Linux, use `npick`, installed beside
`ghostty_egui`; see [npick on Linux](#npick-on-linux). On Windows, the supplied
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

`named` is an exact allowlist. `files` refers to the custom Lua picker above;
built-ins use their `builtin.` names. `allow_stream` permits candidates sent
directly from the shell.

#### Open a configured picker by name

```powershell
. C:\src\ghostty2\example\rust-egui-terminal\Invoke-GhosttyPicker.ps1

$path = Invoke-GhosttyPicker -Name files
$line = Invoke-GhosttyPicker -Name builtin.lines

# For a configured Command picker such as Ripgrep in the sample Lua file:
$match = Invoke-GhosttyPicker -Name Ripgrep -Parameters @{ pattern = 'TODO'; cwd = $PWD.Path }
```

Allowlist `Ripgrep` as well to run that example. Named requests use the source,
preview, and factory configured in Lua. String parameters are available to
callbacks as `params`; Command sources substitute whole `{name}` arguments from
them, and `params.cwd` overrides the command's configured directory. FileSystem
roots still come from Lua, independently of the shell's working directory.

Pipe acceptance returns the selected value to the caller. It bypasses terminal
insertion, navigation, and acceptance callbacks, including for `builtin.lines`.
The shell decides how to use the result.

#### Stream custom candidates from the shell

No named Lua picker is needed; enable `allow_stream` and dot-source the helper
as above. It returns the selected original object, or the property specified
with `-ValueProperty`:

```powershell
# Return the selected FileInfo object.
$file = Get-ChildItem -File | Invoke-GhosttyPicker -DisplayProperty Name

# Return its FullName string instead.
$path = Get-ChildItem -File |
    Invoke-GhosttyPicker -DisplayProperty Name -ValueProperty FullName

# Inspect status, query, and selection.
'first', 'second', 'third' | Invoke-GhosttyPicker -PassThruResult
```

Enter accepts; Escape cancels. Ctrl+C disconnects the client and cancels its
picker. Ordinary completion keeps the authenticated connection for the next
request. Use `Disconnect-GhosttyPicker` to release it before connecting from
another process in the same pane. Another active overlay can return `busy`;
requests are not queued. Returned paths are not automatically shell-quoted.

#### npick on Linux

With `picker_pipe` enabled as above, `npick` opens pickers from the shell
running in a pane and prints the selected value:

```sh
npick files                        # a named picker from ghostty.lua
git branch --format='%(refname:short)' | npick --query main
```

Without a name, each stdin line is a candidate. `--display N` and `--value N`
show and return tab-separated fields (`-d` sets the separator), and `--json`
reads `{"text": ..., "value": ...}` lines. It exits 0 with the value on stdout,
130 when cancelled, and 2 on errors. For example, bind Ctrl+T in bash to insert
a file:

```bash
__npick_file() {
  local file
  file=$(npick files) || return
  READLINE_LINE="${READLINE_LINE:0:READLINE_POINT}$file${READLINE_LINE:READLINE_POINT}"
  READLINE_POINT=$((READLINE_POINT + ${#file}))
}
[[ -n $GHOSTTY_PICKER_ENDPOINT ]] && bind -x '"\C-t": __npick_file'
```

[npick-completion.bash](npick-completion.bash) is the bash counterpart of the
PowerShell profile's completion picker. Source it from `~/.bashrc`
(`deploy.sh` installs it to `share/ghostty-egui/` beside the app). In
ghostty-egui panes, Ctrl+Space lists what Tab would complete for the word at
the cursor, using bash-completion when it is installed; Enter replaces that
word, quoting file names. A key binding cannot enable readline's unquoting, so
for words containing quotes, backslashes or `~` it falls back to bash's own
file, directory and `$variable` completion.

See [PICKER-PIPE.md](PICKER-PIPE.md) and
[Invoke-GhosttyPicker.ps1](Invoke-GhosttyPicker.ps1) for PSReadLine bindings,
stream options, protocol limits, authentication and cancellation.
For a portable PowerShell profile with file, directory, history, text-search,
and completion bindings, see [profile.example.ps1](profile.example.ps1).
Executable sources and command previews must be defined in allowlisted Lua
pickers; clients cannot submit executable definitions.

Keys accept egui names such as `f`, `Space`, and `ArrowUp`; modifiers are joined
with `|` (`CTRL`, `SHIFT`, `ALT`). Definitions and regexes are compiled
at startup; unknown action or picker names, invalid patterns, ambiguous targets,
and duplicate bindings report configuration errors. Picker bindings trigger from
terminal or copy mode, leaving modal query input unaffected.
Without `pickers`, all four named built-ins remain available.
Without `keys`, built-in action bindings are retained, Ctrl+Shift+F opens built-in
`lines`, Ctrl+Shift+P opens built-in `words`, and Ctrl+Shift+Space opens the
built-in command palette.
An explicit empty `keys = {}` disables top-level action and picker shortcuts.
Explicit `keys` replace the default bindings. There are no default custom
pickers; `picker = "lines"` requires a `lines` entry in `pickers`.

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

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> to open the scrollback fuzzy
picker from terminal or copy mode. Nonblank logical lines stream newest first
from a frozen snapshot into `nfm-search-core`. Type to filter; Ctrl+N/P, arrows,
and Page Up/Down move through results. Enter enters copy mode at the first
nonblank cell of the selected line. Escape returns to the originating mode.
Results fill upward from the query, with the first result at the bottom.
Arrow and page keys follow that visual direction; Ctrl+N moves to the next
result in ranking order. Ctrl+P toggles the preview when configured.
The sample uses `nfm_search_core` and the other NFM crates from
[npicker](https://github.com/e82eric/npicker), pinned to
`b0a21d3f1d0c8f9e6c4f68f7dcf68eb126d1c1f7` in Cargo.toml.

The text renderer and preview-plan provider come directly from the pinned NFM
revision; no local NFM checkout or copied preview crates are required.

The picker imports Ghostty's resolved primary regular and bold fonts and uses the current
terminal cell dimensions to size its text and rows, including zoom and DPI changes.
Matched positions use bold red text; selection uses a background highlight.
Unsupported non-SFNT font formats use egui's monospace fallback.

## Vim Copy Mode

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
mode and a yellow `NORMAL` label in copy mode. Visual selection changes the
label and top separator to orange and shows `VISUAL`, `V-LINE`, or `V-BLOCK`.
The right side shows the copy cursor's one-based scrollback row and column.
The status row never covers terminal content or changes size when switching modes.
Keyboard motions and selection commands control the saved output while this mode
is active. New shell output continues in the background and becomes visible when
you exit. Copying writes to the clipboard; it does not paste or run a command.

### A first copy

1. Press Ctrl+Shift+M. Check that the status row says `NORMAL`.
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
`nfm-egui/src/copy_mode.rs` for now, with Lua configuration planned for later.
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

## Shared byte preview ownership

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


Windows shells can invoke named Lua pickers, submit picker definitions, or stream
items through an opt-in authenticated named pipe and receive a selection response.
See [PICKER-PIPE.md](PICKER-PIPE.md) for the security policy, wire protocol, and
[Invoke-GhosttyPicker.ps1](Invoke-GhosttyPicker.ps1) for the PowerShell client.

Snapshot resize is coordinated by Rust. The pane retains a single frozen
terminal: live output stays independent, while that terminal reflows in place.
Before reflow, Rust drains search readers and closes unfinished scrollback captures.
The resize API returns an owned translation map built during Ghostty's existing
reflow pass. Sorted old-row/column segments and a row-offset table let Rust
translate all retained pane cursor and selection cells with local
indexed lookups. The map is released after translation. Zig also preserves the
viewport's top-cell anchor. No original pane layout or permanent pane row table
is retained. See [the reflow map design](ghostty/src/terminal/REFLOW_MAP.md).

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

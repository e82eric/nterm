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
Unknown fields
are rejected. Rust evaluates Lua once, validates the settings, and translates them
into Ghostty configuration arguments. Ghostty's normal configuration files remain
the base; Lua overrides them, and explicit command-line options override Lua.
The picker still imports Ghostty's resolved font and tracks its cell dimensions.
Use the command palette's Reload configuration action to apply supported Lua settings.

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
  { key = "w", mods = "CTRL|SHIFT", action = "CloseFloatingPane" },
  { key = "f", mods = "CTRL|SHIFT", action = "OpenSearch" },
  { key = "Space", mods = "CTRL|SHIFT", builtin_picker = "command_palette" },
},
```

Supported action names are `NewWindow`, `ReloadConfig`, `ShowMessages`,
`OpenErrorLog`, `OpenThemePicker`, `ToggleFloatingPane`, `CreateFloatingPane`,
`NextFloatingPane`, `PreviousFloatingPane`, `CloseFloatingPane`, `OpenSearch`,
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


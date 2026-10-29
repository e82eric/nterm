# Pickers

Pickers provide a fuzzy query, selectable results, and optional previews inside
the active terminal pane. Type to filter; arrows, Ctrl+N, and Page Up/Down
move through results. Enter accepts and Escape cancels. Ctrl+P or Ctrl+Space
toggles the preview, Ctrl+Page Up/Down scrolls it, Ctrl+W focuses its copy mode,
and Ctrl+Shift+/ opens picker keybinding help.

## Built-in pickers

The five named built-ins are always available. With the default keybindings,
use the shortcuts below; theme selection is also available in the command palette.

| Built-in name | Purpose and acceptance behavior | Default presentation / shortcut |
| --- | --- | --- |
| `lines` | Nonblank logical scrollback lines; Enter enters copy mode at the selected line. | Bottom, scrollback preview visible; Ctrl+Shift+F. |
| `words` | Unique whitespace-delimited scrollback words (`\S+`); Enter pastes the word without submitting it. | Bottom, preview hidden; no default shortcut ([nterm.lua](../nterm.lua) binds Ctrl+Shift+P, [nterm.example.lua](../nterm.example.lua) Ctrl+Shift+.). |
| `themes` | Installed user and bundled Ghostty themes; Enter applies the selected theme for the session. | Cursor, score sorting, preview hidden; choose **Choose theme** in the palette. |
| `command_palette` | Application actions and configured pickers; Enter dispatches the selected action. | Center, workspace scope, preview hidden; Ctrl+Shift+Space. |
| `prompts` | Commands entered at prompts, found by OSC 133 marks and [prompt patterns](configuration.md#prompt-patterns); Enter enters copy mode at the selected command. | Bottom, scrollback preview visible; no default shortcut ([nterm.example.lua](../nterm.example.lua) binds Ctrl+Shift+Z). |

The line-split picker is a contextual built-in: Ctrl+B opens it for the selected
scrollback result or copy mode cursor row. Its `line_break` settings control
presentation; it is not one of the five named `builtin_picker` binding targets.
See [Splitting a scrollback result](#splitting-a-scrollback-result) for its actions.
Error history and Messages are also available through the command palette;
[Messages and error history](configuration.md#messages-and-error-history) explains them.

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
    { key = "z", mods = "CTRL|SHIFT", builtin_picker = "prompts" },
}
```

Themes, command palette, lines, words, and prompts always exist, even when
omitted from the config. Their overrides accept the corresponding picker options
except `type`, `regex`, and `prompts`, which are built in. The source defaults are Themes,
CommandPalette, nonblank logical lines/Navigate, and unique `\S+` words/SendInput.
Lines preview starts visible; words preview starts hidden. The theme picker
defaults to Cursor position and the command palette to Center, both with
previews hidden. A built-in
override may also be a function of the same cursor context as custom picker
factories; its returned table is merged with defaults each time it opens.
For example, a words factory can seed `initial_query` and install a callback
to replace the word before the cursor.
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


## Scrollback picker

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> to open the scrollback fuzzy
picker from terminal or copy mode. Nonblank logical lines stream newest first
from a frozen snapshot into `nfm-search-core`. Type to filter; Ctrl+N, arrows,
and Page Up/Down move through results. Enter enters copy mode at the first
nonblank cell of the selected line. Escape returns to the originating mode.
Results fill upward from the query, with the first result at the bottom.
Arrow and page keys follow that visual direction; Ctrl+N moves to the next
result in ranking order. Ctrl+P toggles the preview when configured.

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
Prompt text is split like the rest of the row, so the working directory in a
prompt is a candidate. Soft-wrapped rows are split individually.

## Command palette

The command palette is a named picker with `type = "CommandPalette"`. The default
bindings and [nterm.lua](../nterm.lua) bind it to **Ctrl+Shift+Space**;
[nterm.example.lua](../nterm.example.lua) uses **Ctrl+Shift+P**. Type to filter actions,
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

## Custom pickers in Lua

Add definitions to the `pickers` table returned by your
[Lua configuration](../nterm.lua), then bind them with `picker = "name"`.
Merge examples into your existing tables rather than creating duplicate fields.
Providing `keys` replaces the complete shortcut list; retain any application
and built-in picker bindings you still want.

| Source type | Candidates | Acceptance |
| --- | --- | --- |
| `Scrollback` | Frozen logical lines, or matches extracted with `regex`. | `Navigate` enters copy mode; `SendInput` pastes text. |
| `FileSystem` | Paths under configured `roots`, with depth and file/directory filters. | Paste the raw path without quoting or Enter. |
| `Items` | An `items` Lua provider or JSON items from a named pipe request. | Paste the selected value by default. |
| `Command` | Lines streamed from an executable's stdout. | Paste the selected line by default. |
| `File` | Lines of a text file, optionally newest first. | Paste the selected line. |
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


## Dynamic Lua picker factories

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

## Lua modules

`require("name")` loads `name.lua` (or `name/init.lua`) from the config file's
directory, then from the modules installed with the app in
`share/nterm/lua`. Dots become directories: `require("nterm.git")`
loads `nterm/git.lua`. A module beside the config replaces a bundled one of
the same name. The current directory is never searched, and native (C) modules
cannot be loaded. The config is evaluated again in each preview worker, so
modules load there too.

The app bundles `nterm.git`, with commit and branch pickers that run git
directly and work in bash and PowerShell 7 on Linux and Windows:

```lua
local git = require("nterm.git")
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

## Picker sources and common options

Every picker accepts `scope = "Pane"` or `"Workspace"`. Pane scope is the default;
command palettes default to workspace scope. Workspace pickers draw across the
workspace content while their terminal context and result destination stay with
the originating pane. Cursor anchors remain pane-relative. Switching workspaces
or focusing another pane cancels a workspace picker. See the
[workspace guide](workspaces.md) for configuration examples.

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
'Example' | Invoke-NtermPicker -Options @{
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
the standard `io` and `os` libraries and a `nterm.json_parse(string)` helper.
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

`File` streams the lines of a text file without running a process. `path` is
a file path, or `{name}` for a request parameter such as `{history}`; a
relative path resolves against `params.cwd`. Set `reverse = true` to read from
the end so the newest lines appear first, which suits history and log files.
`line_continuation` and `unique` work as for `Command`: continued lines stay in
file order within their result in either direction, and with `reverse` the
`unique` copy kept is the newest. The file's length is read once when the
picker opens. File pickers only support `SendInput`.

```lua
ProfileHistory = {
  type = "File", position = "Cursor", sort = "Source",
  path = os.getenv("APPDATA") .. "/Microsoft/Windows/PowerShell/PSReadLine/ConsoleHost_history.txt",
  reverse = true, line_continuation = "`", unique = true,
},
```

### Shell working directory

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

## Preview configuration

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
listing. Directory listing uses a native helper rather than a command.

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

`nterm.example.lua` demonstrates host-owned image/video extension routing and
configurable FFmpeg/FFprobe paths.

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
`Invoke-NtermPicker -Options @{ preview_visible = $true; preview = @{ type = 'Text'; text = 'A long line'; wrap = $true } }`.
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
when the selected item changes. All Lua preview callbacks use `Dynamic.resolve`.

Set `cursor_scroll_padding = 2` in the top-level Lua table to control the rows
kept above and below the cursor in terminal copy/search mode and picker previews.
The default is two rows. Zero disables the margin; short viewports reduce it
automatically. Configuration reload applies the value to existing panes and
picker previews.

Command preview limits can be configured globally:

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

## Custom selection callbacks

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

Filesystem pickers use `nfm_file_system` from
[npicker](https://github.com/e82eric/npicker), at the revision pinned in
`Cargo.toml`, on Windows and Linux. Filesystem scans do not capture or read
terminal snapshots, and cancellation leaves existing copy mode intact.


## Codex suggestions

The samples define `CodexSuggestions`: [nterm.lua](../nterm.lua) binds it to
**Ctrl+Shift+K** and [nterm.example.lua](../nterm.example.lua) to **Ctrl+Shift+A**. It uses the last `#` in the text before the cursor to identify
editable input, sends that row and recent terminal context to `codex exec`, and
streams ten suggestions into a cursor-positioned picker. Enter replaces the input
before the cursor with the selected suggestion; it does not execute the command.
The model and CLI arguments are configured in the Lua factory.

[nterm.example.lua](../nterm.example.lua) also defines `ClaudeSuggestions`
(**Ctrl+Shift+L**), which sends the same prompt to Claude Haiku with
`claude --print --model claude-haiku-5-5 --tools ""`. Both pickers come from
its `command_suggestions(label, args)` helper, where `args` turns the prompt
into the command line to run.

Picker factories receive `context.recent_lines`: up to ten recent nonblank terminal
rows, in chronological order, read from at most the last 256 snapshot rows.
Pickers use the original seven-row layout. All picker definitions accept `sort`.
The default, `sort = "Score"`, ranks results by fuzzy match score. Use
`sort = "Source"` to preserve source order while filtering. `"SourceOrder"` is an alias for
`"Source"`. Source order means input order for Items and Command, catalog order
for CommandPalette, newest-first for Scrollback, and scan order for FileSystem.

## Command and preview errors

Search workers start on demand. A worker failure cancels the affected search;
retrying starts a replacement. Command and preview errors include command/cwd,
exit status, and bounded stderr. Command pickers accept exit 0 by default;
ripgrep also accepts exit 1 for no matches. A command picker can specify
`success_exit_codes = { 0, 1 }` to use another convention. Partial command
results remain visible with an error indicating that they may be incomplete.
Preview errors leave the picker usable; selecting another item retries naturally.


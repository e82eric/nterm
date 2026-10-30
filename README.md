# NTerm

NTerm is a terminal built on [Ghostty](https://ghostty.org) for Windows and
Linux. It has two goals: make moving around the scrollback and copying text out
of it as frictionless as possible, and be hackable enough that you can add or
remove your own quality-of-life workflows with a few lines of Lua.

## Features

### Scrollback navigation and copying

Copy mode, search, quick select, and the scrollback pickers all work on a frozen
snapshot of the full scrollback, so output stays still while you move through
it and the shell keeps running behind it. Each one can hand off to copy mode at
the place it found, so finding text and copying it is one continuous sequence of
keys.

- **Getting to the text.** Each entry point opens copy mode with the cursor
  already on the target:
  - Ctrl+Shift+M: at the terminal cursor.
  - Ctrl+Shift+Up / Down: at the previous or next command prompt, found by
    OSC 133 marks or [prompt patterns](docs/configuration.md#prompt-patterns). Inside copy mode
    the same keys move between commands, and a count such as `3` first skips
    several.
  - Ctrl+Up / Down: at the terminal cursor, scrolled up or down one row.
    Hold the key to keep scrolling, as with Ctrl+Y / Ctrl+E in copy mode.
  - Ctrl+Shift+/ then Enter: at the selected search match.
  - Ctrl+Shift+F then Enter: at the line chosen in a fuzzy picker over the
    scrollback.
  - Alt+Space then a hint: at the hinted word (see quick select below).

- **[Copy mode](docs/copy-mode.md):** Vim motions and yanks over the snapshot.
  - Motions: `hjkl`, `w`/`b`/`e`, `0`/`^`/`$`, `f`/`t` with `;` and `,`, `%`,
    `gg`/`G`, `H`/`M`/`L`, Ctrl+U/D, and counts such as `10j`.
  - Text objects: `yiw`, `yi(`, `ya"`, and the same for brackets, braces,
    quotes, and backticks. `yic` copies the output of the command under the
    cursor and `yac` includes its prompt, using the prompt boundaries.
  - Selections: `v` for characters, `V` for lines, and Ctrl+Q for a rectangular
    block, such as one column of a table. `y` copies and stays in copy mode.
  - Search inside copy mode: `/` and `?`, `n`/`N`, and `*`/`#` for the word
    under the cursor.
  - Ctrl+B opens a picker over the pieces of the current line and inserts the
    chosen piece at the prompt.

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/copy-mode.gif" width="800" alt="Captioned copy mode demo: hjkl and word motions, f and percent jumps, yanking inside brackets and quotes, visual, line, and block selections, searching, and pasting the last yank at the prompt">

- **[Search](docs/copy-mode.md#search):** Ctrl+Shift+/ searches the whole
  scrollback, literal by default or regex with Ctrl+R. Ctrl+N and Ctrl+P move
  between matches, and Enter continues in copy mode with the matches kept for
  `n` and `N`.

- **[Quick select](docs/copy-mode.md#quick-select):** Alt+Space labels every
  visible word with a short hint.
  - Typing a hint enters copy mode at that word.
  - Holding Shift on the hint's last letter pastes the word at the prompt
    instead, without entering copy mode, such as a commit hash from `git log`
    into `git show`.
  - In copy mode, Shift+Y shows the hints again and copies every line from the
    cursor through the hinted line.
  - Hints also work in picker previews.
  - [Custom quick selects](docs/copy-mode.md#custom-quick-selects) hint the
    matches of your own regexes instead of words, each on its own key, for
    text a workflow keeps reaching for, such as GDB or CDB memory addresses.
    A label can enter copy mode at the match, copy it, paste it, or call a
    Lua function with it, and a capture group narrows a match to just that
    group. `quick_select_defaults` sets the hint alphabet and can replace the
    words Alt+Space hints:

    ```lua
    quick_selects = {
      addresses = { patterns = { [[\b0x[0-9a-fA-F]+\b]] }, action = "Copy" },
    },
    keys = {
      { key = "x", mods = "ALT|SHIFT", quick_select = "addresses" },
    },
    ```

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/quick-select.gif" width="800" alt="Captioned quick select demo: hints over git log output, pasting a commit hash into git show with Shift on the last hint letter, jumping into copy mode at a word, and yanking lines through a hinted line with Shift+Y">

- **[Scrollback pickers](docs/pickers.md#built-in-pickers):** fuzzy find over
  everything in the scrollback, not only what is on screen.
  - **Lines** (Ctrl+Shift+F): each result previews the line in context, and
    Enter opens copy mode at that line, scrolled back to it.
  - **Words:** completes the word before the cursor from words anywhere in the
    scrollback, like Ctrl+N in Vim, and replaces it with the selection.
    `nterm.example.lua` binds it to Ctrl+Shift+.
  - **Line split** (Ctrl+B in a lines result or in copy mode): breaks a line
    into pieces, such as table fields, quoted or bracketed values, and paths,
    and inserts the chosen piece at the prompt.

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/scrollback-pickers.gif" width="800" alt="Captioned scrollback pickers demo: the lines picker finds a commit far up in git log output and opens copy mode scrolled back to it, the words picker completes src/ter to src/terminal/pane.rs, and Ctrl+B splits a build error line so workspaces.default_layout is inserted into an rg command that is then run">

- **Prompts:** nterm finds where each command starts in the scrollback from two
  sources: shell integration marks (OSC 133), when the shell prints them, and
  [prompt patterns](docs/configuration.md#prompt-patterns), regexes that match
  the prompt text. Both kinds of prompt work the same way:
  - Ctrl+Shift+Up / Down to jump between prompts, from the terminal or in copy
    mode.
  - The `ic` and `ac` text objects: `yic` / `vic` copy or select a command's
    output, and `yac` / `vac` include its prompt.
  - The built-in `prompts` picker, which [nterm.example.lua](nterm.example.lua)
    binds to Ctrl+Shift+Z. It lists the commands entered at every prompt with a
    scrollback preview, and Enter opens copy mode at the chosen one.

  Marks are exact, but only shells configured to print them send them. Patterns
  cover programs that don't: the defaults match PowerShell, CDB and WinDbg
  (`0:000>`), Codex (`›`), and Claude Code's input box. For others, such as GDB,
  set `prompt_patterns` to the defaults plus your own, like `[[^\(gdb\)\s*]]`;
  the list replaces the defaults. In practice the matches are accurate, but
  output that looks like a prompt can create a false boundary.

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/prompts.gif" width="800" alt="Captioned prompts demo: Ctrl+Shift+Up steps through prompts from a shell that sends OSC 133 marks and from a nested PowerShell matched by a pattern, yic copies a command's output, and the prompts picker lists the commands from both and opens copy mode at git status">

### Building a custom workflow in Lua

A workflow is a few entries in your [Lua configuration](docs/configuration.md):
an action or a picker that does the work, and a key binding that runs it.
Choose **Reload configuration** in the command palette to try a change without
restarting.

- **[Actions](docs/configuration.md#actions-and-toasts)** are Lua functions
  that receive an `app` handle. Through it they can read the selection, paste or
  send text to the shell, show a toast, and create, move, or close panes,
  floats, and workspaces.
- **[Key bindings](docs/configuration.md#key-bindings)** map a key to an action
  or a picker. Every action and picker also appears in the
  [command palette](docs/pickers.md#command-palette) (Ctrl+Shift+Space), with
  or without a key.
- **[Pickers](docs/pickers.md)** are a fuzzy finder with an optional preview,
  defined from these parts:
  - **[Sources](docs/pickers.md#picker-sources-and-common-options):** scrollback
    lines or regex matches, files and directories, lines of a file, a command's
    stdout as it streams, or items returned by a Lua function.
  - **[Previews](docs/pickers.md#preview-configuration):** text with syntax
    highlighting, a command's ANSI output, or an image. A Lua resolver can pick
    the preview per item. Previews support copy mode motions, search, text
    objects, and yanks.
  - **[Picker actions](docs/pickers.md#custom-selection-callbacks):** Enter and
    other keys can paste, enter copy mode at the item, replace text at the
    prompt, open another picker, or call a Lua function.
  - **[Factories](docs/pickers.md#dynamic-lua-picker-factories):** a Lua function
    that returns the picker definition when it opens, with the text before the
    cursor, the shell's working directory, and recent output as input.
- **[The shell pipe](docs/picker-pipe.md)** opens a picker from the command
  line. `Get-ChildItem | Invoke-NtermPicker` shows the piped objects and returns
  the selection to the pipeline. Piping into a named picker uses its Lua preview
  and actions. On Linux, use `npick`. The pipe is disabled until enabled in Lua.

#### Example: fzf-style shell pickers

[profile.example.ps1](profile.example.ps1) binds PSReadLine keys that open
named pickers from [nterm.example.lua](nterm.example.lua) through the
[shell pipe](docs/picker-pipe.md), then edit the command line with the result:

| Key | Picker | Result |
| --- | --- | --- |
| Ctrl+O | Directories under the current directory | Runs `Set-Location` to the chosen directory |
| Ctrl+T | Files under the current directory, with a preview | Inserts the quoted path at the cursor |
| Alt+R | PowerShell history, newest first, filtered by the current line | Replaces the line with the chosen command |
| Ctrl+Shift+G | `rg` matches for the current line, previewed at the matching line | Replaces the line with `nvim +LINE 'PATH'` |

Alt+O and Alt+T do the same as Ctrl+O and Ctrl+T over the roots passed to the
profile, and Ctrl+Space picks from PowerShell's tab completions, with
`Get-Help` as the preview. To use them as they are, start nterm with
`nterm.example.lua` and dot-source the profile from your `$PROFILE`:
`. C:\path\to\nterm\profile.example.ps1`.

<img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/fzf.gif" width="800" alt="Captioned fzf-style pickers demo: Ctrl+O changes to src\terminal, Ctrl+T inserts the path of workspace.rs after showing its preview, Alt+R finds cargo test in the history, and Ctrl+Shift+G ripgreps for default_layout, previews a match, and opens it in nvim at that line">

Each binding has two halves. In Lua, a named picker and the pipe allowlist:

```lua
return {
  picker_pipe = {
    enabled = true,
    named = { "ProfileDirectories" },
  },
  pickers = {
    ProfileDirectories = {
      type = "FileSystem", roots = { "." }, directories_only = true,
      max_depth = 0, position = "Cursor",
    },
  },
}
```

In the PowerShell profile, a key handler that opens it by name and uses the
result:

```powershell
. C:\path\to\nterm\Invoke-NtermPicker.ps1

Set-PSReadLineKeyHandler -Chord Ctrl+o -ScriptBlock {
    $result = Invoke-NtermPicker -Name ProfileDirectories -Parameters @{
        roots = (ConvertTo-Json -InputObject @($PWD.Path) -Compress)
        cwd = $PWD.Path
    }
    if ($null -ne $result) {
        $quoted = "'" + ([string]$result).Replace("'", "''") + "'"
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert("Set-Location -LiteralPath $quoted")
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }
}
```

The other bindings follow the same pattern: the ripgrep picker is a Command
source that runs `rg` with the typed text, and the history picker is a File
source over PSReadLine's history file. On Linux, `npick` opens named pickers
from bash; see [Opening pickers from the shell](docs/picker-pipe.md).

#### Example: snippets

[nterm.example.lua](nterm.example.lua) adds command snippets with one action
and one picker. In copy mode, select a command and press Ctrl+Shift+S: the
`SaveSnippet` action appends the selection to a JSON file. Later, type part of
a command and press Ctrl+Shift+B: the `Snippets` picker lists the saved
snippets, filtered by the word at the cursor, and Enter replaces that word with
the selected snippet.

<img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/snippets.gif" width="800" alt="Captioned snippets demo: a git log command is selected in copy mode and saved with Ctrl+Shift+S, then typing git and pressing Ctrl+Shift+B opens the snippets picker, filtering by search selects the saved command, and Enter puts it at the prompt and runs it">

A trimmed version, without the sample's error handling:

```lua
local snippets_path = os.getenv("USERPROFILE") .. "/ghostty-snippets.json"

local function load_snippets()
  local file = assert(io.open(snippets_path, "rb"))
  local data = nterm.json_parse(file:read("*a"))
  file:close()
  return data
end

return {
  actions = {
    SaveSnippet = {
      label = "Save selection as snippet",
      action = function(app)
        local selection = app:get_selection_text()
        local data = load_snippets()
        table.insert(data.snippets, { input = selection })
        local file = assert(io.open(snippets_path, "wb"))
        file:write(nterm.json_encode_pretty(data) .. "\n")
        file:close()
        app:show_toast("Snippet saved", selection, 2000)
      end,
    },
  },
  pickers = {
    -- A factory: runs when the picker opens, with the text before the cursor.
    Snippets = function(context)
      local word = context.line_before_cursor:match("(%S+)$") or ""
      return {
        type = "Items",
        initial_query = word,
        position = "Cursor",
        items = function()
          local items = {}
          for _, snippet in ipairs(load_snippets().snippets) do
            items[#items + 1] = { text = snippet.input, value = snippet.input }
          end
          return items
        end,
        -- Replace the typed word with the snippet, without running it.
        action = function(app, item)
          app:replace_prompt(item.value, utf8.len(word), 0)
        end,
      }
    end,
  },
  keys = {
    { key = "s", mods = "CTRL|SHIFT", action = "SaveSnippet" },
    { key = "b", mods = "CTRL|SHIFT", picker = "Snippets" },
  },
}
```

The snippets file must exist first; start it as `{"snippets": []}`.

#### Example: command suggestions from Claude Code

[nterm.example.lua](nterm.example.lua) has a `ClaudeSuggestions` picker that
asks Claude Haiku for commands. At the prompt, type `#` and describe the command
or start writing it, then press Ctrl+Shift+L. The picker sends the text after
the `#` and recent terminal output to `claude --print`, which streams ten
suggestions into the picker as they arrive. Enter replaces the `#` and your text
with the selected command; it does not run it.

<img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/claude-suggestions.gif" width="800" alt="Captioned Claude suggestions demo: after some directory output, typing #find the 5 largest rust files under src and pressing Ctrl+Shift+L fills a picker with ten PowerShell commands from Claude Haiku, Enter replaces the # text with the selected command, and a second Enter runs it and lists the five largest files">

The picker is a factory, so it builds the prompt and the command line each time
it opens:

```lua
pickers = {
  ClaudeSuggestions = function(context)
    -- The editable input starts after the last # before the cursor.
    local row = context.line_before_cursor
    local input_start = assert(row:match(".*#()"), "type # before the request")
    local erase_count = utf8.len(row:sub(input_start)) + 1
    local prompt = table.concat({
      "Generate exactly 10 likely command completions or alternatives.",
      "# marks the beginning of editable input in the current cursor row.",
      "Return one complete command per line, without the prompt or the #.",
      "Do not number the commands or use Markdown.",
      "",
      "Current cursor row:", row,
      "",
      "Terminal context:", table.concat(context.recent_lines, "\n"),
    }, "\n")
    return {
      type = "Command",
      args = {
        "claude", "--print", "--model", "claude-haiku-5-5",
        "--output-format", "text", "--tools", "",
        "--no-session-persistence", prompt,
      },
      position = "Cursor",
      sort = "Source",
      action = function(app, item)
        app:replace_prompt(item.value, erase_count, 0)
      end,
    }
  end,
},
keys = {
  { key = "l", mods = "CTRL|SHIFT", picker = "ClaudeSuggestions" },
},
```

`context.recent_lines` holds up to ten recent nonblank rows, and the Command
source shows each line of `claude`'s output as it arrives. The sample builds
`CodexSuggestions` (Ctrl+Shift+A) with the same function and `codex exec` in
place of `claude`.

#### Included workflows

These ship with nterm or its samples and are built from the same parts:

- **Lines and words:** Scrollback sources. Lines enters copy mode at the
  selected line; the built-in words picker pastes the selected word, and
  `nterm.example.lua` overrides it to replace the word before the cursor.
  Ctrl+B splits a result into pieces.
- **[Git](docs/pickers.md#lua-modules)** (`require("nterm.git")`): commits,
  branches, status, stashes, reflog, tags, and file history, as git Command
  sources with Lua actions such as `git switch`.
- **Themes, error log, and command palette.**

<img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/pickers.gif" width="800" alt="Captioned pickers demo: completing a word from the scrollback, changing directory and inserting a file with a preview, searching PowerShell history, piping files into Invoke-NtermPicker, opening the git commits picker from the command palette, finding a scrollback line and splitting it, and ripgrepping it with a copy mode preview before opening the match in nvim">

### Tiling windows, panes, and workspaces

- **[Windows, panes, workspaces, and layouts](docs/workspaces.md):** Instead of
  splits and tabs, nterm arranges terminals like a tiling window manager.
  - A **window** is an independent nterm window. Press Ctrl+Shift+N to open
    another in the same process, sharing configuration, fonts, and theme
    settings. Any tiled or floating pane can also move into a new window
    without restarting its shell.
  - A **pane** is one terminal. Panes are tiled automatically, or can float
    above the tiled panes.
  - A **workspace** is a numbered set of panes with its own focus and layout.
    Switching workspaces keeps every terminal running, and the clickable
    workspace bar shows them all.
  - A **layout** decides how a workspace's panes fill the window. Switching
    layouts rearranges the panes without restarting them:
    - **Tile:** one main pane on the left and the rest stacked on the right.
    - **Tile reversed:** tile with the main pane on the right.
    - **Monocle:** the focused pane fills the window.
    - **Deck:** the main pane beside one stack pane, cycling through the rest.
    - **Grid:** two columns, with every pane visible.
    - **Horizontal grid:** two rows, with every pane visible.
  - **[Floating panes](docs/workspaces.md#floating-panes)** open above a workspace's
    tiled panes, a convenient place for scratch work without disturbing the
    layout. Each workspace keeps its own floats: create several, name them,
    cycle between them, and hide one to get back to the tiled panes. Hidden
    floats keep running, and the command palette can restore any of them.
  - **[Lua automation](docs/workspaces.md#lua-automation-and-appearance):**
    a `startup` function runs when each window opens, and actions can run the
    same code from a key or the command palette. Through `app` they create
    workspaces and panes with a command and working directory, set layouts and
    the main pane's share, and move, float, or close panes. This one opens a
    project with an editor as the main pane and a build and a shell beside it:

    ```lua
    local function open_project(app, dir)
      local ws = app:create_workspace { layout = "tile" }
      local first = ws.focused -- a new workspace starts with one default shell
      app:create_pane { workspace = ws.id, cwd = dir, command = "nvim README.md" }
      app:create_pane { workspace = ws.id, cwd = dir, command = "pwsh -NoExit -Command cargo build" }
      app:create_pane { workspace = ws.id, cwd = dir, command = "pwsh -NoExit -Command git status" }
      app:close_pane(first)
      app:set_main_ratio(ws.id, 0.6)
      app:select_workspace(ws.id)
    end

    return {
      actions = {
        OpenNterm = {
          label = "Open the nterm project",
          action = function(app) open_project(app, "C:/src/nterm") end,
        },
      },
      keys = {
        { key = "o", mods = "CTRL|ALT|SHIFT", action = "OpenNterm" },
      },
      -- Runs once when each window opens; reloading does not rerun it.
      startup = function(app) open_project(app, "C:/src/nterm") end,
    }
    ```

    These entries are added to the default bindings; see
    [Key bindings](docs/configuration.md#key-bindings). [workspaces.example.lua](workspaces.example.lua) has more,
    including gaps, borders, and shadows.

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/tour.gif" width="800" alt="Captioned tour: panes tile as they are added and closed, the workspace switches through each layout, a floating nvim pane takes scratch notes and is hidden and restored, a second workspace is created with its own layout, and the scratch pane is tiled, floated again, and moved to a workspace of its own from the command palette">

## Getting started

### Requirements

| Tool | Version | Used for |
| --- | --- | --- |
| [Git](https://git-scm.com) | any | Cloning with submodules; on Windows, Git for Windows also supplies `tic` for the terminfo database |
| [Zig](https://ziglang.org/download/) | 0.17.0 | Building the Ghostty libraries from the submodule |
| [Rust](https://rustup.rs) | stable, 1.85 or newer (Rust 2024 edition) | Building nterm with Cargo |
| C compiler | MSVC Build Tools on Windows; gcc or clang on Linux | The C shim and the bundled Lua, compiled through Cargo |
| [.NET SDK](https://dotnet.microsoft.com/download) | any current SDK, Windows only | Verifying the signature of the pinned ConPTY package |

Running nterm needs a GPU with Vulkan drivers (or another
[wgpu backend](#rendering)). The sample configurations also use
[PowerShell 7.4+](https://github.com/PowerShell/PowerShell),
[ripgrep](https://github.com/BurntSushi/ripgrep),
[bat](https://github.com/sharkdp/bat), [ffmpeg](https://ffmpeg.org), and
[Neovim](https://neovim.io) for some pickers and previews, and the Claude
Code or Codex CLI for command suggestions; nterm itself does not need them.

### Building

Ghostty is built from the `ghostty` submodule, a fork that adds the embedding
APIs nterm uses. Clone with `--recursive`, or run `git submodule update --init`
in an existing clone:

```sh
git clone --recursive https://github.com/e82eric/nterm.git
```

On Windows:

```powershell
.\build.ps1
.\out\bin\nterm.exe --command=pwsh.exe
```

On Linux:

```sh
./build.sh
./out/bin/nterm --command="$SHELL"
```

`deploy.ps1` and `deploy.sh` install a build elsewhere; see
[Building and installing](docs/building.md).

## Configuration

nterm reads a Lua file from the first of these that exists:

1. `--lua-config=PATH`
2. `nterm.lua` in your home directory
3. `nterm.lua` beside the executable

Start from [nterm.lua](nterm.lua), or from [nterm.example.lua](nterm.example.lua)
for the extended sample with custom pickers, previews, snippets, and Claude and
Codex suggestions. [profile.example.ps1](profile.example.ps1) adds the PowerShell
picker bindings shown in the pickers clip. Choose **Reload configuration** in
the command palette to apply changes without restarting. See
[Configuration](docs/configuration.md) for every setting.

## Default key bindings

| Keys | Action |
| --- | --- |
| Ctrl+Shift+Space | Command palette |
| Ctrl+Shift+N | New window |
| Ctrl+Shift+M | Copy mode |
| Ctrl+Shift+Up / Down | Copy mode at the previous / next command prompt |
| Ctrl+Shift+/ | Search scrollback |
| Ctrl+Shift+F | Fuzzy find a scrollback line |
| Alt+Space | Quick select |
| Ctrl+Shift+E | Show or hide the floating pane |
| Ctrl+Alt+Shift+E | New floating pane |
| Ctrl+Alt+] / [ | Next / previous floating pane |
| Ctrl+Shift+W | Close the floating pane |

Workspace and layout keys are listed in [Workspaces](docs/workspaces.md#default-bindings).
A `keys` table in your Lua configuration replaces these defaults, so the samples
bind some actions differently: `nterm.example.lua` opens the palette with
Ctrl+Shift+P and the words picker with Ctrl+Shift+. instead. Every action is
also in the command palette, whether or not it has a key.

## Rendering

nterm embeds Ghostty's terminal emulation in a Rust and egui shell and renders
with wgpu, using Vulkan by default on both Windows and Linux. Ghostty and egui
share one GPU device, so finished terminal frames are textures that egui draws
directly, without a copy. Set `WGPU_BACKEND` (for example `dx12`) to choose
another backend. Without a hardware adapter, as in a virtual machine, it falls
back to software rendering if a software adapter is available (such as WARP
with `dx12`) and reports that as a warning.

## Documentation

- [Workspaces](docs/workspaces.md): windows, panes, layouts, floating panes, and their Lua API
- [Copy mode, search, and quick select](docs/copy-mode.md)
- [Pickers](docs/pickers.md): built-in and custom pickers, sources, previews, and the command palette
- [Opening pickers from the shell](docs/picker-pipe.md): `Invoke-NtermPicker`, `npick`, and the pipe protocol
- [Configuration](docs/configuration.md): settings, colors, themes, key bindings, actions, and toasts
- [UI colors](docs/ui-colors.md)
- [Building and installing](docs/building.md)
- [Architecture](docs/architecture.md): how nterm is put together

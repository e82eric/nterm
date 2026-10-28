# NTerm

NTerm is a terminal built on [Ghostty](https://ghostty.org) that runs on
Windows and Linux. It embeds Ghostty's terminal emulation in a Rust and egui
shell.

It renders with wgpu, using Vulkan by default on both Windows and Linux.
Ghostty and egui share one GPU device, so finished terminal frames are
textures that egui draws directly, without a copy. Set `WGPU_BACKEND` (for
example `dx12`) to choose another backend. Without a hardware adapter, as in a
virtual machine, it falls back to software rendering if a software adapter is
available (such as WARP with `dx12`) and reports that as a warning.

## Features

- **[Windows, panes, workspaces, and layouts](docs/workspaces.md):** Instead of
  splits and tabs, nterm arranges terminals like a tiling window manager.
  - A **window** is an independent nterm window. Press Ctrl+Shift+N to open
    another in the same process, sharing configuration, fonts, and theme
    settings.
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

  Workspaces and layouts can be scripted from Lua. See the
  [workspace guide](docs/workspaces.md) and
  [example Lua configuration](workspaces.example.lua).

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/tour.gif" width="800" alt="Captioned tour: panes tile as they are added and closed, the workspace switches through each layout, a floating nvim pane takes scratch notes and is hidden and restored, a second workspace is created with its own layout, and the scratch pane is tiled, floated again, and moved to a workspace of its own from the command palette">

- **[Vim Copy Mode](docs/copy-mode.md):** Press Ctrl+Shift+M to browse a frozen copy of
  terminal output with Vim-style motions, search, text objects such as `yi(` and
  `ya"`, and character, line, or block selections. Press `v`, move to select
  text, then press `y` to copy it to the clipboard. Escape clears an active
  selection or search before exiting.

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/copy-mode.gif" width="800" alt="Captioned copy mode demo: hjkl and word motions, f and percent jumps, yanking inside brackets and quotes, visual, line, and block selections, searching, and pasting the last yank at the prompt">
- **Search:** Search scrollback with literal or regex queries, or filter lines
  in a fuzzy picker.
- **Quick select:** A really convenient way to jump to any point on screen, or
  to send the text there to the terminal. Press Alt+Space and every visible word
  gets a keyboard hint. Type a hint to jump into copy mode at that word, or hold
  Shift on its last letter to paste the word at the prompt, such as a commit
  hash from `git log` into `git show`. In copy mode, Shift+Y shows the same hints
  and yanks every line from the cursor through the hinted line.

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/quick-select.gif" width="800" alt="Captioned quick select demo: hints over git log output, pasting a commit hash into git show with Shift on the last hint letter, jumping into copy mode at a word, and yanking lines through a hinted line with Shift+Y">
- **[Pickers](docs/pickers.md):** Fuzzy find anything, with a preview. Pickers are
  hackable: sources, previews, and actions are all Lua. nterm also ships with
  plenty of built-in pickers and sources, built from those same pieces.
  - **Scrollback:** fuzzy find a line and jump to it, or complete the word at the
    cursor from anything on screen. Ctrl+B splits a result so you can grab one
    piece of a line.
  - **fzf-style workflows** from [profile.example.ps1](profile.example.ps1):
    change directory, insert a file path, search PowerShell history, or ripgrep
    and open the match in nvim.
  - **Previews** support all of copy mode: motions, search, text objects, and
    yanks.
  - **[The command palette](docs/pickers.md#command-palette)** finds application actions and
    pickers from one searchable menu. It is a picker too, and custom pickers
    like the [Codex suggestions](docs/pickers.md#codex-suggestions) build on the same
    infrastructure.
  - **Ad-hoc pickers:** pipe anything from the shell, as in
    `Get-ChildItem | Invoke-NtermPicker`, and the chosen object comes back down
    the pipeline.
  - **Samples** cover git (commits, branches, status, stashes, and more), themes,
    error logs, and more.

  <img src="https://raw.githubusercontent.com/e82eric/nterm-demo/main/pickers.gif" width="800" alt="Captioned pickers demo: completing a word from the scrollback, changing directory and inserting a file with a preview, searching PowerShell history, piping files into Invoke-NtermPicker, opening the git commits picker from the command palette, finding a scrollback line and splitting it, and ripgrepping it with a copy mode preview before opening the match in nvim">
- **[Lua customization](docs/configuration.md):** Configure keybindings,

## Getting started

Ghostty is built from the `ghostty` submodule, a fork that adds the embedding
APIs nterm uses. Clone with `--recursive`, or run `git submodule update --init`
in an existing clone. Building needs Zig and Cargo, plus the .NET SDK on
Windows.

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
for the extended sample with custom pickers, previews, snippets, and Codex
suggestions. [profile.example.ps1](profile.example.ps1) adds the PowerShell
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

## Documentation

- [Workspaces](docs/workspaces.md): windows, panes, layouts, floating panes, and their Lua API
- [Copy mode, search, and quick select](docs/copy-mode.md)
- [Pickers](docs/pickers.md): built-in and custom pickers, sources, previews, and the command palette
- [Opening pickers from the shell](docs/picker-pipe.md): `Invoke-NtermPicker`, `npick`, and the pipe protocol
- [Configuration](docs/configuration.md): settings, colors, themes, key bindings, actions, and toasts
- [UI colors](docs/ui-colors.md)
- [Building and installing](docs/building.md)
- [Architecture](docs/architecture.md): how nterm is put together

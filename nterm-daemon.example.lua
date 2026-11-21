-- The pickers of this machine, for the session daemon: an example
-- ~/nterm-daemon.lua for a Windows machine with PowerShell. They run where
-- the pane's shell is, and a window draws the rows. nterm.example.lua opens
-- them as { type = "Daemon", picker = "<name>" }; a shell opens them with
-- `npick --daemon <name>` or `Invoke-NtermPicker -DaemonPicker <name>`. An
-- `action` here is run here, and what it asks of `app` is done to the pane;
-- without one, the window's is.
--
-- The daemon reads the first there is of: the file `--lua-config` names,
-- ~/nterm-daemon.lua, nterm-daemon.lua beside the daemon program, and the
-- default installed with it (share/nterm/lua/nterm-daemon.lua). It is read
-- when a picker of it is first asked for, and again when it has changed:
-- there is nothing to restart. A file of your own is in place of the
-- defaults (lua/nterm/daemon.lua), not added to them.
--
-- On another machine, copy this file to its user's home and change the
-- roots below to that machine's folders.
--
-- A program is only ever run because this file names it. What a picker is
-- opened with fills the arguments written as "{name}", each as one argument
-- whole, and is never which program is run.
local completions = require("nterm.completions")
local files = require("nterm.files")
local git = require("nterm.git")
local history = require("nterm.history")
local ripgrep = require("nterm.ripgrep")

-- The PowerShell prompt, so a git picker's Enter can replace what is typed.
local git_prompt = "^PS [^>]*> "

return {
  pickers = {
    -- Files and folders: under the drive, under the shell's folder, or
    -- under the roots they are opened with. Change the roots to your own.
    Files = files.all { roots = { "C:\\" }, position = "Cursor", preview = files.preview() },
    FilesHere = files.all { position = "Cursor", preview = files.preview() },
    -- Folders: under the shell's, or under your source folders.
    Directories = files.directories { position = "Cursor" },
    SourceDirectories = files.directories { roots = { "C:\\src" }, position = "Cursor" },

    -- Grep's matches: the lines a pattern is found on, under the folder the
    -- shell is in. Opened with `pattern`.
    Ripgrep = ripgrep.search {},

    -- What was typed at PowerShell, the last first, the whole command as
    -- its preview (Ctrl+P shows it).
    History = history.powershell { position = "Cursor", preview_visible = false, preview = history.preview() },

    -- PowerShell's completions, piped in by the profile: Get-Help as the
    -- preview, pwsh started for the row the cursor is on, or PowerShell's
    -- tooltip, with no program run.
    CompletionsHelp = completions.powershell { position = "Cursor", preview = completions.help(), preview_visible = false },
    Completions = completions.powershell { position = "Cursor" },

    -- git, in the repository the shell is in. Each Enter is the module's,
    -- run here; cherry-pick and file history open their second step here.
    ["Git commits"] = git.commits(),
    ["Git branches"] = git.branches { prompt = git_prompt, require_clean = true },
    ["Git status"] = git.status { prompt = git_prompt },
    ["Git cherry-pick"] = git.cherry_pick { prompt = git_prompt },
    ["Git cherry-pick commits"] = git.cherry_pick_commits { prompt = git_prompt },
    ["Git stashes"] = git.stashes { prompt = git_prompt },
    ["Git reflog"] = git.reflog(),
    ["Git file history"] = git.file_history { prompt = git_prompt },
    ["Git file history commits"] = git.file_history_commits(),
    ["Git fixup"] = git.fixups { prompt = git_prompt },
    ["Git tags"] = git.tags { prompt = git_prompt },
  },
}

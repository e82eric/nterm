-- The pickers of this machine, for the session daemon: an example
-- ~/nterm-daemon.lua that starts from the defaults and changes them.
--
-- The daemon reads the first there is of: the file `--lua-config` names,
-- ~/nterm-daemon.lua, nterm-daemon.lua beside the daemon program, and the
-- default installed with it (share/nterm/lua/nterm-daemon.lua). It is read
-- when a picker of it is first asked for, and again when it has changed:
-- there is nothing to restart.
--
-- The defaults are those of nterm.daemon (lua/nterm/daemon.lua): Files,
-- Directories, Near, Ripgrep, Commits and the "Git ..." pickers on every
-- machine, and History and Completions of PowerShell on Windows. They work
-- the same on Windows and Linux, so one file can go on every machine: the
-- daemon is on the machine the shell is on, and what they type at the
-- prompt is PowerShell on Windows and bash on Linux.
--
-- A picker here is written as one in nterm.lua is, and runs where the
-- daemon is: its program is run on this machine and its files are this
-- machine's. A window draws the rows, on this machine or another.
--
-- Open one by its name:
--   from a window, in nterm.lua:   Commits = { type = "Daemon", picker = "Commits" }
--   from a shell:                  npick --daemon Commits
--                                  Invoke-NtermPicker -DaemonPicker Commits
--
-- A program is only ever run because this file names it. What a picker is
-- opened with fills the arguments written as "{name}", each as one argument
-- whole, and is never which program is run.
--
-- So far: types Command, File, Items and FileSystem; previews of type
-- Dynamic, whose function runs here; and `action`, run here when a window
-- opened the picker, and `keys`, what they ask of `app` being done to the
-- pane. A picker may be a function of what is at the shell's cursor, a
-- Command may have columns, and modules load: see lua/nterm/git.lua.
-- The name items is the daemon's own: what a program in the shell gives.
local daemon = require("nterm.daemon")

-- The defaults, as a table to change. `prompt` is a Lua pattern for your
-- shell's prompt, which lets the git pickers clear what was typed before
-- typing theirs; `editor` is what Ripgrep opens a match in. Both have
-- defaults that suit PowerShell's and bash's own prompts, and nvim.
local config = daemon.defaults {
  -- prompt = daemon.windows and "^[^>]*> ?" or "^[^$#]*[$#] ",
  -- editor = "nvim",
}
local pickers = config.pickers

-- The folders under your source folders, five deep; Enter in a window goes
-- to the one chosen. daemon.windows says which machine this is.
pickers.Sources = daemon.directories {
  roots = daemon.windows and { "C:\\src" } or { daemon.home .. "/src" },
  max_depth = 5,
}

-- The commits of the repository the shell is in, in place of the default.
pickers.Commits = {
  type = "Command", sort = "SourceOrder",
  args = { "git", "log", "--oneline", "--decorate", "-n", "500" },
  -- What shows for a commit: the daemon runs the program this gives.
  preview = { type = "Dynamic", resolve = function(item, params)
    local hash = item.value:match("^(%x+)")
    if not hash then return nil end
    return { type = "Command", output = "Ansi", cwd = params.cwd,
             args = { "git", "show", "--stat", "--patch", "--color=always", hash } }
  end },
  preview_visible = false,
  -- Keys of its own. One leaves the picker open unless its function
  -- closes it.
  keys = {
    { key = "y", mods = "CTRL", description = "Paste the commit's hash",
      action = function(app, item)
        app:paste(item.value:match("^(%x+)") or "")
        app:close_picker()
      end },
  },
}

-- Rows written here; Enter in a window types the value and runs it.
pickers.Servers = {
  type = "Items",
  items = {
    { text = "build   the build machine", value = "ssh build" },
    { text = "db      the database", value = "ssh db" },
  },
  action = function(app, item) app:send_text(item.value .. "\r") end,
}

return config

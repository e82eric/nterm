-- The pickers the session daemon has on a machine with no nterm-daemon.lua
-- of its own, and the parts to make one's own from. The same file serves
-- Windows and Linux: the daemon runs on the machine the shell is on, so what
-- these look under and what they type is that machine's, PowerShell on
-- Windows and bash on Linux.
--
--   -- ~/nterm-daemon.lua: the defaults, changed and added to
--   local daemon = require("nterm.daemon")
--   local config = daemon.defaults()
--   config.pickers.Sources = daemon.directories {
--     roots = daemon.windows and { "C:\\src" } or { daemon.home .. "/src" },
--     max_depth = 5,
--   }
--   return config
--
-- A window opens one as { type = "Daemon", picker = "Files" } in nterm.lua,
-- and a shell with `npick --daemon Files` or
-- `Invoke-NtermPicker -DaemonPicker Files`.
local completions = require("nterm.completions")
local files = require("nterm.files")
local git = require("nterm.git")
local history = require("nterm.history")
local ripgrep = require("nterm.ripgrep")

local M = {}

-- Whether this machine is Windows, and the account's own folder.
M.windows = package.config:sub(1, 1) == "\\"
M.home = os.getenv(M.windows and "USERPROFILE" or "HOME")

-- How text typed at the prompt is quoted: for PowerShell, or for bash.
M.quote = M.windows and git.quote.powershell or git.quote.posix

-- A Lua pattern for the shell's prompt, which lets the git pickers clear
-- what was typed before typing their command: on Windows what is up to the
-- first ">" (PowerShell's own "PS C:\src> ", or one like "12:34>"), and on
-- Linux what is up to the first $ or # (bash's "me@host:~$ "). They are read
-- on the cursor's row alone, so they also match a long prompt's second
-- row. Give your own to M.git or M.defaults when yours looks otherwise.
M.prompt = M.windows and "^[^>]*> ?" or "^[^$#]*[$#] "

-- The program a match of Ripgrep is opened in, at its line.
M.editor = "nvim"

local function copy(options)
  local made = {}
  for key, value in pairs(options or {}) do
    made[key] = value
  end
  return made
end

-- What to type to go to the folder `path`, Enter included.
function M.go_to(path)
  if M.windows then
    return "Set-Location -LiteralPath " .. M.quote(path) .. "\r"
  end
  return "cd -- " .. M.quote(path) .. "\r"
end

-- What to type to open `path` at `line` in `editor` (M.editor without one).
function M.edit(path, line, editor)
  return (editor or M.editor) .. " +" .. line .. " " .. M.quote(path) .. "\r"
end

-- files.directories, whose Enter from a window goes to the folder. Opened
-- from a shell, it gives the folder back instead, as any picker does.
function M.directories(options)
  options = copy(options)
  options.position = options.position or "Cursor"
  options.action = options.action or function(app, item)
    app:send_text(M.go_to(item.value))
  end
  return files.directories(options)
end

-- ripgrep.search, whose Enter from a window opens the match at its line in
-- options.editor, or M.editor.
function M.ripgrep(options)
  options = copy(options)
  local editor = options.editor
  options.editor = nil
  if options.preview_visible == nil then
    options.preview_visible = false
  end
  options.action = options.action or function(app, item)
    local path, line = ripgrep.place(item.value)
    if path then
      app:send_text(M.edit(path, line, editor))
    end
  end
  return ripgrep.search(options)
end

-- The git pickers of nterm.git, by their names there. options.prompt is
-- the shell's prompt (M.prompt without it).
function M.git(options)
  local prompt = (options or {}).prompt or M.prompt
  return {
    ["Git commits"] = git.commits(),
    ["Git branches"] = git.branches { prompt = prompt },
    ["Git status"] = git.status { prompt = prompt },
    ["Git cherry-pick"] = git.cherry_pick(),
    ["Git cherry-pick commits"] = git.cherry_pick_commits { prompt = prompt },
    ["Git stashes"] = git.stashes { prompt = prompt },
    ["Git reflog"] = git.reflog(),
    ["Git file history"] = git.file_history { prompt = prompt },
    ["Git file history commits"] = git.file_history_commits(),
    ["Git fixup"] = git.fixups { prompt = prompt },
    ["Git tags"] = git.tags { prompt = prompt },
  }
end

-- The default pickers, a new table each time, to change as you like.
-- Options: editor (for Ripgrep) and prompt (for the git pickers).
--
--   Files, Directories  under the folders opened with as `roots` (a JSON
--                       list), or the one the shell is in. Directories goes
--                       to the folder chosen in a window.
--   Near                the files under the shell's folder, four deep, said
--                       from there.
--   Ripgrep             the lines a `pattern` is on under the shell's
--                       folder; opened in the editor at the line.
--   Commits, Git ...    the repository the shell is in: see nterm.git.
--   History,            PowerShell's history and completions, on Windows:
--   Completions         see profile.example.ps1.
function M.defaults(options)
  options = options or {}
  local pickers = M.git { prompt = options.prompt }
  pickers.Commits = pickers["Git commits"]
  pickers.Files = files.files { position = "Cursor" }
  pickers.Directories = M.directories()
  pickers.Near = files.files { max_depth = 4, relative = true, position = "Cursor" }
  pickers.Ripgrep = M.ripgrep { editor = options.editor }
  if M.windows then
    pickers.History = history.powershell { position = "Cursor", preview = history.preview() }
    pickers.Completions = completions.powershell()
  end
  return { pickers = pickers }
end

return M

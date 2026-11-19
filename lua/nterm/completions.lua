-- A picker of a shell's completions, for nterm: its rows are what the shell
-- gives it when it opens it, so it is opened from the shell, not a window.
--
--   -- nterm-daemon.lua, on the machine the shell is on
--   local completions = require("nterm.completions")
--   return {
--     pickers = {
--       -- What PowerShell says of each, at once: its tooltip.
--       Completions = completions.powershell(),
--       -- Or Get-Help, run for each row the cursor rests on.
--       -- Completions = completions.powershell { preview = completions.help() },
--     },
--   }
--
--   # From PowerShell, with the NTerm module that comes with nterm
--   Set-PSReadLineKeyHandler -Chord Ctrl+Shift+Spacebar -ScriptBlock {
--       Complete-NtermLine -DaemonPicker Completions
--   }
--
-- Complete-NtermLine gives each completion with PowerShell's tooltip for it
-- as its description, and opens the picker with `commandName`, the command
-- around the cursor, and `cwd`.
--
-- Options: preview (completions.tooltip() unless given; false for none),
-- and the fields any picker has, which are passed on as they are: position,
-- preview_visible, initial_query, action, keys.
local M = {}

local passed = { "position", "preview_visible", "initial_query", "action", "keys" }

function M.powershell(options)
  options = options or {}
  -- No rows written: they are the program's.
  local made = { type = "Items" }
  for _, name in ipairs(passed) do
    made[name] = options[name]
  end
  if options.preview ~= false then
    made.preview = options.preview or M.tooltip()
  end
  return made
end

-- What shows for a row: what the shell said of it, PowerShell's tooltip. For
-- a command that is how it is called, for a parameter its type, and for a
-- file its whole path. No program is run for it.
--
-- Options: syntax (the language it is coloured as, "PlainText": none; the
-- names are bat's, such as "PowerShell").
function M.tooltip(options)
  options = options or {}
  local syntax = options.syntax or "PlainText"
  return {
    type = "Dynamic",
    resolve = function(item)
      local said = item.description
      if said == nil or said == "" then
        said = item.value
      end
      return { type = "Text", text = said, syntax = syntax }
    end,
  }
end

-- What Get-Help says of the completion: of a parameter (-Path), that
-- parameter of the command it is for; of anything else, all its help. The
-- command and the completion are given to PowerShell as arguments, and are
-- never run as script. Get-Help may load the module a command is in, as
-- PowerShell does to run it: that module's code runs.
local help_script = [==[
$commandName = $args[0]
$item = $args[1].Trim([char[]]@([char]39, [char]34))
try {
    if ($item.StartsWith('-') -and $commandName) {
        Get-Help -Name $commandName -Parameter $item.TrimStart('-') -ErrorAction Stop |
            Out-String -Width $args[2]
    } else {
        Get-Help -Name $item -Full -ErrorAction Stop | Out-String -Width $args[2]
    }
} catch {
    "No help for $item"
}
]==]

-- It runs PowerShell for each row the cursor rests on, which takes about
-- half a second.
--
-- Options: shell ("pwsh.exe", PowerShell 7.4 or later), width (100, the
-- columns the help is laid out in).
function M.help(options)
  options = options or {}
  local shell = options.shell or "pwsh.exe"
  local width = tostring(options.width or 100)
  return {
    type = "Dynamic",
    resolve = function(item, params)
      return {
        type = "Command",
        output = "Text",
        args = { shell, "-NoLogo", "-NoProfile", "-NonInteractive", "-CommandWithArgs", help_script,
                 params.commandName or "", item.value, width },
      }
    end,
  }
end

return M

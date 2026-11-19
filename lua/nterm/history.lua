-- A picker of what was typed at a shell, for nterm: the lines of a history
-- file, the last typed first and each once.
--
--   -- nterm-daemon.lua, on the machine the shell is on
--   local history = require("nterm.history")
--   return {
--     pickers = {
--       History = history.powershell(),
--     },
--   }
--
--   # From PowerShell, in a pane of that machine
--   Invoke-NtermPicker -DaemonPicker History -Parameters @{
--       history = (Get-PSReadLineOption).HistorySavePath
--       query = $line
--       position = 'cursor'
--   }
--
-- It is opened with the file's path as `history`, a whole path or one in the
-- shell's directory. What matches stays in the file's order, so the last
-- typed of it comes first.
--
-- Options: the fields any picker has, which are passed on as they are:
-- position, preview_visible, initial_query, action, keys, preview.
local M = {}

local passed = { "position", "preview_visible", "initial_query", "action", "keys", "preview" }

local function picker(options, continuation)
  options = options or {}
  local made = {
    type = "File",
    path = "{history}",
    reverse = true,
    unique = true,
    line_continuation = continuation,
    sort = "SourceOrder",
  }
  for _, name in ipairs(passed) do
    made[name] = options[name]
  end
  return made
end

-- The file PSReadLine keeps, where a line that ends in a backtick goes on
-- on the next: the two are one row, and what is chosen is both lines.
function M.powershell(options)
  return picker(options, "`")
end

return M

-- Pickers of files and folders for nterm: what is under the folders a
-- picker is opened with as `roots` (a JSON list of whole paths), or else
-- under those the file gives, or else under the shell's directory.
--
--   -- nterm-daemon.lua, on the machine the shell is on
--   local files = require("nterm.files")
--   return {
--     pickers = {
--       Files = files.files(),
--       Directories = files.directories { roots = { "C:\\src" } },
--     },
--   }
--
--   # From PowerShell, in a pane of that machine
--   Invoke-NtermPicker -DaemonPicker Files -Parameters @{ cwd = $PWD.Path }
--
-- A row is the whole path of a file or folder, and shows as the file, or as
-- what is in the folder.
--
-- Options: roots, max_depth (how many levels down; all with none), relative
-- (rows said from the one folder looked under), and the fields any picker
-- has, which are passed on as they are: position, preview_visible (false
-- unless given), sort, initial_query, action, keys, preview.
local M = {}

local passed = {
  "roots", "max_depth", "relative",
  "position", "sort", "initial_query", "action", "keys", "preview",
}

local function picker(options, kind)
  options = options or {}
  local made = { type = "FileSystem", preview_visible = options.preview_visible or false }
  made[kind] = true
  for _, name in ipairs(passed) do
    made[name] = options[name]
  end
  return made
end

-- The files, and not the folders.
function M.files(options)
  return picker(options, "files_only")
end

-- The folders, and not the files.
function M.directories(options)
  return picker(options, "directories_only")
end

return M

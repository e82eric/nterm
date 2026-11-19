-- A ripgrep picker for nterm: the lines of the files under the shell's
-- directory that a pattern is found in, one row for each place, as
-- `path:line:column:text`. rg is run once, with the pattern it is opened
-- with; what is typed in the picker narrows what it found. A row shows its
-- file at the line it was found on.
--
--   -- nterm-daemon.lua, on the machine the shell is on
--   local ripgrep = require("nterm.ripgrep")
--   return {
--     pickers = {
--       Ripgrep = ripgrep.search { preview_visible = false },
--     },
--   }
--
--   -- nterm.lua: a prompt for the pattern, and the picker on top of it
--   Grep = { type = "Argument", prompt = "Grep:",
--            action = function(app, item) app:push_picker("Ripgrep", { pattern = item.value }) end },
--   Ripgrep = { type = "Daemon", picker = "Ripgrep",
--               action = function(app, item)
--                 local path, line = item.value:match("^(.-):(%d+):%d+:")
--                 if path then app:send_text("nvim +" .. line .. " '" .. path:gsub("'", "''") .. "'\r") end
--               end },
--
-- It can be put in nterm.lua as well, for rg to run where the window is.
--
-- Options: program ("rg"), flags (in place of M.flags), preview (in place of
-- M.preview()), and the fields any picker has, which are passed on as they
-- are: position, preview_visible, sort, initial_query, action, keys.
local M = {}

-- What rg is run with before the pattern: each place on one line, at most
-- 500 characters of it, hidden files too, no colours, whatever the case.
M.flags = {
  "--crlf",
  "--max-columns=500",
  "--max-columns-preview",
  "--no-messages",
  "--hidden",
  "--color=never",
  "-i",
  "--vimgrep",
}

-- The most of a file a preview shows. A longer one shows as the lines round
-- the one found.
M.most = 256 * 1024

local passed = { "position", "preview_visible", "sort", "initial_query", "action", "keys" }

-- The picker. It is opened with `pattern`, which comes after `--`, so that
-- it is never taken for a flag, and runs in `cwd`.
function M.search(options)
  options = options or {}
  local args = { options.program or "rg" }
  for _, flag in ipairs(options.flags or M.flags) do
    args[#args + 1] = flag
  end
  args[#args + 1] = "--"
  args[#args + 1] = "{pattern}"
  -- rg ends with 1 when it found nothing, which is no failure.
  local picker = {
    type = "Command",
    args = args,
    success_exit_codes = { 0, 1 },
    preview = options.preview or M.preview(),
  }
  for _, name in ipairs(passed) do
    picker[name] = options[name]
  end
  return picker
end

-- The file and line of a row, or nil for one that is not a place. The path
-- may have colons of its own, as `C:\src\a.rs` does.
function M.place(row)
  local path, line = row:match("^(.-):(%d+):%d+:")
  if not path or path == "" then
    return nil
  end
  return path, tonumber(line)
end

-- Text with what is not UTF-8 in it shown as U+FFFD.
local function text_of(bytes)
  local pieces, at = {}, 1
  while true do
    local whole, bad = utf8.len(bytes, at)
    if whole then
      pieces[#pieces + 1] = bytes:sub(at)
      return table.concat(pieces)
    end
    pieces[#pieces + 1] = bytes:sub(at, bad - 1) .. "\239\191\189"
    at = bad + 1
  end
end

-- Each line of `file`, with its line ending, given to `each` until it says
-- to stop. A line longer than `most` is not given: that is said instead.
local function each_line(file, most, each)
  local pending = ""
  while true do
    local block = file:read(64 * 1024)
    if not block then
      break
    end
    pending = pending .. block
    local start = 1
    while true do
      local ends = pending:find("\n", start, true)
      if not ends then
        break
      end
      if each(pending:sub(start, ends)) then
        return
      end
      start = ends + 1
    end
    pending = pending:sub(start)
    if #pending >= most then
      return "It has a line too long to show"
    end
  end
  if pending ~= "" then
    each(pending)
  end
end

-- What of the file at `path` shows for `line` of it, from one, and which
-- line of that it is: the whole file, or for one longer than `most` the
-- lines round that one, as much before it as after. For a file that is
-- not text, or has a line too long to show, it is nil and what to say.
function M.lines_round(path, line, most)
  most = most or M.most
  local file = assert(io.open(path, "rb"))
  local size = file:seek("end")
  file:seek("set", 0)
  -- A file with a zero byte near its start is not one to read as text.
  local start = file:read(8192) or ""
  if start:find("\0", 1, true) then
    file:close()
    return nil, "Not text: " .. size .. " bytes"
  end
  file:seek("set", 0)
  if size <= most then
    local bytes = file:read("a") or ""
    file:close()
    local _, lines = bytes:gsub("\n", "")
    if bytes ~= "" and bytes:sub(-1) ~= "\n" then
      lines = lines + 1
    end
    return text_of(bytes), (line >= 1 and line <= lines) and line or nil
  end
  local kept, first, last, held, number, at = {}, 1, 0, 0, 0, nil
  local said = each_line(file, most, function(one)
    number = number + 1
    if at then
      -- After the line: as much as there is room left for.
      if held + #one > most then
        return true
      end
    else
      -- Before it: no more than half of what is shown.
      while held + #one > most // 2 and first <= last do
        held = held - #kept[first]
        kept[first] = nil
        first = first + 1
      end
    end
    held = held + #one
    last = last + 1
    kept[last] = one
    if number == line then
      at = last - first + 1
    end
  end)
  file:close()
  if said then
    return nil, said
  end
  return text_of(table.concat(kept, "", first, last)), at
end

-- A preview of a row: its file, at the line it was found on, in the middle
-- and marked. Options: theme (the colours of text, where the window uses
-- it: a window drawing a daemon's picker uses its own).
function M.preview(options)
  local theme = (options or {}).theme
  return {
    type = "Dynamic",
    resolve = function(item, params)
      local path, line = M.place(item.value)
      if not path then
        return { type = "Text", text = item.value, syntax = "PlainText" }
      end
      local whole = path
      if params.cwd and not path:match("^%a:") and not path:match("^[\\/]") then
        whole = params.cwd .. "/" .. path
      end
      local text, at = M.lines_round(whole, line)
      if not text then
        return { type = "Text", text = at, syntax = "PlainText", theme = theme }
      end
      return {
        type = "Text", text = text, name = path:match("[^\\/]+$"),
        syntax = "Auto", extension = path:match("%.([^./\\]+)$"), theme = theme,
        center_line = at, highlight_line = at,
      }
    end,
  }
end

return M

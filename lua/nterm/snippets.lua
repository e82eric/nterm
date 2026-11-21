-- Snippets for nterm: text kept in a JSON file, saved from a selection and
-- typed at the shell from a picker. The file is Windows Terminal's kind,
-- `{ "snippets": [ { "input": "..." } ] }`, and what else it holds is kept.
-- It is read and written on the window's machine.
--
--   local snippets = require("nterm.snippets")
--   local path = (os.getenv("USERPROFILE") or os.getenv("HOME")) .. "/.wtd/.wt.json"
--   return {
--     actions = {
--       SaveSnippet = { label = "Save selection as snippet", action = snippets.save { path = path } },
--     },
--     pickers = {
--       Snippets = snippets.picker { path = path },
--     },
--     keys = {
--       { key = "s", mods = "CTRL|SHIFT", action = "SaveSnippet" },
--       { key = "b", mods = "CTRL|SHIFT", picker = "Snippets" },
--     },
--   }
--
-- A snippet may have lines. Its row shows it on one, each line end as a
-- mark, and the preview shows it as it is typed; the preview is hidden until
-- it is asked for (preview_visible = true opens the picker showing it).
--
-- Options of both: path, the file (needed). The picker also takes word (in
-- place of M.word) and the fields any picker has, which are passed on as
-- they are: position, preview_visible, sort, preview, action, keys.
local M = {}

-- No such file: what io.open gives as its third result.
local MISSING = 2

local function needs_path(options, what)
  options = options or {}
  assert(type(options.path) == "string", "nterm.snippets: " .. what .. " needs path, the snippets file")
  return options
end

-- What the file holds, as a table with a `snippets` list; a file that is
-- not there holds none. On failure, nil and why.
function M.load(path)
  local file, why, code = io.open(path, "rb")
  if not file then
    if code == MISSING then
      return { snippets = {} }
    end
    return nil, "Unable to read snippets file: " .. tostring(why)
  end
  local content = file:read("a") or ""
  file:close()
  local parsed, data = pcall(nterm.json_parse, content)
  if not parsed or type(data) ~= "table" then
    return nil, "Unable to parse snippets JSON: " .. tostring(data)
  end
  if type(data.snippets) ~= "table" then
    data.snippets = {}
  end
  return data
end

-- The text of each snippet in the file, in its order. On failure, nil and
-- why.
function M.inputs(path)
  local data, why = M.load(path)
  if not data then
    return nil, why
  end
  local inputs = {}
  for _, snippet in ipairs(data.snippets) do
    if type(snippet) == "table" and type(snippet.input) == "string" then
      inputs[#inputs + 1] = snippet.input
    end
  end
  return inputs
end

-- `text` kept as the file's last snippet, the rest of the file as it was.
-- The file is made if it is not there. On failure, nil and why; the file is
-- then as it was.
function M.add(path, text)
  local data, why = M.load(path)
  if not data then
    return nil, why
  end
  table.insert(data.snippets, { input = text })
  local encoded, json = pcall(nterm.json_encode_pretty, data)
  if not encoded then
    return nil, "Unable to encode snippets JSON: " .. tostring(json)
  end
  local output, open_error = io.open(path, "wb")
  if not output then
    return nil, "Unable to write snippets file: " .. tostring(open_error)
  end
  local written, write_error = output:write(json .. "\n")
  output:close()
  if not written then
    return nil, "Unable to write snippets file: " .. tostring(write_error)
  end
  return true
end

-- An action: the selection kept as a snippet, and a toast that says so.
function M.save(options)
  local path = needs_path(options, "save").path
  return function(app)
    local selection = app:get_selection_text()
    if selection == "" then
      return app:show_toast("Save snippet", "Select some text first.", 3000)
    end
    local saved, why = M.add(path, selection)
    if not saved then
      return app:show_toast("Could not save snippet", why, 5000)
    end
    app:show_toast("Snippet saved", selection, 2000)
  end
end

-- The word before the cursor, after a PowerShell prompt: what the picker
-- opens filtered by, and what Enter puts the snippet in place of.
function M.word(line_before_cursor)
  local input = line_before_cursor:match("^PS [^>]*>%s?(.*)$") or line_before_cursor
  return input:match("(%S+)$") or ""
end

-- What a row shows in place of a line end.
M.line_end = " \u{21B5} "

-- A snippet as its row shows it: on one line.
function M.row(input)
  return (input:gsub("\r\n", "\n"):gsub("[\r\n]", M.line_end))
end

-- The preview: the snippet on its own lines, as it is typed. Options: syntax
-- (the language, "PowerShell"; the names are bat's, and "PlainText" is
-- none) and theme (the colours).
function M.preview(options)
  options = options or {}
  local syntax = options.syntax or "PowerShell"
  local theme = options.theme
  return {
    type = "Dynamic",
    resolve = function(item)
      local text = item.value:gsub("\r\n", "\n"):gsub("\r", "\n")
      return { type = "Text", text = text, syntax = syntax, theme = theme }
    end,
  }
end

local passed = { "position", "preview_visible", "sort", "preview", "action", "keys" }

-- The picker of the file's snippets, read each time it opens. It opens
-- filtered by the word before the cursor, and Enter types the snippet in
-- place of that word.
function M.picker(options)
  options = needs_path(options, "picker")
  local path, word_of = options.path, options.word or M.word
  return function(context)
    local word = word_of(context.line_before_cursor or "")
    local picker = {
      type = "Items",
      initial_query = word,
      position = "Cursor",
      preview = M.preview(),
      preview_visible = false,
      items = function()
        local inputs = assert(M.inputs(path))
        local items = {}
        for _, input in ipairs(inputs) do
          items[#items + 1] = { text = M.row(input), value = input }
        end
        return items
      end,
      action = function(app, item)
        app:replace_prompt(item.value, utf8.len(word) or 0, 0)
      end,
    }
    for _, name in ipairs(passed) do
      if options[name] ~= nil then
        picker[name] = options[name]
      end
    end
    return picker
  end
end

return M

-- Asking Claude from nterm, about what is on the terminal: a prompt for a
-- question with the answer as a page on top, and a prompt for what you want
-- done with commands that do it as a page on top. The last rows of the
-- terminal go with what is asked. Claude runs on the window's machine.
--
--   local claude = require("nterm.claude")
--   return {
--     pickers = {
--       -- "Ask:", then Claude's answer, as it is written.
--       Ask = claude.ask { page = "Answer" },
--       Answer = claude.answer(),
--       -- "Suggest:", then commands that do it; Enter types one at the
--       -- shell's cursor.
--       Suggest = claude.suggest { page = "Suggestions" },
--       Suggestions = claude.suggestions(),
--     },
--     keys = {
--       { key = "a", mods = "CTRL|SHIFT", picker = "Ask" },
--       { key = "u", mods = "CTRL|SHIFT", picker = "Suggest" },
--     },
--   }
--
-- Each prompt pushes its page by name, so both are in `pickers`, and `page`
-- says which is the prompt's. Escape on a page stops Claude and goes back to
-- the prompt, to change what was asked and ask again.
--
-- Options of every function: program ("claude"), and the fields any picker
-- has, which are passed on as they are: position, preview_visible, sort,
-- initial_query, action, keys. The prompts also take prompt and placeholder,
-- the pages model and flags (more arguments for claude, before what is
-- asked).
local M = {}

-- Hooks are off for every run, so a question is not taken by other tools
-- for a session of Claude Code's own.
M.settings = '{"disableAllHooks":true}'

-- How many commands a Suggestions page asks for.
M.count = 10

-- The model a Suggestions page uses: a quick one, as the commands are short.
M.suggestions_model = "claude-haiku-5-5"

local passed = { "position", "preview_visible", "sort", "initial_query", "action", "keys" }

local function pass(options, picker)
  for _, name in ipairs(passed) do
    if options[name] ~= nil then
      picker[name] = options[name]
    end
  end
  return picker
end

-- The rows of the terminal a picker was opened over, as one text.
function M.recent(context)
  return table.concat(context.recent_lines or {}, "\n")
end

-- What an Answer page is asked: the question, after the terminal's rows.
function M.question(question, recent)
  if recent == nil or recent == "" then
    return question
  end
  return "Recent terminal output:\n" .. recent .. "\n\nQuestion: " .. question
end

-- What a Suggestions page is asked: `count` commands for what is wanted, one
-- on each line, for the shell the terminal's rows show.
function M.request(wanted, recent, count)
  local lines = {
    "Suggest exactly " .. (count or M.count) .. " shell commands that do this:",
    wanted,
    "",
    "Use the shell and the working directory the terminal output shows.",
    "Put the most likely command first.",
    "Return one command per line and nothing else.",
    "Do not number the commands.",
    "Do not use Markdown or code fences.",
  }
  if recent ~= nil and recent ~= "" then
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Terminal output:"
    lines[#lines + 1] = recent
  end
  return table.concat(lines, "\n")
end

-- An Argument prompt that pushes `page` with what `make(value, recent)`
-- gives as its parameters.
local function prompt(options, defaults, make)
  assert(type(options.page) == "string", "nterm.claude: the prompt needs page, the name of its page")
  return function(context)
    local recent = M.recent(context)
    return pass(options, {
      type = "Argument",
      prompt = options.prompt or defaults.prompt,
      placeholder = options.placeholder or defaults.placeholder,
      action = function(app, item)
        if item.value == "" then
          return
        end
        app:push_picker(options.page, make(item.value, recent))
      end,
    })
  end
end

-- A prompt for a question. It opens `page` with `question`: what was typed,
-- after the terminal's rows.
function M.ask(options)
  options = options or {}
  return prompt(options, { prompt = "Ask:", placeholder = "<question>  Ask Claude" }, function(value, recent)
    return { question = M.question(value, recent) }
  end)
end

-- A prompt for what you want done. It opens `page` with `request`: what
-- Claude is asked for, the terminal's rows in it. Option: count (M.count).
function M.suggest(options)
  options = options or {}
  return prompt(options, { prompt = "Suggest:", placeholder = "<what to do>  Commands from Claude" }, function(value, recent)
    return { request = M.request(value, recent, options.count) }
  end)
end

local function arguments(options, model, list)
  local args = { options.program or "claude" }
  for _, one in ipairs(list) do
    args[#args + 1] = one
  end
  if model then
    args[#args + 1] = "--model"
    args[#args + 1] = model
  end
  for _, flag in ipairs(options.flags or {}) do
    args[#args + 1] = flag
  end
  return args
end

-- The text of one line of `claude --output-format stream-json`: a piece of
-- the answer, an error, or nothing for the rest. A line that is not JSON is
-- shown as it is.
function M.answer_line(line)
  local ok, event = pcall(nterm.json_parse, line)
  if not ok or type(event) ~= "table" then
    return line .. "\n"
  end
  if event.type == "stream_event" then
    local delta = event.event and event.event.delta
    return delta and delta.text
  elseif event.type == "result" and event.is_error then
    return "\n\nError: " .. tostring(event.result) .. "\n"
  end
end

-- The page for a question: Claude's answer as it is written, as markdown.
-- It is opened with `question`.
function M.answer(options)
  options = options or {}
  local args = arguments(options, options.model, {
    "-p", "--settings", M.settings,
    "--output-format", "stream-json", "--include-partial-messages", "--verbose",
  })
  args[#args + 1] = "{question}"
  return pass(options, {
    type = "Output",
    format = "Markdown",
    args = args,
    line = M.answer_line,
  })
end

-- The page of commands: one row for each, in the order Claude gave them.
-- It is opened with `request`. Enter types the command at the shell's
-- cursor, leaving what is typed there already.
function M.suggestions(options)
  options = options or {}
  local args = arguments(options, options.model or M.suggestions_model, {
    "--print", "--settings", M.settings,
    "--output-format", "text", "--tools", "", "--no-session-persistence",
  })
  args[#args + 1] = "{request}"
  return pass(options, {
    type = "Command",
    args = args,
    sort = "SourceOrder",
    preview_visible = false,
    position = "Cursor",
    action = function(app, item)
      app:replace_prompt(item.value, 0, 0)
    end,
  })
end

return M

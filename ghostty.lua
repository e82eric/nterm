-- Config lookup: --lua-config=PATH, then ~/ghostty.lua, then ghostty.lua beside
-- the executable. Only the first available file is loaded.
local snippets_path = (os.getenv("USERPROFILE") or os.getenv("HOME")) .. "/.wtd/.wt.json"
return {
  actions = {
    SaveSnippet = {
      label = "Save selection as snippet",
      action = function(app)
        local selection = app:get_selection_text()
        if selection == "" then
          app:show_toast("Save snippet", "Select some text first.", 3000)
          return
        end
        local file, read_error = io.open(snippets_path, "rb")
        if not file then
          app:show_toast("Could not save snippet", "Unable to read snippets file: " .. tostring(read_error), 5000)
          return
        end
        local content = file:read("*a")
        file:close()
        local parsed, data = pcall(ghostty.json_parse, content)
        if not parsed or type(data) ~= "table" then
          app:show_toast("Could not save snippet", "Unable to parse snippets JSON: " .. tostring(data), 5000)
          return
        end
        if type(data.snippets) ~= "table" then data.snippets = {} end
        table.insert(data.snippets, { input = selection })
        local encoded, json = pcall(ghostty.json_encode_pretty, data)
        if not encoded then
          app:show_toast("Could not save snippet", "Unable to encode snippets JSON: " .. tostring(json), 5000)
          return
        end
        local output, open_error = io.open(snippets_path, "wb")
        if not output then
          app:show_toast("Could not save snippet", "Unable to write snippets file: " .. tostring(open_error), 5000)
          return
        end
        local saved, save_error = output:write(json .. "\n")
        output:close()
        if not saved then
          app:show_toast("Could not save snippet", "Unable to write snippets file: " .. tostring(save_error), 5000)
          return
        end
        app:show_toast("Snippet saved", selection, 2000)
      end,
    },
  },
  font = {
    -- family = "JetBrains Mono", -- Uncomment and choose an installed font.
    size = 12,
  },
  terminal = {
    scrollback_limit = 100000000, -- Bytes; zero disables scrollback.
    command = "pwsh.exe",
  },
  colors = {
    -- theme = "Gruvbox Dark", -- Uses Ghostty's theme resolution.
  },
  -- floating = {
  --   padding = { top = 15, bottom = 15, left = 10, right = 10 }, -- Percent of terminal area.
  -- },
  builtin_pickers = {
    themes = { position = "Cursor", sort = "Score" },
    command_palette = { position = "Cursor", preview_visible = false },
    lines = {},
    words = {},
    line_break = { preview_visible = false },
  },
  pickers = {
    CodexSuggestions = function(context)
      local cursor_row = context.line_before_cursor
      local input_start = cursor_row:match(".*#()")
      if not input_start then
        error("Codex suggestions: the current prompt does not contain a # marker")
      end
      local current_input = cursor_row:sub(input_start)
      local erase_count = utf8.len(current_input) or #current_input
      local terminal_context = table.concat(context.recent_lines, "\n")
      if terminal_context == "" then
        error("Codex suggestions: no terminal context is available")
      end
      local prompt = table.concat({
        "Generate exactly 10 likely command completions or alternatives.",
        "",
        "In the current cursor row, # marks the beginning of editable input.",
        "Everything before and including # is non-editable prompt text.",
        "Return only the completed editable command.",
        "Never include the prompt or the # marker.",
        "Return one command per line.",
        "Do not number the commands.",
        "Do not use Markdown or code fences.",
        "",
        "Current cursor row:", cursor_row,
        "",
        "Terminal context:", terminal_context,
      }, "\n")
      return {
        type = "Command",
        args = {
          "codex", "exec", "--ephemeral", "--skip-git-repo-check",
          "--color", "never", "--model", "gpt-5.6-luna",
          "--config", 'model_reasoning_effort="none"', prompt,
        },
        position = "Cursor",
        sort = "Source",
        preview_visible = false,
        action = function(app, item)
          app:replace_prompt(item.value, erase_count, 0)
        end,
      }
    end,
    files = {
      type = "FileSystem",
      -- preview = { type = "Command", args = { "bat.exe", "--color=always", "{item}" } },
      roots = { [[G:\src, C:\src]] },
      files_only = true,
      max_depth = 0, -- Zero means unlimited.
      enter_action = "SendInput",
    },
    PowerShellCompletion = {
      type = "Items",
      action = function(app, item, params)
        local before = math.min(math.max(tonumber(params.replaceBefore) or 0, 0), 4096)
        local after = math.min(math.max(tonumber(params.replaceAfter) or 0, 0), 4096)
        app:send_text(
          string.rep("\b", before) .. string.rep("\x1b[3~", after)
        )
        app:paste(item.value)
      end,
    },
    Ripgrep = {
      type = "Command",
      args = {
        "rg.exe", "--crlf", "--max-columns=500", "--max-columns-preview",
        "--no-messages", "--hidden", "--color=never", "-i", "--vimgrep",
        "--", "{pattern}",
      },
      action = function(app, item, params)
        local path, line = item.value:match("^(.*):(%d+):%d+:")
        if not path then return end
        local length = math.min(math.max(tonumber(params.length) or 0, 0), 4096)
        local cursor = math.min(math.max(tonumber(params.cursor) or length, 0), length)
        local quoted = "'" .. path:gsub("'", "''") .. "'"
        app:send_text(string.rep("\b", cursor) .. string.rep("\x1b[3~", length - cursor))
        app:send_text("nvim +" .. line .. " " .. quoted .. "\r")
      end,
    },
  },
  keys = {
    { key = "s", mods = "CTRL|SHIFT", action = "SaveSnippet" },
    -- An explicit keys table replaces all top-level action and picker defaults.
    { key = "n", mods = "CTRL|SHIFT", action = "NewWindow" },
    { key = "e", mods = "CTRL|SHIFT", action = "ToggleFloatingPane" },
    { key = "w", mods = "CTRL|SHIFT", action = "CloseFloatingPane" },
    { key = "/", mods = "CTRL|SHIFT", action = "OpenSearch" },
    { key = "m", mods = "CTRL|SHIFT", action = "EnterCopyMode" },
    { key = "Space", mods = "ALT", action = "OpenQuickSelect" },
    { key = "ArrowUp", mods = "CTRL|SHIFT", action = "PreviousCommand" },
    { key = "ArrowDown", mods = "CTRL|SHIFT", action = "NextCommand" },
    -- On macOS you can also bind { key = "f", mods = "CMD", action = "OpenSearch" }.
    { key = "Space", mods = "CTRL|SHIFT", builtin_picker = "command_palette" },
    { key = "f", mods = "CTRL|SHIFT", builtin_picker = "lines" },
    { key = "p", mods = "CTRL|SHIFT", builtin_picker = "words" },
    { key = "o", mods = "CTRL|SHIFT", picker = "files" },
    { key = "k", mods = "CTRL|SHIFT", picker = "CodexSuggestions" },
  },
}

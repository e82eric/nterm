-- Extended Windows sample. Customize tool dependencies and picker names as needed.
-- Previews require pwsh with -CommandWithArgs, bat, and file on PATH; text search
-- requires rg. ProfileHistory additionally requires PowershellHistoryReader.exe.
-- Snippets use GHOSTTY_SNIPPETS_FILE or ~/ghostty-snippets.json, with initial JSON:
-- {"snippets": []}. SaveSnippet writes to that file.
-- CodexSuggestions invokes codex exec and sends the current terminal context.
-- profile.example.ps1 uses ProfileFiles/ProfileDirectories and streamed requests.

-- Config lookup: --lua-config=PATH, then ~/ghostty.lua, then ghostty.lua beside
-- the executable. Only the first available file is loaded.
local snippets_path = os.getenv("GHOSTTY_SNIPPETS_FILE")
  or ((os.getenv("USERPROFILE") or os.getenv("HOME") or ".") .. "/ghostty-snippets.json")

local pipe_file_preview = [==[
        $ErrorActionPreference = 'Stop'
        # The file and bat commands must be available on PATH.

        $path = (Resolve-Path -LiteralPath $args[0] -ErrorAction Stop).Path

        if (Test-Path -LiteralPath $path -PathType Container) {
            Get-ChildItem -LiteralPath $path -Force
            return
        }

        $mime = & file --mime-type -b -- $path 2>$null

        if ($mime -match '^text/' -or $mime -in @(
            'application/json'
            'application/xml'
            'application/javascript'
            'application/x-sh'
            'application/x-powershell'
        )) {
            & bat.exe --color=always --paging=never --style=plain `
                --theme=gruvbox-dark --line-range 1:1000 -- $path
        } else {
            Get-ChildItem -LiteralPath $path -Force
        }
]==]
local pipe_rg_preview = [==[
          $match = [regex]::Match($args[0], '^(.+?):(\d+):\d+:')
          if (-not $match.Success) { return }

          $path = $match.Groups[1].Value
          $row = [int]$match.Groups[2].Value
          $start = [Math]::Max(1, $row - 5)
          $end = $row + 10

          & bat.exe --color=always --paging=never --wrap=never `
              --line-range "${start}:${end}" --highlight-line $row `
              --style=plain --theme=gruvbox-dark -- $path
]==]
local pipe_help_preview = [==[
          $commandName = $args[0]
          $item = $args[1].Trim([char[]]@([char]39, [char]34))

          try {
              if ($item.StartsWith('-') -and $commandName) {
                  Get-Help -Name $commandName `
                      -Parameter $item.TrimStart('-') -ErrorAction Stop |
                      Out-String -Width 100
              } else {
                  Get-Help -Name $item -Full -ErrorAction Stop |
                      Out-String -Width 100
              }
          } catch {
              "No help available for: $item"
          }
]==]

local function profile_preview(script, extra)
  return {type="Command", command=function(item, params)
    local args={"pwsh.exe","-NoProfile","-CommandWithArgs",script}
    if extra then args[#args+1]=params[extra] or "" end
    args[#args+1]=item.value
    return {args=args,cwd=params.cwd}
  end}
end

return {
  font = {
    size = 12,
  },
  picker_pipe = {
    enabled = true,
    allow_stream = true,
    named = { "ProfileFiles", "ProfileFilesPreview", "ProfileDirectories", "ProfileRipgrep", "ProfileHistory", "ProfileCompletions" },
  },
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
  terminal = {
    scrollback_limit = 100000000, -- Bytes; zero disables scrollback.
    command = "pwsh.exe",
  },
  theme_colors = {
    ["Gruvbox Dark"] = {
      accent = "#FABD2F",
      panel_background = "#1d2021",
      picker_match_highlight = "#fb4934",
      match_highlight = "#d65d0e",
      search_match_background = "#665c54",
      search_match_text = "#ebdbb2",
      selection_background = "#3d3937",
      selection_text = "#ebdbb2",
      yank_background = "#fe8019",
      yank_text = "#1d2021",
    },
  },
  colors = {
    ui = {
    },
    theme = "Gruvbox Dark",
  },
  floating = {
    padding = { top = 10, bottom = 10, left = 10, right = 10 },
  },
  builtin_pickers = {
    themes = {
      position = "Cursor",
      sort = "Score",
    },
    command_palette = {
      position = "Cursor",
      preview_visible = false,
    },
    lines = {
      position = "Cursor",
      enter_action = "Navigate",
      preview = { type = "Scrollback", rows = 12 },
      keys = {
        {
          key = "Enter",
          mods = "CTRL",
          description = "Send selection to terminal",
          action = function(app, item)
            app:close_picker()
            app:paste(item.value)
          end,
        },
      },
    },
    words = function(context)
      local line = context.line_before_cursor
      local before = line:match("([%w_./\\%-]+)$") or ""

      return {
        preview_visible = true,
        initial_query = before,
        enter_action = "SendInput",
        unique = true,
        position = "Cursor",
        preview_visible = false,
        preview = { type = "Scrollback", rows = 12 },
        action = before ~= "" and function(app, item)
          app:replace_prompt(item.value, utf8.len(before) or 0, 0)
        end or nil,
      }
    end,

    line_break = {
      preview_visible = false,
    },
  },
  pickers = {
    ProfileFiles = {
      type = "FileSystem", roots = {"."}, max_depth = 0,
      position = "Cursor", preview_visible = false,
      preview = profile_preview(pipe_file_preview),
    },
    ProfileFilesPreview = {
      type = "FileSystem", roots = {"."}, max_depth = 0,
      position = "Cursor", preview_visible = true,
      preview = profile_preview(pipe_file_preview),
    },
    ProfileDirectories = {
      type = "FileSystem", roots = {"."}, directories_only = true,
      max_depth = 0, position = "Cursor", preview_visible = false,
    },
    ProfileRipgrep = {
      type = "Command", position = "Cursor", success_exit_codes = {0,1},
      args = {"rg.exe","--crlf","--max-columns=500","--max-columns-preview","--no-messages","--hidden","--color=never","-i","--vimgrep","--","{pattern}"},
      preview_visible = true,
      preview = profile_preview(pipe_rg_preview),
    },
    ProfileHistory = {
      type = "Command", args = {"PowershellHistoryReader.exe"},
      line_continuation = "`", unique = true, sort = "SourceOrder", position = "Cursor",
      preview_visible = false,
      preview = profile_preview([==[$args[0] | & bat.exe --color=always --paging=never --wrap=never --style=plain --language=ps1 --theme="Visual Studio Dark+"]==]),
    },
    ProfileCompletions = {
      type="Items",items=ghostty.json_parse("[]"),position="Cursor",preview_visible=true,
      preview=profile_preview(pipe_help_preview,"commandName"),
    },

    CodexSuggestions = function(context)
      local cursor_row = context.line_before_cursor
      local input_start = cursor_row:match(".*#()")
      if not input_start then
        error("Codex suggestions: the current prompt does not contain a # marker")
      end
      local current_input = cursor_row:sub(input_start)
      local erase_count = (utf8.len(current_input) or #current_input) + 1
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
        sort = "SourceOrder",
        preview_visible = false,
        action = function(app, item)
          app:replace_prompt(item.value, erase_count, 0)
        end,
      }
    end,
    command_history = {
      type = "Scrollback",
      regex = [[(?m)^(?:PS [^>\r\n]*>|\d+:\d+>|›)\s*(.+)$]],
      enter_action = "Navigate",
      preview = { type = "Scrollback", rows = 12 },
      preview_visible = false,
    },
    Snippets = function(context)
      local input = context.line_before_cursor:match("^PS [^>]*>%s?(.*)$")
        or context.line_before_cursor
      local word = input:match("(%S+)$") or ""
      return {
        type = "Items",
        initial_query = word,
        position = "Cursor",
        items = function()
          local file = assert(io.open(snippets_path, "rb"))
          local content = file:read("*a")
          file:close()
          local data = ghostty.json_parse(content)
          local items = {}
          for _, snippet in ipairs(data.snippets or {}) do
            if type(snippet.input) == "string" then
              items[#items + 1] = { text = snippet.input, value = snippet.input }
            end
          end
          return items
        end,
        action = function(app, item)
          app:replace_prompt(item.value, utf8.len(word) or 0, 0)
        end,
      }
    end,
  },
  keys = {
    { key = "s", mods = "CTRL|SHIFT", action = "SaveSnippet" },
    { key = "a", mods = "CTRL|SHIFT", picker = "CodexSuggestions" },
    { key = "n", mods = "CTRL|SHIFT", action = "NewWindow" },
    { key = "e", mods = "CTRL|SHIFT", action = "ToggleFloatingPane" },
    { key = "e", mods = "CTRL|ALT|SHIFT", action = "CreateFloatingPane" },
    { key = "j", mods = "CTRL|SHIFT", action = "NextFloatingPane" },
    { key = "k", mods = "CTRL|SHIFT", action = "PreviousFloatingPane" },
    { key = "w", mods = "CTRL|SHIFT", action = "CloseFloatingPane" },
    { key = "/", mods = "CTRL|SHIFT", action = "OpenSearch" },
    { key = "m", mods = "CTRL|SHIFT", action = "EnterCopyMode" },
    { key = "Space", mods = "ALT", action = "OpenQuickSelect" },
    { key = "ArrowUp", mods = "CTRL|SHIFT", action = "PreviousCommand" },
    { key = "ArrowDown", mods = "CTRL|SHIFT", action = "NextCommand" },
    { key = "b", mods = "CTRL|SHIFT", picker = "Snippets" },
    { key = "f", mods = "CTRL|SHIFT", builtin_picker = "lines" },
    { key = ".", mods = "CTRL|SHIFT", builtin_picker = "words" },
    { key = "p", mods = "CTRL|SHIFT", builtin_picker = "command_palette" },
    { key = "z", mods = "CTRL|SHIFT", picker = "command_history" },
  },
}

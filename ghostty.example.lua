-- Extended Windows sample. Customize tool dependencies and picker names as needed.
-- Lua reads text/images; directory previews use cmd /c dir and video previews use
-- configurable ffmpeg/ffprobe commands. Help/history command previews require pwsh with
-- -CommandWithArgs (history also uses bat). Text search requires rg; ProfileHistory
-- additionally requires PowershellHistoryReader.exe.
-- Snippets use GHOSTTY_SNIPPETS_FILE or ~/ghostty-snippets.json, with initial JSON:
-- {"snippets": []}. SaveSnippet writes to that file.
-- CodexSuggestions invokes codex exec and sends the current terminal context.
-- profile.example.ps1 uses ProfileFiles/ProfileDirectories and streamed requests.

-- Config lookup: --lua-config=PATH, then ~/ghostty.lua, then ghostty.lua beside
-- the executable. Only the first available file is loaded.
local snippets_path = os.getenv("GHOSTTY_SNIPPETS_FILE")
  or ((os.getenv("USERPROFILE") or os.getenv("HOME") or ".") .. "/ghostty-snippets.json")

-- Dynamic previews run in a dedicated Lua state, away from the UI thread.
local preview_tools = { ffmpeg = "ffmpeg.exe", ffprobe = "ffprobe.exe", video_position = 0.25 }
local preview_images = {png=true,jpg=true,jpeg=true,gif=true,bmp=true,webp=true,tif=true,tiff=true}
local preview_videos = {mp4=true,mkv=true,mov=true,avi=true,webm=true,m4v=true,wmv=true}
local function preview_command(args, cwd)
  local result = preview.run {args=args,cwd=cwd,timeout_ms=10000}
  assert(result.exit_code == 0, result.stderr)
  return result.stdout
end
local function preview_path(path, cwd)
  if not cwd or path:match('^%a:') or path:match('^[\\/]') then return path end
  return cwd .. '/' .. path
end
local function preview_read_bytes(path)
  local file = assert(io.open(path, 'rb'))
  local ok, bytes = pcall(function()
    local bytes, read_error = file:read(10 * 1024 * 1024 + 1)
    assert(not read_error, read_error)
    bytes = bytes or ''
    assert(#bytes <= 10 * 1024 * 1024, 'File preview exceeds 10 MiB')
    return bytes
  end)
  file:close()
  assert(ok, bytes)
  return bytes
end
local function preview_read_text(path, display_path)
  local file = assert(io.open(path, 'rb'))
  local ok, result = pcall(function()
    local function read(count)
      local bytes, read_error = file:read(count)
      assert(not read_error, read_error)
      return bytes or ''
    end
    local function binary(bytes)
      return bytes:find(string.char(0), 1, true) ~= nil or utf8.len(bytes) == nil
    end
    local function metadata()
      local size = assert(file:seek('end'))
      return {text='Binary file\n' .. display_path .. '\n' .. size .. ' bytes', binary=true}
    end
    local sample = read(4096)
    -- Finish a UTF-8 character cut by the sample boundary before classifying it.
    local length, invalid = utf8.len(sample)
    if not length and invalid > #sample - 3 then
      local lead = sample:byte(invalid)
      local width = lead >= 194 and lead <= 223 and 2
        or lead >= 224 and lead <= 239 and 3
        or lead >= 240 and lead <= 244 and 4
      if width and #sample - invalid + 1 < width then
        sample = sample .. read(width - (#sample - invalid + 1))
      end
    end
    if binary(sample) then return metadata() end
    assert(file:seek('set', 0))
    local limit = 10 * 1024 * 1024
    local bytes = read(limit + 1)
    local truncated = #bytes > limit
    local warning = '\n\n[Preview truncated: file exceeds the 10 MiB text limit.]\n'
    if truncated then
      bytes = bytes:sub(1, limit - #warning)
      -- Drop an incomplete final UTF-8 character, not a complete code point.
      local valid, position = utf8.len(bytes)
      if not valid and position > #bytes - 3 then
        local lead = bytes:byte(position)
        local width = lead >= 194 and lead <= 223 and 2
          or lead >= 224 and lead <= 239 and 3
          or lead >= 240 and lead <= 244 and 4
        if width and #bytes - position + 1 < width then
          bytes = bytes:sub(1, position - 1)
        end
      end
    end
    if binary(bytes) then return metadata() end
    local text = bytes:gsub('^\239\187\191', '')
    if truncated then text = text .. warning end
    return {text=text, binary=false}
  end)
  file:close()
  assert(ok, result)
  return result.text, result.binary
end

local function preview_directory(path, cwd)
  -- The path is the process working directory, never interpolated into shell code.
  local ok, result = pcall(preview.run, {
    args={'cmd.exe','/d','/v:off','/u','/c',
      'dir /a'},
    cwd=preview_path(path,cwd), timeout_ms=10000,
  })
  -- A regular file cannot be used as a process working directory.
  if not ok then return nil end
  assert(result.exit_code == 0, result.stderr)
  -- cmd /u emits UTF-16LE, including filenames outside the current code page.
  local bytes, chars, index = result.stdout, {}, 1
  while index < #bytes do
    local code = bytes:byte(index) + bytes:byte(index+1) * 256
    index = index + 2
    if code >= 0xd800 and code <= 0xdbff and index < #bytes then
      local low = bytes:byte(index) + bytes:byte(index+1) * 256
      if low >= 0xdc00 and low <= 0xdfff then
        code = 0x10000 + (code-0xd800)*1024 + low-0xdc00
        index = index + 2
      end
    end
    if code >= 0xd800 and code <= 0xdfff then code = 0xfffd end
    chars[#chars+1] = utf8.char(code)
  end
  return table.concat(chars)
end

local function file_preview()
  return {
    type = "Dynamic",
    resolve = function(item, params)
      local path = item.value
      local extension = path:match("%.([^./\\]+)$") or ""
      local directory = preview_directory(path, params.cwd)
      if directory then
        return {type="Text",text=directory,syntax="PlainText",
          theme="gruvbox-dark"}
      end
      if preview_images[extension:lower()] then
        return {type="Image",bytes=preview_read_bytes(preview_path(path,params.cwd))}
      end
      if preview_videos[extension:lower()] then
        local duration = tonumber(preview_command({preview_tools.ffprobe,"-v","error",
          "-show_entries","format=duration","-of","default=noprint_wrappers=1:nokey=1",path}, params.cwd))
        assert(duration and duration >= 0 and duration < math.huge, "Invalid video duration")
        local seek = string.format("%.3f", duration * preview_tools.video_position)
        local bytes = preview_command({preview_tools.ffmpeg,"-loglevel","error","-ss",seek,
          "-i",path,"-frames:v","1","-vf","scale=1280:1280:force_original_aspect_ratio=decrease",
          "-f","image2pipe","-vcodec","png","pipe:1"}, params.cwd)
        return {type="Image",bytes=bytes}
      end
      local text, binary = preview_read_text(preview_path(path,params.cwd), path)
      if binary then return {type="Text",text=text,syntax="PlainText",theme="gruvbox-dark"} end
      return {type="Text",text=text,syntax="Auto",extension=extension,name=path,
        theme="gruvbox-dark"}
    end,
  }
end

local function rg_preview()
  return {
    type = "Dynamic",
    resolve = function(item, params)
      local path, line = item.value:match("^(.-):(%d+):%d+:")
      if not path then return nil end
      local text, binary = preview_read_text(preview_path(path,params.cwd), path)
      if binary then return {type="Text",text=text,syntax="PlainText",theme="gruvbox-dark"} end
      local extension = path:match("%.([^./\\]+)$")
      return {type="Text",text=text,name=path,
        syntax="Auto",extension=extension,
        theme="gruvbox-dark",
        center_line=tonumber(line),
        highlight_line=tonumber(line)}
    end,
  }
end

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
  return {type="Dynamic", resolve=function(item, params)
    local args={"pwsh.exe","-NoProfile","-CommandWithArgs",script}
    if extra then args[#args+1]=params[extra] or "" end
    args[#args+1]=item.value
    local result = preview.run {args=args,cwd=params.cwd}
    assert(result.exit_code == 0, "Preview command failed: " .. result.stderr)
    return {type="Ansi",bytes=result.stdout}
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
      preview = file_preview(),
    },
    ProfileFilesPreview = {
      type = "FileSystem", roots = {"."}, max_depth = 0,
      position = "Cursor", preview_visible = true,
      preview = file_preview(),
    },
    ProfileDirectories = {
      type = "FileSystem", roots = {"."}, directories_only = true,
      max_depth = 0, position = "Cursor", preview_visible = false,
    },
    ProfileRipgrep = {
      type = "Command", position = "Cursor", success_exit_codes = {0,1},
      args = {"rg.exe","--crlf","--max-columns=500","--max-columns-preview","--no-messages","--hidden","--color=never","-i","--vimgrep","--","{pattern}"},
      preview_visible = true,
      preview = rg_preview(),
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

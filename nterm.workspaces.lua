-- Config lookup: --lua-config=PATH, then ~/nterm.lua, then nterm.lua beside
-- the executable. Only the first available file is loaded.
local snippets_path = (os.getenv("USERPROFILE") or os.getenv("HOME")) .. "/.wtd/.wt.json"

local git = require("nterm.git")
-- PowerShell prompt, e.g. "PS C:\src\ghostty2> ".
local git_prompt = "^PS [^>]*> "

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
  local items = preview.directory_items(preview_path(path, cwd))
  if not items then return nil end
  local lines = {}
  for _, item in ipairs(items) do
    lines[#lines + 1] = item.name .. (item.is_directory and "/" or "")
  end
  return #lines > 0 and table.concat(lines, "\n") or "[Empty directory]"
end

local function pipe_file_preview()
  return {
    type = "Dynamic",
    resolve = function(item, params)
      local path = item.value
      local extension = path:match("%.([^./\\]+)$") or ""
      -- Windows cannot open a directory as a file. Avoid spawning dir for files.
      local probe = io.open(preview_path(path, params.cwd), "rb")
      local directory
      if probe then probe:close()
      else directory = preview_directory(path, params.cwd) end
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

local config = {
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
        local parsed, data = pcall(nterm.json_parse, content)
        if not parsed or type(data) ~= "table" then
          app:show_toast("Could not save snippet", "Unable to parse snippets JSON: " .. tostring(data), 5000)
          return
        end
        if type(data.snippets) ~= "table" then data.snippets = {} end
        table.insert(data.snippets, { input = selection })
        local encoded, json = pcall(nterm.json_encode_pretty, data)
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
      scope = "Workspace",
      position = "Bottom",
      preview_visible = false,
    },
    lines = {
      position = "FullPane",
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
      type = "FileSystem", roots = {"G:\\src", "C:\\src"}, max_depth = 0,
      position = "Cursor", preview_visible = false,
      preview = pipe_file_preview(),
      preview_visible = false,
    },
    ProfileFilesPreview = {
      type = "FileSystem", roots = {"G:\\src", "C:\\src"}, max_depth = 0,
      position = "Cursor", preview_visible = true,
      preview = pipe_file_preview(),
    },
    ProfileDirectories = {
      type = "FileSystem", roots = {"G:\\src", "C:\\src"}, directories_only = true,
      max_depth = 0, position = "Cursor", preview_visible = false,
    },
    ProfileRipgrep = {
      type = "Command", position = "Cursor", success_exit_codes = {0,1},
      args = {"rg.exe","--crlf","--max-columns=500","--max-columns-preview","--no-messages","--hidden","--color=never","-i","--vimgrep","--","{pattern}"},
      preview_visible = true,
      preview = rg_preview(),
    },
    ProfileHistory = {
      -- params.history is the PSReadLine history file, read newest first.
      type = "File", path = "{history}", reverse = true,
      line_continuation = "`", unique = true, sort = "SourceOrder", position = "Cursor",
      preview_visible = false,
      preview = profile_preview([==[$args[0] | & bat.exe --color=always --paging=never --wrap=never --style=plain --language=ps1 --theme="Visual Studio Dark+"]==]),
    },
    -- profile.example.ps1 streams CompletionMatches into this Items picker
    -- (piped into Invoke-NtermPicker -Name); the preview runs Get-Help.
    ProfileCompletions = {
      type="Items",items=nterm.json_parse("[]"),position="Cursor",preview_visible=true,
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
    "claude", "--print",
    "--model", "claude-haiku-5-5",
    "--output-format", "text",
    "--tools", "",
    "--no-session-persistence", prompt,
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
    ["Git commits"] = git.commits(),
    ["Git branches"] = git.branches({ prompt = git_prompt, require_clean = true }),
    ["Git status"] = git.status({ prompt = git_prompt }),
    ["Git cherry-pick"] = git.cherry_pick({ prompt = git_prompt }),
    ["Git cherry-pick commits"] = git.cherry_pick_commits({ prompt = git_prompt }),
    ["Git stashes"] = git.stashes({ prompt = git_prompt }),
    ["Git reflog"] = git.reflog(),
    ["Git file history"] = git.file_history({ prompt = git_prompt }),
    ["Git file history commits"] = git.file_history_commits(),
    ["Git fixup"] = git.fixups({ prompt = git_prompt }),
    ["Git tags"] = git.tags({ prompt = git_prompt }),
    Snippets = function(context)
      local input = context.line_before_cursor:match("^PS [^>]*>%s?(.*)$")
        or context.line_before_cursor
      local word = input:match("(%S+)$") or ""
      return {
        type = "Items",
        initial_query = word,
        position = "Cursor",
        items = function()
          local home = os.getenv("USERPROFILE") or os.getenv("HOME")
          local file = assert(io.open(home .. "/.wtd/.wt.json", "rb"))
          local content = file:read("*a")
          file:close()
          local data = nterm.json_parse(content)
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
    { key = "g", mods = "CTRL|ALT", picker = "Git commits" },
    { key = "b", mods = "CTRL|ALT", picker = "Git branches" },
    { key = "s", mods = "CTRL|ALT", picker = "Git status" },
  },
}

-- Workspace shortcuts use Alt; Shift moves panes instead of selecting them.
-- Keep Alt+Space for the existing quick-select shortcut.
local workspace_keys = {
  {key="Left",mods="ALT|SHIFT",action="ResizeLeft"},
  {key="Right",mods="ALT|SHIFT",action="ResizeRight"},
  {key="Up",mods="ALT|SHIFT",action="ResizeUp"},
  {key="Down",mods="ALT|SHIFT",action="ResizeDown"},
  {key="h",mods="ALT",action="FocusLeft"},
  {key="j",mods="ALT",action="FocusDown"},
  {key="k",mods="ALT",action="FocusUp"},
  {key="l",mods="ALT",action="FocusRight"},
  {key="h",mods="ALT|SHIFT",action="MoveLeft"},
  {key="j",mods="ALT|SHIFT",action="MoveDown"},
  {key="k",mods="ALT|SHIFT",action="MoveUp"},
  {key="l",mods="ALT|SHIFT",action="MoveRight"},
  {key="d",mods="ALT",action="SetDeck"},
  {key="d",mods="ALT|SHIFT",action="SetHorizontalGrid"},
  {key="g",mods="ALT",action="SetGrid"},
  {key="t",mods="ALT",action="SetTile"},
  {key="m",mods="ALT",action="SetMonocle"},
  {key="c",mods="ALT",action="CreatePane"},
  {key="c",mods="ALT|SHIFT",action="ClosePane"},
  {key="w",mods="ALT",action="WorkspacePicker"},
  {key="w",mods="ALT|SHIFT",action="CreateWorkspace"},
  {key="Enter",mods="ALT",action="MoveToMain"},
  {key="f",mods="ALT",action="FloatOrTilePane"},
  {key="n",mods="ALT",action="ResetLayout"},
}
for index=1,10 do
  workspace_keys[#workspace_keys+1]={key=tostring(index%10),mods="ALT",action="SelectWorkspace:"..index}
  workspace_keys[#workspace_keys+1]={key=tostring(index%10),mods="ALT|SHIFT",action="MoveToWorkspace:"..index}
end
for _, binding in ipairs(workspace_keys) do
  config.keys[#config.keys + 1] = binding
end

return config

-- Extended Windows sample: a working configuration, with what is of one
-- machine left as a placeholder. Change the roots, the remotes and the keys.
-- It goes with nterm-daemon.example.lua, copied to ~/nterm-daemon.lua: the
-- Files, Directories, Ripgrep and Git pickers are the session daemon's, by
-- the names that file gives them. rg and git have to be on the PATH of the
-- machine the shell is on. Ripgrep opens a match in nvim.
-- profile.example.ps1 binds PowerShell's keys to the same daemon's pickers.
-- Ask and Suggest run claude, and send it the last rows of the terminal.
-- Snippets use GHOSTTY_SNIPPETS_FILE or ~/ghostty-snippets.json; SaveSnippet
-- writes to that file, and makes it if it is not there.

-- Config lookup: --lua-config=PATH, then ~/nterm.lua, then nterm.lua beside
-- the executable. Only the first available file is loaded.
local clipboard = require("nterm.clipboard")
local ripgrep = require("nterm.ripgrep")
local claude = require("nterm.claude")
local snippets = require("nterm.snippets")
local snippets_path = os.getenv("GHOSTTY_SNIPPETS_FILE")
  or ((os.getenv("USERPROFILE") or os.getenv("HOME") or ".") .. "/ghostty-snippets.json")

-- A picker of the session daemon, run on the machine the pane's shell is on,
-- so it works the same in a workspace on a remote.
local function daemon(name, options)
  local picker = { type = "Daemon", picker = name, position = "Cursor" }
  for key, value in pairs(options or {}) do picker[key] = value end
  return picker
end

local config = {
  -- Prompts of programs that print no OSC 133 marks, for command navigation
  -- and the prompts picker. The list replaces the default patterns.
  prompt_patterns = {
    [[^PS [^>]*>\s*]],
    [[^\d+:\d+>\s*]],
    [[^›\s*]],
    [[^────────────────────*]],
    [[^> ]],
  },
  font = {
    size = 12,
  },
  picker_pipe = {
    enabled = true,
    allow_stream = true,
    allow_panes = true,
    named = {},
  },
  actions = {
    -- The clipboard's picture, sent to the machine the pane's shell is on
    -- and its path pasted; see lua/nterm/clipboard.lua.
    PasteImage = { label = "Paste picture from clipboard", action = clipboard.paste_image() },
    -- The selection kept in the snippets file; see lua/nterm/snippets.lua.
    SaveSnippet = { label = "Save selection as snippet", action = snippets.save { path = snippets_path } },
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
      cursor_line_background = "#a89984",
      cursor_line_text = "#1d2021",
    },
  },
  colors = {
    theme = "Gruvbox Dark",
  },
  floating = {
    padding = { top = 10, bottom = 10, left = 10, right = 10 },
  },
  workspaces = {
    default_layout = "horizontal_grid",
    -- The workspace picker (Alt+W) lists the folders inside these, to start
    -- a workspace in: for example { "C:/src", "D:/work" }.
    new_roots = { "~" },
    new_depth = 1, -- How many levels of folders inside them.
  },
  status_bar = { font_size = 16 },
  -- Machines whose nterm daemon a workspace's shells can run in, reached by
  -- ssh. Each has a row in the workspace picker. See docs/configuration.md.
  -- remotes = {
  --   vm = {
  --     address = "me@192.168.56.10",
  --     daemon = "C:\\Program Files\\nterm\\nterm-daemon.exe",
  --     command = "pwsh -nologo",
  --     clipboard_read = "allow",
  --     new_roots = { "C:/src" },
  --     new_depth = 1,
  --   },
  --   linux = {
  --     address = "me@linux-host",
  --     daemon = "/home/me/.local/bin/nterm-daemon",
  --   },
  -- },
  builtin_pickers = {
    command_palette = {
      position = "Bottom",
    },
    lines = {
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
        initial_query = before,
        position = "Cursor",
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
    -- Pickers of nterm-daemon.example.lua, which says what they look under:
    -- copy it to ~/nterm-daemon.lua and change its roots.
    Files = daemon("Files"),
    FilesHere = daemon("FilesHere"),
    Directories = daemon("SourceDirectories", {
      action = function(app, item)
        app:send_text("Set-Location -LiteralPath '" .. item.value:gsub("'", "''") .. "'\r")
      end,
    }),
    -- Grep asks for a pattern and shows Ripgrep on top with what is found.
    Grep = {
      type = "Argument",
      prompt = "Grep:",
      placeholder = "<text>  Search the working directory",
      action = function(app, item)
        if item.value == "" then return end
        app:push_picker("Ripgrep", { pattern = item.value })
      end,
    },
    Ripgrep = {
      type = "Daemon", picker = "Ripgrep",
      action = function(app, item)
        local path, line = ripgrep.place(item.value)
        if not path then return end
        -- Change nvim to your preferred editor.
        app:send_text("nvim +" .. line .. " '" .. path:gsub("'", "''") .. "'\r")
      end,
    },
    -- Asks for a question, then shows Claude's answer as a page on top.
    Ask = claude.ask { page = "Answer" },
    Answer = claude.answer(),
    -- Asks what you want done, then shows commands from Claude that do it;
    -- Enter types one at the shell's cursor.
    Suggest = claude.suggest { page = "Suggestions", position = "Cursor" },
    Suggestions = claude.suggestions(),
    command_history = {
      type = "Scrollback",
      preview = { type = "Scrollback", rows = 12 },
      prompts = true,
    },
    -- The repository the shell is in; see lua/nterm/git.lua.
    ["Git commits"] = daemon("Git commits"),
    ["Git branches"] = daemon("Git branches"),
    ["Git status"] = daemon("Git status"),
    ["Git cherry-pick"] = daemon("Git cherry-pick"),
    ["Git cherry-pick commits"] = daemon("Git cherry-pick commits"),
    ["Git stashes"] = daemon("Git stashes"),
    ["Git reflog"] = daemon("Git reflog"),
    ["Git file history"] = daemon("Git file history"),
    ["Git file history commits"] = daemon("Git file history commits"),
    ["Git fixup"] = daemon("Git fixup"),
    ["Git tags"] = daemon("Git tags"),
    Snippets = snippets.picker { path = snippets_path },
  },
  -- Merged over the default keys by chord; see docs/configuration.md.
  keys = {
    { key = "s", mods = "CTRL|SHIFT", action = "SaveSnippet" },
    { key = "a", mods = "CTRL|SHIFT", picker = "Ask" },
    { key = "u", mods = "CTRL|SHIFT", picker = "Suggest" },
    { key = "g", mods = "CTRL|SHIFT", picker = "Grep" },
    { key = "b", mods = "CTRL|SHIFT", picker = "Snippets" },
    { key = ".", mods = "CTRL|SHIFT", builtin_picker = "words" },
    { key = "z", mods = "CTRL|SHIFT", picker = "command_history" },
    { key = "g", mods = "CTRL|ALT", picker = "Git commits" },
    { key = "b", mods = "CTRL|ALT", picker = "Git branches" },
    { key = "s", mods = "CTRL|ALT", picker = "Git status" },
    { key = "t", mods = "ALT|SHIFT", picker = "Files" },
    { key = "o", mods = "ALT|SHIFT", picker = "Directories" },
  },
}

-- Workspace keys on Alt, beside the default ones on Ctrl+Alt.
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

-- Config lookup: --lua-config=PATH, then ~/ghostty.lua, then ghostty.lua beside
-- the executable. Only the first available file is loaded.
return {
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
  pickers = {
    lines = {
      type = "Scrollback",
      preview = { type = "Scrollback", rows = 12 },
      -- Default regex extracts each nonblank logical line, trimming its padding.
      enter_action = "Navigate",
    },
    words = {
      type = "Scrollback",
      regex = [[\S+]],
      enter_action = "SendInput",
    },
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
    { key = "f", mods = "CTRL|SHIFT", picker = "lines" },
    { key = "p", mods = "CTRL|SHIFT", picker = "words" },
    { key = "o", mods = "CTRL|SHIFT", picker = "files" },
  },
}

-- Config lookup: --lua-config=PATH, then ~/config.lua, then config.lua beside
-- the executable. Only the first available file is loaded.
return {
  font = {
    -- family = "JetBrains Mono", -- Uncomment and choose an installed font.
    size = 12,
  },
  terminal = {
    scrollback_limit = 100000000, -- Bytes; zero disables scrollback.
  },
  colors = {
    -- theme = "Gruvbox Dark", -- Uses Ghostty's theme resolution.
  },
  pickers = {
    lines = {
      type = "Scrollback",
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
      roots = { [[G:\src, C:\src]] },
      files_only = true,
      max_depth = 0, -- Zero means unlimited.
      enter_action = "SendInput",
    },
  },
  keys = {
    { key = "f", mods = "CTRL|SHIFT", picker = "lines" },
    { key = "p", mods = "CTRL|SHIFT", picker = "words" },
    { key = "o", mods = "CTRL|SHIFT", picker = "files" },
  },
}

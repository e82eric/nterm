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
}

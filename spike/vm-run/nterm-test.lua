-- Test VM config: Ctrl+Alt chords do not reach nterm through VBoxManage
-- scancodes, so the pane keys get Ctrl+Shift ones.
return {
  keys = {
    { key = "p", mods = "CTRL|SHIFT", action = "CreatePane" },
    { key = "h", mods = "CTRL|SHIFT", action = "FocusLeft" },
    { key = "l", mods = "CTRL|SHIFT", action = "FocusRight" },
  },
}

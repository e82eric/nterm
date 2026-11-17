-- Test VM config: Ctrl+Alt chords do not reach nterm through VBoxManage
-- scancodes, so the pane keys get Ctrl+Shift ones.
return {
  picker_pipe = { enabled = true, named = { "builtin.lines" }, allow_stream = true },
  keys = {
    { key = "p", mods = "CTRL|SHIFT", action = "CreatePane" },
    { key = "h", mods = "CTRL|SHIFT", action = "FocusLeft" },
    { key = "l", mods = "CTRL|SHIFT", action = "FocusRight" },
    { key = "u", mods = "CTRL|SHIFT", action = "CreateWorkspace" },
    { key = "j", mods = "CTRL|SHIFT", action = "SelectWorkspace:1" },
    { key = "k", mods = "CTRL|SHIFT", action = "SelectWorkspace:2" },
    { key = "g", mods = "CTRL|SHIFT", action = "SetGrid" },
    { key = "t", mods = "CTRL|SHIFT", action = "SetHorizontalGrid" },
    { key = "d", mods = "CTRL|SHIFT", action = "DetachWorkspace" },
    { key = "z", mods = "CTRL|SHIFT", action = "EndWorkspace" },
    { key = "o", mods = "CTRL|SHIFT", action = "WorkspacePicker" },
  },
}

-- Launch with --lua-config=example/rust-egui-terminal/workspaces.example.lua.
-- Each native window gets an independent set of workspaces.
local keys = ghostty.workspace_keys("CTRL|ALT")
keys[#keys + 1] = {key="n",mods="CTRL|SHIFT",action="NewWindow"}
keys[#keys + 1] = {key="e",mods="CTRL|SHIFT",action="ToggleFloatingPane"}

return {
  keys = keys,
  workspaces = {
    inner_gap = 10, outer_gap = 10, padding = 4,
    corner_radius = 8, border_width = 1.5,
    shadow_size = 10, shadow_opacity = 0.3, shadow_offset = {0, 2},
  },
  -- Runs once after each native window is ready. Reloading does not rerun it.
  startup = function(app)
    local first = app:get_workspace()
    app:create_pane {workspace=first.id}
    local servers = app:ensure_workspace {index=2, layout="deck"}
    -- Newly created workspaces already contain one terminal pane.
    -- command is a command-line string; cwd is an optional directory.
    -- app:create_pane {workspace=servers.id, command="pwsh.exe -NoLogo", cwd="C:/src"}
    app:select_workspace(first.id)
  end,
}

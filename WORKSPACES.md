# Workspaces and layouts

Each native window owns independent indexed workspaces. A workspace remembers its
ordered terminal panes, focused pane, layout, and main size. Switching
workspaces or layouts preserves the running terminals and scrollback. Each
workspace also owns its floating panes: floating shortcuts, cycling, and the
palette's restore entries only see the current workspace's floats, and each
workspace remembers which of its floats was visible. Switching away hides the
overlay; returning shows that workspace's float again without restarting the
terminal.

Indexes start at 1 and do not change when another workspace is removed. Creation
uses the lowest available index unless Lua supplies one. Use stable IDs for
automation. New workspaces automatically
start with one terminal pane. Closing or moving their last tiled pane removes the
workspace, closing its floating panes, and selects the last-used remaining
workspace when available. Closing
the final pane in the final workspace closes its native window. Other native
windows remain open. Closing a native window releases all its terminals.

The workspace picker and move-pane destination picker use NFM with fuzzy search
by index. Both appear at the bottom of the workspace, spanning its panes,
and show each workspace's layout, pane count, and current-workspace marker.
Enter accepts the selected workspace; Escape dismisses the picker.

## Default bindings

The workspace modifier is **Ctrl+Alt** by default. Existing shortcuts for windows,
floating panes, copy mode, and pickers remain available.

| Keys | Action |
| --- | --- |
| Ctrl+Alt+1…9,0 | Select workspace index 1…10 |
| Ctrl+Alt+Shift+1…9,0 | Move focused pane, or the visible float, to an existing workspace |
| Ctrl+Alt+W | Workspace picker; type an index, arrows and Enter to select |
| Ctrl+Alt+Shift+W | Create a workspace |
| Ctrl+Alt+C | Create terminal pane (a float while a float is visible) |
| Ctrl+Alt+Shift+C | Close focused terminal pane |
| Ctrl+Alt+H/J/K/L | Focus left/down/up/right |
| Ctrl+Alt+Shift+H/J/K/L | Move pane left/down/up/right |
| Ctrl+Alt+Shift+arrows | Resize panes by moving a shared divider |
| Ctrl+Alt+Enter | Promote focused pane to main |
| Ctrl+Alt+F | Float the focused pane, or tile the visible floating pane |
| Ctrl+Alt+T/M/Y/U | Tile / monocle / deck / grid |
| Ctrl+Alt+Shift+Y | Horizontal grid (two rows, all panes visible) |
| Ctrl+Alt+N | Reset main size and individual pane sizes |

`FloatOrTilePane` (Ctrl+Alt+F) moves a terminal between the tiled layout and the
workspace's floating panes without restarting it. A tiled pane becomes the
visible float; the visible float becomes a tiled pane after the focused pane and
takes focus. A workspace keeps at least one tiled pane, and the window's first
pane cannot float because it owns the window's terminal app.

While a floating pane is visible, the focus keys cycle through the workspace's
floating panes instead of the tiled ones: H/K go to the previous float, J/L
to the next, in creation order and wrapping around. Hide the float to return to tiled focus.

A workspace's floats act as a layer over its tiled panes. While a float is
visible, the layer has focus: `CreatePane` (Ctrl+Alt+C) creates a new float
instead of a tiled pane. Closing or moving away the visible float shows the
workspace's most recently focused remaining float, and the layer hides only
when no floats remain. Tiling the visible float hides the layer, because the
new tiled pane takes focus.

The move-pane shortcuts and picker act on the visible float when one is shown,
otherwise on the focused tiled pane. A float moved to an existing workspace
stays floating and becomes the destination's visible float, so it is showing
when you select that workspace; you stay on the current workspace.
`MovePaneToNewWorkspace` moves the pane into a new workspace at the
lowest free index; you stay on the current workspace. A float moved this way becomes the new
workspace's tiled pane, since every workspace needs one. As with other moves,
moving a workspace's last tiled pane removes that workspace and closes its
floats.

The bar shows a clickable button per workspace index next to the status text.
The active workspace is filled with the accent color, workspaces containing
panes get a subtle fill, and empty ones are outlined. The status text after the buttons shows `layout · focused/total` panes
(in deck layout panes are numbered by creation order), before the pipe
connection indicator. Index 10 is displayed as `0`, matching its shortcut.
`+N` opens the picker when indexes overflow the available space. Create
workspaces through the shortcut or palette.

The command palette also exposes remove, creation, previous/next/
last workspace, move-pane workspace picker, move pane to new workspace,
tile-reversed, and horizontal-grid. Bind their action names in Lua:
`RemoveWorkspace`, `CreateWorkspace`, `NextWorkspace`,
`PreviousWorkspace`, `LastWorkspace`,
`MovePaneToWorkspacePicker`, `MovePaneToNewWorkspace`,
`SetTileReversed`, and `SetHorizontalGrid`.

`ResizeLeft`, `ResizeRight`, `ResizeUp`, and `ResizeDown` move a shared divider
in that direction; hold the key to repeat. Left/right adjust the outer split in
tile, reversed tile, deck, and grid. Up/down adjust rows beside the focused pane
in tile and grid. Horizontal grid transposes these axes: up/down move the outer
split, and left/right adjust panes within the focused row. When the focused pane
is at an outer edge, its opposite shared divider moves instead. Deck has no
vertical subdivision; monocle and single-pane workspaces do not resize.
Sizes stay with the workspace and pane IDs through focus changes. The main
split is shared by all layouts; individual pane sizes are kept per layout, so
resizing the tile stack does not change grid rows. `ResetLayout` restores the
default main ratio and equal pane sizes in the current layout. The layout reserves a
48-pixel minimum per pane along each split when space permits.

The copied `ghostty.workspaces.lua` binds **Alt+Shift+arrows** to these commands.
To use other keys or modifiers, add bindings such as:

```lua
{key="Left", mods="ALT|CTRL", action="ResizeLeft"},
{key="Down", mods="ALT|CTRL", action="ResizeDown"},
```

`ghostty.workspace_keys("ALT")` generates the SWM-style bindings with a
different modifier. Supplying `keys` replaces default bindings, so append the
other application shortcuts you want. See [workspaces.example.lua](workspaces.example.lua).

## Layout behavior

Tile has one main pane on the left and a vertical stack on the right.
Tile-reversed places the main pane on the right. Monocle shows the focused pane alone.
Deck shows the main pane beside the selected stack pane. Grid uses SimpleWindowManager's
two-column arrangement, with `floor(n/2)` panes in the first column. Horizontal
grid transposes that arrangement into two rows: `floor(n/2)` panes across the
top and the remainder across the bottom. All panes are visible. Main size
adjusts the column balance in grid and row balance in horizontal grid.

Focus and movement follow visible geometry in tile/grid/horizontal grid. In
monocle, directional navigation cycles the pane order. In deck, H/L switch between
the main pane and stack; J/K rotate deck contents forward/backward
while focus remains on the same side, matching SWM. Cycling in the stack wraps
within the stack rather than jumping to the main pane. Cycling from the main pane uses SWM's
main-content rotation. Moving a pane left or right swaps the two visible panes;
moving up or down cycles like J/K. As in SWM, pane order is the deck's state:
focusing a hidden pane (from Lua, or by switching to deck) rotates the stack to show it, and it
stays shown while focus is on the main pane. Click a visible pane to focus it. Hidden terminals continue
running at their last terminal size. The old `horizontal_deck` layout name and
`SetHorizontalDeck` action remain aliases for horizontal grid.

## Lua automation and appearance

Pickers accept `scope = "Pane"` (the default) or `scope = "Workspace"`. Workspace
scope places the overlay over the workspace content, excluding the status bar,
while cwd, scrollback, selection, and callback output still belong to the pane
that opened it. Cursor placement stays anchored to that pane's cursor. Clicking
inside a workspace picker does not focus a terminal underneath it. Switching
workspaces, moving focus away from the originating pane, or closing that pane
cancels the picker. Other terminal processes keep running.

The command palette defaults to workspace scope and centered placement. Override
it explicitly through Lua:

```lua
builtin_pickers = {
  command_palette = { scope="Workspace", position="Center" },
}
```

Custom picker definitions and factory results accept the same `scope` field.
For example, `scope="Workspace", position="FullPane"` fills the workspace;
`scope="Pane"` restores pane-local command-palette placement.

`startup = function(app) ... end` runs once after each native window is ready.
Reload updates settings, keybindings, and actions without rerunning startup or
recreating terminals. Runtime APIs are available inside startup, named actions,
and picker callbacks. Retained `app` handles expire when the callback returns.

```lua
return {
  workspaces = {
    default_layout="horizontal_grid",
    inner_gap=10, outer_gap=10, padding=4, corner_radius=8, border_width=1.5,
    shadow_size=10, shadow_opacity=0.3, shadow_offset={0,2},
  },
  startup = function(app)
    local ws = app:ensure_workspace {index=2, layout="tile"}
    app:create_pane {workspace=ws.id, cwd="C:/src", command="pwsh.exe -NoLogo"}
    app:select_workspace(ws.id)
  end,
}
```

Commands are command-line strings, matching the existing floating-pane API.
Omit `command`/`cwd` to use native terminal defaults. Dimensions are logical pixels
and scale with DPI. Shadows are painted in the gaps; actual terminal textures are
clipped to rounded corners. A workspace containing one pane automatically removes
gaps, borders, rounded corners, and shadows; configured padding remains. The
configured appearance returns when another pane is added. Dimensions must be
0–256; opacity must be 0–1.

`workspaces.default_layout` (default `tile`) is the layout of each window's first
workspace and of workspaces created without an explicit `layout`, including
`CreateWorkspace`. It accepts the same names as `app:set_workspace_layout`.

| Method | Result / behavior |
| --- | --- |
| `app:create_workspace {index=?,layout=?}` | Workspace description; starts with one focused pane |
| `app:ensure_workspace {...}` | Existing workspace matching index, otherwise creates it |
| `app:get_workspaces()` | Workspace descriptions in index order |
| `app:get_workspace(id?)` | Description, current by default; nil for missing ID |
| `app:select_workspace(id)` | Switch workspace |
| `app:remove_workspace(id)` | Remove a workspace without tiled panes; its floating panes close |
| `app:set_workspace_layout(id,layout)` | `tile`, `tile_reversed`, `monocle`, `deck`, `horizontal_grid`, `grid` |
| `app:set_main_ratio(id,ratio)` | Main fraction from 0.1 to 0.9 |
| `app:set_workspace_appearance(options)` | Change window defaults |
| `app:set_workspace_style(id,options)` | Set per-workspace appearance; omitted fields use standard defaults |
| `app:create_pane {workspace=?,command=?,cwd=?}` | `{id,workspace}`; focuses pane within destination workspace |
| `app:get_panes()` | Pane IDs, workspace IDs, and remembered focus flags |
| `app:focus_pane(id)` | Switch workspace and focus pane |
| `app:move_pane(id,workspace_id)` | Move without restarting; stay on current workspace |
| `app:move_float(id,workspace_id)` | Move a floating pane; it becomes the destination's visible float; stay on current workspace |
| `app:close_pane(id)` | Close terminal; remove empty workspace, or close window if it was the final pane |
| `app:float_pane(id)` | Turn a tiled pane into a floating pane in its workspace; returns the float's description |
| `app:tile_floating_pane(id_or_name?)` | Turn the current workspace's float (default: last focused) into a tiled pane after the focused pane; returns `{id,workspace}` |

Workspace descriptions include `id`, `index`, ordered `panes`, `focused`, `layout`,
`main_ratio`, optional `appearance`, ordered `floats`, and `visible_float`. IDs are window-local and valid for
that window's lifetime. Workspace indexes are display metadata; Lua operations
take IDs unless a method explicitly accepts an options table.

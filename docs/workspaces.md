# Workspaces and layouts

Each native window owns independent indexed workspaces. A workspace remembers its
ordered terminal panes, focused pane, layout, and main size. Switching
workspaces or layouts preserves the running terminals and scrollback. Each
workspace also owns its floating panes: floating shortcuts, cycling, and the
palette's restore entries only see the current workspace's floats, and each
workspace remembers which of its floats was visible. Switching away hides the
overlay; returning shows that workspace's float again without restarting the
terminal.

Indexes are positions: workspaces are always numbered 1…n, and removing one
renumbers the workspaces after it, like tmux's `renumber-windows`. New
workspaces take the next index, n+1. A Lua `index` above n+1 would leave a gap
and is an error. Selecting or moving to an index past the last workspace does
nothing but report an error. Indexes can change, so use stable IDs for
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
| Ctrl+Shift+W | Close the visible float, or else the focused terminal pane (`ClosePane`) |
| Ctrl+Alt+H/J/K/L | Focus left/down/up/right |
| Ctrl+Alt+Shift+H/J/K/L | Move pane left/down/up/right, or the visible float |
| Ctrl+Alt+Shift+arrows | Resize panes by moving a shared divider |
| Ctrl+Alt+Enter | Promote focused pane to main |
| Ctrl+Alt+F | Float the focused pane, or tile the visible floating pane |
| Ctrl+Alt+T/M/Y/U | Tile / monocle / deck / grid |
| Ctrl+Alt+Shift+Y | Horizontal grid (two rows, all panes visible) |
| Ctrl+Alt+N | Reset main size and individual pane sizes |

`FloatOrTilePane` (Ctrl+Alt+F) moves a terminal between the tiled layout and the
workspace's floating panes without restarting it. A tiled pane becomes the
visible float; the visible float becomes a tiled pane after the focused pane and
takes focus. A workspace keeps at least one tiled pane.

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
`MovePaneToNewWorkspace` moves the pane into a new last workspace; you stay on
the current workspace. A float moved this way becomes the new
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
move pane to new window, tile-reversed, and horizontal-grid. Bind their
action names in Lua: `RemoveWorkspace`, `CreateWorkspace`, `NextWorkspace`,
`PreviousWorkspace`, `LastWorkspace`,
`MovePaneToWorkspacePicker`, `MovePaneToNewWorkspace`, `MovePaneToNewWindow`,
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
default main ratio and equal pane sizes in the current layout, and the default
floating pane size. The layout reserves a 48-pixel minimum per pane along each
split when space permits.

While a floating pane is shown, the resize commands resize it instead: each
press moves one edge by 5% of the workspace in the key's direction.
`ResizeLeft`/`ResizeRight` move its left edge and `ResizeUp`/`ResizeDown` its top
edge; `ResizeFarLeft`, `ResizeFarRight`, `ResizeFarUp`, and `ResizeFarDown` move
its right and bottom edges. Tiled panes work the same way: where the focused
pane has dividers on both sides, as in the middle of a stack, the plain
commands move the one above or to the left and the `ResizeFar` commands the one
below or to the right. Where it has a divider on only one side, both move it.
Dividers and floating pane edges can also be
dragged with the mouse. Floating pane sizes are kept per workspace as a share
of the window, and do not change the configured `floating.padding`.

**Ctrl+Shift+R** enters the built-in resize key table (see
[Key tables](#key-tables)), and the copied `nterm.workspaces.lua` also binds
**Alt+Shift+arrows** to these commands. To use other keys or modifiers, add
bindings such as:

```lua
{key="Left", mods="ALT|CTRL", action="ResizeLeft"},
{key="Down", mods="ALT|CTRL", action="ResizeDown"},
```

### Key tables

A key table is a named set of keys that becomes the only active keys until you
leave it, like a resize mode. The action `EnterKeyTable:<name>` enters one.

nterm has two built-in tables. `resize`, entered with **Ctrl+Shift+R**:

| Keys | Action |
|---|---|
| `h` `j` `k` `l`, or the arrows | `ResizeLeft`, `ResizeDown`, `ResizeUp`, `ResizeRight` |
| the same with Shift (the table's `far` modifier) | `ResizeFarLeft`, `ResizeFarDown`, `ResizeFarUp`, `ResizeFarRight` |
| any of these with Ctrl added (the `nudge` modifier) | the same, by a much smaller step (1% instead of 5%) |
| the same with Alt | `FocusLeft`, `FocusDown`, `FocusUp`, `FocusRight`: pick another pane to resize, or with a float shown, cycle the floats |
| `=` | `ResetLayout` |
| `m` | switch to the `move` table |
| `Enter` or `Escape` | leave the table |

And `move`, entered with **Ctrl+Shift+G**:

| Keys | Action |
|---|---|
| `h` `j` `k` `l`, or the arrows | `MoveLeft`, `MoveDown`, `MoveUp`, `MoveRight`: swap the focused pane with its neighbour that way, or move a shown float |
| the same with Shift (`far`) | `MoveFarLeft`, `MoveFarDown`, `MoveFarUp`, `MoveFarRight`: move the pane as far as it goes, or a float against that edge of the window |
| any of these with Ctrl added (`nudge`) | for a float, the same by a much smaller step |
| the same with Alt | `FocusLeft`, `FocusDown`, `FocusUp`, `FocusRight` |
| `c` | `CenterFloat`: centre a shown float, keeping its size |
| `r` | switch to the `resize` table |
| `Enter` or `Escape` | leave the table |

Moving a tiled pane swaps it with the neighbour the layout puts that way, as
the `Move*` shortcuts always have, so it changes the pane's place in the layout
rather than its size. Moving as far as it goes repeats that until nothing lies
further; in Monocle it moves the pane to the start or end of the stack, and in
Deck it is the same as one move. With a float shown, `Move*` and `MoveFar*`
move the float instead, keeping its size, as `Resize*` resizes it.

Tables in `key_tables` use the same fields as `keys`, and merge with the
built-in tables the same way: a table named `resize` adds its keys to the
built-in one, replaces any built-in key it also binds, and removes one with
`action = "None"`. Other names make new tables. `disable_default_keys = true`
leaves out the built-in tables and their Ctrl+Shift+R and Ctrl+Shift+G bindings
too.

```lua
key_tables = {
  -- Add w/a/s/d to the built-in resize table and drop Enter.
  resize = {
    {key="w", action="ResizeUp"},   {key="s", action="ResizeDown"},
    {key="a", action="ResizeLeft"}, {key="d", action="ResizeRight"},
    {key="Enter", action="None"},
  },
},
keys = {
  -- Enter resize mode with Alt+R instead of Ctrl+Shift+R.
  {key="r", mods="ALT", action="EnterKeyTable:resize"},
  {key="r", mods="CTRL|SHIFT", action="None"},
},
```

A table can also name a `far` and a `nudge` modifier. Held with a key the
table does not bind with that modifier, `far` runs the `Far` version of the
key's `Resize*` or `Move*` action, and `nudge` runs a resize or move by the
much smaller step; both can be held at once. Other actions are left alone.
The built-in tables use `far = "SHIFT"` and `nudge = "CTRL"`, which is all
their Shift and Ctrl keys are. A configured table of the same name keeps those
unless it sets its own, and `"NONE"` turns one off. A key bound with the
modifier itself is an error when the modifier would change that key's action,
since it could never take effect: unbind the key, or pick another modifier.

```lua
key_tables = {
  -- Shift+Alt moves as far as it goes in the move table; no nudging.
  move = { far = "SHIFT|ALT", nudge = "NONE" },
},
```

While a table with resize or move keys is active, the focused pane, or a shown
floating pane, is the only thing in focus: everything else is darkened, and the
pane sits on a slightly larger backdrop tinted with the theme's warning colour.
Its border is drawn in a dimmed warning colour, in place of the focus
highlight. For resizing, a bright warning-coloured square sits on the corner
whose edges the next key press moves. Holding the table's `far` modifier
(Shift) switches a pane between two neighbours to its other edge, and a float
from its left and top edges to its right and bottom ones. For moving, an arrow
sits on each side the pane can move toward.

While a table is active, the status bar's mode chip shows its name, followed by
hints for its keys: what the keys move now, what the `far` modifier switches
to, the `nudge` modifier, the keys that move focus, centring a float,
switching to the other table, reset, and leaving. The hints use the table's
own keys and modifiers, so they follow any rebinding. The
focused terminal's size in columns×rows is shown on the right. The table's keys run their
actions (held keys repeat for actions that repeat), and every other key is
ignored, so nothing is typed into the terminal. Escape leaves the table unless
the table binds it, as does `ExitKeyTable`, switching away from the window, or
reloading the configuration. The command palette lists each table.

`nterm.workspace_keys("ALT")` generates the SWM-style bindings with a
different modifier. They are added to the default Ctrl+Alt bindings, which stay
active; set `disable_default_keys = true` to use only your `keys`. See
[workspaces.example.lua](../workspaces.example.lua).

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
| `app:focus_direction(dir)` | Same as `FocusLeft`/`Right`/`Up`/`Down`; `dir` is `"left"`, `"right"`, `"up"`, or `"down"` |
| `app:move_pane_direction(dir)` | Same as `MoveLeft`/`Right`/`Up`/`Down` |
| `app:move_pane(id,workspace_id)` | Move without restarting; stay on current workspace |
| `app:move_float(id,workspace_id)` | Move a floating pane; it becomes the destination's visible float; stay on current workspace |
| `app:move_pane_to_new_window(id)` | Move a tiled pane into a new window without restarting |
| `app:move_float_to_new_window(id)` | Move a floating pane into a new window as its tiled pane |
| `app:close_pane(id)` | Close terminal; remove empty workspace, or close window if it was the final pane |
| `app:float_pane(id)` | Turn a tiled pane into a floating pane in its workspace; returns the float's description |
| `app:tile_floating_pane(id_or_name?)` | Turn the current workspace's float (default: last focused) into a tiled pane after the focused pane; returns `{id,workspace}` |

Workspace descriptions include `id`, `index`, ordered `panes`, `focused`, `layout`,
`main_ratio`, optional `appearance`, ordered `floats`, and `visible_float`. IDs are window-local and valid for
that window's lifetime. Workspace indexes are positions that change when an
earlier workspace is removed; Lua operations
take IDs unless a method explicitly accepts an options table.

## Windows

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> to open another terminal in
a new window within the same process. Each window has its own shell, terminal
view, floating pane, and UI state. Windows share the native Ghostty app and its
configuration; theme and font changes apply to all windows. Closing a window
closes its terminals, and the app exits after the last window closes.

`MovePaneToNewWindow` (unbound by default; also in the command palette) moves
the visible float, or else the focused tiled pane, into a new window. The shell
keeps running, and copy mode, search, quick select, and pane-scope pickers
stay open; a workspace-scope picker is cancelled. A float becomes the new
window's tiled pane. The new window starts with one workspace and does not run
`startup`. Any pane can move, including the window's first. Moving a
workspace's last tiled pane removes that
workspace and closes its floats, as with other moves, and moving a window's
last pane closes that window.

## Floating panes

Floating panes open above a workspace's tiled panes. Press
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>E</kbd> to show or hide one; the first press
creates it, and hiding it keeps its shell running. While visible, it receives
keyboard, paste, and mouse input. Exiting its shell, or pressing
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>W</kbd>, closes only that floating pane.

The command palette includes a restore entry for each of the current workspace's
floating panes, including hidden ones. Entries always show its stable ID,
followed by its explicit name
or current terminal title, and mark the visible pane as active. Labels are
captured when the palette opens; selecting an entry restores that pane by ID.
If its shell exits while the palette is open, selection reports that the pane
has closed.

Each workspace owns its floating panes, with one visible at a time. The actions below act on the current
workspace's floats. Hidden panes, and floats of other workspaces, keep their
shells running. Floats close with their workspace. The default shortcuts are:

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+E | `ToggleFloatingPane` |
| Ctrl+Shift+W | `ClosePane` (closes the visible float) |

These action names can also be assigned in Lua `keys`. Toggle hides the visible
pane or restores the last focused pane, creating one if none exist. `CreatePane`
adds another float while one is shown; `CreateFloatingPane` (command palette,
no default shortcut) adds one at any time. While a float is
shown, the focus actions and `app:focus_direction` treat the floats like a
Monocle layout: down or right shows the next float in creation order, up or
left the previous one, wrapping around. Closing the visible pane restores the most recently
focused remaining pane; closing a hidden pane leaves the current visibility
unchanged. A shell exiting removes its pane in the same way.

Named actions and picker selection callbacks support:

```lua
actions = {
  ToolsPane = {
    label = "Tools pane",
    action = function(app)
      local pane = app:get_floating_pane("tools")
      if pane then
        app:restore_floating_pane(pane.id)
      else
        pane = app:create_floating_pane({
          name = "tools", command = "pwsh.exe", cwd = "C:\\src",
        })
        app:show_toast("Floating pane", "Created " .. pane.name .. " (" .. pane.id .. ")")
      end
    end,
  },
},
keys = {
  { key = "t", mods = "CTRL|ALT", action = "ToolsPane" },
},
```

`app:create_floating_pane(options)` returns `{id, name, workspace, visible, focused}` after
native creation succeeds. All options are optional; omitted command and working
directory use the normal terminal defaults. `command` is a Ghostty command
string. Names must be nonempty and unique among the workspace's floating panes.
IDs are stable, monotonically increasing within a window, and never reused.

`app:get_floating_pane(id_or_name, workspace_id?)` returns a description or
`nil`; with no target it returns the last focused floating pane. It searches the
current workspace unless given a workspace ID; to search every workspace, loop
over `app:get_workspaces()`. `app:get_floating_panes(workspace_id?)` returns a
workspace's descriptions in creation order. Descriptions are snapshots; `focused`
means the pane is the window's terminal input target, even if the OS window
itself is unfocused.

`app:restore_floating_pane(id_or_name)`, `app:toggle_floating_pane()`, and
`app:close_floating_pane(id_or_name)` change pane state immediately and act on
the current workspace's floats. `app:tile_floating_pane` and `app:float_pane`
move terminals between floating and tiled; see
[Lua automation and appearance](#lua-automation-and-appearance).
Close without a target closes the last focused pane, including when hidden.
Restore or explicit close of a missing pane raises a Lua error; lookup returns
`nil`. Creation errors are synchronous and can be handled with `pcall`.
Pane operations already performed remain in effect if a callback subsequently
fails. Queued paste/send-text commands still apply to the originating pane and
are discarded if that pane was closed by the callback. Retained app handles
expire when their callback returns.


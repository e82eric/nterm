# Shared UI colors

`src/colors.rs` owns the active `UiPalette` through `COLORS`. Components read
`colors::palette()` and use semantic roles rather than defining color literals.
The palette is a small copyable value; capture it once when drawing several
elements. `COLORS.set(ctx, palette)` replaces it, applies egui styling, and
requests a repaint. At startup the C shim copies colors from Ghostty's finalized
configuration before it is freed. The manager derives the UI palette and applies
Lua overrides last. The command palette's **Choose theme** action loads a
candidate from retained startup inputs, validates it, calls
`ghostty_app_update_config` for the shared app, then updates the UI palette.
The choice is session-only. The native update API returns void; internal update
errors are logged by Ghostty rather than returned to the picker.

Precedence: default palette -> resolved Ghostty colors -> `colors.ui` overrides.
Ghostty's resolved colors already include explicit native/CLI color overrides.
Selecting a theme in the picker replaces native/CLI terminal color overrides
for the running session. Other terminal settings and Lua UI overrides remain.

```lua
return {
    colors = {
        theme = "Gruvbox Dark",
        ui = {
            accent = "#fabd2f",
            panel_background = "#1d2021",
        },
    },
}
```

All color roles accept `#RRGGBB` or `RRGGBB` strings. Omitted
roles inherit derived values. Invalid strings/types or unknown roles reject the
Lua configuration and appear in configuration Messages. Changes support manual config reload.
Explicit overrides are applied exactly, without contrast correction.

Background/text use resolved background/foreground. Panel, border, and muted
text blend foreground into background at 6%, 20%, and 60%. Accent uses ANSI cyan
(6), success green (2), warning and match highlight yellow (3), and error red (1).
Muted and semantic text colors are adjusted toward black/white when necessary to
reach 4.5:1 contrast against the panel. Literal selection colors are preserved;
missing or cell-relative selection colors use a derived background and readable
foreground because there is no individual terminal cell to resolve against.

| Role | Usage |
| --- | --- |
| background | UI backdrop, unstyled preview background, hint background |
| text | Main text, unstyled preview foreground |
| panel_background | Status bar, Messages, pickers, search, toasts |
| muted_text | Secondary labels and inactive indicators |
| border | Outlines, dividers, inactive status label |
| selection_background | Selected rows, editable text selections, snapshot selections |
| selection_text | Text on selection backgrounds |
| accent | Active status, caret, picker indicator, quick-select hint |
| copy_cursor_background | Copy-mode cursor fill in the terminal and NFM preview; defaults to accent |
| success | Copied confirmation |
| warning | Persistent configuration issue indicator |
| error | Picker failures, invalid search, egui error text |
| match_highlight | Selected search match; also the fallback for picker matched characters |
| picker_match_highlight | Picker matched characters only; defaults to `match_highlight` when omitted |
| search_match_background | Secondary search matches; defaults to ANSI yellow blended 30% into terminal background |
| search_match_text | Text in secondary search matches; defaults to readable terminal foreground |
| yank_background | Peak of the picker preview yank pulse; defaults to the preview selection pushed toward the text color |

Copy-mode yanks in the terminal and the picker preview pulse the background of
the copied text for 250 ms. The pulse uses the selection background pushed
toward the text color, so it is lighter than the selection on dark themes and
darker on light ones. It holds steady and softens once just before it clears.
This applies to every yank, including `yy`, `y$`, and `Y` line hints. Only the
background changes, so the copied text keeps its colors. In the terminal, cells
with a colored background invert instead, as they do in the selection, and stay
inverted until the pulse clears.
The terminal pulse follows the theme's selection background, including
`selection_background` overrides in `colors.ui` or `theme_colors`, and does not
use `yank_background`. Setting `yank_background` replaces the peak of the
picker preview pulse only.

Global overrides live in Lua `colors.ui`. Theme-specific overrides live in
`theme_colors`, keyed by the exact resolved Ghostty theme name:

```lua
theme_colors = {
  ["Gruvbox Dark"] = { accent = "#fabd2f" },
}
```

The manager applies theme-derived colors, global overrides, then theme-specific
overrides. The same precedence applies at startup and after theme selection;
overrides from the previous theme do not carry over. Keys are case-sensitive,
and each table accepts the same roles and RGB formats as `colors.ui`.
Reload configuration applies changes without restarting.

Fallback defaults use the existing Gruvbox-like colors. Similar per-component shades have
been consolidated into the shared roles. Match highlight keeps the previous red
picker highlight; its role is independent of error even when defaults coincide.
Active copy and visual status now share accent. Geometry and alpha/dimming factors
remain component styling rather than additional palette roles.

UI-owned terminal snapshot decorations convert colors through `colors::rgb` for
Ghostty's packed RGB API. Actual terminal rendering remains controlled by Ghostty.
Explicit ANSI/RGB preview content colors are preserved; only its default text and
background come from the shared palette. The isolated preview VT still uses its
own ANSI palette until theme integration is implemented.

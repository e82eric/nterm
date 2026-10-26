# Shared NFM egui picker

Status: agreed direction for later implementation; no implementation yet.
Recorded: 2026-09-30.

## Goal

Move the common egui picker experience from this terminal into NFM so it can
be used by both Ghostty and SimpleWindowManager. These are currently the two
consumers, maintained by the same user. Favor a concrete API for these consumers
over a general UI framework.

Repositories:

- Ghostty: `C:\src\ghostty2`
- NFM: `G:\src\RustWin32Host`
- SimpleWindowManager: `G:\src\SimpleWindowManager`

The terminal currently implements an egui picker over NFM search/source crates.
SimpleWindowManager uses NFM's existing Skia presentation through `PickerRuntime`.
Consolidate those presentation implementations around the terminal's egui UI.

## Agreed ownership

Both applications own their windows, input/event loops, OpenGL contexts,
render targets, and presentation. SimpleWindowManager will be updated to own
these instead of relying on NFM to create its picker window.

NFM owns common picker interaction and presentation: query input, results,
navigation, highlighting, placement, styling, typography, preview presentation,
common command sources, asynchronous search, and cancellation.

Application-specific behavior stays in the hosts: terminal surfaces, scrollback
snapshots, window enumeration/actions, focus policy, application configuration,
and execution of accepted actions. Reusable ancillary UI can move when useful;
the extraction does not move the entire terminal application shell into NFM.

## Suggested components

Names are provisional:

- Existing NFM search/source crates: searching, scanning, structured candidates.
- `nfm-egui`: picker state, widgets, layout, styling, preview presentation.
- `nfm-preview-vt`: Ghostty VT C API bindings and ANSI-to-styled-cell parsing.
- Optional OpenGL integration module/crate: egui renderer wrapper for hosts
  that do not already have an egui integration. It does not own a window.
- Host adapters in each application: sources, previews, fonts, action handling.

Keep picker state separate from graphics resources so recreating an OpenGL
context does not discard the active search session.

## Scrollback through extension points

Scrollback remains in Ghostty. Use the existing store/source abstraction as the
starting point for publishing searchable candidates with stable IDs and payloads
containing terminal cell ranges. Inspect the existing traits before deciding
whether they need changes.

The store supplies search data. Add a separate preview-provider connection for
retrieving styled rows and return selection/action events to the host for copy,
selection, or other terminal actions. NFM must not call Ghostty application FFI
or retain a raw terminal host pointer.

Ghostty owns snapshot lifetime. A picker session retains an owned snapshot
handle or provider whose lifetime covers outstanding work. Define cancellation,
generation/revision checks, and shutdown ordering so closing a picker or terminal
cannot leave workers reading invalid state. Stable candidate IDs and payloads
must remain meaningful for the snapshot associated with the result.

SimpleWindowManager can supply candidates with window IDs or other application
payloads and interpret the resulting selection events itself.

## Embedding interface

Fonts, graphics setup, and positioning may enter through a single convenience
interface, but update them according to their lifetimes:

| Lifetime | Host inputs |
| --- | --- |
| Renderer initialization | OpenGL context and renderer options |
| Appearance setup/update | Font bytes, face indices, font size/cell metrics, palette |
| Picker opening | Store/source, preview provider, picker configuration |
| Each frame | Input, available bounds, anchor, DPI scale, render target information |
| Picker output | Selection/action events, clipboard requests, repaint scheduling |

Copy or own font data; do not borrow temporary Ghostty FFI buffers. Supply colors
explicitly instead of reading terminal-global palette state. Position, bounds,
and DPI must respond to resize and monitor changes. Define logical-point versus
physical-pixel coordinates clearly.

Offer a UI entry point using the host's `egui::Context`, plus an optional renderer
wrapper using the host's OpenGL context. The terminal should reuse its existing
egui context and painter rather than create a second integration and duplicate
font textures. SimpleWindowManager can use the optional wrapper.

Conceptual API only, not a committed signature:

```rust
picker.show(&egui_context, placement, appearance) -> PickerOutput;
renderer.render(frame, render_target);
```

Before implementing the renderer wrapper, specify current-context/thread
requirements, framebuffer binding, viewport/clipping, GL state expectations,
texture ownership, and destruction while the context remains valid. Shared
texture objects are needed only for features that actually consume them.

## Ghostty VT and build boundary

The current `src/picker/preview_vt.c` already uses the standalone Ghostty VT C
API to produce styled cells. egui paints those cells; previews do not require
the full Ghostty terminal renderer. Move this wrapper and its VT dependency
into NFM, with safe ownership around the native API.

The current terminal `build.rs` assumes Ghostty headers and built libraries in
this checkout. Replace that assumption for the shared preview crate with an
explicit VT dependency/build contract. Support a pinned standalone VT source
or artifact and a host-supplied compatible VT build override, allowing this
terminal to reuse its existing build. Ensure headers and binaries match and
avoid linking multiple incompatible copies or duplicate native symbols.

Avoid a dependency cycle: the terminal may depend on NFM, and NFM may depend on
the standalone VT library, but NFM must not depend on the terminal application
or invoke a terminal build that itself builds NFM.

Packaging is still to be decided. Earlier discussion favored pinned Cargo Git
dependencies over submodules for the Rust crates, with local development
overrides. NFM currently has no Git remote configured. Revisit hosting and VT
source acquisition when implementing; absolute drive paths must not be the
only supported build configuration.

## Migration sequence

1. Inventory current terminal picker behavior and NFM/Skia behavior used by the
   window manager. Identify native-window/file preview and focus requirements.
2. Extract shared picker types, appearance inputs, and events into `nfm-egui`.
   Replace terminal FFI access with host source/preview/font adapters. Keep
   Ghostty as the first consumer and preserve its existing behavior.
3. Extract the VT preview wrapper and establish the native build/link contract.
4. Update SimpleWindowManager to own its picker window and OpenGL integration,
   then use the same shared egui picker with its application adapters.
5. Remove the old Skia presentation once the window-manager behaviors in use
   are covered. Retain reusable sources and application logic.
6. Document reproducible builds and local development across repositories.

## Validation when implementing

- Both hosts show the same common picker UI and support search, navigation,
  acceptance, cancellation, and asynchronous updates.
- Ghostty scrollback search preserves range payloads, styled previews, and
  snapshot validity across close/reopen and terminal shutdown.
- ANSI preview behavior preserves Unicode, colors, attributes, and scrolling.
- Fonts, cell metrics, placement, clipping, and DPI work across resize and
  monitor changes; terminal-hosted UI reuses its existing egui renderer.
- SimpleWindowManager preserves the window/process/file actions and native
  preview behavior actually used, including focus restoration.
- Worker cancellation and graphics teardown have explicit safe lifetimes.
- A documented fresh checkout builds without machine-specific drive paths.

## Starting points in this checkout

- `src/picker.rs` and `src/picker/`: UI, sources, commands, previews, configuration.
- `src/picker/font.rs`: Ghostty font export and cell-metric coupling.
- `src/picker/source.rs`: stores and direct scrollback snapshot access.
- `src/picker/preview.rs`: preview workers and direct terminal snapshot access.
- `src/picker/preview_vt.c`: standalone VT parsing wrapper.
- `src/picker/action.rs`: Lua callbacks and providers to adapt at the boundary.
- `src/colors.rs`: application palette to replace with explicit shared inputs.
- `build.rs`, `Cargo.toml`, and root `build.zig`: dependency/build integration.

Paths in this section are relative to this repository, except `build.zig`,
which is in the `ghostty` submodule.

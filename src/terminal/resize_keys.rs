//! What resize mode shows while a key table with resize keys is active: the
//! focused pane's, or the shown float's, border in another colour with a
//! square on the corner the keys move, and the status bar's hints for them.
use super::{
    TerminalView,
    layout::{Axis, DividerAt, DividerId},
    workspace_ui::WorkspaceAction,
};
use crate::actions::AppAction;
use crate::picker::config::TableModifiers;
use egui::{Pos2, Rect, Vec2, pos2, vec2};

/// How thick the border is drawn in resize mode, at least.
const BORDER: f32 = 2.0;
/// The side of the square on the corner the keys move.
const CORNER: f32 = 9.0;
/// The side of the arrows on the sides move mode can move a pane toward.
const ARROW: f32 = 12.0;
/// How far the backdrop behind the pane being resized reaches past it.
const BACKDROP: f32 = 8.0;
/// How opaque the background is laid over everything but the pane being
/// resized.
pub(super) const DIM: f32 = 0.75;

/// Lays the background over the panes `others`, with `corner_radius`, so
/// the one being resized stands out.
pub(super) fn dim_others(
    painter: &egui::Painter,
    others: impl IntoIterator<Item = Rect>,
    corner_radius: f32,
) {
    let color = crate::colors::palette().background.gamma_multiply(DIM);
    for rect in others {
        painter.rect_filled(rect, corner_radius, color);
    }
}

/// An edge of a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];
    fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
    /// Whether it is a vertical line, moved by left and right keys.
    fn vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
    /// Whether the `ResizeFar*` keys move it when a pane has both.
    fn far(self) -> bool {
        matches!(self, Self::Right | Self::Bottom)
    }
}

/// An edge the resize keys can move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ResizeEdge {
    pub(super) edge: Edge,
    /// Whether the next key press moves it. Where a pane has edges on both
    /// sides, one moves without Shift and the other with it.
    pub(super) active: bool,
}

/// The resize edges of a pane whose edges in `movable`, in `Edge::ALL`
/// order, can move. With `far` (Shift held), the right and bottom edges are
/// active where there are two on an axis.
fn resize_edges(movable: [bool; 4], far: bool) -> Vec<ResizeEdge> {
    let pairs = [movable[0] && movable[1], movable[2] && movable[3]];
    Edge::ALL
        .into_iter()
        .zip(movable)
        .filter(|(_, movable)| *movable)
        .map(|(edge, _)| {
            let pair = if edge.vertical() { pairs[0] } else { pairs[1] };
            ResizeEdge {
                edge,
                active: !pair || edge.far() == far,
            }
        })
        .collect()
}

/// The resize edges of the focused tiled pane `pane`, from the workspace's
/// `dividers`: the outer split, and those between it and its neighbours.
pub(super) fn tile_resize_edges(
    pane: Rect,
    focus: u64,
    dividers: &[DividerAt],
    far: bool,
) -> Vec<ResizeEdge> {
    let mut movable = [false; 4];
    for d in dividers {
        let moves = match d.id {
            // Every pane borders the outer split.
            DividerId::Main => true,
            DividerId::Between(a, b) => a == focus || b == focus,
        };
        if moves {
            let index = match d.axis {
                Axis::X if d.at > pane.center().x => 1,
                Axis::X => 0,
                Axis::Y if d.at > pane.center().y => 3,
                Axis::Y => 2,
            };
            movable[index] = true;
        }
    }
    resize_edges(movable, far)
}

/// The resize edges of a shown float: all four.
pub(super) fn float_resize_edges(far: bool) -> Vec<ResizeEdge> {
    resize_edges([true; 4], far)
}

/// The corner of `rect` where the active `edges` meet: on each axis the
/// side of the active edge, or with none, the side Shift (`far`) picks.
fn active_corner(rect: Rect, edges: &[ResizeEdge], far: bool) -> Pos2 {
    let far_side = |vertical: bool| {
        edges
            .iter()
            .find(|m| m.active && m.edge.vertical() == vertical)
            .map_or(far, |m| m.edge.far())
    };
    pos2(
        if far_side(true) {
            rect.right()
        } else {
            rect.left()
        },
        if far_side(false) {
            rect.bottom()
        } else {
            rect.top()
        },
    )
}

/// What a key table with resize or move keys draws on the pane they act on.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum KeyOverlay {
    /// No such table is active.
    #[default]
    None,
    /// The edges the resize keys move, and whether Shift is held.
    Resize { edges: Vec<ResizeEdge>, far: bool },
    /// The sides, in `Edge::ALL` order, the move keys can move the pane
    /// toward.
    Move { sides: [bool; 4] },
}

impl KeyOverlay {
    pub(super) fn is_none(&self) -> bool {
        *self == Self::None
    }
    /// The resize edges, for the status bar's hints.
    pub(super) fn edges(&self) -> &[ResizeEdge] {
        match self {
            Self::Resize { edges, .. } => edges,
            _ => &[],
        }
    }
    /// Draws on the pane or float with frame `rect`: a backdrop reaching
    /// past it in the panel colour tinted with the warning colour, its
    /// border, `width` thick or more, in a dimmed warning colour, and in the
    /// warning colour, for resizing a square on the corner the keys move
    /// now, or for moving an arrow on each side it can move toward.
    pub(super) fn paint(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        corner_radius: f32,
        width: f32,
    ) {
        if self.is_none() {
            return;
        }
        let width = width.max(BORDER);
        let palette = crate::colors::palette();
        painter.rect_stroke(
            rect,
            corner_radius,
            egui::Stroke::new(
                width + BACKDROP,
                crate::colors::blend(palette.panel_background, palette.warning, 0.12),
            ),
            egui::StrokeKind::Outside,
        );
        painter.rect_stroke(
            rect,
            corner_radius,
            egui::Stroke::new(width, palette.warning.gamma_multiply(0.45)),
            egui::StrokeKind::Outside,
        );
        // Centred on the middle of the border's stroke.
        let line = rect.expand(width / 2.0);
        let outline = egui::Stroke::new(1.0, palette.background);
        match self {
            Self::Resize { edges, far } if !edges.is_empty() => {
                let corner = active_corner(line, edges, *far);
                let square = Rect::from_center_size(corner, Vec2::splat(CORNER.max(width + 4.0)));
                painter.rect(
                    square,
                    1.0,
                    palette.warning,
                    outline,
                    egui::StrokeKind::Outside,
                );
            }
            Self::Move { sides } => {
                for (edge, _) in Edge::ALL.into_iter().zip(sides).filter(|(_, s)| **s) {
                    let points = arrow(line, edge, ARROW.max(width + 6.0));
                    painter.add(egui::Shape::convex_polygon(
                        points.to_vec(),
                        palette.warning,
                        outline,
                    ));
                }
            }
            _ => {}
        }
    }
}

/// A triangle of `size` on the middle of `rect`'s `edge`, pointing out.
fn arrow(rect: Rect, edge: Edge, size: f32) -> [Pos2; 3] {
    let (center, out) = match edge {
        Edge::Left => (rect.left_center(), -Vec2::X),
        Edge::Right => (rect.right_center(), Vec2::X),
        Edge::Top => (rect.center_top(), -Vec2::Y),
        Edge::Bottom => (rect.center_bottom(), Vec2::Y),
    };
    let across = out.rot90();
    let half = size / 2.0;
    // Its base sits across the border and its tip points away from the pane.
    let base = center - out * half * 0.5;
    [
        base + across * half,
        base + out * half * 1.5,
        base - across * half,
    ]
}

/// The sides of a float with frame `rect` that are not against `bounds`, so
/// the move keys can move it that way.
pub(super) fn float_sides(rect: Rect, bounds: Rect) -> [bool; 4] {
    const SLACK: f32 = 0.5;
    [
        rect.left() > bounds.left() + SLACK,
        rect.right() < bounds.right() - SLACK,
        rect.top() > bounds.top() + SLACK,
        rect.bottom() < bounds.bottom() - SLACK,
    ]
}

/// A hint in the status bar: keys, then what they do.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct KeyHint {
    pub(super) keys: Vec<String>,
    pub(super) label: String,
}

fn hint(keys: Vec<String>, label: impl Into<String>) -> KeyHint {
    KeyHint {
        keys,
        label: label.into(),
    }
}

/// How a key is shown on a keycap. The modifiers in `held` are left out:
/// the hint is for what the keys do while they are held.
fn key_label(key: egui::Key, mods: egui::Modifiers, held: egui::Modifiers) -> String {
    use egui::Key;
    let name = match key {
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::Escape => "Esc",
        key => key.symbol_or_name(),
    };
    format!("{}{name}", mods_label(mods, held))
}

/// The modifiers before a key on a keycap, each followed by `+`, leaving
/// out those in `held`.
fn mods_label(mods: egui::Modifiers, held: egui::Modifiers) -> String {
    let mut label = String::new();
    for (on, name) in [
        (mods.ctrl && !held.ctrl, "Ctrl+"),
        (mods.alt && !held.alt, "Alt+"),
        (mods.shift && !held.shift, "Shift+"),
    ] {
        if on {
            label.push_str(name);
        }
    }
    label
}

/// A modifier on its own keycap: `Shift`, `Ctrl+Alt`.
fn modifier_key(mods: egui::Modifiers) -> String {
    mods_label(mods, egui::Modifiers::NONE)
        .trim_end_matches('+')
        .to_owned()
}

/// The key in `table` that runs `action`, preferring an arrow.
fn key_for(
    table: &[(egui::Key, egui::Modifiers, AppAction)],
    action: &AppAction,
) -> Option<(egui::Key, egui::Modifiers)> {
    let mut keys = table.iter().filter(|(_, _, a)| a == action);
    let first = keys.clone().next()?;
    let arrow = keys.find(|(key, ..)| {
        use egui::Key::*;
        matches!(key, ArrowLeft | ArrowRight | ArrowUp | ArrowDown)
    });
    let (key, mods, _) = arrow.unwrap_or(first);
    Some((*key, *mods))
}

/// The hint for the table's keys that run `actions`, one per direction, as
/// `label`. Where they share modifiers, those are shown once: `Alt+←→↑↓`.
fn direction_hint(
    table: &[(egui::Key, egui::Modifiers, AppAction)],
    actions: [WorkspaceAction; 4],
    label: &str,
) -> Option<KeyHint> {
    let keys: Vec<_> = actions
        .into_iter()
        .filter_map(|a| key_for(table, &AppAction::Workspace(a)))
        .collect();
    let (_, mods) = *keys.first()?;
    let keys = if keys.iter().all(|(_, m)| *m == mods) {
        let names: String = keys
            .iter()
            .map(|(key, _)| key_label(*key, egui::Modifiers::NONE, egui::Modifiers::NONE))
            .collect();
        vec![format!(
            "{}{names}",
            mods_label(mods, egui::Modifiers::NONE)
        )]
    } else {
        keys.iter()
            .map(|(key, mods)| key_label(*key, *mods, egui::Modifiers::NONE))
            .collect()
    };
    Some(hint(keys, label))
}

/// The hints for a key table's keys `table`, with its `far` and `nudge`
/// `modifiers`, most useful first. With resize `edges`, what the keys move
/// now, then what the `far` modifier switches them to; with move keys,
/// moving, and moving as far as it goes, which is to an edge for a
/// `floating` pane. Then nudging, focus, centring a float, switching to
/// another table, reset and leaving, which any table can have.
pub(super) fn hints(
    table: &[(egui::Key, egui::Modifiers, AppAction)],
    modifiers: TableModifiers,
    edges: &[ResizeEdge],
    far: bool,
    floating: bool,
) -> Vec<KeyHint> {
    use WorkspaceAction::*;
    let mut result = Vec::new();
    // While the far modifier is held, the keys are shown without it.
    let held = modifiers
        .far
        .filter(|_| far)
        .unwrap_or(egui::Modifiers::NONE);
    for vertical in [true, false] {
        let Some(active) = edges
            .iter()
            .find(|m| m.active && m.edge.vertical() == vertical)
        else {
            continue;
        };
        let actions = match (vertical, far) {
            (true, false) => [ResizeLeft, ResizeRight],
            (true, true) => [ResizeFarLeft, ResizeFarRight],
            (false, false) => [ResizeUp, ResizeDown],
            (false, true) => [ResizeFarUp, ResizeFarDown],
        };
        let keys: Vec<String> = actions
            .into_iter()
            .filter_map(|a| key_for(table, &AppAction::Workspace(a)))
            .map(|(key, mods)| key_label(key, mods, held))
            .collect();
        if !keys.is_empty() {
            result.push(hint(keys, format!("{} edge", active.edge.name())));
        }
    }
    let others: Vec<&str> = edges
        .iter()
        .filter(|m| !m.active)
        .map(|m| m.edge.name())
        .collect();
    if let Some(far_mods) = modifiers.far
        && !far
        && !others.is_empty()
    {
        let plural = if others.len() > 1 { "s" } else { "" };
        result.push(hint(
            vec![modifier_key(far_mods)],
            format!("{} edge{plural}", others.join("/")),
        ));
    }
    let moves = direction_hint(table, [MoveLeft, MoveRight, MoveUp, MoveDown], "move");
    let far_label = if floating { "to edge" } else { "all the way" };
    let far_moves = direction_hint(
        table,
        [MoveFarLeft, MoveFarRight, MoveFarUp, MoveFarDown],
        far_label,
    );
    let stepped = !result.is_empty() || moves.is_some();
    result.extend(moves);
    result.extend(far_moves);
    // The nudge modifier on any of the resize or move keys moves by a small
    // step; it only changes how far a float moves, not where tiled panes go.
    if let Some(nudge) = modifiers.nudge
        && stepped
        && (floating || !edges.is_empty())
    {
        result.push(hint(vec![modifier_key(nudge)], "nudge"));
    }
    result.extend(direction_hint(
        table,
        [FocusLeft, FocusRight, FocusUp, FocusDown],
        "focus",
    ));
    let single = |action: &AppAction, label: &str| {
        key_for(table, action)
            .map(|(key, mods)| hint(vec![key_label(key, mods, egui::Modifiers::NONE)], label))
    };
    if floating {
        result.extend(single(&AppAction::Workspace(CenterFloat), "center"));
    }
    for (_, _, action) in table {
        if let AppAction::EnterKeyTable(name) = action
            && !result.iter().any(|h| h.label == *name)
        {
            result.extend(single(action, name));
        }
    }
    result.extend(single(&AppAction::Workspace(ResetLayout), "reset"));
    // Escape leaves unless the table binds it; otherwise show its own key.
    let escape_bound = table.iter().any(|(key, ..)| *key == egui::Key::Escape);
    let done = if escape_bound {
        key_for(table, &AppAction::ExitKeyTable)
            .map(|(key, mods)| key_label(key, mods, egui::Modifiers::NONE))
    } else {
        Some("Esc".into())
    };
    if let Some(done) = done {
        result.push(hint(vec![done], "done"));
    }
    result
}

/// Draws `hints` in the status bar after the mode chip `chip`, each key on a
/// keycap followed by what it does, leaving out those that would pass
/// `right`.
pub(super) fn paint_hints(painter: &egui::Painter, hints: &[KeyHint], chip: Rect, right: f32) {
    let palette = crate::colors::palette();
    let key_font = egui::FontId::monospace(12.0);
    let label_font = egui::FontId::monospace(13.0);
    let cap_height = (chip.height() - 2.0).max(8.0);
    let y = chip.center().y;
    let mut x = chip.right() + 14.0;
    for hint in hints {
        let caps: Vec<_> = hint
            .keys
            .iter()
            .map(|key| painter.layout_no_wrap(key.clone(), key_font.clone(), palette.text))
            .collect();
        let widths: Vec<f32> = caps
            .iter()
            .map(|cap| (cap.size().x + 10.0).max(cap_height))
            .collect();
        let label =
            painter.layout_no_wrap(hint.label.clone(), label_font.clone(), palette.muted_text);
        let width = widths.iter().map(|w| w + 3.0).sum::<f32>() + 3.0 + label.size().x;
        if x + width > right {
            break;
        }
        for (galley, w) in caps.into_iter().zip(widths) {
            let cap = Rect::from_min_size(pos2(x, y - cap_height / 2.0), vec2(w, cap_height));
            painter.rect(
                cap,
                4.0,
                egui::Color32::from_white_alpha(10),
                egui::Stroke::new(1.0, palette.border),
                egui::StrokeKind::Inside,
            );
            painter.galley(cap.center() - galley.size() / 2.0, galley, palette.text);
            x += w + 3.0;
        }
        x += 3.0;
        let width = label.size().x;
        painter.galley(pos2(x, y - label.size().y / 2.0), label, palette.muted_text);
        x += width + 16.0;
    }
}

impl TerminalView {
    /// Whether the active key table has resize keys, so the edges they move
    /// are shown.
    pub(super) fn key_table_resizes(&self) -> bool {
        self.key_table
            .as_deref()
            .is_some_and(|name| self.picker_bindings.key_table_resizes(name))
    }
    /// Whether the active key table has move keys, and no resize keys, so
    /// the pane they move is shown.
    pub(super) fn key_table_moves(&self) -> bool {
        !self.key_table_resizes()
            && self
                .key_table
                .as_deref()
                .is_some_and(|name| self.picker_bindings.key_table_moves(name))
    }
}

#[cfg(test)]
mod tests {
    use super::super::workspace::{Appearance, Layout, Workspace};
    use super::*;

    fn bounds() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0))
    }

    /// The edges of `focus` the resize keys move, and whether each is active.
    fn edge_states(layout: Layout, focus: u64, far: bool) -> Vec<(Edge, bool)> {
        resize_edges_of(layout, focus, far)
            .into_iter()
            .map(|m| (m.edge, m.active))
            .collect()
    }

    #[test]
    fn every_movable_edge_is_found_and_shift_picks_between_two() {
        use Edge::*;
        // Edges with nothing opposite are active with or without Shift.
        for (layout, focus, edges) in [
            (Layout::Tile, 1, vec![Right]),
            (Layout::Tile, 2, vec![Left, Bottom]),
            (Layout::Tile, 6, vec![Left, Top]),
            (Layout::TileReversed, 1, vec![Left]),
            (Layout::HorizontalGrid, 1, vec![Right, Bottom]),
            (Layout::Deck, 2, vec![Left]),
            (Layout::Monocle, 1, vec![]),
        ] {
            let all: Vec<_> = edges.iter().map(|e| (*e, true)).collect();
            assert_eq!(edge_states(layout, focus, false), all, "{layout:?} {focus}");
            assert_eq!(edge_states(layout, focus, true), all, "{layout:?} {focus}");
        }
        // Between two neighbours, both move; Shift picks the bottom.
        assert_eq!(
            edge_states(Layout::Tile, 4, false),
            [(Left, true), (Top, true), (Bottom, false)]
        );
        assert_eq!(
            edge_states(Layout::Tile, 4, true),
            [(Left, true), (Top, false), (Bottom, true)]
        );
    }

    #[test]
    fn the_square_sits_where_the_active_edges_meet() {
        let rect = Rect::from_min_max(pos2(100.0, 100.0), pos2(900.0, 700.0));
        let corner = |far| active_corner(rect, &float_resize_edges(far), far);
        assert_eq!(corner(false), rect.left_top());
        assert_eq!(corner(true), rect.right_bottom());
        let r = Rect::from_min_size(Pos2::ZERO, vec2(10.0, 10.0));
        // Only a right edge moves: the square is on the right, at the top.
        let right = resize_edges_of(Layout::Tile, 1, false);
        assert_eq!(active_corner(r, &right, false), r.right_top());
        // Between a top and a bottom neighbour, Shift moves the bottom.
        let middle = resize_edges_of(Layout::Tile, 4, true);
        assert_eq!(active_corner(r, &middle, true), r.left_bottom());
    }

    #[test]
    fn floats_resize_all_four_edges() {
        let active = |far| -> Vec<(Edge, bool)> {
            float_resize_edges(far)
                .into_iter()
                .map(|m| (m.edge, m.active))
                .collect()
        };
        use Edge::*;
        assert_eq!(
            active(false),
            [(Left, true), (Right, false), (Top, true), (Bottom, false)]
        );
        assert_eq!(
            active(true),
            [(Left, false), (Right, true), (Top, false), (Bottom, true)]
        );
    }

    /// The built-in tables' modifiers.
    fn defaults() -> TableModifiers {
        TableModifiers {
            far: Some(egui::Modifiers::SHIFT),
            nudge: Some(egui::Modifiers::CTRL),
        }
    }

    fn table_keys() -> Vec<(egui::Key, egui::Modifiers, AppAction)> {
        let bindings = crate::picker::config::Bindings::default();
        bindings.key_table_keys("resize")
    }

    fn texts(hints: &[KeyHint]) -> Vec<String> {
        hints
            .iter()
            .map(|h| format!("{} {}", h.keys.join(" "), h.label))
            .collect()
    }

    #[test]
    fn hints_show_the_keys_for_what_moves_now_and_with_far() {
        let middle = resize_edges_of(Layout::Tile, 4, false);
        assert_eq!(
            texts(&hints(&table_keys(), defaults(), &middle, false, false)),
            [
                "← → left edge",
                "↑ ↓ top edge",
                "Shift bottom edge",
                "Ctrl nudge",
                "Alt+←→↑↓ focus",
                "M move",
                "= reset",
                "Esc done"
            ]
        );
        // Holding the far modifier, Shift, the same keys move the bottom edge.
        let held = resize_edges_of(Layout::Tile, 4, true);
        assert_eq!(
            texts(&hints(&table_keys(), defaults(), &held, true, false)),
            [
                "← → left edge",
                "↑ ↓ bottom edge",
                "Ctrl nudge",
                "Alt+←→↑↓ focus",
                "M move",
                "= reset",
                "Esc done"
            ]
        );
        assert_eq!(
            texts(&hints(
                &table_keys(),
                defaults(),
                &float_resize_edges(false),
                false,
                false
            ))[2],
            "Shift right/bottom edges"
        );
        // Without edges, only the table's own keys.
        assert_eq!(
            texts(&hints(&table_keys(), defaults(), &[], false, false)),
            ["Alt+←→↑↓ focus", "M move", "= reset", "Esc done"]
        );
    }

    #[test]
    fn move_hints_show_moving_then_what_else_the_table_does() {
        let bindings = crate::picker::config::Bindings::default();
        let table = bindings.key_table_keys("move");
        assert_eq!(
            texts(&hints(&table, defaults(), &[], false, false)),
            [
                "←→↑↓ move",
                "Shift+←→↑↓ all the way",
                "Alt+←→↑↓ focus",
                "R resize",
                "Esc done"
            ]
        );
        // A float can be nudged too, moved to an edge, and centred.
        assert_eq!(
            texts(&hints(&table, defaults(), &[], false, true)),
            [
                "←→↑↓ move",
                "Shift+←→↑↓ to edge",
                "Ctrl nudge",
                "Alt+←→↑↓ focus",
                "C center",
                "R resize",
                "Esc done"
            ]
        );
    }

    #[test]
    fn move_arrows_show_on_the_sides_a_float_can_move_toward() {
        let bounds = bounds();
        let inside = Rect::from_min_max(pos2(100.0, 100.0), pos2(900.0, 700.0));
        assert_eq!(float_sides(inside, bounds), [true; 4]);
        let corner = Rect::from_min_max(pos2(0.0, 0.0), pos2(500.0, 400.0));
        assert_eq!(float_sides(corner, bounds), [false, true, false, true]);
        // Each arrow's tip points away from the pane.
        let tip = |edge| arrow(inside, edge, 12.0)[1];
        assert!(tip(Edge::Left).x < inside.left());
        assert!(tip(Edge::Right).x > inside.right());
        assert!(tip(Edge::Top).y < inside.top());
        assert!(tip(Edge::Bottom).y > inside.bottom());
    }

    #[test]
    fn hints_follow_rebound_keys_and_modifiers() {
        use WorkspaceAction::*;
        let none = egui::Modifiers::NONE;
        let alt = egui::Modifiers::ALT;
        let table = vec![
            (egui::Key::W, none, AppAction::Workspace(ResizeUp)),
            (egui::Key::S, none, AppAction::Workspace(ResizeDown)),
            (egui::Key::Escape, none, AppAction::Workspace(ResetLayout)),
            (egui::Key::Q, egui::Modifiers::CTRL, AppAction::ExitKeyTable),
        ];
        // Nudging with Alt, and no far modifier.
        let rebound = TableModifiers {
            far: None,
            nudge: Some(alt),
        };
        let edges = resize_edges_of(Layout::Tile, 2, false);
        assert_eq!(
            texts(&hints(&table, rebound, &edges, false, false)),
            ["W S bottom edge", "Alt nudge", "Esc reset", "Ctrl+Q done"]
        );
        // The far modifier's hint names it, and without a nudge modifier
        // there is no nudge hint.
        let far_alt = TableModifiers {
            far: Some(alt),
            nudge: None,
        };
        let middle = resize_edges_of(Layout::Tile, 4, false);
        let texts = texts(&hints(&table_keys(), far_alt, &middle, false, false));
        assert!(texts.contains(&"Alt bottom edge".to_owned()), "{texts:?}");
        assert!(!texts.iter().any(|t| t.ends_with("nudge")), "{texts:?}");
    }

    fn resize_edges_of(layout: Layout, focus: u64, far: bool) -> Vec<ResizeEdge> {
        let style = Appearance::default();
        let mut w = Workspace::new(1, 1);
        w.layout = layout;
        w.panes = vec![1, 2, 3, 4, 5, 6];
        w.focused = Some(focus);
        let rect = w
            .rectangles(bounds(), &style)
            .into_iter()
            .find(|(id, _)| *id == focus)
            .map(|(_, r)| r)
            .unwrap();
        tile_resize_edges(rect, focus, &w.dividers(bounds(), &style), far)
    }
}

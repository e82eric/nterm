//! Resizing with the mouse: dragging the dividers between tiled panes, or the
//! edges of the shown floating pane. The layout decides where dividers are
//! and what moving one does; this module follows the pointer.
use super::{
    TerminalView,
    layout::{Axis, DividerAt, DividerId, Motion},
    workspace::Appearance,
};
use crate::config::FloatingPadding;
use egui::{CursorIcon, Pos2, Rect, Vec2, pos2};

/// How far outside a floating pane's frame its edges can still be grabbed.
const FLOAT_GRIP_OUTSIDE: f32 = 6.0;
/// How far along an edge from a corner grabs the corner.
const FLOAT_CORNER: f32 = 16.0;
/// The smallest the mouse or keys make a floating pane, when the workspace
/// fits it.
const FLOAT_MINIMUM: Vec2 = egui::vec2(160.0, 80.0);
/// How far one key press moves a floating pane's edge, as a share of the
/// workspace along that axis, and how far when nudged.
const FLOAT_KEY_STEP: f32 = 0.05;
const FLOAT_NUDGE_KEY_STEP: f32 = 0.01;

/// What the mouse can resize this frame.
#[derive(Clone, Copy, Debug)]
pub(super) enum Resizable {
    /// The tiled panes' dividers.
    Panes,
    /// The shown floating pane: its frame `rect`, and the terminal `inner`
    /// to it, whose clicks stay with the terminal.
    Float { rect: Rect, inner: Rect },
}

fn coordinate(axis: Axis, pos: Pos2) -> f32 {
    match axis {
        Axis::X => pos.x,
        Axis::Y => pos.y,
    }
}

fn axis_cursor(axis: Axis) -> CursorIcon {
    match axis {
        Axis::X => CursorIcon::ResizeHorizontal,
        Axis::Y => CursorIcon::ResizeVertical,
    }
}

/// Where the pointer can grab a divider: its gap, at least 8 pixels wide.
fn hit(d: &DividerAt) -> Rect {
    let half = (d.gap / 2.0).max(4.0);
    match d.axis {
        Axis::X => Rect::from_min_max(
            pos2(d.at - half, d.span.top()),
            pos2(d.at + half, d.span.bottom()),
        ),
        Axis::Y => Rect::from_min_max(
            pos2(d.span.left(), d.at - half),
            pos2(d.span.right(), d.at + half),
        ),
    }
}

/// Some edges of a pane: those a grab moves (one, or two at a corner), or
/// those the resize keys move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Edges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}
impl Edges {
    fn cursor(self) -> CursorIcon {
        match (self.left || self.right, self.top || self.bottom) {
            // Top-left and bottom-right share a diagonal.
            (true, true) if self.left == self.top => CursorIcon::ResizeNwSe,
            (true, true) => CursorIcon::ResizeNeSw,
            (true, false) => CursorIcon::ResizeHorizontal,
            _ => CursorIcon::ResizeVertical,
        }
    }
    /// The point on `rect` the grabbed edges pass through. Axes no grabbed
    /// edge moves along are ignored when resizing.
    fn anchor(self, rect: Rect) -> Pos2 {
        let x = if self.left { rect.left() } else { rect.right() };
        let y = if self.top { rect.top() } else { rect.bottom() };
        pos2(x, y)
    }
}

/// The edges `pos` grabs: on the frame between `rect` and `inner`, or just
/// outside it. Near a corner both of its edges.
fn float_edges(rect: Rect, inner: Rect, pos: Pos2) -> Option<Edges> {
    if !rect.expand(FLOAT_GRIP_OUTSIDE).contains(pos) || inner.contains(pos) {
        return None;
    }
    let beside = pos.x < inner.left() || pos.x > inner.right();
    let above_or_below = pos.y < inner.top() || pos.y > inner.bottom();
    Some(Edges {
        left: pos.x < inner.left() || above_or_below && pos.x < rect.left() + FLOAT_CORNER,
        right: pos.x > inner.right() || above_or_below && pos.x > rect.right() - FLOAT_CORNER,
        top: pos.y < inner.top() || beside && pos.y < rect.top() + FLOAT_CORNER,
        bottom: pos.y > inner.bottom() || beside && pos.y > rect.bottom() - FLOAT_CORNER,
    })
}

/// Moves the grabbed edges of `rect` to `to`, keeping `FLOAT_MINIMUM` and
/// staying inside `bounds`.
fn resize_float(rect: Rect, bounds: Rect, edges: Edges, to: Pos2) -> Rect {
    let minimum = FLOAT_MINIMUM.min(bounds.size());
    let mut r = rect;
    if edges.left {
        r.min.x = to.x.min(r.max.x - minimum.x).max(bounds.left());
    }
    if edges.right {
        r.max.x = to.x.max(r.min.x + minimum.x).min(bounds.right());
    }
    if edges.top {
        r.min.y = to.y.min(r.max.y - minimum.y).max(bounds.top());
    }
    if edges.bottom {
        r.max.y = to.y.max(r.min.y + minimum.y).min(bounds.bottom());
    }
    r
}

/// `rect` as percentages in from each edge of `bounds`, the form the floating
/// size is kept in so it follows the window.
fn padding_for(rect: Rect, bounds: Rect) -> FloatingPadding {
    let (width, height) = (bounds.width().max(1.0), bounds.height().max(1.0));
    FloatingPadding {
        top: (rect.top() - bounds.top()) / height * 100.0,
        bottom: (bounds.bottom() - rect.bottom()) / height * 100.0,
        left: (rect.left() - bounds.left()) / width * 100.0,
        right: (bounds.right() - rect.right()) / width * 100.0,
    }
}

/// Moves one edge of `rect` one key step within `bounds`: the left or top
/// edge, or with `far` the right or bottom one, `forward` meaning right or
/// down, and with `nudge` a much smaller step. The same limits as dragging
/// apply.
fn step_float(
    rect: Rect,
    bounds: Rect,
    horizontal: bool,
    far: bool,
    forward: bool,
    nudge: bool,
) -> Rect {
    let edges = Edges {
        left: horizontal && !far,
        right: horizontal && far,
        top: !horizontal && !far,
        bottom: !horizontal && far,
    };
    let length = if horizontal {
        bounds.width()
    } else {
        bounds.height()
    };
    let share = if nudge {
        FLOAT_NUDGE_KEY_STEP
    } else {
        FLOAT_KEY_STEP
    };
    let step = share * length * if forward { 1.0 } else { -1.0 };
    let offset = if horizontal {
        egui::vec2(step, 0.0)
    } else {
        egui::vec2(0.0, step)
    };
    resize_float(rect, bounds, edges, edges.anchor(rect) + offset)
}

/// `rect` moved by the least that puts it inside `bounds`; it must fit.
fn keep_inside(rect: Rect, bounds: Rect) -> Rect {
    let push =
        |low: f32, high: f32, min: f32, max: f32| (min - low).max(0.0) + (max - high).min(0.0);
    rect.translate(egui::vec2(
        push(rect.left(), rect.right(), bounds.left(), bounds.right()),
        push(rect.top(), rect.bottom(), bounds.top(), bounds.bottom()),
    ))
}

/// Moves `rect`, keeping its size, one key step toward `motion` within
/// `bounds`, with `nudge` a much smaller step, or with `far` against that
/// edge of `bounds`.
fn shift_float(rect: Rect, bounds: Rect, motion: Motion, far: bool, nudge: bool) -> Rect {
    let rect = Rect::from_min_size(rect.min, rect.size().min(bounds.size()));
    let horizontal = matches!(motion, Motion::Left | Motion::Right);
    let length = if horizontal {
        bounds.width()
    } else {
        bounds.height()
    };
    let share = if nudge {
        FLOAT_NUDGE_KEY_STEP
    } else {
        FLOAT_KEY_STEP
    };
    // Far enough to reach the edge from anywhere; `keep_inside` stops it.
    let step = if far { length } else { share * length };
    let step = if motion.backwards() { -step } else { step };
    let offset = if horizontal {
        egui::vec2(step, 0.0)
    } else {
        egui::vec2(0.0, step)
    };
    keep_inside(rect.translate(offset), bounds)
}

/// What a drag holds, and where on it the pointer grabbed, so it does not
/// jump to the pointer.
#[derive(Clone, Copy, Debug)]
enum Grip {
    Divider { id: DividerId, offset: f32 },
    Float { edges: Edges, offset: Vec2 },
}

pub(super) struct Drag {
    workspace: u64,
    grip: Grip,
    /// Set when what it holds disappears or input is lost. The drag still
    /// owns pointer events until the button is released.
    cancelled: bool,
}

impl TerminalView {
    /// Resizes `resizable` with the mouse. Returns whether this frame's
    /// pointer events belonged to resizing, in which case they are consumed.
    pub(super) fn resize_with_mouse(
        &mut self,
        ui: &mut egui::Ui,
        bounds: Rect,
        style: &Appearance,
        resizable: Resizable,
        enabled: bool,
    ) -> bool {
        let (pos, pressed, down, released, focused) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.viewport().focused.unwrap_or(false),
            )
        });
        let mut owned = self.pane_drag.is_some();
        if let Some(mut drag) = self.pane_drag.take() {
            drag.cancelled |= !enabled || !focused || drag.workspace != self.workspace().id;
            if !drag.cancelled {
                match self.drag(drag.grip, bounds, style, resizable, pos) {
                    Some(icon) => ui.ctx().set_cursor_icon(icon),
                    None => drag.cancelled = true,
                }
            }
            if down && !released && focused {
                self.pane_drag = Some(drag);
            }
        } else if enabled
            && focused
            && (!ui.input(|i| i.pointer.any_down()) || pressed)
            && let Some(pos) = pos
            && let Some((grip, icon)) = self.grip_at(bounds, style, resizable, pos)
        {
            ui.ctx().set_cursor_icon(icon);
            owned = pressed;
            if pressed {
                self.pane_drag = Some(Drag {
                    workspace: self.workspace().id,
                    grip,
                    cancelled: false,
                });
            }
        }
        if owned {
            // Consume the release too, including a cancelled drag. Keyboard
            // events remain available to the terminal and workspace shortcuts.
            ui.input_mut(|i| {
                i.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::PointerMoved(_)
                            | egui::Event::PointerButton { .. }
                            | egui::Event::MouseWheel { .. }
                    )
                })
            });
        }
        owned
    }

    /// Moves an edge of the shown floating pane one key step, or with
    /// `nudge` a small one, as the `Resize*` and `ResizeFar*` actions do.
    /// Returns whether a float is shown; if not, the keys resize the tiled
    /// panes instead.
    pub(super) fn resize_float_with_keys(
        &mut self,
        horizontal: bool,
        far: bool,
        forward: bool,
        nudge: bool,
    ) -> bool {
        if self.floats().visible().is_none() {
            return false;
        }
        self.place_float(|rect, bounds| step_float(rect, bounds, horizontal, far, forward, nudge));
        true
    }

    /// Moves the shown floating pane, keeping its size, as the `Move*` and
    /// `MoveFar*` actions do with a float shown: one key step, with `nudge` a
    /// small one, or with `far` against that edge of the workspace.
    pub(super) fn move_float_with_keys(&mut self, motion: Motion, far: bool, nudge: bool) {
        self.place_float(|rect, bounds| shift_float(rect, bounds, motion, far, nudge));
    }

    /// Centres the shown floating pane in the workspace, keeping its size.
    pub(super) fn center_float(&mut self) {
        self.place_float(|rect, bounds| Rect::from_center_size(bounds.center(), rect.size()));
    }

    /// Sets the shown float's frame to `place` of its frame and the
    /// workspace's bounds.
    fn place_float(&mut self, place: impl FnOnce(Rect, Rect) -> Rect) {
        let Some(id) = self.floats().visible() else {
            return;
        };
        // Bounds are set when the float is first laid out; before that there
        // is nothing on screen to move or resize.
        let Some(bounds) = self.floating.get(id).and_then(|f| f.pane.workspace_bounds) else {
            return;
        };
        let rect = place(self.floating_rect_for(bounds), bounds);
        self.workspace_mut()
            .floats
            .set_size(Some(padding_for(rect, bounds)));
    }

    /// What the pointer would grab at `pos`, and the cursor that shows it.
    fn grip_at(
        &self,
        bounds: Rect,
        style: &Appearance,
        resizable: Resizable,
        pos: Pos2,
    ) -> Option<(Grip, CursorIcon)> {
        match resizable {
            Resizable::Panes => {
                let d = self
                    .workspace()
                    .dividers(bounds, style)
                    .into_iter()
                    .find(|d| hit(d).contains(pos))?;
                let offset = coordinate(d.axis, pos) - d.at;
                Some((Grip::Divider { id: d.id, offset }, axis_cursor(d.axis)))
            }
            Resizable::Float { rect, inner } => {
                let edges = float_edges(rect, inner, pos)?;
                let offset = pos - edges.anchor(rect);
                Some((Grip::Float { edges, offset }, edges.cursor()))
            }
        }
    }

    /// Follows the pointer for one frame of a drag. Returns the cursor to
    /// show, or `None` when what the drag holds is gone.
    fn drag(
        &mut self,
        grip: Grip,
        bounds: Rect,
        style: &Appearance,
        resizable: Resizable,
        pos: Option<Pos2>,
    ) -> Option<CursorIcon> {
        match (grip, resizable) {
            // Looked up fresh each frame, so the drag survives window
            // resizes and pane changes that keep the divider.
            (Grip::Divider { id, offset }, Resizable::Panes) => {
                let w = self.workspace_mut();
                let d = w.dividers(bounds, style).into_iter().find(|d| d.id == id)?;
                if let Some(pos) = pos {
                    w.move_divider(bounds, style, id, coordinate(d.axis, pos) - offset);
                }
                Some(axis_cursor(d.axis))
            }
            (Grip::Float { edges, offset }, Resizable::Float { rect, .. }) => {
                if let Some(pos) = pos {
                    let resized = resize_float(rect, bounds, edges, pos - offset);
                    let size = padding_for(resized, bounds);
                    self.workspace_mut().floats.set_size(Some(size));
                }
                Some(edges.cursor())
            }
            // A float was shown or hidden mid-drag.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::workspace::{Layout, Workspace};
    use super::*;

    fn bounds() -> Rect {
        Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 800.0))
    }

    fn workspace(layout: Layout) -> Workspace {
        let mut w = Workspace::new(1, 1);
        w.panes = vec![1, 2, 3, 4, 5, 6];
        w.focused = Some(1);
        w.layout = layout;
        w
    }

    fn find(w: &Workspace, style: &Appearance, id: DividerId) -> Option<DividerAt> {
        w.dividers(bounds(), style).into_iter().find(|d| d.id == id)
    }

    #[test]
    fn dragging_tracks_pointer_and_preserves_unrelated_panes() {
        let bounds = bounds();
        for layout in [
            Layout::Tile,
            Layout::TileReversed,
            Layout::Grid,
            Layout::HorizontalGrid,
            Layout::Deck,
        ] {
            for gap in [0.0, 8.0] {
                let style = Appearance {
                    inner_gap: gap,
                    ..Default::default()
                };
                let original = workspace(layout);
                let dividers = original.dividers(bounds, &style);
                assert!(!dividers.is_empty());
                for d in &dividers {
                    let mut w = original.clone();
                    let before = w.rectangles(bounds, &style);
                    w.move_divider(bounds, &style, d.id, d.at + 20.0);
                    let moved = find(&w, &style, d.id).unwrap();
                    assert!(
                        (moved.at - d.at - 20.0).abs() < 0.01,
                        "{layout:?} {:?}: {}",
                        d.id,
                        moved.at - d.at
                    );
                    if let DividerId::Between(a, b) = d.id {
                        for (id, r) in w.rectangles(bounds, &style) {
                            if id != a && id != b {
                                let old = before.iter().find(|(i, _)| *i == id).unwrap().1;
                                assert!((r.min - old.min).length() < 0.01);
                                assert!((r.max - old.max).length() < 0.01);
                            }
                        }
                    }
                    for to in [-10000.0, 10000.0] {
                        w.move_divider(bounds, &style, d.id, to);
                        for (_, r) in w.rectangles(bounds, &style) {
                            assert!(r.width() >= 47.99 && r.height() >= 47.99);
                            assert!(bounds.contains_rect(r));
                        }
                    }
                    w.move_divider(bounds, &style, d.id, d.at);
                    for ((_, a), (_, b)) in before.iter().zip(w.rectangles(bounds, &style)) {
                        assert!((a.min - b.min).length() < 0.01);
                        assert!((a.max - b.max).length() < 0.01);
                    }
                }
            }
        }
    }

    #[test]
    fn dividers_are_found_by_their_panes_after_other_changes() {
        let style = Appearance::default();
        let mut w = workspace(Layout::Tile);
        let id = DividerId::Between(3, 4);
        let at = find(&w, &style, id).unwrap().at;
        // Another pane closing moves the divider but keeps it.
        w.remove(6);
        assert_ne!(find(&w, &style, id).unwrap().at, at);
        // Separating the pair removes it, and moving it does nothing.
        w.panes.swap(3, 4);
        assert!(find(&w, &style, id).is_none());
        let before = w.rectangles(bounds(), &style);
        w.move_divider(bounds(), &style, id, 0.0);
        assert_eq!(w.rectangles(bounds(), &style), before);
    }

    #[test]
    fn hidden_and_single_panes_have_no_dividers() {
        let style = Appearance::default();
        let mut w = workspace(Layout::Monocle);
        assert!(w.dividers(bounds(), &style).is_empty());
        w.layout = Layout::Tile;
        w.panes.truncate(1);
        assert!(w.dividers(bounds(), &style).is_empty());
    }

    fn float() -> (Rect, Rect) {
        let rect = Rect::from_min_max(pos2(100.0, 100.0), pos2(900.0, 700.0));
        (rect, rect.shrink(4.0))
    }

    #[test]
    fn float_frame_grabs_edges_and_corners_but_not_the_terminal() {
        let (rect, inner) = float();
        let edges = |x, y| float_edges(rect, inner, pos2(x, y));
        let side = |left, right, top, bottom| {
            Some(Edges {
                left,
                right,
                top,
                bottom,
            })
        };
        // On the frame, and just outside it.
        assert_eq!(edges(102.0, 400.0), side(true, false, false, false));
        assert_eq!(edges(905.0, 400.0), side(false, true, false, false));
        assert_eq!(edges(500.0, 95.0), side(false, false, true, false));
        assert_eq!(edges(500.0, 702.0), side(false, false, false, true));
        // Near a corner, along either edge.
        assert_eq!(edges(110.0, 98.0), side(true, false, true, false));
        assert_eq!(edges(898.0, 690.0), side(false, true, false, true));
        assert_eq!(edges(110.0, 98.0).unwrap().cursor(), CursorIcon::ResizeNwSe);
        assert_eq!(
            edges(895.0, 102.0).unwrap().cursor(),
            CursorIcon::ResizeNeSw
        );
        // The terminal and anywhere well away keep their clicks.
        assert_eq!(edges(500.0, 400.0), None);
        assert_eq!(edges(50.0, 400.0), None);
    }

    #[test]
    fn float_resizing_moves_only_grabbed_edges_within_limits() {
        let (rect, _) = float();
        let left = Edges {
            left: true,
            ..Default::default()
        };
        let r = resize_float(rect, bounds(), left, pos2(50.0, 0.0));
        assert_eq!(r, Rect::from_min_max(pos2(50.0, 100.0), rect.max));
        // Past the far edge it stops at the minimum width; past the window
        // it stops at the window.
        let r = resize_float(rect, bounds(), left, pos2(10000.0, 0.0));
        assert_eq!(r.width(), FLOAT_MINIMUM.x);
        let r = resize_float(rect, bounds(), left, pos2(-10000.0, 0.0));
        assert_eq!(r.left(), 0.0);
        let corner = Edges {
            right: true,
            bottom: true,
            ..Default::default()
        };
        let r = resize_float(rect, bounds(), corner, pos2(950.0, 750.0));
        assert_eq!(r, Rect::from_min_max(rect.min, pos2(950.0, 750.0)));
    }

    #[test]
    fn float_size_follows_the_window_as_percentages() {
        let (rect, _) = float();
        let size = padding_for(rect, bounds());
        assert_eq!(TerminalView::floating_rect(bounds(), Some(size)), rect);
        // Twice the window, twice the float, in the same place.
        let larger = Rect::from_min_size(Pos2::ZERO, egui::vec2(2000.0, 1600.0));
        let scaled = TerminalView::floating_rect(larger, Some(size));
        assert_eq!(
            scaled,
            Rect::from_min_max(pos2(200.0, 200.0), pos2(1800.0, 1400.0))
        );
    }

    #[test]
    fn float_key_steps_move_one_edge_and_reverse_exactly() {
        let (rect, _) = float();
        // 5% of the 1000x800 workspace: 50px across, 40px down.
        let r = step_float(rect, bounds(), true, false, false, false);
        assert_eq!(r, Rect::from_min_max(pos2(50.0, 100.0), rect.max));
        let r = step_float(rect, bounds(), true, true, true, false);
        assert_eq!(r, Rect::from_min_max(rect.min, pos2(950.0, 700.0)));
        let r = step_float(rect, bounds(), false, false, true, false);
        assert_eq!(r, Rect::from_min_max(pos2(100.0, 140.0), rect.max));
        let r = step_float(rect, bounds(), false, true, false, false);
        assert_eq!(r, Rect::from_min_max(rect.min, pos2(900.0, 660.0)));
        // Back and forth returns to the start.
        let there = step_float(rect, bounds(), true, true, false, false);
        assert_eq!(step_float(there, bounds(), true, true, true, false), rect);
        // Repeated steps stop at the window and the minimum size.
        let mut r = rect;
        for _ in 0..100 {
            r = step_float(r, bounds(), true, false, false, false);
        }
        assert_eq!(r.left(), 0.0);
        for _ in 0..100 {
            r = step_float(r, bounds(), true, false, true, false);
        }
        assert_eq!(r.width(), FLOAT_MINIMUM.x);
        // A nudge is 1%: 10px across.
        let r = step_float(rect, bounds(), true, false, false, true);
        assert_eq!(r, Rect::from_min_max(pos2(90.0, 100.0), rect.max));
    }

    #[test]
    fn moving_a_float_keeps_its_size_and_stops_at_the_window() {
        let (rect, _) = float();
        let moved = |motion, far, nudge| shift_float(rect, bounds(), motion, far, nudge);
        // 5% of the 1000x800 workspace, or 1% nudged.
        assert_eq!(
            moved(Motion::Left, false, false),
            rect.translate(egui::vec2(-50.0, 0.0))
        );
        assert_eq!(
            moved(Motion::Down, false, false),
            rect.translate(egui::vec2(0.0, 40.0))
        );
        assert_eq!(
            moved(Motion::Right, false, true),
            rect.translate(egui::vec2(10.0, 0.0))
        );
        // Far moves it against that edge.
        assert_eq!(moved(Motion::Up, true, false).top(), 0.0);
        assert_eq!(moved(Motion::Right, true, false).right(), 1000.0);
        assert_eq!(moved(Motion::Right, true, false).size(), rect.size());
        // Steps stop at the window rather than shrinking it.
        let mut r = rect;
        for _ in 0..10 {
            r = shift_float(r, bounds(), Motion::Left, false, false);
        }
        assert_eq!(r, Rect::from_min_size(pos2(0.0, 100.0), rect.size()));
        // A float bigger than the window is cut down to fit.
        let big = Rect::from_min_size(pos2(-50.0, 0.0), egui::vec2(2000.0, 100.0));
        let fitted = shift_float(big, bounds(), Motion::Down, false, false);
        assert!(bounds().contains_rect(fitted));
    }

    #[test]
    fn float_size_is_per_workspace_and_leaves_the_default_alone() {
        let mut a = Workspace::new(1, 1);
        let b = Workspace::new(2, 2);
        let (rect, _) = float();
        a.floats.set_size(Some(padding_for(rect, bounds())));
        assert!(a.floats.size().is_some());
        assert!(b.floats.size().is_none());
    }
}

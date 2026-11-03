//! Dragging the dividers between panes. The layout decides where dividers are
//! and what moving one does; this module only follows the pointer.
use super::{
    TerminalView,
    layout::{Axis, DividerAt, DividerId},
    workspace::Appearance,
};
use egui::{Pos2, Rect, pos2};

fn coordinate(axis: Axis, pos: Pos2) -> f32 {
    match axis {
        Axis::X => pos.x,
        Axis::Y => pos.y,
    }
}

fn cursor(axis: Axis) -> egui::CursorIcon {
    match axis {
        Axis::X => egui::CursorIcon::ResizeHorizontal,
        Axis::Y => egui::CursorIcon::ResizeVertical,
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

pub(super) struct Drag {
    workspace: u64,
    divider: DividerId,
    /// How far from the divider's center the pointer grabbed it, so the
    /// divider does not jump to the pointer.
    offset: f32,
    /// Set when the divider disappears or input is lost. The drag still owns
    /// pointer events until the button is released.
    cancelled: bool,
}

impl TerminalView {
    pub(super) fn resize_panes_with_mouse(
        &mut self,
        ui: &mut egui::Ui,
        bounds: Rect,
        style: &Appearance,
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
            let w = self.workspace_mut();
            // Looked up fresh each frame, so the drag survives window resizes
            // and pane changes that keep the divider.
            let divider = w
                .dividers(bounds, style)
                .into_iter()
                .find(|d| d.id == drag.divider);
            drag.cancelled |= !enabled || !focused || drag.workspace != w.id;
            match divider {
                Some(d) if !drag.cancelled => {
                    ui.ctx().set_cursor_icon(cursor(d.axis));
                    if let Some(pos) = pos {
                        let to = coordinate(d.axis, pos) - drag.offset;
                        w.move_divider(bounds, style, d.id, to);
                    }
                }
                _ => drag.cancelled = true,
            }
            if down && !released && focused {
                self.pane_drag = Some(drag);
            }
        } else if enabled
            && focused
            && (!ui.input(|i| i.pointer.any_down()) || pressed)
            && let Some(pos) = pos
            && let Some(d) = self
                .workspace()
                .dividers(bounds, style)
                .into_iter()
                .find(|d| hit(d).contains(pos))
        {
            ui.ctx().set_cursor_icon(cursor(d.axis));
            owned = pressed;
            if pressed {
                self.pane_drag = Some(Drag {
                    workspace: self.workspace().id,
                    divider: d.id,
                    offset: coordinate(d.axis, pos) - d.at,
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
}

//! A small drawing of a workspace, in the preview of the picker of
//! workspaces.
//!
//! Every pane keeps the last frame its terminal drew, also while its
//! workspace is hidden, and a hidden terminal goes on drawing as its shell
//! writes. The miniature of a workspace is those frames, drawn smaller and
//! laid out as the workspace lays out its panes. Nothing is drawn again for
//! it.

use super::{TerminalPane, TerminalView, workspace::Appearance};
use crate::actions::AppAction;

/// The gap around the miniature inside the preview, in points.
const MARGIN: f32 = 6.0;
/// How much of what is behind a floating pane still shows, as in the window.
const DIM: f32 = 90.0 / 255.0;

/// The largest rectangle shaped like `content` that fits the middle of
/// `preview`, with the margin around it. `None` when nothing useful fits.
fn fit(preview: egui::Rect, content: egui::Vec2) -> Option<egui::Rect> {
    if content.x < 1.0 || content.y < 1.0 {
        return None;
    }
    let room = preview.size() - egui::Vec2::splat(MARGIN * 2.0);
    let scale = (room.x / content.x).min(room.y / content.y);
    let size = content * scale;
    if size.x < 16.0 || size.y < 16.0 {
        return None;
    }
    Some(egui::Rect::from_center_size(preview.center(), size))
}

/// Where `rect`, a part of `from`, lands when `from` is drawn as `to`.
fn place(rect: egui::Rect, from: egui::Rect, to: egui::Rect) -> egui::Rect {
    let scale = to.width() / from.width();
    egui::Rect::from_min_size(
        to.min + (rect.min - from.min) * scale,
        rect.size() * scale,
    )
}

/// The largest rectangle shaped like `size` at the top left of `rect`. A
/// pane's last frame has the shape the pane had when it drew, which is
/// not always the shape of its place now.
fn keep_shape(rect: egui::Rect, size: [u32; 2]) -> egui::Rect {
    let size = egui::vec2(size[0].max(1) as f32, size[1].max(1) as f32);
    let scale = (rect.width() / size.x).min(rect.height() / size.y);
    egui::Rect::from_min_size(rect.min, size * scale)
}

impl TerminalView {
    /// With the window's picker showing a preview for one of this window's
    /// workspaces, draw the workspace small in the middle of the preview.
    /// `content` is where the current workspace's panes are laid out.
    /// Called every frame the picker is shown.
    pub(super) fn show_miniature(&self, ctx: &egui::Context, content: egui::Rect, style: &Appearance) {
        let Some((action, preview)) = self.window_picker.as_ref().and_then(|picker| picker.previewed())
        else {
            return;
        };
        let AppAction::SelectWorkspaceId(id) = action else { return };
        let Some(workspace) = self.workspaces.iter().find(|workspace| workspace.id == *id) else {
            return;
        };
        let Some(frame) = fit(*preview, content.size()) else { return };
        let palette = crate::colors::palette();
        // Above the picker, which is drawn in its own layer.
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("workspace-miniature"));
        let painter = ctx.layer_painter(layer);
        let outline = |rect: egui::Rect, focused: bool| {
            let color = if focused { palette.accent } else { palette.border };
            painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, color), egui::StrokeKind::Inside);
        };
        // A pane's frame keeps its own shape wherever it is put.
        let draw = |pane: Option<&TerminalPane>, rect: egui::Rect| {
            // A pane that has never been shown has nothing to draw.
            if let Some((callback, size)) = pane.and_then(|pane| pane.miniature()) {
                painter.add(egui::PaintCallback { rect: keep_shape(rect, size), callback });
            }
        };
        painter.rect_filled(frame.expand(MARGIN / 2.0), 2.0, palette.background);
        let floating = workspace.floats.visible().and_then(|id| self.floating.get(id));
        for (pane, rect) in workspace.rectangles(content, style) {
            let rect = place(rect, content, frame).shrink(0.5);
            if rect.width() < 1.0 || rect.height() < 1.0 {
                continue;
            }
            draw(self.tiled_ref(pane), rect);
            outline(rect, floating.is_none() && workspace.focused == Some(pane));
        }
        // The floating pane, over the tiled ones, which it dims as in the
        // window.
        if let Some(floating) = floating {
            let padding = workspace.floats.size().or(self.floating_padding);
            let rect = place(Self::floating_rect(content, padding), content, frame);
            painter.rect_filled(frame, 0.0, palette.background.gamma_multiply(DIM));
            painter.rect_filled(rect, 0.0, palette.background);
            draw(Some(&floating.value.pane), rect);
            outline(rect, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_miniature_keeps_the_workspace_shape_in_the_middle_of_the_preview() {
        let preview = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(824.0, 212.0));
        let frame = fit(preview, egui::vec2(1600.0, 800.0)).unwrap();
        // All the height, less the margins: 400 by 200.
        assert_eq!(frame.size(), egui::vec2(400.0, 200.0));
        assert_eq!(frame.center(), preview.center());
        // A tall workspace is limited by height too, and a preview too small
        // for anything shows nothing.
        assert_eq!(fit(preview, egui::vec2(400.0, 800.0)).unwrap().size(), egui::vec2(100.0, 200.0));
        assert!(fit(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(40.0, 20.0)), egui::vec2(1600.0, 800.0)).is_none());
    }

    #[test]
    fn panes_keep_their_places_in_the_miniature() {
        let content = egui::Rect::from_min_size(egui::pos2(0.0, 30.0), egui::vec2(1000.0, 500.0));
        let frame = egui::Rect::from_min_size(egui::pos2(600.0, 100.0), egui::vec2(200.0, 100.0));
        let right_half = egui::Rect::from_min_size(egui::pos2(500.0, 30.0), egui::vec2(500.0, 500.0));
        assert_eq!(
            place(right_half, content, frame),
            egui::Rect::from_min_size(egui::pos2(700.0, 100.0), egui::vec2(100.0, 100.0))
        );
    }

    #[test]
    fn a_frame_keeps_its_shape_in_its_place() {
        let cell = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
        // Twice as wide as tall, as the place is: it fills it.
        assert_eq!(keep_shape(cell, [800, 400]), cell);
        // Square: as tall as the place, at its left.
        assert_eq!(
            keep_shape(cell, [500, 500]),
            egui::Rect::from_min_size(cell.min, egui::vec2(100.0, 100.0))
        );
    }
}

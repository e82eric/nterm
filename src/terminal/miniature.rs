//! A small drawing of a workspace or of a waiting session, in the preview
//! of the picker of workspaces.
//!
//! Every pane keeps the last frame its terminal drew, also while its
//! workspace is hidden, and a hidden terminal goes on drawing as its shell
//! writes. The miniature of a workspace is those frames, drawn smaller and
//! laid out as the workspace lays out its panes. Nothing is drawn again for
//! it.
//!
//! A waiting session has no panes here. While its row is selected, a pane
//! is made for each of its shells that only looks: it is the size the
//! shell's terminal has in the daemon, it is given that terminal's screen,
//! and it asks for the screen again every so often. It has no shell, takes
//! no input, and the daemon does not count it as showing the session. Its
//! frames are drawn like any other pane's.

use super::{
    TerminalPane, TerminalView,
    workspace::{Appearance, Group, Workspace},
};
use crate::{actions::AppAction, graphics as glow};
use std::time::{Duration, Instant};

/// The gap around the miniature inside the preview, in points.
const MARGIN: f32 = 6.0;
/// How much of what is behind a floating pane still shows, as in the window.
const DIM: f32 = 90.0 / 255.0;
/// How often a waiting session's screens are asked for again.
const REFRESH: Duration = Duration::from_secs(1);
/// The pixels a looking pane is given until it has said how big its cells
/// are. Any size with room for a grid does.
const FIRST: [u32; 2] = [640, 480];

/// The waiting session whose row is selected, and the panes looking at it.
pub(super) struct Peek {
    /// The session's id; empty for the shells no session describes.
    session: String,
    /// The session's layout. Its pane ids are the daemon's pane numbers.
    workspace: Workspace,
    panes: Vec<Looking>,
    /// The float that was showing, its panes among `panes`. Its pane ids
    /// are the daemon's pane numbers too.
    float: Option<Group>,
    asked: Instant,
}

/// A pane that looks at one of a waiting session's shells.
struct Looking {
    /// The daemon's number for the pane looked at.
    number: u32,
    /// The grid that pane's terminal has.
    grid: [u16; 2],
    /// The pixels this pane needs to have that grid, once its cells' size
    /// is known. It takes them some time after it is given them, and its
    /// frames before then are not shown.
    pixels: Option<[u32; 2]>,
    pane: TerminalPane,
}

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
    /// workspaces or for a waiting session, draw it small in the middle of
    /// the preview. `content` is where the window lays out its panes.
    /// Called every frame: it also makes and drops the panes that look at a
    /// waiting session as the selection changes.
    pub(super) fn show_miniature(
        &mut self,
        ctx: &egui::Context,
        content: egui::Rect,
        style: &Appearance,
        gl: &glow::Context,
    ) {
        let selected = self.window_picker.as_ref().and_then(|picker| picker.previewed()).cloned();
        let session = match &selected {
            Some((AppAction::AttachSession(id), _)) => Some(id.as_str()),
            _ => None,
        };
        if self.peek.as_ref().map(|peek| peek.session.as_str()) != session {
            self.drop_peek();
            self.peek = session.map(|id| self.open_peek(id, gl));
        }
        self.refresh_peek(ctx);
        let Some((action, preview)) = selected else { return };
        let Some(frame) = fit(preview, content.size()) else { return };
        let palette = crate::colors::palette();
        // Above the picker, which is drawn in its own layer.
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("workspace-miniature"));
        let painter = ctx.layer_painter(layer);
        let outline = |rect: egui::Rect, focused: bool| {
            let color = if focused { palette.accent } else { palette.border };
            painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, color), egui::StrokeKind::Inside);
        };
        // A pane's frame keeps its own shape wherever it is put. `wanted`
        // for a pane that is to be drawn only at one size: a frame of
        // another is from before it had the size of the pane it looks at.
        let draw = |pane: Option<&TerminalPane>, rect: egui::Rect, wanted: Option<[u32; 2]>| {
            // A pane that has never been shown has nothing to draw.
            if let Some((callback, size)) = pane.and_then(|pane| pane.miniature())
                && wanted.is_none_or(|wanted| wanted == size)
            {
                painter.add(egui::PaintCallback { rect: keep_shape(rect, size), callback });
            }
        };
        let backdrop = || painter.rect_filled(frame.expand(MARGIN / 2.0), 2.0, palette.background);
        // A float, over the tiled panes, which it dims as in the window, with
        // its panes laid out in it. `pane` draws one of them in its place.
        let float = |padding, group: &Group, pane: &dyn Fn(u64, egui::Rect)| {
            let whole = Self::floating_rect(content, padding);
            painter.rect_filled(frame, 0.0, palette.background.gamma_multiply(DIM));
            painter.rect_filled(place(whole, content, frame), 0.0, palette.background);
            let several = group.panes.len() > 1;
            for (id, rect) in Self::group_rects(group, whole, style) {
                let rect = place(rect, content, frame);
                pane(id, rect);
                if several {
                    outline(rect, group.focused == Some(id));
                }
            }
            outline(place(whole, content, frame), !several);
        };
        let tile = |rect: egui::Rect| {
            let rect = place(rect, content, frame).shrink(0.5);
            (rect.width() >= 1.0 && rect.height() >= 1.0).then_some(rect)
        };
        match action {
            AppAction::SelectWorkspaceId(id) => {
                let Some(workspace) = self.workspaces.iter().find(|w| w.id == id) else { return };
                let floating = workspace.floats.visible().and_then(|id| self.floating.get(id));
                backdrop();
                for (pane, rect) in workspace.rectangles(content, style) {
                    let Some(rect) = tile(rect) else { continue };
                    draw(self.tiled_ref(pane), rect, None);
                    outline(rect, floating.is_none() && workspace.focused == Some(pane));
                }
                if let Some(floating) = floating {
                    let padding = workspace.floats.size().or(self.floating_padding);
                    float(padding, &floating.value, &|pane, rect| draw(self.tiled_ref(pane), rect, None));
                }
            }
            AppAction::AttachSession(_) => {
                let Some(peek) = self.peek.as_ref().filter(|peek| !peek.panes.is_empty()) else {
                    return;
                };
                let looking = |number: u64| {
                    peek.panes.iter().find(|looking| u64::from(looking.number) == number)
                };
                // One with no pixels yet has no frame to show either.
                let wanted = |looking: &Looking| Some(looking.pixels.unwrap_or_default());
                backdrop();
                for (pane, rect) in peek.workspace.rectangles(content, style) {
                    let Some(rect) = tile(rect) else { continue };
                    if let Some(looking) = looking(pane) {
                        draw(Some(&looking.pane), rect, wanted(looking));
                    }
                    outline(rect, peek.float.is_none() && peek.workspace.focused == Some(pane));
                }
                if let Some(group) = &peek.float {
                    float(self.floating_padding, group, &|pane, rect| {
                        if let Some(looking) = looking(pane) {
                            draw(Some(&looking.pane), rect, wanted(looking));
                        }
                    });
                }
            }
            _ => {}
        }
    }

    /// Panes that look at each shell a waiting session shows. One that
    /// cannot be looked at comes back with no panes, so that it is not
    /// asked about again every frame.
    fn open_peek(&mut self, id: &str, gl: &glow::Context) -> Peek {
        let mut peek = Peek {
            session: id.to_owned(),
            workspace: Workspace::new(0, 0),
            panes: Vec::new(),
            float: None,
            asked: Instant::now(),
        };
        let Some(waiting) = super::restore::waiting_workspace(id) else { return peek };
        peek.workspace = waiting.workspace;
        peek.float = waiting.float;
        for shell in waiting.shells {
            let number = shell.info.pane;
            let grid = [shell.info.size.cols, shell.info.size.rows];
            let options = super::pane::SpawnOptions {
                start: crate::session::Start::Look(Box::new(shell)),
                ..Default::default()
            };
            let Ok(pane) = self.create_pane(gl, &options) else { continue };
            if !pane.looks() {
                // Shells are not kept in a daemon, and it has one of its own.
                self.closed_panes.push(pane);
                continue;
            }
            peek.panes.push(Looking { number, grid, pixels: None, pane });
        }
        peek
    }

    /// Keep the looking panes at their sizes, which is also what lets each
    /// ask for its first screen, and ask for the screens again when due.
    fn refresh_peek(&mut self, ctx: &egui::Context) {
        let Some(peek) = &mut self.peek else { return };
        let scale = ctx.pixels_per_point();
        let again = peek.asked.elapsed() >= REFRESH;
        for Looking { grid, pixels, pane, .. } in &mut peek.panes {
            let [width, height] = pixels.unwrap_or(FIRST);
            let size = egui::vec2(width as f32, height as f32) / scale;
            pane.resize(egui::Rect::from_min_size(egui::Pos2::ZERO, size), scale);
            if pixels.is_none()
                && let Some((extra, cell)) = pane.pixel_metrics()
            {
                *pixels = Some([
                    u32::from(grid[0]) * cell[0] + extra[0],
                    u32::from(grid[1]) * cell[1] + extra[1],
                ]);
                ctx.request_repaint();
            }
            if again {
                pane.look_again();
            }
        }
        if again {
            peek.asked = Instant::now();
        }
        if !peek.panes.is_empty() {
            ctx.request_repaint_after(REFRESH);
        }
    }

    /// Stop looking at a waiting session. Its panes are destroyed with the
    /// others that have closed.
    pub(super) fn drop_peek(&mut self) {
        if let Some(peek) = self.peek.take() {
            self.closed_panes.extend(peek.panes.into_iter().map(|looking| looking.pane));
        }
    }

    /// Take the latest frame of each looking pane. True if any had one.
    pub(super) fn acquire_peek_frames(&mut self) -> bool {
        let Some(peek) = &mut self.peek else { return false };
        let mut acquired = false;
        for looking in &mut peek.panes {
            acquired |= looking.pane.acquire_frame();
        }
        acquired
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

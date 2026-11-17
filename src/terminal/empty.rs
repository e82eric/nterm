//! A window with no terminal in it, and why.
//!
//! When shells are kept in the session daemon, a window is made before
//! anything is known about the daemon. Until it has a terminal it says what
//! it is waiting for, asks which session to show, or says that the daemon
//! cannot be reached and offers to try again.

use super::{TerminalView, pane::PaneStatus, workspace::Request};
use crate::{actions::AppAction, graphics as glow, session::Status};

pub(super) enum Empty {
    /// The session daemon is being reached.
    Connecting,
    /// Sessions wait in the daemon: which to show, or a new one. The picker
    /// is the window's own, as there is no pane to hold it.
    Choose(Box<crate::picker::Picker>),
    /// The daemon could not be reached, or was and has gone.
    Unavailable { lost: bool, reason: String },
}

impl Empty {
    fn label(&self) -> &'static str {
        match self {
            Self::Connecting => "CONNECTING",
            Self::Choose(_) => "SESSIONS",
            Self::Unavailable { .. } => "NO DAEMON",
        }
    }
}

impl TerminalView {
    /// Whether the window has a tiled terminal. Floats alone do not keep
    /// a window.
    pub(super) fn has_panes(&self) -> bool {
        self.workspaces.iter().any(|w| !w.panes.is_empty())
    }

    /// What the status bar shows in place of a pane's status.
    pub(super) fn empty_status(&self) -> PaneStatus<'static> {
        let palette = crate::colors::palette();
        PaneStatus {
            label: self.empty.as_ref().map_or("", Empty::label),
            accent: palette.muted_text,
            separator: palette.panel_background,
            detail: None,
            pipe_status: None,
        }
    }

    /// Before a frame: a window whose terminals all went with the daemon
    /// stays, to say so. One whose last terminal was closed, or whose last
    /// shell ended, closes as usual.
    pub(super) fn note_daemon_loss(&mut self) {
        if self.empty.is_some() || self.close_requested || self.has_panes() {
            return;
        }
        let (lost, reason) = match crate::session::status() {
            Status::Lost => (true, String::new()),
            Status::Unavailable(reason) => (false, reason),
            _ => return,
        };
        // Nothing of what the window knew of the daemon holds any more.
        self.sessions.clear();
        self.sessions_sent.clear();
        self.offered = None;
        self.key_table = None;
        self.empty = Some(Empty::Unavailable { lost, reason });
        self.context.request_repaint();
    }

    /// A frame of a window without a terminal.
    pub(super) fn update_empty(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        let ctx = ui.ctx().clone();
        let available = ui.max_rect();
        let status_height = 24.0_f32.min(available.height());
        let status = egui::Rect::from_min_max(
            egui::pos2(available.left(), available.bottom() - status_height),
            available.max,
        );
        let content = egui::Rect::from_min_max(available.min, status.right_top());
        ui.painter().rect_filled(content, 0.0, crate::colors::palette().background);

        let now = crate::session::status();
        let Some(empty) = self.empty.take() else { return };
        let before = std::mem::discriminant(&empty);
        self.empty = match empty {
            Empty::Connecting => match now {
                Status::Idle => {
                    crate::session::connect();
                    Some(Empty::Connecting)
                }
                Status::Connecting => {
                    say(ui, content, "Connecting to the session daemon", "");
                    Some(Empty::Connecting)
                }
                Status::Unavailable(reason) => Some(Empty::Unavailable { lost: false, reason }),
                Status::Lost => Some(Empty::Unavailable { lost: true, reason: String::new() }),
                Status::Off => self.first_shell(gl),
                Status::Connected => {
                    let waiting = self.chooser(&ctx, None);
                    match waiting {
                        Some(picker) => Some(Empty::Choose(picker)),
                        None => self.first_shell(gl),
                    }
                }
            },
            // The daemon went while the choice was open: say so next frame.
            Empty::Choose(picker) if now != Status::Connected => {
                picker.surrender_focus();
                self.offered = None;
                Some(Empty::Connecting)
            }
            // Another window took a session, or left one: the choice is
            // made again from what waits now, with what was typed into it.
            Empty::Choose(picker) if self.sessions_changed() => {
                let query = picker.query();
                picker.surrender_focus();
                drop(picker);
                ctx.request_repaint();
                match self.chooser(&ctx, Some(query)) {
                    Some(picker) => Some(Empty::Choose(picker)),
                    None => self.first_shell(gl),
                }
            }
            Empty::Choose(mut picker) => match picker.show_scoped(&ctx, content, None, true) {
                None => Some(Empty::Choose(picker)),
                Some(action) => {
                    picker.surrender_focus();
                    drop(picker);
                    self.chosen(action, gl)
                }
            },
            // Another window asked again, and it is under way or done.
            Empty::Unavailable { .. } if matches!(now, Status::Connecting | Status::Connected) => {
                Some(Empty::Connecting)
            }
            Empty::Unavailable { lost, reason } => {
                let title = match lost {
                    true => "The session daemon has gone",
                    false => "The session daemon cannot be reached",
                };
                let detail = match lost {
                    true => "The shells it was running ended with it.",
                    false => reason.as_str(),
                };
                let below = say(ui, content, title, detail);
                if retry(ui, content, below) {
                    crate::session::connect();
                    Some(Empty::Connecting)
                } else {
                    Some(Empty::Unavailable { lost, reason })
                }
            }
        };
        // What this frame drew is what the window was. Draw what it has
        // become: nothing else asks for the frame, and a terminal just made
        // starts its shell only once it has been laid out.
        if self.empty.as_ref().map(std::mem::discriminant) != Some(before) {
            ctx.request_repaint();
        }
        self.show_mode_status(ui, status, &[]);
        self.toasts.show(&ctx, content, egui::Id::new("window-toasts"), None, 0.0);
    }

    /// The picker that asks which waiting session to show, with `query`
    /// typed into it. `None` when
    /// none waits.
    fn chooser(
        &mut self,
        ctx: &egui::Context,
        query: Option<String>,
    ) -> Option<Box<crate::picker::Picker>> {
        let mut entries = self.offer_waiting();
        if entries.is_empty() {
            self.offered = None;
            return None;
        }
        entries.push(crate::actions::Entry {
            id: "session.new".into(),
            label: "New session".into(),
            shortcut: None,
            action: AppAction::NewSession,
        });
        let mut options = super::workspace_ui::entry_picker(entries);
        if let Some(query) = query {
            options.params.insert("query".into(), query);
        }
        // Its rows are given to it, so it reads no terminal.
        Some(Box::new(crate::picker::Picker::start_replacing(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            options,
            &self.picker_bindings.prompt_patterns,
            None,
        )))
    }

    /// The choice was made: a session, or, by its row or by Escape, a new
    /// shell.
    fn chosen(&mut self, action: crate::picker::Action, gl: &glow::Context) -> Option<Empty> {
        let crate::picker::Action::Execute(AppAction::AttachSession(id)) = action else {
            self.offered = None;
            return self.first_shell(gl);
        };
        match self.attach_session(&id, gl) {
            Ok(()) => None,
            Err(error) => {
                self.show_error("Session could not be shown", error);
                self.first_shell(gl)
            }
        }
    }

    /// The window's first terminal, on a new shell. On failure the window
    /// stays empty and says why.
    fn first_shell(&mut self, gl: &glow::Context) -> Option<Empty> {
        match self.workspace_request(Request::CreatePane(Default::default()), gl) {
            Ok(_) => None,
            Err(reason) => Some(Empty::Unavailable { lost: false, reason }),
        }
    }
}

/// A title and what is behind it, in the middle of `content`. Returns the
/// height at which they end.
fn say(ui: &egui::Ui, content: egui::Rect, title: &str, detail: &str) -> f32 {
    let palette = crate::colors::palette();
    let painter = ui.painter().with_clip_rect(content);
    let width = (content.width() - 48.0).max(0.0);
    let title =
        painter.layout(title.to_owned(), egui::FontId::monospace(15.0), palette.text, width);
    let detail = painter.layout(
        detail.trim().to_owned(),
        egui::FontId::monospace(13.0),
        palette.muted_text,
        width,
    );
    let gap = if detail.size().y > 0.0 { 10.0 } else { 0.0 };
    let top = content.center().y - (title.size().y + gap + detail.size().y) / 2.0;
    let left = |width: f32| content.center().x - width / 2.0;
    let detail_top = top + title.size().y + gap;
    let bottom = detail_top + detail.size().y;
    painter.galley(egui::pos2(left(title.size().x), top), title, palette.text);
    painter.galley(egui::pos2(left(detail.size().x), detail_top), detail, palette.muted_text);
    bottom
}

/// The offer to try again, under what `say` wrote. True when it is taken,
/// by the button or by Enter.
fn retry(ui: &mut egui::Ui, content: egui::Rect, below: f32) -> bool {
    let size = egui::vec2(132.0, 28.0);
    let rect = egui::Rect::from_center_size(
        egui::pos2(content.center().x, below + 18.0 + size.y / 2.0),
        size,
    );
    let clicked = ui.put(rect, egui::Button::new("Retry  (Enter)")).clicked();
    clicked || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
}

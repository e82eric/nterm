//! A picker the session daemon runs for a program in the pane's shell. The
//! daemon lists and searches where the program is, which may be another
//! machine; this page draws the rows it is sent and says what is typed and
//! which row is chosen. It reads no file and runs nothing here.
use super::{Action, config, font};
use crate::session::Shown;
use nfm_egui::fed::Rows;
use std::sync::Arc;

pub(crate) struct Fed {
    context: egui::Context,
    shared: nfm_egui::Picker<Rows>,
    shown: Arc<Shown>,
    label: String,
    anchor_bounds: Option<egui::Rect>,
    pub(crate) cursor_row: Option<u32>,
    pub(crate) typography: font::Typography,
}

impl Fed {
    pub(crate) fn start(ctx: egui::Context, shown: Arc<Shown>) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let label = shown.label.to_uppercase();
        let asked = shown.clone();
        let (shared, feed) = nfm_egui::Picker::new_fed(
            ("nterm-fed-picker", id),
            &ctx,
            nfm_egui::PickerConfig {
                title: label.clone(),
                initial_query: shown.query.chars().take(4096).collect(),
                // The daemon sends no previews yet.
                preview_visible: false,
                ..Default::default()
            },
            move |ask| asked.ask(ask),
        );
        shown.feed(feed);
        Self {
            context: ctx,
            shared,
            shown,
            label,
            anchor_bounds: None,
            cursor_row: None,
            typography: font::Typography::default(),
        }
    }
    pub(crate) fn label(&self) -> &str {
        &self.label
    }
    /// Whether the daemon ended it, and with what message if it gave one.
    pub(crate) fn closed(&self) -> Option<Option<String>> {
        self.shown.closed()
    }
    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        self.shared.query()
    }
    pub(crate) fn surrender_focus(&self) {
        self.context.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        self.context.request_repaint();
    }
    /// Draw the picker and report what the user did. Escape closes it with
    /// nothing chosen; Enter tells the daemon which row, and the daemon
    /// gives the program what the row is.
    pub(crate) fn show_scoped(
        &mut self,
        ctx: &egui::Context,
        pane: egui::Rect,
        _workspace: Option<egui::Rect>,
        interactive: bool,
    ) -> Option<Action> {
        self.anchor_bounds = Some(pane);
        let appearance = super::appearance(&self.typography, ctx);
        let placement = super::placement(
            config::Position::default(),
            self.cursor_row,
            self.anchor_bounds,
            self.typography.row_height,
            pane,
        );
        match self.shared.show(ctx, placement, &appearance, !interactive).event {
            Some(nfm_egui::PickerEvent::Cancelled) => {
                self.shown.close(None);
                Some(Action::Cancel)
            }
            Some(nfm_egui::PickerEvent::Accepted(selection)) => {
                let rows = selection.source_snapshot::<Rows>()?;
                self.shown.accept(&rows, selection.item).then_some(Action::Close)
            }
            _ => None,
        }
    }
}

impl Drop for Fed {
    /// However the page goes, the program is not left waiting.
    fn drop(&mut self) {
        self.shown.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfm_egui::fed::{Found, Page, Row};
    use nterm_session::protocol::Control;
    use std::sync::mpsc;

    /// A picker as the daemon would have asked for it, with what the page
    /// tells the daemon.
    fn shown(query: &str) -> (Arc<Shown>, mpsc::Receiver<Control>) {
        let (tell, told) = mpsc::channel();
        let tell = std::sync::Mutex::new(tell);
        let shown = Shown::new(
            7,
            "Files".into(),
            query.into(),
            move |control| {
                let _ = tell.lock().unwrap().send(control);
            },
            || {},
        );
        (Arc::new(shown), told)
    }
    /// The daemon's answer `serial`, to the query of `revision`.
    fn found(shown: &Shown, serial: u64, revision: u64, query: &str, rows: &[(usize, &str)]) {
        let rows: Vec<Row> =
            rows.iter().map(|(index, text)| Row { index: *index, text: (*text).into(), fields: None }).collect();
        let matched = rows.len();
        let page = Page { version: 1, rows, matched_against: query.into(), header: None, template: None };
        let (source, results) = page.into_rows();
        shown.found(
            serial,
            Found {
                revision,
                query: query.into(),
                source: Arc::new(source),
                epoch: 0,
                append_only: true,
                results,
                matched,
                total: 40,
                skipped: 0,
                done: true,
            },
        );
    }
    fn frame(ctx: &egui::Context, fed: &mut Fed, events: Vec<egui::Event>) -> Option<Action> {
        let mut action = None;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let mut output = ctx.run_ui(
            egui::RawInput { screen_rect: Some(screen), events, ..Default::default() },
            |ui| {
                // A discarded pass is run again; keep the action of either.
                let shown = fed.show_scoped(ui.ctx(), ui.ctx().content_rect(), None, true);
                if shown.is_some() {
                    action = shown;
                }
            },
        );
        output.textures_delta.clear();
        action
    }
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn told(told: &mpsc::Receiver<Control>) -> Vec<Control> {
        told.try_iter().collect()
    }

    #[test]
    fn the_daemon_is_asked_what_is_typed_and_told_which_row_is_chosen() {
        let ctx = egui::Context::default();
        let (shown, daemon) = shown("al");
        let mut fed = Fed::start(ctx.clone(), shown.clone());
        assert_eq!((fed.label(), fed.query()), ("FILES", "al"));
        // It asks at once for the query it opens with.
        let asked = Control::PickerQuery { picker: 7, revision: 0, query: "al".into(), rows: 200 };
        assert_eq!(told(&daemon), [asked]);

        found(&shown, 1, 0, "al", &[(12, "a\\alpha.txt"), (3, "b\\also.rs")]);
        frame(&ctx, &mut fed, vec![]);
        frame(&ctx, &mut fed, vec![]);
        // Nothing is said to the daemon for rows only being drawn.
        assert_eq!(told(&daemon), []);

        // What is typed is asked for, as the next revision.
        frame(&ctx, &mut fed, vec![egui::Event::Text("s".into())]);
        let asked = Control::PickerQuery { picker: 7, revision: 1, query: "als".into(), rows: 200 };
        assert_eq!(told(&daemon), [asked]);
        // An answer to what was asked before is not what Enter chooses from.
        assert!(frame(&ctx, &mut fed, vec![key(egui::Key::Enter)]).is_none());
        assert_eq!(told(&daemon), []);

        found(&shown, 2, 1, "als", &[(3, "b\\also.rs")]);
        frame(&ctx, &mut fed, vec![]);
        let action = frame(&ctx, &mut fed, vec![key(egui::Key::Enter)]);
        assert!(matches!(action, Some(Action::Close)));
        // Only which row, of which answer: the daemon knows what it is.
        assert_eq!(told(&daemon), [Control::PickerAccept { picker: 7, serial: 2, row: 3 }]);
        // The page going says nothing more.
        drop(fed);
        assert_eq!(told(&daemon), []);
    }

    #[test]
    fn a_picker_closed_here_is_said_to_the_daemon_once() {
        let ctx = egui::Context::default();
        let (shown, daemon) = shown("");
        let mut fed = Fed::start(ctx.clone(), shown.clone());
        found(&shown, 1, 0, "", &[(0, "top.md")]);
        frame(&ctx, &mut fed, vec![]);
        told(&daemon);
        let action = frame(&ctx, &mut fed, vec![key(egui::Key::Escape)]);
        assert!(matches!(action, Some(Action::Cancel)));
        assert_eq!(told(&daemon), [Control::PickerClose { picker: 7, refused: None }]);
        drop(fed);
        assert_eq!(told(&daemon), []);

        // A page that goes with its pane, with nothing pressed.
        let (shown, daemon) = self::shown("");
        let fed = Fed::start(ctx.clone(), shown);
        told(&daemon);
        drop(fed);
        assert_eq!(told(&daemon), [Control::PickerClose { picker: 7, refused: None }]);
    }

    #[test]
    fn a_picker_the_daemon_ended_says_so_and_is_not_answered() {
        let ctx = egui::Context::default();
        let (shown, daemon) = shown("");
        let mut fed = Fed::start(ctx.clone(), shown.clone());
        // Found before the page was there to be told is kept for it.
        let (early, _) = self::shown("");
        found(&early, 1, 0, "", &[(0, "top.md")]);
        let mut late = Fed::start(ctx.clone(), early.clone());
        frame(&ctx, &mut late, vec![]);
        assert!(matches!(frame(&ctx, &mut late, vec![key(egui::Key::Enter)]), Some(Action::Close)));

        found(&shown, 1, 0, "", &[(0, "top.md")]);
        frame(&ctx, &mut fed, vec![]);
        told(&daemon);
        assert_eq!(fed.closed(), None);
        shown.ended(Some("the walk failed".into()));
        assert_eq!(fed.closed(), Some(Some("the walk failed".into())));
        // Nothing is chosen from it, and the daemon is told nothing of it.
        assert!(frame(&ctx, &mut fed, vec![key(egui::Key::Enter)]).is_none());
        frame(&ctx, &mut fed, vec![egui::Event::Text("x".into())]);
        drop(fed);
        assert_eq!(told(&daemon), []);
    }
}

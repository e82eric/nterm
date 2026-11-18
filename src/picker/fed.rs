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
                preview_visible: shown.preview == Some(true),
                ..Default::default()
            },
            move |ask| asked.ask(ask),
        );
        let mut shared = shared;
        if shown.preview.is_some() {
            // The daemon says what shows for a row, as text, and it is laid
            // out here: coloured by the name of the file it is of. Nothing
            // is opened or run for it on this machine.
            let asked = shown.clone();
            let provider = nfm_preview_file::resolved::ResolvedPreviewProvider::new(
                move |selection: &nfm_egui::Selection<usize>, cancel| {
                    let rows = selection.source_snapshot::<Rows>().ok_or("The rows have changed")?;
                    let previewed = asked.preview_of(&rows, selection.item, &|| cancel.is_cancelled())?;
                    Ok(Some(plan(previewed)))
                },
            );
            shared.set_preview_provider(&ctx, Arc::new(provider));
        }
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

/// How what the daemon sent is laid out.
fn plan(previewed: crate::session::Previewed) -> nfm_preview_file::resolved::PreviewPlan {
    use nfm_preview_file::text::{SnapshotSource, TextContent, TextPreviewOptions, TextPreviewRequest};
    let mut text = previewed.text;
    if previewed.truncated {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("[there is more: only the start is shown]\n");
    }
    // Text of no file is shown as it is: a listing, or a note.
    let content = match &previewed.name {
        Some(_) => TextContent::new(text),
        None => TextContent::plain(text),
    };
    nfm_preview_file::resolved::PreviewPlan::Text {
        request: TextPreviewRequest {
            source: Arc::new(SnapshotSource(content)),
            name: previewed.name,
            extension: None,
            center_line: None,
            highlight_line: None,
            syntax: None,
        },
        options: TextPreviewOptions::default(),
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

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::session::Previewed;
    use nfm_egui::fed::{Found, Page, Row};
    use nterm_session::protocol::Control;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn shown(preview: Option<bool>) -> (Arc<Shown>, mpsc::Receiver<Control>) {
        let (tell, told) = mpsc::channel();
        let tell = std::sync::Mutex::new(tell);
        let tell = move |control| {
            let _ = tell.lock().unwrap().send(control);
        };
        let shown = Shown::new(7, "Files".into(), String::new(), tell, || {});
        let mut shown = shown;
        shown.preview = preview;
        (Arc::new(shown), told)
    }
    /// The daemon's answer `serial` to the first query: one row, number 5.
    fn found(shown: &Shown, serial: u64) -> Arc<Rows> {
        let rows = vec![Row { index: 5, text: "src\\main.rs".into(), fields: None }];
        let page = Page { version: 1, rows, matched_against: String::new(), header: None, template: None };
        let (source, results) = page.into_rows();
        let source = Arc::new(source);
        shown.found(
            serial,
            Found {
                revision: 0,
                query: String::new(),
                source: source.clone(),
                epoch: 0,
                append_only: true,
                results,
                matched: 1,
                total: 1,
                skipped: 0,
                done: true,
            },
        );
        source
    }
    fn frame(ctx: &egui::Context, fed: &mut Fed) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let mut output = ctx.run_ui(
            egui::RawInput { screen_rect: Some(screen), ..Default::default() },
            |ui| {
                fed.show_scoped(ui.ctx(), ui.ctx().content_rect(), None, true);
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn the_daemon_is_asked_what_shows_for_the_selected_row() {
        let ctx = egui::Context::default();
        let (shown, daemon) = shown(Some(true));
        let mut fed = Fed::start(ctx.clone(), shown.clone());
        found(&shown, 3);
        // The preview is asked for from the thread that lays it out, once
        // the row has been drawn as the selected one.
        let deadline = Instant::now() + Duration::from_secs(10);
        let asked = loop {
            frame(&ctx, &mut fed);
            let asked = daemon.try_iter().find(|said| matches!(said, Control::PickerPreview { .. }));
            if let Some(asked) = asked {
                break asked;
            }
            assert!(Instant::now() < deadline, "no preview was asked for");
            std::thread::sleep(Duration::from_millis(10));
        };
        // Which row of which answer, and nothing of what the row is.
        assert_eq!(asked, Control::PickerPreview { picker: 7, request: 1, serial: 3, row: 5 });
        let text = "fn main() {}\n".to_owned();
        shown.previewed(1, Ok(Previewed { text, name: Some("main.rs".into()), truncated: false }));
        frame(&ctx, &mut fed);

        // A picker with no previews asks for none.
        let (plain, daemon) = self::shown(None);
        let mut fed = Fed::start(ctx.clone(), plain.clone());
        found(&plain, 1);
        for _ in 0..20 {
            frame(&ctx, &mut fed);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!daemon.try_iter().any(|said| matches!(said, Control::PickerPreview { .. })));
    }

    #[test]
    fn a_preview_is_waited_for_until_it_comes_or_is_not_wanted() {
        let (shown, daemon) = shown(Some(false));
        let rows = found(&shown, 4);
        let wait = |shown: &Arc<Shown>, rows: &Arc<Rows>, cancelled: bool| {
            let (shown, rows) = (shown.clone(), rows.clone());
            std::thread::spawn(move || shown.preview_of(&rows, 5, &move || cancelled))
        };
        let asked = |daemon: &mpsc::Receiver<Control>| loop {
            match daemon.recv_timeout(Duration::from_secs(10)).expect("no preview was asked for") {
                Control::PickerPreview { request, serial, row, .. } => break (request, serial, row),
                _ => {}
            }
        };

        // It comes.
        let waiting = wait(&shown, &rows, false);
        assert_eq!(asked(&daemon), (1, 4, 5));
        // An answer to another request is not this one's.
        shown.previewed(9, Err("stale".into()));
        let text = Previewed { text: "x".into(), name: None, truncated: false };
        shown.previewed(1, Ok(Previewed { text: "x".into(), name: None, truncated: false }));
        assert_eq!(waiting.join().unwrap(), Ok(text));

        // The daemon says there is none.
        let waiting = wait(&shown, &rows, false);
        assert_eq!(asked(&daemon).0, 2);
        shown.previewed(2, Err("Access is denied.".into()));
        assert_eq!(waiting.join().unwrap(), Err("Access is denied.".into()));

        // Another row was selected.
        assert!(wait(&shown, &rows, true).join().unwrap().is_err());
        // Rows that are not of an answer the picker was given.
        let (other, _) = self::shown(Some(false));
        let stranger = found(&other, 1);
        assert!(shown.preview_of(&stranger, 5, &|| false).is_err());

        // The picker ends while one is waited for.
        let waiting = wait(&shown, &rows, false);
        asked(&daemon);
        asked(&daemon);
        shown.ended(None);
        assert!(waiting.join().unwrap().is_err());
        assert!(shown.preview_of(&rows, 5, &|| false).is_err());
    }

    #[test]
    fn what_the_daemon_sent_is_laid_out_as_text_and_said_to_be_cut() {
        use nfm_preview_file::resolved::PreviewPlan;
        let read = |previewed: Previewed| {
            let PreviewPlan::Text { request, .. } = plan(previewed) else { panic!("not text") };
            let content = request.source.read(usize::MAX, &|| false).unwrap();
            (content.text, content.syntax.is_some(), request.name)
        };
        // A file is coloured by its name.
        let file = Previewed { text: "fn a() {}\n".into(), name: Some("a.rs".into()), truncated: false };
        assert_eq!(read(file), ("fn a() {}\n".into(), false, Some("a.rs".into())));
        // A listing is shown plain.
        let listing = Previewed { text: "sub\\\nb.txt".into(), name: None, truncated: true };
        let (text, plain, name) = read(listing);
        assert_eq!(text, "sub\\\nb.txt\n[there is more: only the start is shown]\n");
        assert!(plain && name.is_none());
    }
}

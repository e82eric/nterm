//! A picker the session daemon runs for a program in the pane's shell. The
//! daemon lists and searches where the program is, which may be another
//! machine; this page draws the rows it is sent and says what is typed and
//! which row is chosen. It reads no file and runs nothing here.
use super::{Action, config, font};
use crate::session::Shown;
use nfm_egui::fed::Rows;
use std::sync::Arc;

/// What is done with the row chosen of a picker the pane asked the daemon
/// for: the Lua action of its definition, run with what it was opened with,
/// or with none the row's value is typed into the pane.
pub(crate) struct Accept {
    pub(crate) callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    pub(crate) params: std::collections::BTreeMap<String, String>,
}

pub(crate) struct Fed {
    context: egui::Context,
    shared: nfm_egui::Picker<Rows>,
    shown: Arc<Shown>,
    /// What is done with the row chosen, of a picker the pane asked for.
    accept: Option<Accept>,
    label: String,
    anchor_bounds: Option<egui::Rect>,
    pub(crate) cursor_row: Option<u32>,
    pub(crate) typography: font::Typography,
}

impl Fed {
    /// A picker the pane asked the daemon for itself: no program is given
    /// the row chosen, and `accept` says what is done with it here.
    pub(crate) fn start_asked(ctx: egui::Context, shown: Arc<Shown>, accept: Accept) -> Self {
        let mut page = Self::start(ctx, shown);
        page.accept = Some(accept);
        page
    }
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
            accept: None,
            label,
            anchor_bounds: None,
            cursor_row: None,
            typography: font::Typography::default(),
        }
    }
    pub(crate) fn label(&self) -> &str {
        &self.label
    }
    /// What the daemon says is done to the pane the picker is over, once.
    pub(crate) fn steps(&self) -> Vec<nterm_session::protocol::PickerStep> {
        self.shown.steps()
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
        // The daemon has said what the row chosen is.
        if let Some((text, value)) = self.shown.chosen() {
            return Some(match &self.accept {
                Some(Accept { callback: Some(callback), params }) => Action::Custom(super::CustomAction {
                    callback: callback.clone(),
                    text,
                    filesystem: false,
                    range: None,
                    value,
                    params: params.clone(),
                    accepted: true,
                }),
                _ => Action::SendInput(value),
            });
        }
        let appearance = super::appearance(&self.typography, ctx);
        let placement = super::placement(
            if self.shown.at_cursor { config::Position::Cursor } else { config::Position::default() },
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
                // A picker the pane asked for stays until the daemon says
                // what the row is: see above.
                let waits = self.shown.asked.is_some();
                (self.shown.accept(&rows, selection.item) && !waits).then_some(Action::Close)
            }
            _ => None,
        }
    }
}

/// The most lines kept of what a program printed, sent as a preview.
const MOST_PRINTED_LINES: usize = 10_000;

/// How what the daemon sent is laid out.
fn plan(previewed: crate::session::Previewed) -> nfm_preview_file::resolved::PreviewPlan {
    use nfm_preview_file::text::{SnapshotSource, TextContent, TextPreviewOptions, TextPreviewRequest};
    use nterm_session::protocol::PreviewKind;
    match previewed.bytes {
        Some((PreviewKind::Image, bytes)) => {
            return nfm_preview_file::resolved::PreviewPlan::Image { bytes: bytes.into() };
        }
        Some((PreviewKind::Ansi, bytes)) => {
            return nfm_preview_file::resolved::PreviewPlan::Ansi {
                max_bytes: bytes.len().max(1),
                bytes: bytes.into(),
                center_line: previewed.line.filter(|line| *line > 0),
                max_scrollback_lines: MOST_PRINTED_LINES,
            };
        }
        None => {}
    }
    let mut text = previewed.text;
    if previewed.truncated {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("[there is more: only part is shown]\n");
    }
    // Text of no file, with nothing said of how to colour it, is shown as
    // it is: a listing, or a note.
    let said = previewed.name.is_some() || previewed.extension.is_some() || previewed.syntax.is_some();
    let content = if said { TextContent::new(text) } else { TextContent::plain(text) };
    nfm_preview_file::resolved::PreviewPlan::Text {
        request: TextPreviewRequest {
            source: Arc::new(SnapshotSource(content)),
            name: previewed.name,
            extension: previewed.extension,
            center_line: previewed.line,
            highlight_line: previewed.line,
            syntax: previewed.syntax.as_deref().map(crate::picker::config::file_syntax),
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
mod asked_tests {
    use super::*;
    use nfm_egui::fed::{Found, Page, Row};
    use nterm_session::protocol::Control;
    use std::sync::mpsc;

    /// A picker the pane asked the daemon for, with what the page tells
    /// the daemon.
    fn asked() -> (Arc<Shown>, mpsc::Receiver<Control>) {
        let (tell, told) = mpsc::channel();
        let tell = std::sync::Mutex::new(tell);
        let mut shown = Shown::new(
            9,
            "Ripgrep".into(),
            String::new(),
            move |control| {
                let _ = tell.lock().unwrap().send(control);
            },
            || {},
        );
        shown.asked = Some(4);
        (Arc::new(shown), told)
    }
    fn found(shown: &Shown, text: &str) {
        let rows = vec![Row { index: 5, text: text.into(), fields: None }];
        let page = Page { version: 1, rows, matched_against: String::new(), header: None, template: None };
        let (source, results) = page.into_rows();
        shown.found(
            1,
            Found {
                revision: 0,
                query: String::new(),
                source: Arc::new(source),
                epoch: 0,
                append_only: true,
                results,
                matched: 1,
                total: 1,
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
                let shown = fed.show_scoped(ui.ctx(), ui.ctx().content_rect(), None, true);
                if shown.is_some() {
                    action = shown;
                }
            },
        );
        output.textures_delta.clear();
        action
    }
    fn enter() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn accept() -> Accept {
        Accept { callback: None, params: Default::default() }
    }

    #[test]
    fn a_picker_the_pane_asked_for_stays_until_it_is_told_what_the_row_chosen_is() {
        let ctx = egui::Context::default();
        let (shown, daemon) = asked();
        let mut fed = Fed::start_asked(ctx.clone(), shown.clone(), accept());
        found(&shown, "src/main.rs:3:1:fn main() {");
        frame(&ctx, &mut fed, vec![]);
        daemon.try_iter().count();
        // The daemon is told which row, and the page waits for what it is.
        assert!(frame(&ctx, &mut fed, vec![enter()]).is_none());
        let told: Vec<Control> = daemon.try_iter().collect();
        assert_eq!(told, [Control::PickerAccept { picker: 9, serial: 1, row: 5 }]);
        assert!(frame(&ctx, &mut fed, vec![]).is_none());
        // With no action of its own, what the row is is typed into the pane.
        shown.chose("src/main.rs:3:1:fn main() {".into(), "the value".into());
        let action = frame(&ctx, &mut fed, vec![]);
        assert!(matches!(action, Some(Action::SendInput(value)) if value == "the value"));
        // Once: the page is not told it again, and says no more to the daemon.
        assert!(frame(&ctx, &mut fed, vec![]).is_none());
        drop(fed);
        assert_eq!(daemon.try_iter().count(), 0);
    }

    #[test]
    fn a_picker_that_ends_before_saying_what_was_chosen_says_so() {
        let ctx = egui::Context::default();
        let (shown, _daemon) = asked();
        let mut fed = Fed::start_asked(ctx.clone(), shown.clone(), accept());
        found(&shown, "one");
        frame(&ctx, &mut fed, vec![]);
        assert!(frame(&ctx, &mut fed, vec![enter()]).is_none());
        assert_eq!(fed.closed(), None);
        // The daemon ends it in place of saying what the row is.
        shown.ended(Some("the row is not one the picker has".into()));
        assert_eq!(fed.closed(), Some(Some("the row is not one the picker has".into())));
        assert!(frame(&ctx, &mut fed, vec![]).is_none());
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
        shown.previewed(1, Ok(Previewed { text, name: Some("main.rs".into()), truncated: false, line: None, bytes: None, syntax: None, extension: None }));
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
        let text = Previewed { text: "x".into(), name: None, truncated: false, line: None, bytes: None, syntax: None, extension: None };
        shown.previewed(1, Ok(Previewed { text: "x".into(), name: None, truncated: false, line: None, bytes: None, syntax: None, extension: None }));
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
        // The line a row is about is shown in the middle, and marked.
        let at = Previewed { text: "a\nb\nc\n".into(), name: Some("a.rs".into()), truncated: false, line: Some(2), bytes: None, syntax: None, extension: None };
        let PreviewPlan::Text { request, .. } = plan(at) else { panic!("not text") };
        assert_eq!((request.center_line, request.highlight_line), (Some(2), Some(2)));
        // A picture, and what a program printed, are laid out as those.
        use nterm_session::protocol::PreviewKind;
        let bytes = |kind, bytes: &[u8]| Previewed {
            text: String::new(),
            name: None,
            truncated: false,
            line: None,
            bytes: Some((kind, bytes.to_vec())),
            syntax: None,
            extension: None,
        };
        let picture = plan(bytes(PreviewKind::Image, &[137, 80, 78, 71]));
        assert!(matches!(&picture, PreviewPlan::Image { bytes } if bytes[..] == [137, 80, 78, 71]));
        assert!(picture.validate().is_ok());
        for printed in [&b"\x1b[31mred"[..], &b""[..]] {
            let laid = plan(bytes(PreviewKind::Ansi, printed));
            assert!(matches!(&laid, PreviewPlan::Ansi { bytes, .. } if &bytes[..] == printed));
            assert!(laid.validate().is_ok());
        }
        // Text the daemon says how to colour is not shown plain, and what a
        // program printed is shown at the line named.
        let coloured = |syntax: Option<&str>, extension: Option<&str>| Previewed {
            text: "x = 1\n".into(),
            name: None,
            truncated: false,
            line: None,
            bytes: None,
            syntax: syntax.map(str::to_owned),
            extension: extension.map(str::to_owned),
        };
        let PreviewPlan::Text { request, .. } = plan(coloured(None, Some("lua"))) else { panic!("not text") };
        assert_eq!(request.extension.as_deref(), Some("lua"));
        assert!(request.source.read(usize::MAX, &|| false).unwrap().syntax.is_none());
        let PreviewPlan::Text { request, .. } = plan(coloured(Some("PlainText"), None)) else { panic!("not text") };
        assert!(matches!(request.syntax, Some(nfm_preview_file::SyntaxChoice::PlainText)));
        let mut printed = bytes(PreviewKind::Ansi, b"a\nb\nc\n");
        printed.line = Some(3);
        assert!(matches!(plan(printed), PreviewPlan::Ansi { center_line: Some(3), .. }));
        // A file is coloured by its name.
        let file = Previewed { text: "fn a() {}\n".into(), name: Some("a.rs".into()), truncated: false, line: None, bytes: None, syntax: None, extension: None };
        assert_eq!(read(file), ("fn a() {}\n".into(), false, Some("a.rs".into())));
        // A listing is shown plain.
        let listing = Previewed { text: "sub\\\nb.txt".into(), name: None, truncated: true, line: None, bytes: None, syntax: None, extension: None };
        let (text, plain, name) = read(listing);
        assert_eq!(text, "sub\\\nb.txt\n[there is more: only part is shown]\n");
        assert!(plain && name.is_none());
    }
}

//! An output page: a program runs and what it prints is shown as it prints
//! it, to read. An `Output` definition opens one. The program is stopped when
//! the page goes.
//!
//! With a `line` function, the definition says what to show for each line
//! the program prints: the lines are taken here as the page is drawn, where
//! Lua runs, and the text made of them is published to the page.
use super::{Action, config, font};

pub(crate) struct Output {
    context: egui::Context,
    shared: nfm_egui::Output,
    position: config::Position,
    pub(crate) scope: config::Scope,
    anchor_bounds: Option<egui::Rect>,
    pub(crate) cursor_row: Option<u32>,
    pub(crate) from_copy: bool,
    pub(crate) typography: font::Typography,
    /// The program's lines, for a definition that says what to show for each.
    lines: Option<Lines>,
}

impl Output {
    /// Start the program and show what it prints.
    pub(crate) fn start(
        ctx: egui::Context,
        from_copy: bool,
        options: config::OutputOptions,
    ) -> Result<Self, String> {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let spec = command(&options)?;
        let writer = nfm_egui::OutputWriter::new();
        let lines = match options.line {
            None => nfm_output::run(spec, writer.clone(), options.format).map(|()| None),
            Some(filter) => {
                let (send, receive) = std::sync::mpsc::channel();
                let wake = ctx.clone();
                nfm_output::run_lines(spec, &writer, move |report| {
                    // The page is gone if nothing receives; the run ends with it.
                    send.send(report).ok();
                    wake.request_repaint();
                })
                .map(|()| Some(Lines::new(filter, receive, writer.clone(), options.format)))
            }
        }
        .map_err(|error| format!("could not start the program: {error}"))?;
        Ok(Self {
            context: ctx,
            shared: nfm_egui::Output::new(
                ("nterm-output", id),
                nfm_egui::OutputConfig { rows: options.rows },
                writer,
            ),
            position: options.position,
            scope: options.scope,
            anchor_bounds: None,
            cursor_row: None,
            from_copy,
            typography: font::Typography::default(),
            lines,
        })
    }
    pub(crate) fn label(&self) -> &'static str {
        "OUTPUT"
    }
    pub(crate) fn surrender_focus(&self) {
        self.context.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        self.context.request_repaint();
    }
    /// Draw the page and report Escape. It stays as it is; the pane says what
    /// comes next.
    pub(crate) fn show_scoped(
        &mut self,
        ctx: &egui::Context,
        pane: egui::Rect,
        workspace: Option<egui::Rect>,
        interactive: bool,
    ) -> Option<Action> {
        self.anchor_bounds = Some(pane);
        let bounds = if self.scope == config::Scope::Workspace {
            workspace.unwrap_or(pane)
        } else {
            pane
        };
        if let Some(lines) = &mut self.lines {
            if lines.take() {
                ctx.request_repaint();
            }
        }
        let appearance = super::appearance(&self.typography, ctx);
        let placement = super::placement(
            self.position,
            self.cursor_row,
            self.anchor_bounds,
            self.typography.row_height,
            bounds,
        );
        match self.shared.show(ctx, placement, &appearance, !interactive) {
            Some(nfm_egui::OutputEvent::Cancelled) => Some(Action::Cancel),
            None => None,
        }
    }
}

/// The most lines taken while drawing one frame, so a program that prints
/// fast does not hold the terminal up.
const LINES_PER_FRAME: usize = 256;
/// The most text kept. What comes after is not shown.
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// The lines of a program whose definition says what to show for each, and
/// the text made of them so far.
struct Lines {
    filter: std::rc::Rc<crate::lua_callbacks::LineFilter>,
    receive: std::sync::mpsc::Receiver<nfm_output::Report>,
    writer: nfm_egui::OutputWriter,
    text: String,
    /// What the page itself says after the text, such as how the program
    /// ended.
    notes: Vec<String>,
    format: nfm_output::Format,
    /// The program finished, or the function failed: nothing more is taken.
    done: bool,
}
impl Lines {
    fn new(
        filter: std::rc::Rc<crate::lua_callbacks::LineFilter>,
        receive: std::sync::mpsc::Receiver<nfm_output::Report>,
        writer: nfm_egui::OutputWriter,
        format: nfm_output::Format,
    ) -> Self {
        Self {
            filter,
            receive,
            writer,
            text: String::new(),
            notes: Vec::new(),
            format,
            done: false,
        }
    }
    /// Take the lines that arrived, ask the function what to show for each,
    /// and publish the text if it grew. True if more are waiting.
    fn take(&mut self) -> bool {
        if self.done {
            return false;
        }
        let before = (self.text.len(), self.notes.len());
        let mut taken = 0;
        while taken < LINES_PER_FRAME && !self.done {
            let Ok(report) = self.receive.try_recv() else {
                break;
            };
            taken += 1;
            match report {
                nfm_output::Report::Line {
                    stream: nfm_output::Stream::Stdout,
                    text,
                } => match self.filter.text(&text) {
                    Ok(Some(text)) => self.text.push_str(&text),
                    Ok(None) => {}
                    Err(error) => {
                        // The first line says what; the rest is where in Lua.
                        let error = error.lines().next().unwrap_or_default();
                        self.note(&format!("line function failed: {error}"));
                        self.done = true;
                    }
                },
                // What it says of its own trouble is shown as it is.
                nfm_output::Report::Line { text, .. } => {
                    self.start_line();
                    // In markdown a line joins the paragraph before it
                    // unless a blank line parts them.
                    let markdown = self.format == nfm_output::Format::Markdown;
                    if markdown && !self.text.is_empty() {
                        self.text.push('\n');
                    }
                    self.text.push_str(&text);
                    self.text.push('\n');
                    if markdown {
                        self.text.push('\n');
                    }
                }
                nfm_output::Report::Note(note) => self.note(&note),
                nfm_output::Report::Finished => self.done = true,
            }
            if self.text.len() > MAX_TEXT_BYTES {
                let mut end = MAX_TEXT_BYTES;
                while !self.text.is_char_boundary(end) {
                    end -= 1;
                }
                self.text.truncate(end);
                self.note("Stopped: too much output");
                self.done = true;
            }
        }
        if (self.text.len(), self.notes.len()) != before {
            self.writer.publish(nfm_output::document(
                self.format,
                self.text.as_bytes(),
                &self.notes,
            ));
        }
        if self.done {
            self.writer.finish();
            // Nothing more is taken, so the program is stopped if it is
            // still running: the function failed, or it printed too much.
            self.writer.close();
        }
        taken == LINES_PER_FRAME && !self.done
    }
    fn start_line(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }
    /// Add a line of the page's own, shown dimmed after the text.
    fn note(&mut self, note: &str) {
        self.notes.push(note.into());
    }
}

/// The name in an argument that is `{name}`: letters, digits and `_` between
/// the braces. Anything else in braces is an argument as written, such as a
/// JSON object.
fn parameter_name(arg: &str) -> Option<&str> {
    let name = arg.strip_prefix('{')?.strip_suffix('}')?;
    let named = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    named.then_some(name)
}

/// The program to run: `args` with each argument that is `{name}` replaced by
/// the parameter of that name, in `cwd`, or the `cwd` parameter if there is
/// one.
fn command(options: &config::OutputOptions) -> Result<nfm_output::CommandSpec, String> {
    let mut args = Vec::with_capacity(options.args.len());
    for arg in &options.args {
        let resolved = match parameter_name(arg) {
            Some(name) => options
                .params
                .get(name)
                .ok_or_else(|| format!("missing output parameter {name:?}"))?
                .clone(),
            None => arg.clone(),
        };
        if resolved.contains('\0') {
            return Err("output argument contains NUL".into());
        }
        args.push(resolved);
    }
    if args.first().is_none_or(String::is_empty) {
        return Err("output requires an executable".into());
    }
    let mut spec = nfm_output::CommandSpec::new(args.remove(0));
    spec.arguments = args;
    let cwd = options.params.get("cwd").or(options.cwd.as_ref());
    if cwd.is_some_and(|cwd| cwd.contains('\0')) {
        return Err("output cwd contains NUL".into());
    }
    spec.working_directory = cwd.map(std::path::PathBuf::from);
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[&str], params: &[(&str, &str)]) -> config::OutputOptions {
        config::OutputOptions {
            scope: Default::default(),
            position: Default::default(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: Some("C:\\defined".into()),
            rows: 24,
            format: nfm_output::Format::Text,
            line: None,
            params: params
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        }
    }

    fn filter(source: &str) -> std::rc::Rc<crate::lua_callbacks::LineFilter> {
        let lua = std::rc::Rc::new(mlua::Lua::new());
        let function = lua.load(source).eval::<mlua::Function>().unwrap();
        std::rc::Rc::new(crate::lua_callbacks::LineFilter {
            key: lua.create_registry_value(function).unwrap(),
            lua,
        })
    }
    fn shown(writer: &nfm_egui::OutputWriter) -> String {
        use nfm_egui::copy_document::CopyDocument;
        let document = writer.document().expect("something was published");
        let layout = document
            .layout(200, &nfm_egui::Cancellation::never())
            .unwrap();
        (0..layout.total_rows())
            .map(|row| layout.text_row(row as u64).unwrap().unwrap().text.text)
            .map(|row| row.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    }
    fn line(stream: nfm_output::Stream, text: &str) -> nfm_output::Report {
        nfm_output::Report::Line {
            stream,
            text: text.into(),
        }
    }

    #[test]
    fn the_line_function_says_what_is_shown_for_each_line_printed() {
        use nfm_output::{Report, Stream};
        let (send, receive) = std::sync::mpsc::channel();
        let writer = nfm_egui::OutputWriter::new();
        let mut lines = Lines::new(
            filter(
                "return function(line) if line:sub(1, 1) == '#' then return nil end return line:upper() end",
            ),
            receive,
            writer.clone(),
            nfm_output::Format::Text,
        );
        // Nothing arrived: nothing is published.
        assert!(!lines.take());
        assert!(writer.document().is_none());

        // What it returns is added as it is, with no line ending of its own;
        // nil adds nothing.
        send.send(line(Stream::Stdout, "hel")).unwrap();
        send.send(line(Stream::Stdout, "# skipped")).unwrap();
        send.send(line(Stream::Stdout, "lo")).unwrap();
        assert!(!lines.take());
        assert_eq!(shown(&writer), "HELLO");
        assert!(!writer.is_finished());

        // What the program says of its own trouble, and how it ended, are
        // shown as they are on lines of their own.
        send.send(line(Stream::Stderr, "not logged in")).unwrap();
        send.send(Report::Note("Exited with code 1".into()))
            .unwrap();
        send.send(Report::Finished).unwrap();
        assert!(!lines.take());
        assert_eq!(shown(&writer), "HELLO\nnot logged in\nExited with code 1");
        assert!(writer.is_finished());
    }

    #[test]
    fn a_line_function_that_fails_says_so_and_takes_no_more() {
        use nfm_output::Stream;
        let (send, receive) = std::sync::mpsc::channel();
        let writer = nfm_egui::OutputWriter::new();
        let mut lines = Lines::new(
            filter("return function(line) if line == 'bad' then error('no') end return line end"),
            receive,
            writer.clone(),
            nfm_output::Format::Text,
        );
        send.send(line(Stream::Stdout, "good")).unwrap();
        send.send(line(Stream::Stdout, "bad")).unwrap();
        send.send(line(Stream::Stdout, "after")).unwrap();
        lines.take();
        let text = shown(&writer);
        assert!(text.starts_with("good\nline function failed:"), "{text:?}");
        assert!(!text.contains("after"), "{text:?}");
        assert!(writer.is_finished());
        // The program is stopped, and what was shown stays.
        assert!(writer.is_closed());

        // Something that is not text is a failure too.
        let (send, receive) = std::sync::mpsc::channel();
        let writer = nfm_egui::OutputWriter::new();
        let mut lines = Lines::new(
            filter("return function() return 5 end"),
            receive,
            writer.clone(),
            nfm_output::Format::Text,
        );
        send.send(line(Stream::Stdout, "x")).unwrap();
        lines.take();
        assert!(shown(&writer).contains("must return a string or nil"));
    }

    #[test]
    fn the_text_made_of_the_lines_is_laid_out_as_markdown_when_asked() {
        use nfm_output::{Report, Stream};
        let (send, receive) = std::sync::mpsc::channel();
        let writer = nfm_egui::OutputWriter::new();
        let mut lines = Lines::new(
            filter("return function(line) return line end"),
            receive,
            writer.clone(),
            nfm_output::Format::Markdown,
        );
        // A piece may end anywhere, as an answer does while it arrives.
        send.send(line(Stream::Stdout, "# Borrow")).unwrap();
        send.send(line(Stream::Stdout, "ing\n\n- one **bold**\n"))
            .unwrap();
        // What it says of its own trouble is a paragraph of its own.
        send.send(line(Stream::Stdout, "\nThat is all.\n")).unwrap();
        send.send(line(Stream::Stderr, "a warning")).unwrap();
        send.send(Report::Note("Exited with code 1".into()))
            .unwrap();
        send.send(Report::Finished).unwrap();
        lines.take();
        let text = shown(&writer);
        let rows: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(
            rows,
            [
                "Borrowing",
                "",
                "\u{2022} one bold",
                "",
                "That is all.",
                "",
                "a warning",
                "",
                "Exited with code 1"
            ]
        );
    }

    #[test]
    fn a_fast_program_is_taken_a_part_at_a_time() {
        use nfm_output::Stream;
        let (send, receive) = std::sync::mpsc::channel();
        let writer = nfm_egui::OutputWriter::new();
        let mut lines = Lines::new(
            filter("return function(line) return line end"),
            receive,
            writer,
            nfm_output::Format::Text,
        );
        for _ in 0..LINES_PER_FRAME + 1 {
            send.send(line(Stream::Stdout, "x")).unwrap();
        }
        // More are waiting: the page asks to be drawn again.
        assert!(lines.take());
        assert!(!lines.take());
        assert_eq!(lines.text.len(), LINES_PER_FRAME + 1);
    }

    #[test]
    fn a_parameter_takes_the_place_of_the_argument_that_names_it() {
        let spec = command(&options(
            &["ping", "-n", "{count}", "{host}"],
            &[("count", "4"), ("host", "a b")],
        ))
        .unwrap();
        assert_eq!(spec.program, std::path::PathBuf::from("ping"));
        assert_eq!(spec.arguments, ["-n", "4", "a b"]);
        assert_eq!(
            spec.working_directory,
            Some(std::path::PathBuf::from("C:\\defined"))
        );

        // The request's directory comes before the definition's.
        let spec = command(&options(&["ping"], &[("cwd", "C:\\asked")])).unwrap();
        assert_eq!(
            spec.working_directory,
            Some(std::path::PathBuf::from("C:\\asked"))
        );

        let error = command(&options(&["ping", "{host}"], &[])).unwrap_err();
        assert!(error.contains("host"), "{error}");

        // Braces around something that is not a name are an argument as
        // written: a JSON object, or nothing at all.
        let spec = command(&options(
            &["claude", "--settings", r#"{"disableAllHooks":true}"#, "{}"],
            &[],
        ))
        .unwrap();
        assert_eq!(
            spec.arguments,
            ["--settings", r#"{"disableAllHooks":true}"#, "{}"]
        );
    }
}

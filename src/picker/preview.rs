use crate::ghostty_ffi::{self, Host, SnapshotCellRange, SnapshotStyledRows};
use crate::picker::config::Preview;
use crate::picker::font::Typography;
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

const ROWS: usize = 12;
const MAX_BYTES: usize = 64 * 1024;
const INITIAL_PRESENTATION_WAIT: Duration = Duration::from_millis(100);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Cell {
    codepoints: [u32; 8],
    foreground: u32,
    background: u32,
    underline_color: u32,
    length: u8,
    flags: u8,
    underline: u8,
    reserved: u8,
}

unsafe extern "C" {
    fn ghostty_egui_preview_parse(
        bytes: *const u8,
        length: usize,
        columns: u16,
        rows: u16,
        scroll_offset: usize,
        default_foreground: u32,
        default_background: u32,
        cells: *mut Cell,
        out_total_rows: *mut usize,
    ) -> bool;
}

struct Update {
    revision: u64,
    cells: Result<Vec<Cell>, String>,
    columns: usize,
    first_row: u64,
    total_rows: usize,
    bytes: Option<Vec<u8>>,
    scroll_offset: usize,
}

enum PreviewJob {
    Text(String),
    Ansi(String),
    Scrollback,
    Command {
        args: Vec<String>,
        cwd: Option<String>,
        center_line: Option<usize>,
    },
    Empty,
    Error(String),
}

pub(crate) struct PickerPreview {
    source: Preview,
    revision: Arc<AtomicU64>,
    sender: mpsc::Sender<Update>,
    receiver: mpsc::Receiver<Update>,
    key: Option<String>,
    cells: Vec<Cell>,
    columns: usize,
    display_columns: usize,
    rows: usize,
    scroll_offset: usize,
    max_scroll_offset: usize,
    scroll_selection: Option<(u64, u32, u64, u32)>,
    cached_bytes: Option<Vec<u8>>,
    host: usize,
    generation: Option<u64>,
    total_rows: u64,
    snapshot_columns: usize,
    first_row: u64,
    match_range: Option<SnapshotCellRange>,
    requested_match_range: Option<SnapshotCellRange>,
    workers: Vec<std::thread::JoinHandle<()>>,
    error: Option<String>,
    pending: bool,
    presentation_started: Option<Instant>,
    presented: bool,
}

fn substitute(input: &str, item: &str) -> String {
    input.replace("{item}", item)
}

fn scrollback_first_row(total: u64, rows: u64, selected: u64) -> u64 {
    selected
        .saturating_sub(rows / 2)
        .min(total.saturating_sub(rows))
}

fn match_contains(range: SnapshotCellRange, row: u64, x: u32) -> bool {
    row >= range.start.y
        && row <= range.end.y
        && (row > range.start.y || x >= range.start.x)
        && (row < range.end.y || x <= range.end.x)
}

fn styled_rows(host: usize, generation: u64, first: u64, rows: u32) -> Result<Vec<u8>, String> {
    let mut result = SnapshotStyledRows::default();
    if !unsafe {
        ghostty_ffi::ghostty_egui_host_snapshot_styled_rows(
            host as *mut Host,
            generation,
            first,
            rows,
            &mut result,
        )
    } {
        return Err("Unable to read scrollback preview".into());
    }
    let bytes = if result.len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(result.text, result.len) }.to_vec()
    };
    unsafe { ghostty_ffi::ghostty_egui_host_snapshot_styled_rows_free(&mut result) };
    Ok(bytes)
}

fn command_output(
    args: &[String],
    cwd: Option<&str>,
    revision: &AtomicU64,
    id: u64,
) -> Result<Vec<u8>, String> {
    use super::process;
    if args.is_empty() {
        return Err("preview command requires an executable".into());
    }
    let context = process::describe(args, cwd);
    let mut command = std::process::Command::new(&args[0]);
    command
        .args(&args[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = process::ChildGuard(
        command
            .spawn()
            .map_err(|error| format!("{context}\nUnable to start preview: {error}"))?,
    );
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("preview command has no stdout")?;
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or("preview command has no stderr")?;
    let reader = process::reader("preview stdout reader", move || {
        process::read_bounded(stdout, MAX_BYTES)
            .map_err(|error| format!("Unable to read preview stdout: {error}"))
    })?;
    let stderr_reader = process::reader("preview stderr reader", move || {
        process::read_bounded(stderr, process::STDERR_LIMIT)
            .map_err(|error| format!("Unable to read preview stderr: {error}"))
    })?;
    let status = loop {
        if revision.load(Ordering::Acquire) != id {
            let _ = child.0.kill();
            let _ = child.0.wait();
            let _ = process::join(reader, "preview stdout reader");
            let _ = process::join(stderr_reader, "preview stderr reader");
            return Err("preview cancelled".into());
        }
        match child.0.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(format!("{context}\nUnable to wait for preview: {error}")),
        }
    };
    let stdout_result = process::join(reader, "preview stdout reader");
    let stderr_result = process::join(stderr_reader, "preview stderr reader");
    if revision.load(Ordering::Acquire) != id {
        return Err("preview cancelled".into());
    }
    let bytes = stdout_result.map_err(|error| format!("{context}\n{error}"))?;
    let stderr = stderr_result.map_err(|error| format!("{context}\n{error}"))?;
    if !status.success() && !(process::no_match_exit(&args[0]) && status.code() == Some(1)) {
        return Err(format!(
            "{context}\n{}",
            process::status_error(status, &stderr)
        ));
    }
    Ok(bytes)
}
fn parse(
    bytes: &[u8],
    columns: usize,
    rows: usize,
    scroll_offset: usize,
) -> Result<(Vec<Cell>, usize), String> {
    let mut normalized = Vec::with_capacity(bytes.len());
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r') {
            normalized.push(b'\r');
        }
        normalized.push(byte);
    }
    let mut cells = vec![Cell::default(); columns * rows];
    let mut total_rows = 0;
    if unsafe {
        ghostty_egui_preview_parse(
            normalized.as_ptr(),
            normalized.len(),
            columns as u16,
            rows as u16,
            scroll_offset,
            crate::colors::rgb(crate::colors::palette().text),
            crate::colors::rgb(crate::colors::palette().background),
            cells.as_mut_ptr(),
            &mut total_rows,
        )
    } {
        Ok((cells, total_rows))
    } else {
        Err("Unable to parse preview".into())
    }
}

impl PickerPreview {
    pub(crate) fn new(
        source: Preview,
        host: *mut Host,
        generation: Option<u64>,
        total_rows: u64,
        snapshot_columns: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            source,
            revision: Arc::new(AtomicU64::new(0)),
            sender,
            receiver,
            key: None,
            cells: Vec::new(),
            columns: 0,
            display_columns: 0,
            rows: 0,
            scroll_offset: 0,
            max_scroll_offset: 0,
            scroll_selection: None,
            cached_bytes: None,
            host: host as usize,
            generation,
            total_rows,
            snapshot_columns,
            first_row: 0,
            match_range: None,
            requested_match_range: None,
            workers: Vec::new(),
            error: None,
            pending: false,
            presentation_started: None,
            presented: false,
        }
    }

    pub(crate) fn row_limit(&self) -> usize {
        match self.source {
            Preview::Scrollback { rows } => rows,
            _ => ROWS,
        }
    }

    pub(crate) fn select(
        &mut self,
        item: Option<&str>,
        range: Option<SnapshotCellRange>,
        columns: usize,
        rows: usize,
        ctx: &egui::Context,
        filesystem: bool,
        params: &BTreeMap<String, String>,
    ) {
        let rows = rows.clamp(1, ROWS);
        let display_columns = columns.clamp(1, 240);
        let scrollback = matches!(self.source, Preview::Scrollback { .. });
        let columns = if scrollback {
            self.snapshot_columns.max(1).min(u16::MAX as usize)
        } else {
            display_columns
        };
        if scrollback {
            let selection =
                range.map(|range| (range.start.y, range.start.x, range.end.y, range.end.x));
            if self.scroll_selection != selection {
                self.scroll_offset = range.map_or(0, |range| {
                    scrollback_first_row(self.total_rows, rows as u64, range.start.y) as usize
                });
                self.scroll_selection = selection;
            }
            self.max_scroll_offset = self.total_rows.saturating_sub(rows as u64) as usize;
            self.scroll_offset = self.scroll_offset.min(self.max_scroll_offset);
        } else if self.key.as_deref() != item {
            self.scroll_offset = 0;
            self.max_scroll_offset = 0;
            self.cached_bytes = None;
        }
        let first_row = if scrollback {
            self.scroll_offset as u64
        } else {
            0
        };
        let key = item.map(|item| {
            if scrollback {
                format!("{first_row}:{rows}")
            } else {
                item.to_owned()
            }
        });
        self.requested_match_range = if scrollback { range } else { None };
        if self.key == key && self.columns == columns && self.rows == rows {
            self.display_columns = display_columns;
            if scrollback && !self.cells.is_empty() && self.first_row == first_row {
                self.match_range = range;
            }
            return;
        }
        self.key = key.clone();
        self.columns = columns;
        self.display_columns = display_columns;
        self.rows = rows;
        self.error = None;
        let id = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.pending = key.is_some();
        let Some(item) = key else {
            self.cells.clear();
            self.match_range = None;
            self.cached_bytes = None;
            return;
        };
        if !scrollback {
            if let Some(bytes) = &self.cached_bytes {
                self.pending = false;
                match parse(bytes, columns, rows, self.scroll_offset) {
                    Ok((cells, total_rows)) => {
                        self.cells = cells;
                        self.max_scroll_offset = total_rows.saturating_sub(rows);
                        self.scroll_offset = self.scroll_offset.min(self.max_scroll_offset);
                    }
                    Err(error) => {
                        self.cells.clear();
                        if error != "preview cancelled" && error != "No scrollback match selected" {
                            crate::error_log::error("picker preview", "Preview failed", &error);
                        }
                        self.error = Some(error);
                    }
                }
                return;
            }
        }
        let job = match &self.source {
            Preview::Text { text } => PreviewJob::Text(substitute(text, &item)),
            Preview::Ansi { text } => PreviewJob::Ansi(substitute(text, &item)),
            Preview::Scrollback { .. } => PreviewJob::Scrollback,
            Preview::Command { args, cwd, command } => {
                if let Some(command) = command {
                    match command.call(&item, filesystem, params) {
                        Ok(Some(spec)) => PreviewJob::Command {
                            args: spec.args,
                            cwd: spec.cwd,
                            center_line: spec.center_line,
                        },
                        Ok(None) => PreviewJob::Empty,
                        Err(error) => PreviewJob::Error(error),
                    }
                } else {
                    PreviewJob::Command {
                        args: args.iter().map(|arg| substitute(arg, &item)).collect(),
                        cwd: cwd.as_ref().map(|cwd| substitute(cwd, &item)),
                        center_line: None,
                    }
                }
            }
        };
        let sender = self.sender.clone();
        let revision = self.revision.clone();
        let ctx = ctx.clone();
        let host = self.host;
        let generation = self.generation;
        let total_rows = self.total_rows;
        let scroll_offset = self.scroll_offset;
        self.workers.retain(|worker| !worker.is_finished());
        let failure_sender = sender.clone();
        let failure_revision = revision.clone();
        let failure_ctx = ctx.clone();
        if let Some(worker) = crate::worker::spawn(
            "picker preview worker",
            move || {
                let center_line = match &job {
                    PreviewJob::Command { center_line, .. } => *center_line,
                    _ => None,
                };
                let bytes = match job {
                    PreviewJob::Text(plain) => {
                        // Escape control bytes so text previews cannot alter the VT state.
                        Ok(plain
                            .chars()
                            .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
                            .collect::<String>()
                            .into_bytes())
                    }
                    PreviewJob::Ansi(text) => Ok(text.into_bytes()),
                    PreviewJob::Scrollback => match generation {
                        Some(generation) if range.is_some() && rows > 0 => styled_rows(
                            host,
                            generation,
                            first_row,
                            (rows as u64).min(total_rows) as u32,
                        ),
                        _ => Err("No scrollback match selected".into()),
                    },
                    PreviewJob::Command { args, cwd, .. } => {
                        command_output(&args, cwd.as_deref(), &revision, id)
                    }
                    PreviewJob::Empty => Ok(Vec::new()),
                    PreviewJob::Error(error) => Err(error),
                };
                if revision.load(Ordering::Acquire) != id {
                    return;
                }
                let (cells, total_preview_rows, cached_bytes, scroll_offset) = match bytes {
                    Ok(bytes) => {
                        let bytes = bytes[..bytes.len().min(MAX_BYTES)].to_vec();
                        let parsed = parse(
                            &bytes,
                            columns,
                            rows,
                            if scrollback { 0 } else { scroll_offset },
                        );
                        let parsed = parsed.and_then(|(cells, total_rows)| {
                            let offset = center_line.map_or(scroll_offset, |line| {
                                line.saturating_sub(rows / 2)
                                    .min(total_rows.saturating_sub(rows))
                            });
                            if !scrollback && offset != scroll_offset {
                                parse(&bytes, columns, rows, offset)
                                    .map(|(cells, total_rows)| (cells, total_rows, offset))
                            } else {
                                Ok((cells, total_rows, offset))
                            }
                        });
                        match parsed {
                            Ok((cells, total_rows, offset)) => (
                                Ok(cells),
                                total_rows,
                                if scrollback { None } else { Some(bytes) },
                                offset,
                            ),
                            Err(error) => (Err(error), 0, None, scroll_offset),
                        }
                    }
                    Err(error) => (Err(error), 0, None, scroll_offset),
                };
                if revision.load(Ordering::Acquire) == id {
                    let _ = sender.send(Update {
                        revision: id,
                        cells,
                        columns,
                        first_row,
                        total_rows: total_preview_rows,
                        bytes: cached_bytes,
                        scroll_offset,
                    });
                    ctx.request_repaint();
                }
            },
            move |error| {
                if failure_revision.load(Ordering::Acquire) == id {
                    let _ = failure_sender.send(Update {
                        revision: id,
                        cells: Err(error),
                        columns,
                        first_row,
                        total_rows: 0,
                        bytes: None,
                        scroll_offset,
                    });
                    failure_ctx.request_repaint();
                }
            },
        ) {
            self.workers.push(worker);
        }
    }

    fn apply_updates(&mut self) {
        while let Ok(update) = self.receiver.try_recv() {
            if update.revision != self.revision.load(Ordering::Acquire)
                || update.columns != self.columns
            {
                continue;
            }
            self.pending = false;
            self.cached_bytes = update.bytes;
            match update.cells {
                Ok(cells) => {
                    self.cells = cells;
                    self.first_row = update.first_row;
                    self.match_range = self.requested_match_range;
                    if !matches!(self.source, Preview::Scrollback { .. }) {
                        self.max_scroll_offset = update.total_rows.saturating_sub(self.rows);
                        self.scroll_offset = update.scroll_offset.min(self.max_scroll_offset);
                    }
                }
                Err(error) => {
                    self.cells.clear();
                    if error != "preview cancelled" && error != "No scrollback match selected" {
                        crate::error_log::error("picker preview", "Preview failed", &error);
                    }
                    self.error = Some(error);
                }
            }
        }
    }

    pub(crate) fn scroll_half_page(&mut self, direction: isize, ctx: &egui::Context) {
        self.apply_updates();
        if self.key.is_none() || self.rows == 0 {
            return;
        }
        let amount = (self.rows / 2).max(1);
        let next = if direction < 0 {
            self.scroll_offset.saturating_sub(amount)
        } else {
            self.scroll_offset
                .saturating_add(amount)
                .min(self.max_scroll_offset)
        };
        if next == self.scroll_offset {
            return;
        }
        self.scroll_offset = next;
        if matches!(self.source, Preview::Scrollback { .. }) {
            self.key = None;
        } else if let Some(bytes) = &self.cached_bytes {
            match parse(bytes, self.columns, self.rows, next) {
                Ok((cells, _)) => self.cells = cells,
                Err(error) => {
                    self.cells.clear();
                    if error != "preview cancelled" && error != "No scrollback match selected" {
                        crate::error_log::error("picker preview", "Preview failed", &error);
                    }
                    self.error = Some(error);
                }
            }
        }
        ctx.request_repaint();
    }

    pub(crate) fn pending(&self) -> bool {
        self.pending
    }

    pub(crate) fn ready_to_present(&mut self, ctx: &egui::Context) -> bool {
        self.apply_updates();
        let started = *self.presentation_started.get_or_insert_with(Instant::now);
        self.presented |= !self.cells.is_empty()
            || self.error.is_some()
            || started.elapsed() >= INITIAL_PRESENTATION_WAIT;
        if !self.presented {
            // Keep sizing and handling input while waiting for the first frame.
            // A slow command must never keep the picker hidden indefinitely.
            ctx.request_repaint_after(INITIAL_PRESENTATION_WAIT.saturating_sub(started.elapsed()));
        }
        self.presented
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui, typography: &Typography, rows: usize) {
        let width = ui.available_width();
        let cell_width = width / self.display_columns.max(1) as f32;
        let row_height = typography.row_height;
        for y in 0..rows.min(ROWS) {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover());
            let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
            if self.cells.is_empty() {
                if y == 0 {
                    let label = self.error.as_deref().unwrap_or("");
                    painter.text(
                        rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        label,
                        typography.font.clone(),
                        crate::colors::palette().error,
                    );
                }
                continue;
            }
            for x in 0..self.display_columns.min(self.columns) {
                let cell = &self.cells[y * self.columns + x];
                let fg = color(cell.foreground);
                let bg = color(cell.background);
                let (fg, bg) = if cell.flags & 8 != 0 {
                    (bg, fg)
                } else {
                    (fg, bg)
                };
                let cell_rect = egui::Rect::from_min_size(
                    rect.min + egui::vec2(x as f32 * cell_width, 0.0),
                    egui::vec2(cell_width, row_height),
                );
                let highlighted = self.match_range.is_some_and(|range| {
                    match_contains(range, self.first_row + y as u64, x as u32)
                });
                if highlighted {
                    painter.rect_filled(
                        cell_rect,
                        0.0,
                        crate::colors::palette().selection_background,
                    );
                } else if cell.background != 0x282828 || cell.flags & 8 != 0 {
                    painter.rect_filled(cell_rect, 0.0, bg);
                }
                if cell.length > 0 && cell.flags & 16 == 0 {
                    let text = cell.codepoints[..cell.length as usize]
                        .iter()
                        .filter_map(|codepoint| char::from_u32(*codepoint))
                        .collect::<String>();
                    painter.text(
                        cell_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        text,
                        if cell.flags & 1 != 0 {
                            typography.matched_font.clone()
                        } else {
                            typography.font.clone()
                        },
                        if cell.flags & 32 != 0 {
                            fg.gamma_multiply(0.6)
                        } else {
                            fg
                        },
                    );
                }
                if cell.underline != 0 {
                    painter.line_segment(
                        [
                            cell_rect.left_bottom() - egui::vec2(0.0, 2.0),
                            cell_rect.right_bottom() - egui::vec2(0.0, 2.0),
                        ],
                        egui::Stroke::new(1.0, color(cell.underline_color)),
                    );
                }
                if cell.flags & 4 != 0 {
                    painter.line_segment(
                        [cell_rect.left_center(), cell_rect.right_center()],
                        egui::Stroke::new(1.0, fg),
                    );
                }
            }
        }
    }
}

impl Drop for PickerPreview {
    fn drop(&mut self) {
        self.revision.fetch_add(1, Ordering::Release);
        if matches!(self.source, Preview::Scrollback { .. }) {
            // The snapshot must outlive all readers. Command previews only
            // hold their own process and buffers and can stop asynchronously.
            for worker in self.workers.drain(..) {
                let _ = worker.join();
            }
        }
    }
}

fn color(rgb: u32) -> egui::Color32 {
    egui::Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(windows)]
    fn command_failure_retains_exit_status_and_stderr() {
        let args = vec![
            "cmd.exe".into(),
            "/C".into(),
            "echo preview diagnostic 1>&2 & exit /b 2".into(),
        ];
        let error = super::command_output(&args, None, &std::sync::atomic::AtomicU64::new(1), 1)
            .unwrap_err();
        assert!(error.contains("preview diagnostic"));
        assert!(error.contains("cmd.exe"));
        assert!(error.contains("exited"));
    }
    use super::*;

    #[test]
    fn first_preview_waits_for_content_with_a_bounded_delay() {
        let ctx = egui::Context::default();
        let make_preview = || {
            PickerPreview::new(
                Preview::Text {
                    text: "hello".into(),
                },
                std::ptr::null_mut(),
                None,
                0,
                80,
            )
        };
        let mut preview = make_preview();
        assert!(!preview.ready_to_present(&ctx));
        preview
            .sender
            .send(Update {
                revision: 0,
                cells: Ok(vec![Cell::default()]),
                columns: 0,
                first_row: 0,
                total_rows: 1,
                bytes: None,
                scroll_offset: 0,
            })
            .unwrap();
        assert!(preview.ready_to_present(&ctx));
        preview.cells.clear();
        assert!(preview.ready_to_present(&ctx));

        let mut slow = make_preview();
        slow.presentation_started = Some(Instant::now() - INITIAL_PRESENTATION_WAIT);
        assert!(slow.ready_to_present(&ctx));
    }

    #[test]
    fn ghostty_vt_preview_parses_text_color_and_newlines() {
        let (cells, _) = parse(b"\x1b[1;31mA\x1b[0m\nB", 8, ROWS, 0).unwrap();
        assert_eq!(cells[0].codepoints[0], 'A' as u32);
        assert_ne!(cells[0].foreground, cells[1].foreground);
        assert_ne!(cells[0].flags & 1, 0);
        assert_eq!(cells[8].codepoints[0], 'B' as u32);

        let (styled, _) = parse("\x1b[48;2;18;52;86;4m猫".as_bytes(), 8, ROWS, 0).unwrap();
        assert_eq!(styled[0].codepoints[0], '猫' as u32);
        assert_eq!(styled[0].background, 0x123456);
        assert_ne!(styled[0].underline, 0);
    }

    #[test]
    fn preview_parser_renders_requested_page_from_history() {
        let bytes = b"A\nB\nC\nD\nE\nF";
        let (first, total) = parse(bytes, 8, 4, 0).unwrap();
        assert!(total >= 6);
        assert_eq!(first[0].codepoints[0], 'A' as u32);
        let (second, _) = parse(bytes, 8, 4, 2).unwrap();
        assert_eq!(second[0].codepoints[0], 'C' as u32);
    }

    #[test]
    fn paging_reuses_preview_output_and_stops_at_last_page() {
        let ctx = egui::Context::default();
        let mut preview = PickerPreview::new(
            Preview::Text {
                text: "A\nB\nC\nD\nE\nF".into(),
            },
            std::ptr::null_mut(),
            None,
            0,
            80,
        );
        preview.select(
            Some("selected"),
            None,
            8,
            4,
            &ctx,
            false,
            &Default::default(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while preview.pending() {
            preview.apply_updates();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(preview.cells[0].codepoints[0], 'A' as u32);
        preview.scroll_half_page(1, &ctx);
        assert_eq!(preview.scroll_offset, 2);
        assert_eq!(preview.cells[0].codepoints[0], 'C' as u32);
        preview.scroll_half_page(1, &ctx);
        assert_eq!(preview.scroll_offset, 2);
        preview.scroll_half_page(-1, &ctx);
        assert_eq!(preview.cells[0].codepoints[0], 'A' as u32);
    }

    #[test]
    fn scrollback_preview_pages_from_selected_match_and_clamps() {
        let ctx = egui::Context::default();
        let mut preview = PickerPreview::new(
            Preview::Scrollback { rows: 8 },
            std::ptr::null_mut(),
            None,
            100,
            80,
        );
        let range = SnapshotCellRange {
            start: ghostty_ffi::SnapshotCell { x: 0, y: 50 },
            end: ghostty_ffi::SnapshotCell { x: 1, y: 50 },
        };
        preview.select(
            Some("selected"),
            Some(range),
            80,
            8,
            &ctx,
            false,
            &Default::default(),
        );
        assert_eq!(preview.scroll_offset, 46);
        preview.scroll_half_page(1, &ctx);
        assert_eq!(preview.scroll_offset, 50);
        preview.select(
            Some("selected"),
            Some(range),
            80,
            8,
            &ctx,
            false,
            &Default::default(),
        );
        assert_eq!(preview.key.as_deref(), Some("50:8"));
    }

    #[test]
    fn item_template_only_replaces_explicit_placeholder() {
        assert_eq!(substitute("File: {item}", "a.txt"), "File: a.txt");
    }

    #[test]
    fn scrollback_preview_centers_and_highlights_multiline_match() {
        assert_eq!(scrollback_first_row(100, 12, 50), 44);
        assert_eq!(scrollback_first_row(100, 12, 2), 0);
        assert_eq!(scrollback_first_row(100, 12, 98), 88);
        let range = SnapshotCellRange {
            start: ghostty_ffi::SnapshotCell { x: 5, y: 50 },
            end: ghostty_ffi::SnapshotCell { x: 2, y: 51 },
        };
        assert!(!match_contains(range, 50, 4));
        assert!(match_contains(range, 50, 5));
        assert!(match_contains(range, 51, 0));
        assert!(match_contains(range, 51, 2));
        assert!(!match_contains(range, 51, 3));
    }

    #[cfg(windows)]
    #[test]
    fn dynamic_command_center_line_positions_preview_and_preserves_paging() {
        let lua = std::rc::Rc::new(mlua::Lua::new());
        let function = lua.load(r#"return function(item)
            return {
                args={'cmd.exe', '/d', '/c', 'echo A&& echo B&& echo C&& echo D&& echo E&& echo F&& <nul set /p =G & exit /b 0'},
                center_line=tonumber(item.value),
            }
        end"#).eval::<mlua::Function>().unwrap();
        let key = lua.create_registry_value(function).unwrap();
        let command = std::rc::Rc::new(crate::picker::action::PreviewCommand {
            lua,
            key,
            name: "test".into(),
        });
        let ctx = egui::Context::default();
        let mut preview = PickerPreview::new(
            Preview::Command {
                args: Vec::new(),
                cwd: None,
                command: Some(command),
            },
            std::ptr::null_mut(),
            None,
            0,
            80,
        );
        for (line, offset, first) in [("4", 2, 'C'), ("0", 0, 'A'), ("6", 3, 'D'), ("999", 3, 'D')]
        {
            preview.select(Some(line), None, 8, 4, &ctx, false, &Default::default());
            for worker in preview.workers.drain(..) {
                worker.join().unwrap();
            }
            preview.apply_updates();
            assert!(preview.error.is_none(), "{:?}", preview.error);
            assert_eq!(preview.scroll_offset, offset);
            assert_eq!(preview.cells[0].codepoints[0], first as u32);
            preview.scroll_half_page(-1, &ctx);
            assert_eq!(preview.scroll_offset, offset.saturating_sub(2));
        }
    }

    #[cfg(windows)]
    #[test]
    fn command_preview_uses_selected_item_as_argument() {
        let mut preview = PickerPreview::new(
            Preview::Command {
                args: vec![
                    "cmd.exe".into(),
                    "/d".into(),
                    "/c".into(),
                    "echo {item}".into(),
                ],
                cwd: None,
                command: None,
            },
            std::ptr::null_mut(),
            None,
            0,
            80,
        );
        preview.select(
            Some("selected"),
            None,
            20,
            12,
            &egui::Context::default(),
            false,
            &Default::default(),
        );
        let update = preview
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let cells = update.cells.unwrap();
        assert_eq!(cells[0].codepoints[0], 's' as u32);
    }
}

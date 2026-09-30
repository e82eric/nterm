use crate::ghostty_ffi::{self, SnapshotCell, SnapshotCellRange, SnapshotTextChunk};
use regex::{Regex, RegexBuilder};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread;

#[derive(Default)]
pub(crate) struct SearchUi {
    pub(crate) active: bool,
    pub(crate) query: String,
    pub(crate) regex: bool,
    pub(crate) error: Option<String>,
    pub(crate) snapshot_generation: Option<u64>,
    pub(crate) cursor: usize,
    pub(crate) matches: Vec<SnapshotCellRange>,
    pub(crate) selected: Option<usize>,
    pub(crate) focus_requested: bool,
    pub(crate) query_revision: u64,
    pub(crate) waiting: bool,
    pub(crate) accept_requested: bool,
}

impl SearchUi {
    pub(crate) fn apply_result(&mut self, matches: Vec<SnapshotCellRange>, error: Option<String>) {
        self.matches = matches;
        self.selected = (!self.matches.is_empty()).then_some(0);
        self.error = error;
        self.waiting = false;
    }

    pub(crate) fn navigate(&mut self, previous: bool) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        self.selected = Some(match self.selected {
            Some(selected) if previous => (selected + count - 1) % count,
            Some(selected) => (selected + 1) % count,
            None if previous => count - 1,
            None => 0,
        });
    }

    pub(crate) fn selected_range(&self) -> Option<&SnapshotCellRange> {
        self.selected.and_then(|index| self.matches.get(index))
    }

    pub(crate) fn copy_target(&self) -> Option<SnapshotCell> {
        if !self.active || self.waiting || self.error.is_some() {
            return None;
        }
        self.selected_range().map(|range| range.start)
    }

    pub(crate) fn match_label(&self) -> String {
        match self.selected {
            Some(selected) => format!("{}/{}", selected + 1, self.matches.len()),
            None => format!("0/{}", self.matches.len()),
        }
    }
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        bounds: egui::Rect,
        pending: bool,
        interactive: bool,
    ) -> SearchUiAction {
        if !self.active {
            return SearchUiAction::default();
        }

        let mut query_changed = false;
        let mut mode_changed = false;
        let mut navigate = None;
        let mut close = false;
        let mut accept = false;
        let available_width = bounds.width();
        let search_width = (available_width - 24.0).max(1.0);
        // Gruvbox dark: lift the search box one shade above the terminal background.
        let fill = egui::Color32::from_rgb(0x3c, 0x38, 0x36); // dark1
        let border = egui::Color32::from_rgb(0x66, 0x5c, 0x54); // dark3
        let accent = egui::Color32::from_rgb(0xfa, 0xbd, 0x2f); // bright yellow
        let text_color = egui::Color32::from_rgb(0xeb, 0xdb, 0xb2); // light1
        let muted_text = egui::Color32::from_rgb(0xd5, 0xc4, 0xa1); // light2
        let selection = egui::Color32::from_rgb(0x66, 0x5c, 0x54); // dark3

        let id = if interactive {
            "terminal-search"
        } else {
            "terminal-search-background"
        };
        let order = if interactive {
            egui::Order::Foreground
        } else {
            egui::Order::Middle
        };
        egui::Area::new(id.into())
            .fixed_pos(bounds.left_bottom() - egui::vec2(0.0, 30.0))
            .order(order)
            .interactable(interactive)
            .fade_in(false)
            .movable(false)
            .show(ctx, |ui| {
                let frame = egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(12, 3));
                let frame_response = frame.show(ui, |ui| {
                    ui.set_width(search_width);
                    ui.visuals_mut().text_cursor.stroke = egui::Stroke::new(2.0, text_color);
                    ui.visuals_mut().selection.bg_fill = selection;
                    ui.visuals_mut().selection.stroke.color = text_color;
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add_sized(
                            [16.0, 24.0],
                            egui::Label::new(
                                egui::RichText::new("/")
                                    .font(egui::FontId::monospace(16.0))
                                    .color(accent),
                            ),
                        );

                        let match_label = if pending {
                            "…".to_owned()
                        } else if self.error.is_some() {
                            "INVALID REGEX".to_owned()
                        } else {
                            self.match_label()
                        };
                        let match_label = if available_width >= 600.0 {
                            let mode = if self.regex { "REGEX" } else { "LITERAL" };
                            format!("{match_label}   ·   {mode}")
                        } else {
                            match_label
                        };
                        let match_width = if available_width >= 600.0 {
                            210.0
                        } else {
                            74.0
                        };
                        let input_width = (ui.available_width() - match_width - 10.0).max(1.0);
                        let response = if interactive {
                            let response = ui.add_sized(
                                [input_width, 24.0],
                                egui::TextEdit::singleline(&mut self.query)
                                    .frame(egui::Frame::NONE)
                                    .font(egui::FontId::monospace(15.0))
                                    .vertical_align(egui::Align::Center)
                                    .text_color(text_color),
                            );
                            if self.focus_requested {
                                response.request_focus();
                                self.focus_requested = false;
                            }
                            query_changed = response.changed();
                            Some(response)
                        } else {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(input_width, 24.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().with_clip_rect(rect).text(
                                rect.left_center(),
                                egui::Align2::LEFT_CENTER,
                                &self.query,
                                egui::FontId::monospace(15.0),
                                text_color,
                            );
                            None
                        };

                        let (count_rect, count_response) = ui.allocate_exact_size(
                            egui::vec2(match_width, 24.0),
                            egui::Sense::hover(),
                        );
                        if let Some(error) = &self.error {
                            count_response.on_hover_text(error);
                        }
                        let match_color = if self.error.is_some() {
                            accent
                        } else {
                            muted_text
                        };
                        ui.painter().text(
                            count_rect.right_center(),
                            egui::Align2::RIGHT_CENTER,
                            match_label,
                            egui::FontId::monospace(14.0),
                            match_color,
                        );

                        if response
                            .as_ref()
                            .is_some_and(|response| response.has_focus() || response.lost_focus())
                        {
                            if ui.input(|input| {
                                input.modifiers.ctrl && input.key_pressed(egui::Key::R)
                            }) {
                                self.regex = !self.regex;
                                self.error = None;
                                mode_changed = true;
                            }
                            let search_key = ui.input(|input| {
                                if input.modifiers.ctrl && input.key_pressed(egui::Key::N) {
                                    Some(false)
                                } else if input.modifiers.ctrl && input.key_pressed(egui::Key::P) {
                                    Some(true)
                                } else {
                                    None
                                }
                            });
                            if search_key.is_some() {
                                navigate = search_key;
                            }
                            if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                                accept = true;
                            }
                            if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                                close = true;
                            }
                        }
                    });
                });
                ui.painter().line_segment(
                    [
                        frame_response.response.rect.left_top(),
                        frame_response.response.rect.right_top(),
                    ],
                    egui::Stroke::new(1.0, border),
                );
            });

        SearchUiAction {
            changed: query_changed || mode_changed,
            navigate,
            close,
            accept,
        }
    }
}

#[derive(Default)]
pub(crate) struct SearchUiAction {
    pub(crate) changed: bool,
    pub(crate) navigate: Option<bool>,
    pub(crate) close: bool,
    pub(crate) accept: bool,
}
pub(crate) struct SearchResult {
    pub(crate) snapshot_generation: u64,
    pub(crate) query_revision: u64,
    pub(crate) matches: Vec<SnapshotCellRange>,
    pub(crate) error: Option<String>,
}

struct SearchRequest {
    snapshot_generation: u64,
    query_revision: u64,
    query: String,
    regex: bool,
    cursor: usize,
}

enum SearchCommand {
    Search(SearchRequest),
    Barrier(mpsc::Sender<()>),
    Shutdown,
}

pub(crate) struct SearchWorker {
    commands: mpsc::Sender<SearchCommand>,
    results: mpsc::Receiver<SearchResult>,
    latest_query_revision: Arc<AtomicU64>,
    next_query_revision: u64,
    thread: Option<thread::JoinHandle<()>>,
}

impl SearchWorker {
    pub(crate) fn new(ctx: egui::Context) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let latest_query_revision = Arc::new(AtomicU64::new(0));
        let worker_query_revision = Arc::clone(&latest_query_revision);
        let thread = thread::Builder::new()
            .name("ghostty-egui-search".to_owned())
            .spawn(move || worker_loop(command_rx, result_tx, worker_query_revision, ctx))
            .expect("start search worker");
        Self {
            commands: command_tx,
            results: result_rx,
            latest_query_revision,
            next_query_revision: 0,
            thread: Some(thread),
        }
    }

    pub(crate) fn submit(
        &mut self,
        snapshot_generation: u64,
        query: String,
        regex: bool,
        cursor: usize,
    ) -> u64 {
        let query_revision = self.advance_query_revision();
        let _ = self.commands.send(SearchCommand::Search(SearchRequest {
            snapshot_generation,
            query_revision,
            query,
            regex,
            cursor,
        }));
        query_revision
    }

    pub(crate) fn cancel(&mut self) {
        self.advance_query_revision();
    }

    /// Wait until the worker has stopped using the previous snapshot cursor.
    pub(crate) fn cancel_and_wait(&mut self) {
        self.cancel();
        let (tx, rx) = mpsc::channel();
        if self.commands.send(SearchCommand::Barrier(tx)).is_ok() {
            let _ = rx.recv();
        }
    }

    pub(crate) fn take_latest_result(&self) -> Option<SearchResult> {
        self.results.try_iter().last()
    }

    fn advance_query_revision(&mut self) -> u64 {
        self.next_query_revision = self.next_query_revision.wrapping_add(1);
        if self.next_query_revision == 0 {
            self.next_query_revision = 1;
        }
        self.latest_query_revision
            .store(self.next_query_revision, Ordering::Release);
        self.next_query_revision
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        self.cancel();
        let _ = self.commands.send(SearchCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn worker_loop(
    commands: mpsc::Receiver<SearchCommand>,
    results: mpsc::Sender<SearchResult>,
    latest_query_revision: Arc<AtomicU64>,
    ctx: egui::Context,
) {
    'requests: while let Ok(command) = commands.recv() {
        let mut request = match command {
            SearchCommand::Search(request) => request,
            SearchCommand::Barrier(done) => {
                let _ = done.send(());
                continue;
            }
            SearchCommand::Shutdown => return,
        };

        // Collapse queued edits before starting work. A query revision check
        // inside the byte scan also abandons an in-progress search after a
        // newer edit.
        loop {
            match commands.try_recv() {
                Ok(SearchCommand::Search(newer)) => request = newer,
                Ok(SearchCommand::Barrier(done)) => {
                    let _ = done.send(());
                    continue 'requests;
                }
                Ok(SearchCommand::Shutdown) => return,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        let query_revision = request.query_revision;
        if latest_query_revision.load(Ordering::Acquire) != query_revision {
            continue;
        }
        let Some(outcome) =
            find_search_matches_cancellable(request.cursor, &request.query, request.regex, || {
                latest_query_revision.load(Ordering::Acquire) != query_revision
            })
        else {
            continue;
        };
        if latest_query_revision.load(Ordering::Acquire) != query_revision {
            continue;
        }
        if results
            .send(SearchResult {
                snapshot_generation: request.snapshot_generation,
                query_revision,
                matches: outcome.matches,
                error: outcome.error,
            })
            .is_err()
        {
            return;
        }
        ctx.request_repaint();
    }
}

fn find_search_matches_cancellable(
    cursor: usize,
    query: &str,
    regex: bool,
    cancelled: impl Fn() -> bool,
) -> Option<SearchOutcome> {
    if query.is_empty() {
        return Some(SearchOutcome::default());
    }
    let pattern = if regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let matcher = match RegexBuilder::new(&pattern).case_insensitive(!regex).build() {
        Ok(matcher) => matcher,
        Err(error) => {
            return Some(SearchOutcome {
                matches: Vec::new(),
                error: Some(error.to_string()),
            });
        }
    };
    let mut scan = LineScan::new(matcher);
    if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_cursor_reset(cursor as *mut _) } {
        return None;
    }
    loop {
        if cancelled() {
            return None;
        }
        let mut chunk = SnapshotTextChunk::default();
        match unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_cursor_next(cursor as *mut _, &mut chunk)
        } {
            0 => break,
            1 => {}
            _ => return None,
        }
        let bytes = unsafe { std::slice::from_raw_parts(chunk.text, chunk.len) };
        let points = unsafe { std::slice::from_raw_parts(chunk.points, chunk.len) };
        scan.feed(bytes, points, chunk.row_base, chunk.start_y, &cancelled)?;
    }
    scan.finish(&cancelled)?;
    scan.matches.reverse();
    Some(SearchOutcome {
        matches: scan.matches,
        error: None,
    })
}

#[derive(Default)]
struct SearchOutcome {
    matches: Vec<SnapshotCellRange>,
    error: Option<String>,
}

struct LineScan {
    matcher: Regex,
    line: Vec<u8>,
    cells: Vec<SnapshotCell>,
    matches: Vec<SnapshotCellRange>,
}

impl LineScan {
    fn new(matcher: Regex) -> Self {
        Self {
            matcher,
            line: Vec::new(),
            cells: Vec::new(),
            matches: Vec::new(),
        }
    }

    fn feed(
        &mut self,
        chunk: &[u8],
        points: &[ghostty_ffi::SnapshotTextPoint],
        row_base: u64,
        start_y: u16,
        cancelled: &impl Fn() -> bool,
    ) -> Option<()> {
        if chunk.len() != points.len() {
            return None;
        }
        for (i, (&byte, point)) in chunk.iter().zip(points).enumerate() {
            if i & 0xfff == 0 && cancelled() {
                return None;
            }
            if byte == b'\n' {
                self.finish_line(cancelled)?;
                continue;
            }
            self.line.push(byte);
            self.cells.push(SnapshotCell {
                x: u32::from(point.x),
                y: row_base + u64::from(point.y.checked_sub(u32::from(start_y))?),
            });
        }
        Some(())
    }

    fn finish(&mut self, cancelled: &impl Fn() -> bool) -> Option<()> {
        self.finish_line(cancelled)
    }

    fn finish_line(&mut self, cancelled: &impl Fn() -> bool) -> Option<()> {
        if cancelled() {
            return None;
        }
        let line = std::str::from_utf8(&self.line).ok()?;
        for found in self.matcher.find_iter(line) {
            if cancelled() {
                return None;
            }
            if found.is_empty() {
                continue;
            }
            self.matches.push(SnapshotCellRange {
                start: *self.cells.get(found.start())?,
                end: *self.cells.get(found.end() - 1)?,
            });
        }
        self.line.clear();
        self.cells.clear();
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::{LineScan, SearchUi, find_search_matches_cancellable};
    use crate::ghostty_ffi::{SnapshotCell, SnapshotCellRange, SnapshotTextPoint};
    use regex::{Regex, RegexBuilder};

    #[test]
    fn copy_target_uses_selected_start_and_rejects_pending_or_invalid_results() {
        let mut search = SearchUi {
            active: true,
            ..Default::default()
        };
        let matches = vec![
            SnapshotCellRange {
                start: SnapshotCell { x: 4, y: 20 },
                end: SnapshotCell { x: 7, y: 20 },
            },
            SnapshotCellRange {
                start: SnapshotCell { x: 2, y: 10 },
                end: SnapshotCell { x: 1, y: 11 },
            },
        ];
        search.apply_result(matches, None);
        search.navigate(false);
        assert_eq!(search.copy_target(), Some(SnapshotCell { x: 2, y: 10 }));
        search.waiting = true;
        assert_eq!(search.copy_target(), None);
        search.waiting = false;
        search.error = Some("invalid regex".into());
        assert_eq!(search.copy_target(), None);
        search.apply_result(Vec::new(), None);
        assert_eq!(search.copy_target(), None);
    }

    fn feed(scan: &mut LineScan, text: &str, row_base: u64) {
        let points: Vec<_> = (0..text.len())
            .map(|x| SnapshotTextPoint { x: x as u16, y: 0 })
            .collect();
        scan.feed(text.as_bytes(), &points, row_base, 0, &|| false)
            .unwrap();
    }

    #[test]
    fn literal_matches_across_chunks_and_preserves_cell_locations() {
        let matcher = RegexBuilder::new(&regex::escape("界x"))
            .case_insensitive(true)
            .build()
            .unwrap();
        let mut scan = LineScan::new(matcher);
        feed(&mut scan, "a界", 0);
        feed(&mut scan, "x and 界x", 1);
        scan.finish(&|| false).unwrap();
        assert_eq!(
            scan.matches,
            vec![
                SnapshotCellRange {
                    start: SnapshotCell { x: 1, y: 0 },
                    end: SnapshotCell { x: 0, y: 1 }
                },
                SnapshotCellRange {
                    start: SnapshotCell { x: 6, y: 1 },
                    end: SnapshotCell { x: 9, y: 1 }
                },
            ]
        );
    }

    #[test]
    fn unicode_literal_search_is_case_insensitive_and_nonoverlapping() {
        let matcher = RegexBuilder::new(&regex::escape("éa"))
            .case_insensitive(true)
            .build()
            .unwrap();
        let mut scan = LineScan::new(matcher);
        feed(&mut scan, "ÉaéA", 0);
        scan.finish(&|| false).unwrap();
        assert_eq!(scan.matches.len(), 2);
        assert_eq!(scan.matches[0].start.x, 0);
        assert_eq!(scan.matches[1].start.x, 3);
    }

    #[test]
    fn utf8_match_can_span_chunks_without_losing_its_cell() {
        let mut scan = LineScan::new(Regex::new("界").unwrap());
        let first = [
            SnapshotTextPoint { x: 0, y: 0 },
            SnapshotTextPoint { x: 1, y: 0 },
        ];
        let second = [
            SnapshotTextPoint { x: 1, y: 0 },
            SnapshotTextPoint { x: 1, y: 0 },
        ];
        scan.feed(&[b'a', 0xe7], &first, 0, 0, &|| false).unwrap();
        scan.feed(&[0x95, 0x8c], &second, 0, 0, &|| false).unwrap();
        scan.finish(&|| false).unwrap();
        assert_eq!(
            scan.matches,
            vec![SnapshotCellRange {
                start: SnapshotCell { x: 1, y: 0 },
                end: SnapshotCell { x: 1, y: 0 },
            }]
        );
    }

    #[test]
    fn regex_search_stays_within_logical_lines() {
        let mut scan = LineScan::new(Regex::new(r"a.+b").unwrap());
        feed(&mut scan, "a1b\na", 0);
        feed(&mut scan, "2b\na3\nb", 1);
        scan.finish(&|| false).unwrap();
        assert_eq!(scan.matches.len(), 2);
        assert_eq!(scan.matches[0].start, SnapshotCell { x: 0, y: 0 });
        assert_eq!(scan.matches[1].end, SnapshotCell { x: 1, y: 1 });
    }

    #[test]
    fn literal_metacharacters_are_escaped() {
        let matcher = RegexBuilder::new(&regex::escape("a.b"))
            .case_insensitive(true)
            .build()
            .unwrap();
        let mut scan = LineScan::new(matcher);
        feed(&mut scan, "a-b A.B", 0);
        scan.finish(&|| false).unwrap();
        assert_eq!(scan.matches.len(), 1);
        assert_eq!(scan.matches[0].start.x, 4);
    }

    #[test]
    fn zero_width_regex_matches_have_no_highlight() {
        let mut scan = LineScan::new(Regex::new(r"^|b").unwrap());
        feed(&mut scan, "abc", 0);
        scan.finish(&|| false).unwrap();
        assert_eq!(scan.matches.len(), 1);
        assert_eq!(scan.matches[0].start.x, 1);
    }

    #[test]
    fn invalid_regex_returns_an_error_without_accessing_the_cursor() {
        let outcome = find_search_matches_cancellable(0, "(", true, || false).unwrap();
        assert!(outcome.matches.is_empty());
        assert!(outcome.error.is_some());
    }
}

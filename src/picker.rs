use crate::copy_mode::TextRow;
use crate::ffi::{self, Host, SnapshotCell, SnapshotCellRange};
use nfm_search_core::fuzzy_search_session::{FuzzySearchSession, FuzzySearchUpdate};
use nfm_search_core::search::{SearchResult, resolve_match_positions};
use nfm_search_core::snapshot_store::SnapshotStore;
use nfm_search_core::store::{
    PublishingStreamingItemStoreWithPayload, StreamingItemSnapshotWithPayload,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::{self, JoinHandle};

type Items = StreamingItemSnapshotWithPayload<SnapshotCellRange>;
const RESULT_ROWS: usize = 7;

fn visible_start(start: usize, selected: usize) -> usize {
    if selected < start {
        selected
    } else if selected >= start + RESULT_ROWS {
        selected + 1 - RESULT_ROWS
    } else {
        start
    }
}

fn result_index(start: usize, slot: usize) -> usize {
    start + RESULT_ROWS - 1 - slot
}

fn result_layout(
    text: &str,
    query: &str,
    font: &egui::FontId,
    matched_font: &egui::FontId,
) -> egui::text::LayoutJob {
    let normal = egui::TextFormat {
        font_id: font.clone(),
        color: egui::Color32::from_rgb(235, 219, 178),
        ..Default::default()
    };
    let matched = egui::TextFormat {
        color: egui::Color32::from_rgb(254, 128, 25),
        font_id: matched_font.clone(),
        ..normal.clone()
    };
    let positions = resolve_match_positions(query, text);
    let mut job = egui::text::LayoutJob::default();
    // Positions are UTF-8 byte offsets, not character or terminal columns.
    let mut start = 0;
    let mut highlighted = positions.contains(&0);
    for (offset, _) in text.char_indices().skip(1) {
        let next = positions.contains(&offset);
        if next != highlighted {
            job.append(
                &text[start..offset],
                0.0,
                if highlighted {
                    matched.clone()
                } else {
                    normal.clone()
                },
            );
            start = offset;
            highlighted = next;
        }
    }
    job.append(
        &text[start..],
        0.0,
        if highlighted { matched } else { normal },
    );
    job
}

pub(crate) struct Picker {
    pub(crate) generation: u64,
    pub(crate) from_copy: bool,
    pub(crate) typography: crate::picker_font::Typography,
    session_id: u64,
    session: FuzzySearchSession<Items, SnapshotStore<Items>>,
    store: Arc<SnapshotStore<Items>>,
    updates: Arc<std::sync::Mutex<Option<FuzzySearchUpdate>>>,
    cancelled: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    query: String,
    submitted_query: String,
    results_query: String,
    query_revision: u64,
    results_revision: Option<u64>,
    results: Vec<SearchResult>,
    selected: usize,
    visible_start: usize,
    results_shown: bool,
    done: bool,
    error: Arc<std::sync::Mutex<Option<String>>>,
}

pub(crate) enum Action {
    Cancel,
    Accept(SnapshotCell),
}

impl Picker {
    pub(crate) fn start(
        host: *mut Host,
        generation: u64,
        from_copy: bool,
        rows: u64,
        ctx: egui::Context,
    ) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let session_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let store = Arc::new(SnapshotStore::new());
        let (tx, updates_rx) = crossbeam_channel::unbounded();
        let session = FuzzySearchSession::new(session_id, store.clone(), String::new(), tx);
        session.start();
        let cancelled = Arc::new(AtomicBool::new(false));
        let updates = Arc::new(std::sync::Mutex::new(None));
        {
            let (updates, cancelled, ctx) = (updates.clone(), cancelled.clone(), ctx.clone());
            thread::spawn(move || {
                while !cancelled.load(Ordering::Acquire) {
                    match updates_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                        Ok(mut update) => {
                            while let Ok(newer) = updates_rx.try_recv() {
                                update = newer;
                            }
                            *updates.lock().unwrap() = Some(update);
                            ctx.request_repaint();
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
            });
        }
        let error = Arc::new(std::sync::Mutex::new(None));
        let reader = {
            let (store, cancelled, error) = (store.clone(), cancelled.clone(), error.clone());
            let host = host as usize;
            thread::spawn(move || {
                let mut publisher = PublishingStreamingItemStoreWithPayload::new(store);
                let read = |row| {
                    if cancelled.load(Ordering::Acquire) {
                        return None;
                    }
                    let result = read_row(host as *mut Host, generation, row);
                    if result.is_none() {
                        *error.lock().unwrap() = Some("Unable to read scrollback snapshot".into());
                    }
                    result
                };
                let mut bottom = rows;
                let mut batch = 0;
                while bottom > 0 && !cancelled.load(Ordering::Acquire) {
                    let end = bottom - 1;
                    let mut start = end;
                    let Some(last) = read(end) else {
                        break;
                    };
                    let mut lines = vec![(end, last)];
                    while start > 0 {
                        let Some(previous) = read(start - 1) else {
                            break;
                        };
                        if !previous.wraps_to_next {
                            break;
                        }
                        start -= 1;
                        lines.push((start, previous));
                    }
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    lines.reverse();
                    if let Some((text, range)) = logical_item(&lines) {
                        publisher.add_item(text.as_bytes(), range);
                        batch += 1;
                    }
                    bottom = start;
                    if batch >= 128 {
                        publisher.publish();
                        batch = 0;
                        ctx.request_repaint();
                    }
                }
                publisher.complete();
                ctx.request_repaint();
            })
        };
        Self {
            generation,
            from_copy,
            typography: crate::picker_font::Typography::default(),
            session_id,
            session,
            store,
            updates,
            cancelled,
            reader: Some(reader),
            query: String::new(),
            submitted_query: String::new(),
            results_query: String::new(),
            query_revision: 0,
            results_revision: None,
            results: Vec::new(),
            selected: 0,
            visible_start: 0,
            results_shown: false,
            done: false,
            error,
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, bottom_margin: f32) -> Option<Action> {
        let latest = self.updates.lock().unwrap().take().filter(|update| {
            update.session_id == self.session_id
                && update.generation == self.query_revision
                && update.query == self.query
        });
        if let Some(update) = latest {
            let selected_id = (self.results_query == update.query)
                .then(|| self.results.get(self.selected).map(|r| r.node_index))
                .flatten();
            self.results = update.results;
            self.selected = selected_id
                .and_then(|id| self.results.iter().position(|r| r.node_index == id))
                .unwrap_or(0);
            self.visible_start = visible_start(self.visible_start, self.selected);
            self.results_query = update.query;
            self.results_revision = Some(update.generation);
            self.done = update.source_done;
        }
        // Measure and focus immediately, but reveal the panel with its first
        // useful contents. Once shown, keep it visible during later searches.
        self.results_shown |=
            !self.results.is_empty() || self.done || self.error.lock().unwrap().is_some();
        let mut action = None;
        let mut accept = false;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                action = Some(Action::Cancel);
            }
            for (key, mods, delta) in [
                (egui::Key::N, egui::Modifiers::CTRL, 1isize),
                (egui::Key::P, egui::Modifiers::CTRL, -1),
                (egui::Key::ArrowDown, egui::Modifiers::NONE, -1),
                (egui::Key::ArrowUp, egui::Modifiers::NONE, 1),
                (
                    egui::Key::PageDown,
                    egui::Modifiers::NONE,
                    -(RESULT_ROWS as isize),
                ),
                (
                    egui::Key::PageUp,
                    egui::Modifiers::NONE,
                    RESULT_ROWS as isize,
                ),
            ] {
                if i.consume_key(mods, key) && !self.results.is_empty() {
                    self.selected = self
                        .selected
                        .saturating_add_signed(delta)
                        .min(self.results.len() - 1);
                    self.visible_start = visible_start(self.visible_start, self.selected);
                }
            }
            accept = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        });
        // Auto-generated widget IDs change when streaming result rows appear.
        // Give the sole input a stable session ID and focus it before it reads
        // events, including the Area's initial sizing pass.
        let query_id = egui::Id::new(("scrollback_picker_query", self.session_id));
        ctx.memory_mut(|memory| memory.request_focus(query_id));
        let area_id = egui::Id::new("scrollback_picker");
        let previous_size = ctx.memory(|memory| memory.area_rect(area_id).map(|rect| rect.size()));
        let area = egui::Area::new(area_id)
            .fade_in(false)
            .anchor(
                egui::Align2::CENTER_BOTTOM,
                egui::vec2(0.0, -bottom_margin - 10.0),
            )
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                if !self.results_shown {
                    ui.set_opacity(0.0);
                }
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(40, 40, 40))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(215, 153, 33),
                    ))
                    .corner_radius(5)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        // Ten points outside each edge, plus frame padding/stroke.
                        ui.set_width((ctx.content_rect().width() - 38.0).max(1.0));
                        ui.visuals_mut().override_text_color =
                            Some(egui::Color32::from_rgb(235, 219, 178));
                        ui.visuals_mut().text_cursor.stroke =
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(235, 219, 178));
                        ui.visuals_mut().selection.bg_fill = egui::Color32::from_rgb(80, 73, 69);
                        ui.visuals_mut().selection.stroke.color =
                            egui::Color32::from_rgb(235, 219, 178);
                        let error = self.error.lock().unwrap().clone();
                        // Keep the bottom-anchored Area's measured height stable.
                        // Otherwise new rows render using the previous height
                        // before the anchor catches up on the following frame.
                        let row_height = self.typography.row_height;
                        for slot in 0..RESULT_ROWS {
                            // Rank zero sits directly above the query; empty
                            // slots stay above short result lists.
                            let index = result_index(self.visible_start, slot);
                            let text = if slot == RESULT_ROWS - 1 && self.results.is_empty() {
                                if let Some(error) = &error {
                                    error.clone()
                                } else if self.done {
                                    "No matches".into()
                                } else {
                                    // Avoid flashing a loading label before the
                                    // first batch of real results arrives.
                                    " ".into()
                                }
                            } else {
                                " ".into()
                            };
                            let (row_rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_height),
                                egui::Sense::hover(),
                            );
                            if index == self.selected && self.results.get(index).is_some() {
                                ui.painter().rect_filled(
                                    row_rect,
                                    2,
                                    egui::Color32::from_rgb(80, 73, 69),
                                );
                            }
                            let painter = ui
                                .painter()
                                .with_clip_rect(row_rect.intersect(ui.clip_rect()));
                            if let Some(result) = self.results.get(index) {
                                // Use the query that produced these results while
                                // a newer edit is still being searched.
                                let job = result_layout(
                                    &result.path,
                                    &self.results_query,
                                    &self.typography.font,
                                    &self.typography.matched_font,
                                );
                                let galley = painter.layout_job(job);
                                painter.galley(
                                    egui::pos2(
                                        row_rect.left(),
                                        row_rect.center().y - galley.size().y / 2.0,
                                    ),
                                    galley,
                                    egui::Color32::from_rgb(235, 219, 178),
                                );
                            } else {
                                painter.text(
                                    row_rect.left_center(),
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    self.typography.font.clone(),
                                    egui::Color32::from_rgb(235, 219, 178),
                                );
                            }
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(
                                egui::RichText::new(">").font(self.typography.font.clone()),
                            ));
                            let response = ui.add_sized(
                                [ui.available_width(), self.typography.row_height],
                                egui::TextEdit::singleline(&mut self.query)
                                    .id(query_id)
                                    .font(self.typography.font.clone())
                                    .vertical_align(egui::Align::Center)
                                    .desired_width(f32::INFINITY)
                                    .frame(egui::Frame::NONE),
                            );
                            if !response.has_focus() {
                                response.request_focus();
                                ctx.request_repaint();
                            }
                            if response.changed() && self.query != self.submitted_query {
                                self.query_revision += 1;
                                self.submitted_query = self.query.clone();
                                self.session.set_query(self.query.clone());
                                self.selected = 0;
                                self.visible_start = 0;
                            }
                        });
                    });
            });
        if action.is_none()
            && !accept
            && previous_size.is_none_or(|size| {
                (size.x - area.response.rect.width()).abs() > 0.5
                    || (size.y - area.response.rect.height()).abs() > 0.5
            })
        {
            // Areas anchor using their previously measured size. Resolve a
            // changed size in another UI pass before presenting this frame.
            ctx.request_discard("resolve scrollback picker size before presentation");
        }
        if action.is_none() && accept && self.results_revision == Some(self.query_revision) {
            if let Some(result) = self.results.get(self.selected) {
                if let Some(snapshot) = self.store.snapshot() {
                    action = Some(Action::Accept(snapshot.payload(result.node_index).start));
                }
            }
        }
        if accept && action.is_none() {
            ctx.request_repaint();
        }
        action
    }
}

pub(crate) fn consume_shortcut(events: &mut Vec<egui::Event>) -> (bool, bool) {
    let mut incoming = std::mem::take(events).into_iter().peekable();
    let (mut open, mut consumed) = (false, false);
    while let Some(event) = incoming.next() {
        if let egui::Event::Key {
            key: egui::Key::F,
            pressed,
            repeat,
            modifiers,
            ..
        } = &event
            && modifiers.ctrl
            && modifiers.shift
            && !modifiers.alt
            && !modifiers.mac_cmd
        {
            consumed = true;
            open |= *pressed && !*repeat;
            if *pressed && matches!(incoming.peek(), Some(egui::Event::Text(_))) {
                incoming.next();
            }
        } else {
            events.push(event);
        }
    }
    (open, consumed)
}

impl Drop for Picker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.session.stop();
        // Snapshot pages must outlive the reader. It checks cancellation
        // between row reads; fuzzy workers use only their Rust-owned store.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_row(host: *mut Host, generation: u64, row: u64) -> Option<TextRow> {
    let mut result = ffi::SnapshotTextRow::default();
    if !unsafe { ffi::ghostty_egui_host_snapshot_text_row(host, generation, row, &mut result) } {
        return None;
    }
    let (text, columns) = if result.len == 0 {
        (Vec::new(), Vec::new())
    } else {
        unsafe {
            (
                std::slice::from_raw_parts(result.text, result.len).to_vec(),
                std::slice::from_raw_parts(result.columns, result.len).to_vec(),
            )
        }
    };
    let wraps_to_next = result.wraps_to_next;
    unsafe { ffi::ghostty_egui_host_snapshot_text_row_free(&mut result) };
    Some(TextRow {
        text: String::from_utf8(text).ok()?,
        columns,
        wraps_to_next,
    })
}

fn logical_item(rows: &[(u64, TextRow)]) -> Option<(String, SnapshotCellRange)> {
    let mut text = String::new();
    let mut points = Vec::new();
    for (y, row) in rows {
        if row.text.len() != row.columns.len() {
            return None;
        }
        text.push_str(&row.text);
        points.extend(row.columns.iter().map(|&x| SnapshotCell { x, y: *y }));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len() - 1;
    Some((
        trimmed.to_owned(),
        SnapshotCellRange {
            start: points[start],
            end: points[end],
        },
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn position_highlights_preserve_text_and_utf8_boundaries() {
        let font = egui::FontId::monospace(14.0);
        let bold = egui::FontId::new(14.0, egui::FontFamily::Name("bold".into()));
        for (text, query, expected) in [("alpha-beta", "ab", "ab"), ("café", "é", "é")] {
            let job = super::result_layout(text, query, &font, &bold);
            assert_eq!(job.text, text);
            let highlighted: String = job
                .sections
                .iter()
                .filter(|section| section.format.color == egui::Color32::from_rgb(254, 128, 25))
                .map(|section| &job.text[section.byte_range.start.0..section.byte_range.end.0])
                .collect();
            assert_eq!(highlighted, expected);
            assert!(
                job.sections
                    .iter()
                    .filter(|section| section.format.color == egui::Color32::from_rgb(254, 128, 25))
                    .all(|section| section.format.font_id == bold)
            );
        }
        let job = super::result_layout("plain", "", &font, &bold);
        assert!(
            job.sections
                .iter()
                .all(|section| section.format.color == egui::Color32::from_rgb(235, 219, 178))
        );
    }

    #[test]
    fn results_fill_from_bottom_and_scroll_only_at_window_edges() {
        assert_eq!(super::result_index(0, 6), 0);
        assert_eq!(super::result_index(0, 0), 6);
        assert_eq!(super::result_index(3, 6), 3);
        assert_eq!(super::visible_start(0, 6), 0);
        assert_eq!(super::visible_start(0, 7), 1);
        assert_eq!(super::visible_start(3, 5), 3);
        assert_eq!(super::visible_start(3, 2), 2);
        // Two results occupy only the bottom two slots.
        let occupied: Vec<_> = (0..super::RESULT_ROWS)
            .filter(|slot| super::result_index(0, *slot) < 2)
            .collect();
        assert_eq!(occupied, vec![5, 6]);
    }

    use super::*;

    #[test]
    fn query_focus_and_panel_position_stay_stable_as_results_change() {
        let ctx = egui::Context::default();
        let store = Arc::new(SnapshotStore::<Items>::new());
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut picker = Picker {
            generation: 1,
            from_copy: false,
            typography: crate::picker_font::Typography::default(),
            session_id: 123,
            session: FuzzySearchSession::new(123, store.clone(), String::new(), tx),
            store,
            updates: Arc::new(std::sync::Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
            reader: None,
            query: String::new(),
            submitted_query: String::new(),
            results_query: String::new(),
            query_revision: 0,
            results_revision: None,
            results: Vec::new(),
            selected: 0,
            visible_start: 0,
            results_shown: false,
            done: false,
            error: Arc::new(std::sync::Mutex::new(None)),
        };
        let query_id = egui::Id::new(("scrollback_picker_query", 123u64));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 24.0);
        });
        output.textures_delta.clear();
        // The first Area pass measures its contents before making them visible.
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 24.0);
        });
        output.textures_delta.clear();
        assert!(ctx.memory(|m| m.has_focus(query_id)));
        assert!(!picker.results_shown);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 24.0);
        });
        output.textures_delta.clear();
        let panel = ctx
            .memory(|m| m.area_rect(egui::Id::new("scrollback_picker")))
            .unwrap();
        for (rows, text) in [(0, "w"), (7, "in"), (1, "dows")] {
            picker.results = (0..rows)
                .map(|node_index| SearchResult {
                    node_index,
                    score: 1,
                    path: format!("row {node_index}"),
                })
                .collect();
            let input = egui::RawInput {
                events: vec![egui::Event::Text(text.into())],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                picker.show(ui.ctx(), 24.0);
            });
            output.textures_delta.clear();
            assert!(ctx.memory(|m| m.has_focus(query_id)));
            assert_eq!(picker.results_shown, rows != 0);
            assert_eq!(
                ctx.memory(|m| m.area_rect(egui::Id::new("scrollback_picker")))
                    .unwrap(),
                panel
            );
        }
        assert_eq!(picker.query, "windows");
        picker.results.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 24.0);
        });
        output.textures_delta.clear();
        assert!(
            picker.results_shown,
            "later empty updates must not hide the panel"
        );
    }

    #[test]
    fn shortcut_consumes_paired_text_and_ignores_repeats() {
        let key = |repeat| egui::Event::Key {
            key: egui::Key::F,
            physical_key: Some(egui::Key::F),
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        };
        let mut events = vec![
            key(false),
            egui::Event::Text("F".into()),
            egui::Event::Text("x".into()),
        ];
        assert_eq!(consume_shortcut(&mut events), (true, true));
        assert!(matches!(events.as_slice(), [egui::Event::Text(t)] if t == "x"));
        let mut events = vec![key(true)];
        assert_eq!(consume_shortcut(&mut events), (false, true));
    }

    #[test]
    fn nfm_streaming_search_preserves_candidate_cell_payload() {
        let store = Arc::new(SnapshotStore::<Items>::new());
        let mut publisher = PublishingStreamingItemStoreWithPayload::new(store.clone());
        let range = SnapshotCellRange {
            start: SnapshotCell { x: 2, y: 40 },
            end: SnapshotCell { x: 18, y: 40 },
        };
        publisher.add_item(b"WindowsTerminal", range);
        publisher.publish();
        let (tx, rx) = crossbeam_channel::unbounded();
        let session = FuzzySearchSession::new(987, store.clone(), "wndtrm".into(), tx);
        session.start();
        let first = rx.recv_timeout(std::time::Duration::from_secs(5));
        session.stop();
        let first = first.expect("nfm worker publishes initial matches");
        assert_eq!(first.session_id, 987);
        assert_eq!(first.results.len(), 1);
        assert_eq!(
            *store
                .snapshot()
                .unwrap()
                .payload(first.results[0].node_index),
            range
        );
        publisher.add_item(b"Windows Terminal second", SnapshotCellRange::default());
        publisher.complete();
        let (tx, rx) = crossbeam_channel::unbounded();
        let session = FuzzySearchSession::new(988, store, "wndtrm".into(), tx);
        session.start();
        let update = rx.recv_timeout(std::time::Duration::from_secs(5));
        session.stop();
        let update = update.unwrap();
        assert_eq!(update.total, 2);
        assert_eq!(update.matched, 2);
        assert!(update.source_done);
    }
    #[test]
    fn logical_lines_preserve_unicode_cells_and_soft_wraps() {
        let rows = vec![
            (
                3,
                TextRow {
                    text: "  界".into(),
                    columns: vec![0, 1, 2, 2, 2],
                    wraps_to_next: true,
                },
            ),
            (
                4,
                TextRow {
                    text: "foo  ".into(),
                    columns: vec![0, 1, 2, 3, 4],
                    wraps_to_next: false,
                },
            ),
        ];
        let (text, range) = logical_item(&rows).unwrap();
        assert_eq!(text, "界foo");
        assert_eq!(range.start, SnapshotCell { x: 2, y: 3 });
        assert_eq!(range.end, SnapshotCell { x: 2, y: 4 });
        assert!(
            logical_item(&[(
                0,
                TextRow {
                    text: " ".into(),
                    columns: vec![0],
                    wraps_to_next: false
                }
            )])
            .is_none()
        );
    }
}

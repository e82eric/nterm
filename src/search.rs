use crate::ghostty_ffi::{self, SnapshotCell, SnapshotCellRange, SnapshotTextChunk};
pub(crate) use nfm_egui::search_service::SearchResult;
use nfm_egui::search_service::{SearchDocument, SearchService, SearchTicket};
#[cfg(test)]
use regex::Regex;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct SearchUi {
    pub(crate) copy_origin: Option<nfm_egui::copy_mode::CopyMode>,
    pub(crate) backward: bool,
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
        self.selected = nfm_egui::document_search::initial_match_index(
            &self.matches,
            self.copy_origin.as_ref().map(|origin| origin.cursor),
            !self.backward,
        );
        self.error = error;
        self.waiting = false;
    }

    pub(crate) fn navigate(&mut self, previous: bool) {
        self.selected = nfm_egui::document_search::navigate_match_index(
            self.selected,
            self.matches.len(),
            previous,
        );
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
        let palette = crate::colors::palette();
        let fill = palette.panel_background;
        let border = palette.border;
        let accent = palette.accent;
        let text_color = palette.text;
        let muted_text = palette.muted_text;
        let selection = palette.selection_background;

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
                    ui.visuals_mut().selection.stroke.color = palette.selection_text;
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add_sized(
                            [16.0, 24.0],
                            egui::Label::new(
                                egui::RichText::new(if self.backward { "?" } else { "/" })
                                    .font(egui::FontId::monospace(16.0))
                                    .color(accent),
                            ),
                        );

                        let match_label = if pending {
                            "…".to_owned()
                        } else if self.error.is_some() {
                            "SEARCH ERROR".to_owned()
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
/// Ghostty owns the native cursor; this guarded lease is invalidated before
/// the pane frees it. All scheduling, cancellation and feedback live in NFM.
struct SnapshotSearchDocument {
    id: usize,
    cursor: Mutex<usize>,
}
impl SearchDocument for SnapshotSearchDocument {
    fn visit_text(
        &self,
        visitor: &mut dyn FnMut(&[u8], &[SnapshotCell], bool) -> Result<(), String>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let cursor = self
            .cursor
            .lock()
            .map_err(|_| "Search cursor lease failed")?;
        if *cursor == 0 {
            return Err("Search snapshot lease has closed".into());
        }
        if cancelled() {
            return Err("Search cancelled".into());
        }
        if !unsafe { ghostty_ffi::nterm_host_snapshot_cursor_reset(*cursor as *mut _) } {
            return Err(ghostty_ffi::native_error(
                "Unable to reset search snapshot cursor",
            ));
        }
        loop {
            if cancelled() {
                return Err("Search cancelled".into());
            }
            let mut chunk = SnapshotTextChunk::default();
            match unsafe {
                ghostty_ffi::nterm_host_snapshot_cursor_next(*cursor as *mut _, &mut chunk)
            } {
                0 => return Ok(()),
                1 => {}
                _ => return Err(ghostty_ffi::native_error("Unable to read search snapshot")),
            }
            let bytes = unsafe { std::slice::from_raw_parts(chunk.text, chunk.len) };
            let points = unsafe { std::slice::from_raw_parts(chunk.points, chunk.len) };
            let positions = points
                .iter()
                .map(|point| {
                    point
                        .y
                        .checked_sub(u32::from(chunk.start_y))
                        .and_then(|offset| chunk.row_base.checked_add(u64::from(offset)))
                        .map(|y| SnapshotCell {
                            x: u32::from(point.x),
                            y,
                        })
                        .ok_or("Invalid snapshot search coordinate")
                })
                .collect::<Result<Vec<_>, _>>()?;
            visitor(bytes, &positions, false)?;
        }
    }
}
pub(crate) struct SnapshotSearchAdapter {
    service: SearchService,
    document: Option<Arc<SnapshotSearchDocument>>,
    ticket: Option<SearchTicket>,
}
impl SnapshotSearchAdapter {
    pub(crate) fn new(ctx: egui::Context) -> Self {
        Self {
            service: SearchService::new(move || ctx.request_repaint()),
            document: None,
            ticket: None,
        }
    }
    pub(crate) fn submit(
        &mut self,
        generation: u64,
        query: String,
        regex: bool,
        cursor: usize,
    ) -> Result<u64, String> {
        if self.document.as_ref().is_none_or(|doc| doc.id != cursor) {
            self.cancel_and_wait();
            self.document = Some(Arc::new(SnapshotSearchDocument {
                id: cursor,
                cursor: Mutex::new(cursor),
            }));
        }
        let ticket = self.service.submit(
            generation,
            self.document.as_ref().unwrap().clone(),
            query,
            regex,
        )?;
        let revision = ticket.query_revision;
        self.ticket = Some(ticket);
        Ok(revision)
    }
    pub(crate) fn cancel(&mut self) {
        self.ticket = None;
        self.service.cancel();
    }
    pub(crate) fn cancel_and_wait(&mut self) {
        self.ticket = None;
        if let Err(error) = self.service.cancel_and_wait() {
            crate::error_log::error("search", "Worker stopped while releasing snapshot", &error);
        }
        if let Some(document) = self.document.take() {
            *document.cursor.lock().unwrap_or_else(|e| e.into_inner()) = 0;
        }
    }
    pub(crate) fn take_latest_result(&mut self) -> Result<Option<SearchResult>, String> {
        let result = self.service.take_latest_result()?;
        if let Some(error) = result
            .as_ref()
            .and_then(|result| result.error.as_ref())
            .filter(|error| error.starts_with("Unable to"))
        {
            crate::error_log::error("search", "Snapshot scan failed", error);
        }
        Ok(result)
    }
}
impl Drop for SnapshotSearchAdapter {
    fn drop(&mut self) {
        self.cancel_and_wait();
    }
}

// Coordinate conversion regression harness for the shared scanner.
#[cfg(test)]
struct LineScan {
    scan: nfm_egui::document_search::LineScanner,
}
#[cfg(test)]
impl LineScan {
    fn new(matcher: Regex) -> Self {
        Self {
            scan: nfm_egui::document_search::LineScanner::new(matcher),
        }
    }
    fn feed(
        &mut self,
        chunk: &[u8],
        points: &[ghostty_ffi::SnapshotTextPoint],
        row_base: u64,
        start_y: u16,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), nfm_egui::document_search::SearchError> {
        let positions = points
            .iter()
            .map(|point| {
                point
                    .y
                    .checked_sub(u32::from(start_y))
                    .and_then(|offset| row_base.checked_add(u64::from(offset)))
                    .map(|y| SnapshotCell {
                        x: u32::from(point.x),
                        y,
                    })
                    .ok_or(nfm_egui::document_search::SearchError::InvalidDocument)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.scan.feed(chunk, positions, cancelled)
    }
    #[cfg(test)]
    fn finish(
        &mut self,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), nfm_egui::document_search::SearchError> {
        self.scan.finish_line(cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::{LineScan, SearchUi};
    use crate::ghostty_ffi::{SnapshotCell, SnapshotCellRange, SnapshotTextPoint};
    use regex::{Regex, RegexBuilder};

    #[test]
    fn copy_search_selects_relative_to_cursor_in_both_directions() {
        let matches: Vec<_> = [2, 8, 14]
            .into_iter()
            .map(|x| SnapshotCellRange {
                start: SnapshotCell { x, y: 10 },
                end: SnapshotCell { x: x + 1, y: 10 },
            })
            .collect();
        let mut origin = nfm_egui::copy_mode::CopyMode::default();
        origin.cursor = SnapshotCell { x: 8, y: 10 };
        let mut search = SearchUi {
            active: true,
            copy_origin: Some(origin),
            ..Default::default()
        };
        search.apply_result(matches.clone(), None);
        assert_eq!(search.copy_target(), Some(SnapshotCell { x: 14, y: 10 }));
        search.backward = true;
        search.apply_result(matches, None);
        assert_eq!(search.copy_target(), Some(SnapshotCell { x: 2, y: 10 }));
    }

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
            scan.scan.matches(),
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
        assert_eq!(scan.scan.matches().len(), 2);
        assert_eq!(scan.scan.matches()[0].start.x, 0);
        assert_eq!(scan.scan.matches()[1].start.x, 3);
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
            scan.scan.matches(),
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
        assert_eq!(scan.scan.matches().len(), 2);
        assert_eq!(scan.scan.matches()[0].start, SnapshotCell { x: 0, y: 0 });
        assert_eq!(scan.scan.matches()[1].end, SnapshotCell { x: 1, y: 1 });
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
        assert_eq!(scan.scan.matches().len(), 1);
        assert_eq!(scan.scan.matches()[0].start.x, 4);
    }

    #[test]
    fn zero_width_regex_matches_have_no_highlight() {
        let mut scan = LineScan::new(Regex::new(r"^|b").unwrap());
        feed(&mut scan, "abc", 0);
        scan.finish(&|| false).unwrap();
        assert_eq!(scan.scan.matches().len(), 1);
        assert_eq!(scan.scan.matches()[0].start.x, 1);
    }

    #[test]
    fn invalid_regex_returns_an_error_without_accessing_the_cursor() {
        let document = super::SnapshotSearchDocument {
            id: 0,
            cursor: std::sync::Mutex::new(0),
        };
        let error = nfm_egui::search_service::search_document(
            &document,
            "(",
            true,
            Default::default(),
            &|| false,
        )
        .unwrap_err();
        assert!(error.contains("regex"));
    }
}

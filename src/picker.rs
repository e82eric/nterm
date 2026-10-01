pub(crate) mod action;
pub(crate) mod command;
pub(crate) mod config;
pub(crate) mod files;
pub(crate) mod font;
pub(crate) mod preview;
mod process;
pub(crate) mod protocol;
mod source;
pub(crate) mod style;
use source::PickerSource;

#[cfg(test)]
use crate::copy_mode::TextRow;
use crate::ghostty_ffi::{self, Host, SnapshotCell, SnapshotCellRange};
#[cfg(test)]
use nfm_search_core::fuzzy_search_session::FuzzySearchSession;
use nfm_search_core::fuzzy_search_session::FuzzySearchUpdate;
use nfm_search_core::search::{SearchResult, resolve_match_positions};
#[cfg(test)]
use nfm_search_core::snapshot_store::SnapshotStore;
#[cfg(test)]
use nfm_search_core::store::PublishingStreamingItemStoreWithPayload;
use nfm_search_core::store::StreamingItemSnapshotWithPayload;
#[cfg(test)]
use source::{logical_item, logical_items, newest_logical_items};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

type Items = StreamingItemSnapshotWithPayload<SnapshotCellRange>;
const RESULT_ROWS: usize = 7;

fn preview_rows(height: f32, row_height: f32) -> usize {
    ((height - RESULT_ROWS as f32 * row_height - 72.0 - 2.0 * style::DIVIDER_EXTRA) / row_height)
        .floor()
        .clamp(1.0, 12.0) as usize
}

pub(crate) fn picker_anchor(
    position: crate::picker::config::Position,
    cursor_row: Option<u32>,
    row_height: f32,
    bounds: egui::Rect,
    screen: egui::Rect,
    measured_size: Option<egui::Vec2>,
) -> (egui::Align2, egui::Vec2) {
    use crate::picker::config::Position;
    let x = bounds.center().x - screen.center().x;
    match position {
        Position::Top => (
            egui::Align2::CENTER_TOP,
            egui::vec2(x, bounds.top() + 10.0 - screen.top()),
        ),
        Position::Center => (
            egui::Align2::CENTER_CENTER,
            egui::vec2(x, bounds.center().y - screen.center().y),
        ),
        Position::Bottom => (
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(x, bounds.bottom() - screen.bottom() - 10.0),
        ),
        Position::Cursor => {
            let Some(row) = cursor_row else {
                return picker_anchor(
                    Position::Bottom,
                    None,
                    row_height,
                    bounds,
                    screen,
                    measured_size,
                );
            };
            let height = measured_size
                .map_or((RESULT_ROWS as f32 + 1.0) * row_height + 32.0, |size| {
                    size.y
                });
            let below_top = bounds.top() + (row as f32 + 1.0) * row_height + 10.0;
            // When there is no room below, let the picker overlap the cursor row.
            let above_bottom = bounds.top() + (row as f32 + 1.0) * row_height;
            let min_top = bounds.top() + 10.0;
            let max_bottom = bounds.bottom() - 10.0;
            if below_top + height <= max_bottom {
                (
                    egui::Align2::CENTER_TOP,
                    egui::vec2(x, below_top - screen.top()),
                )
            } else if above_bottom - height >= min_top && above_bottom <= max_bottom {
                (
                    egui::Align2::CENTER_BOTTOM,
                    egui::vec2(x, above_bottom - screen.bottom()),
                )
            } else if max_bottom - below_top >= above_bottom - min_top {
                let top = below_top.clamp(min_top, (max_bottom - height).max(min_top));
                (egui::Align2::CENTER_TOP, egui::vec2(x, top - screen.top()))
            } else {
                let bottom = above_bottom.clamp((min_top + height).min(max_bottom), max_bottom);
                (
                    egui::Align2::CENTER_BOTTOM,
                    egui::vec2(x, bottom - screen.bottom()),
                )
            }
        }
    }
}

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

fn paint_loading_indicator(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let center = rect.center();
    let dot_size = rect.height() / 4.0;
    let dot_step = (rect.height() - dot_size) / 2.0;
    let phase = (ui.input(|input| input.time) * 12.0) as usize % 8;
    let dots = [
        (-1.0, -1.0),
        (0.0, -1.0),
        (1.0, -1.0),
        (1.0, 0.0),
        (1.0, 1.0),
        (0.0, 1.0),
        (-1.0, 1.0),
        (-1.0, 0.0),
    ];
    for (dot, (x, y)) in dots.into_iter().enumerate() {
        let distance = (dot + 8 - phase) % 8;
        let alpha = 1.0 - distance as f32 * 0.11;
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                center + egui::vec2(x * dot_step, y * dot_step),
                egui::vec2(dot_size, dot_size),
            ),
            0.0,
            color.gamma_multiply(alpha),
        );
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(80));
}

fn result_layout(
    text: &str,
    query: &str,
    font: &egui::FontId,
    matched_font: &egui::FontId,
    selected: bool,
) -> egui::text::LayoutJob {
    let normal = egui::TextFormat {
        font_id: font.clone(),
        color: if selected {
            crate::colors::palette().selection_text
        } else {
            crate::colors::palette().text
        },
        ..Default::default()
    };
    let matched = egui::TextFormat {
        color: crate::colors::palette().match_highlight,
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
    pub(crate) cursor_row: Option<u32>,
    position: crate::picker::config::Position,
    pub(crate) generation: Option<u64>,
    pub(crate) from_copy: bool,
    pub(crate) typography: crate::picker::font::Typography,
    preview: Option<crate::picker::preview::PickerPreview>,
    preview_visible: bool,
    presented_once: bool,
    callback: Option<std::rc::Rc<crate::picker::action::Callback>>,
    request_params: std::collections::BTreeMap<String, String>,
    enter_action: crate::picker::config::EnterAction,
    source: PickerSource,
    session_id: u64,
    updates: Arc<std::sync::Mutex<Option<FuzzySearchUpdate>>>,
    query: String,
    submitted_query: String,
    results_query: String,
    query_revision: u64,
    results_revision: Option<u64>,
    results: Vec<SearchResult>,
    matched: usize,
    total: usize,
    selected: usize,
    visible_start: usize,
    copied_until: Option<Instant>,
    done: bool,
    error: Arc<std::sync::Mutex<Option<String>>>,
}

pub(crate) enum Action {
    Execute(crate::actions::AppAction),
    Custom(
        std::rc::Rc<crate::picker::action::Callback>,
        String,
        bool,
        Option<SnapshotCellRange>,
        String,
        std::collections::BTreeMap<String, String>,
    ),
    Cancel,
    Accept(SnapshotCell),
    SendInput(String),
}

impl Picker {
    pub(crate) fn is_command_palette(&self) -> bool {
        self.source.is_command_palette()
    }
    pub(crate) fn label(&self) -> &'static str {
        self.source.label()
    }

    pub(crate) fn start(
        host: *mut Host,
        generation: Option<u64>,
        from_copy: bool,
        rows: u64,
        ctx: egui::Context,
        mut options: crate::picker::config::Options,
    ) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let session_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let initial_query: String = options
            .params
            .get("query")
            .map(|query| query.chars().take(4096).collect())
            .unwrap_or_default();
        let snapshot_columns = generation
            .and_then(|generation| {
                let mut view = ghostty_ffi::SnapshotView::default();
                unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view) }
                    .then_some(view.columns as usize)
            })
            .unwrap_or(80);
        let (source, updates, error) = PickerSource::start(
            host,
            generation,
            rows,
            ctx.clone(),
            &mut options,
            session_id,
            initial_query.clone(),
        );
        Self {
            cursor_row: None,
            position: options.position,
            generation,
            from_copy,
            typography: crate::picker::font::Typography::default(),
            preview: options.preview.map(|source| {
                crate::picker::preview::PickerPreview::new(
                    source,
                    host,
                    generation,
                    rows,
                    snapshot_columns,
                )
            }),
            preview_visible: options.preview_visible,
            presented_once: false,
            callback: options.callback,
            request_params: options.params,
            enter_action: options.enter_action,
            source,
            session_id,
            updates,
            query: initial_query.clone(),
            submitted_query: initial_query,
            results_query: String::new(),
            query_revision: 0,
            results_revision: None,
            results: Vec::new(),
            matched: 0,
            total: 0,
            selected: 0,
            visible_start: 0,
            copied_until: None,
            done: false,
            error,
        }
    }

    fn accepted_value(&self, result: &SearchResult) -> String {
        self.source.accepted_value(result)
    }

    #[cfg(test)]
    pub(crate) fn show(&mut self, ctx: &egui::Context, bottom_margin: f32) -> Option<Action> {
        let mut bounds = ctx.content_rect();
        bounds.max.y -= bottom_margin;
        self.show_in_bounds(ctx, bounds)
    }

    pub(crate) fn show_in_bounds(
        &mut self,
        ctx: &egui::Context,
        bounds: egui::Rect,
    ) -> Option<Action> {
        self.show_in_bounds_mode(ctx, bounds, true)
    }

    pub(crate) fn show_background_in_bounds(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        self.show_in_bounds_mode(ctx, bounds, false);
    }

    fn show_in_bounds_mode(
        &mut self,
        ctx: &egui::Context,
        bounds: egui::Rect,
        interactive: bool,
    ) -> Option<Action> {
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
            self.matched = update.matched;
            self.total = update.total;
            self.selected = selected_id
                .and_then(|id| self.results.iter().position(|r| r.node_index == id))
                .unwrap_or(0);
            self.visible_start = visible_start(self.visible_start, self.selected);
            self.results_query = update.query;
            self.results_revision = Some(update.generation);
            self.done = update.source_done;
        }
        let mut action = None;
        let mut accept = false;
        let mut copy = false;
        let mut preview_toggled_off = false;
        let mut preview_page = 0;
        if interactive {
            ctx.input_mut(|i| {
                // egui-winit turns Ctrl+C into Copy before a Key event reaches us.
                // Keep the query TextEdit from copying its own selection instead.
                i.events.retain(|event| {
                    if matches!(event, egui::Event::Copy) {
                        copy = true;
                        false
                    } else {
                        true
                    }
                });
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    action = Some(Action::Cancel);
                }
                if i.consume_key(egui::Modifiers::CTRL, egui::Key::P) && self.preview.is_some() {
                    self.preview_visible = !self.preview_visible;
                    preview_toggled_off = !self.preview_visible;
                }
                if i.consume_key(egui::Modifiers::CTRL, egui::Key::PageUp) {
                    preview_page = -1;
                }
                if i.consume_key(egui::Modifiers::CTRL, egui::Key::PageDown) {
                    preview_page = 1;
                }
                for (key, mods, delta) in [
                    (egui::Key::N, egui::Modifiers::CTRL, 1isize),
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
        }
        if preview_toggled_off {
            self.preview.as_mut().unwrap().select(
                None,
                None,
                1,
                1,
                ctx,
                self.source.is_filesystem(),
                &self.request_params,
            );
        }
        if preview_page != 0 && self.preview_visible {
            if let Some(preview) = &mut self.preview {
                preview.scroll_half_page(preview_page, ctx);
            }
        }
        if copy && let Some(result) = self.results.get(self.selected) {
            ctx.copy_text(result.path.clone());
            self.copied_until = Some(Instant::now() + Duration::from_secs(2));
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        if let Some(preview) = self.preview.as_mut().filter(|_| self.preview_visible) {
            let range = self
                .results
                .get(self.selected)
                .and_then(|result| self.source.range(result));
            let item = self
                .results
                .get(self.selected)
                .map(|result| self.source.accepted_value(result));
            let width = ctx
                .fonts_mut(|fonts| fonts.glyph_width(&self.typography.font, 'M'))
                .max(1.0);
            let columns = ((bounds.width() - style::PANEL_CONTENT_INSET) / width)
                .floor()
                .max(1.0) as usize;
            let rows =
                preview_rows(bounds.height(), self.typography.row_height).min(preview.row_limit());
            preview.select(
                item.as_deref(),
                range,
                columns,
                rows,
                ctx,
                self.source.is_filesystem(),
                &self.request_params,
            );
        }
        let ready_to_present = self
            .preview
            .as_mut()
            .filter(|_| self.preview_visible)
            .is_none_or(|preview| preview.ready_to_present(ctx));
        // Auto-generated widget IDs change when streaming result rows appear.
        // Give the sole input a stable session ID and focus it before it reads
        // events, including the Area's initial sizing pass.
        let query_id = egui::Id::new(("scrollback_picker_query", self.session_id));
        if interactive {
            ctx.memory_mut(|memory| memory.request_focus(query_id));
        }
        let area_id = egui::Id::new(("scrollback_picker", self.session_id));
        let previous_size = ctx.memory(|memory| memory.area_rect(area_id).map(|rect| rect.size()));
        let measured_size = previous_size.or_else(|| {
            self.preview
                .as_ref()
                .filter(|_| self.preview_visible)
                .map(|_| {
                    let rows = preview_rows(bounds.height(), self.typography.row_height).min(
                        self.preview
                            .as_ref()
                            .map_or(12, |preview| preview.row_limit()),
                    );
                    egui::vec2(
                        0.0,
                        (RESULT_ROWS + rows + 1) as f32 * self.typography.row_height
                            + 44.0
                            + 2.0 * style::DIVIDER_EXTRA,
                    )
                })
        });
        let (align, offset) = picker_anchor(
            self.position,
            self.cursor_row,
            self.typography.row_height,
            bounds,
            ctx.content_rect(),
            measured_size,
        );
        let area = egui::Area::new(area_id)
            .fade_in(false)
            .anchor(align, offset)
            .interactable(interactive)
            .order(if interactive {
                egui::Order::Foreground
            } else {
                egui::Order::Middle
            })
            .show(ctx, |ui| {
                if !ready_to_present && !self.presented_once {
                    ui.set_opacity(0.0);
                }
                egui::Frame::new()
                    .fill(crate::colors::palette().panel_background)
                    .stroke(egui::Stroke::new(1.0, crate::colors::palette().border))
                    .corner_radius(style::PANEL_RADIUS)
                    .inner_margin(egui::Margin::symmetric(18, 10))
                    .show(ui, |ui| {
                        // Ten points outside each edge, plus frame padding and stroke.
                        ui.set_width((bounds.width() - style::PANEL_CONTENT_INSET).max(1.0));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.visuals_mut().override_text_color = Some(crate::colors::palette().text);
                        ui.visuals_mut().text_cursor.stroke =
                            egui::Stroke::new(2.0, crate::colors::palette().text);
                        ui.visuals_mut().selection.bg_fill =
                            crate::colors::palette().selection_background;
                        ui.visuals_mut().selection.stroke.color =
                            crate::colors::palette().selection_text;
                        if let Some(preview) =
                            self.preview.as_mut().filter(|_| self.preview_visible)
                        {
                            let rows = preview_rows(bounds.height(), self.typography.row_height)
                                .min(preview.row_limit());
                            preview.show(ui, &self.typography, rows);
                            style::divider(ui);
                        }
                        let error = self.error.lock().unwrap().clone();
                        // Keep the bottom-anchored Area's measured height stable.
                        // Otherwise new rows render using the previous height
                        // before the anchor catches up on the following frame.
                        let row_height = self.typography.row_height;
                        let mut results_top = None;
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
                                    // The query row carries the loading indicator.
                                    " ".into()
                                }
                            } else {
                                " ".into()
                            };
                            let (row_rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_height),
                                egui::Sense::hover(),
                            );
                            if slot == 0 {
                                results_top = Some(row_rect);
                            }
                            if matches!(self.source.label(), "GHOSTTY THEMES" | "ERROR HISTORY")
                                && self.results.get(index).is_some()
                                && ui
                                    .interact(
                                        row_rect,
                                        ui.id().with(("theme-row", index)),
                                        egui::Sense::click(),
                                    )
                                    .clicked()
                            {
                                self.selected = index;
                                accept = true;
                            }
                            if index == self.selected && self.results.get(index).is_some() {
                                ui.painter().rect_filled(
                                    row_rect,
                                    4,
                                    crate::colors::palette().selection_background,
                                );
                                ui.painter().rect_filled(
                                    egui::Rect::from_center_size(
                                        egui::pos2(row_rect.left() + 1.5, row_rect.center().y),
                                        egui::vec2(3.0, 16.0),
                                    ),
                                    1.5,
                                    crate::colors::palette().accent,
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
                                    &self.source.effective_query(&self.results_query),
                                    &self.typography.font,
                                    &self.typography.matched_font,
                                    index == self.selected,
                                );
                                let galley = painter.layout_job(job);
                                painter.galley(
                                    egui::pos2(
                                        row_rect.left() + 12.0,
                                        row_rect.center().y - galley.size().y / 2.0,
                                    ),
                                    galley,
                                    crate::colors::palette().text,
                                );
                            } else {
                                painter.text(
                                    row_rect.left_center(),
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    self.typography.font.clone(),
                                    if error.is_some() {
                                        crate::colors::palette().error
                                    } else {
                                        crate::colors::palette().muted_text
                                    },
                                );
                            }
                        }
                        if self
                            .copied_until
                            .is_some_and(|until| until > Instant::now())
                            && let Some(top) = results_top
                        {
                            let label = "Copied to clipboard";
                            let galley = ui.painter().layout_no_wrap(
                                label.to_owned(),
                                self.typography.font.clone(),
                                crate::colors::palette().success,
                            );
                            let toast = egui::Rect::from_min_size(
                                egui::pos2(top.right() - galley.size().x - 20.0, top.top() + 2.0),
                                galley.size() + egui::vec2(16.0, 6.0),
                            );
                            ui.painter().rect_filled(
                                toast,
                                3,
                                crate::colors::palette().selection_background,
                            );
                            ui.painter().galley(
                                toast.min + egui::vec2(8.0, 3.0),
                                galley,
                                crate::colors::palette().text,
                            );
                        }
                        style::divider(ui);
                        ui.horizontal(|ui| {
                            let prompt_width =
                                ui.fonts_mut(|fonts| fonts.glyph_width(&self.typography.font, '>'));
                            let (prompt_rect, _) = ui.allocate_exact_size(
                                egui::vec2(prompt_width, self.typography.row_height),
                                egui::Sense::hover(),
                            );
                            ui.painter().text(
                                prompt_rect.left_center(),
                                egui::Align2::LEFT_CENTER,
                                ">",
                                self.typography.font.clone(),
                                crate::colors::palette().accent,
                            );
                            let count = format!("{}/{}", self.matched, self.total);
                            let count_size = ui
                                .painter()
                                .layout_no_wrap(
                                    format!("{0}/{0}", self.total),
                                    self.typography.font.clone(),
                                    crate::colors::palette().text,
                                )
                                .size();
                            let indicator_size = 12.0;
                            let status_width = count_size.x + indicator_size + 12.0;
                            let query_width =
                                (ui.available_width() - status_width - ui.spacing().item_spacing.x)
                                    .max(1.0);
                            if interactive {
                                let response = ui.add_sized(
                                    [query_width, self.typography.row_height],
                                    egui::TextEdit::singleline(&mut self.query)
                                        .id(query_id)
                                        .font(self.typography.font.clone())
                                        .vertical_align(egui::Align::Center)
                                        .margin(0)
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
                                    self.source.set_query(self.query.clone());
                                    self.matched = 0;
                                    self.selected = 0;
                                    self.visible_start = 0;
                                }
                            } else {
                                let (query_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(query_width, self.typography.row_height),
                                    egui::Sense::hover(),
                                );
                                ui.painter().text(
                                    query_rect.left_center(),
                                    egui::Align2::LEFT_CENTER,
                                    &self.query,
                                    self.typography.font.clone(),
                                    crate::colors::palette().text,
                                );
                            }
                            let (status_rect, _) = ui.allocate_exact_size(
                                egui::vec2(status_width, self.typography.row_height),
                                egui::Sense::hover(),
                            );
                            let color = crate::colors::palette().muted_text;
                            if error.is_none()
                                && (self
                                    .preview
                                    .as_ref()
                                    .filter(|_| self.preview_visible)
                                    .is_some_and(|preview| preview.pending())
                                    || !self.done
                                    || self.results_revision != Some(self.query_revision))
                            {
                                let text_size = ui
                                    .painter()
                                    .layout_no_wrap(
                                        count.clone(),
                                        self.typography.font.clone(),
                                        color,
                                    )
                                    .size();
                                let center = egui::pos2(
                                    status_rect.right() - text_size.x - indicator_size / 2.0 - 8.0,
                                    status_rect.center().y,
                                );
                                paint_loading_indicator(
                                    ui,
                                    egui::Rect::from_center_size(
                                        center,
                                        egui::vec2(indicator_size, indicator_size),
                                    ),
                                    color,
                                );
                            }
                            ui.painter().text(
                                status_rect.right_center(),
                                egui::Align2::RIGHT_CENTER,
                                count,
                                self.typography.font.clone(),
                                color,
                            );
                        });
                    });
            });
        if ready_to_present {
            self.presented_once = true;
        }
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
                action = if let Some(action) = self.source.application_action(result) {
                    Some(Action::Execute(action))
                } else if let Some(callback) = &self.callback {
                    let range = self.source.range(result);
                    Some(Action::Custom(
                        callback.clone(),
                        result.path.clone(),
                        self.source.is_filesystem(),
                        range,
                        self.accepted_value(result),
                        self.request_params.clone(),
                    ))
                } else {
                    match self.enter_action {
                        crate::picker::config::EnterAction::Navigate => self
                            .source
                            .range(result)
                            .map(|range| Action::Accept(range.start)),
                        crate::picker::config::EnterAction::SendInput => {
                            Some(Action::SendInput(self.accepted_value(result)))
                        }
                    }
                };
            }
        }
        if accept && action.is_none() {
            ctx.request_repaint();
        }
        action
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_palette_source_returns_typed_actions_and_preserves_picker_options() {
        let ctx = egui::Context::default();
        let entries = vec![
            crate::actions::Entry {
                id: "floating.toggle".into(),
                label: "Toggle floating pane".into(),
                shortcut: Some("Ctrl+Shift+E".into()),
                action: crate::actions::AppAction::ToggleFloatingPane,
            },
            crate::actions::Entry {
                id: "picker.files".into(),
                label: "Open picker: files".into(),
                shortcut: None,
                action: crate::actions::AppAction::OpenPicker("files".into()),
            },
        ];
        let mut picker = Picker::start(
            std::ptr::null_mut(),
            None,
            true,
            0,
            ctx.clone(),
            crate::picker::config::Options {
                source: crate::picker::config::Source::CommandPalette(entries),
                position: crate::picker::config::Position::Top,
                params: std::collections::BTreeMap::from([(
                    "query".into(),
                    "/:Action=~files".into(),
                )]),
                preview: Some(crate::picker::config::Preview::Text {
                    text: "Choose an action".into(),
                }),
                preview_visible: false,
                ..Default::default()
            },
        );
        assert!(picker.is_command_palette());
        assert_eq!(picker.label(), "COMMAND PALETTE");
        assert_eq!(picker.position, crate::picker::config::Position::Top);
        assert!(!picker.preview_visible);
        assert!(picker.from_copy && picker.generation.is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                assert!(picker.show(ui.ctx(), 24.0).is_none());
            });
            output.textures_delta.clear();
            if picker.done && picker.results.len() == 1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(picker.accepted_value(&picker.results[0]), "picker.files");
        assert!(picker.results[0].path.starts_with("Open picker: files"));
        assert!(!picker.results[0].path.contains(','));
        let mut selected = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                selected = picker.show(ui.ctx(), 24.0);
            },
        );
        output.textures_delta.clear();
        assert!(
            matches!(selected, Some(Action::Execute(crate::actions::AppAction::OpenPicker(name))) if name == "files")
        );
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: Some(egui::Key::Escape),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                assert!(matches!(picker.show(ui.ctx(), 24.0), Some(Action::Cancel)));
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn command_picker_preserves_source_order_with_a_query() {
        let ctx = egui::Context::default();
        let mut picker = Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            crate::picker::config::Options {
                sort: crate::picker::config::Sort::SourceOrder,
                source: crate::picker::config::Source::Command(
                    crate::picker::config::CommandOptions {
                        args: if cfg!(windows) {
                            vec![
                                "cmd.exe".into(),
                                "/d".into(),
                                "/c".into(),
                                "echo xxa&& echo a".into(),
                            ]
                        } else {
                            vec!["sh".into(), "-c".into(), "printf 'xxa\\na\\n'".into()]
                        },
                        cwd: None,
                        line_continuation: None,
                        unique: false,
                        success_exit_codes: None,
                    },
                ),
                params: [("query".into(), "a".into())].into(),
                ..Default::default()
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                picker.show(ui.ctx(), 24.0);
            });
            output.textures_delta.clear();
            if picker.done {
                break;
            }
            assert!(Instant::now() < deadline, "command results not published");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            picker
                .results
                .iter()
                .map(|r| r.path.trim())
                .collect::<Vec<_>>(),
            ["xxa", "a"]
        );
        assert_eq!(
            picker
                .results
                .iter()
                .map(|r| r.node_index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn ctrl_p_toggles_configured_preview() {
        let ctx = egui::Context::default();
        let mut picker = super::Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            crate::picker::config::Options {
                source: crate::picker::config::Source::Items(crate::picker::config::ItemsOptions {
                    values: Vec::new(),
                    provider: None,
                }),
                preview: Some(crate::picker::config::Preview::Text { text: "x".into() }),
                preview_visible: false,
                ..Default::default()
            },
        );
        assert!(!picker.preview_visible);
        let mut initial = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 0.0);
        });
        initial.textures_delta.clear();
        assert!(picker.presented_once);
        for visible in [true, false] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::P,
                        physical_key: Some(egui::Key::P),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::CTRL,
                    }],
                    ..Default::default()
                },
                |ui| {
                    picker.show(ui.ctx(), 0.0);
                },
            );
            output.textures_delta.clear();
            assert_eq!(picker.preview_visible, visible);
        }
    }

    #[test]
    fn background_picker_remains_visible_without_taking_foreground_focus() {
        let ctx = egui::Context::default();
        let make_picker = || {
            super::Picker::start(
                std::ptr::null_mut(),
                None,
                false,
                0,
                ctx.clone(),
                crate::picker::config::Options {
                    position: crate::picker::config::Position::Bottom,
                    sort: crate::picker::config::Sort::Score,
                    source: crate::picker::config::Source::Items(
                        crate::picker::config::ItemsOptions {
                            values: Vec::new(),
                            provider: None,
                        },
                    ),
                    callback: None,
                    enter_action: crate::picker::config::EnterAction::SendInput,
                    params: Default::default(),
                    preview: None,
                    preview_visible: true,
                },
            )
        };
        let mut background = make_picker();
        let mut foreground = make_picker();
        let main = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0));
        let floating =
            egui::Rect::from_min_size(egui::pos2(200.0, 100.0), egui::vec2(600.0, 400.0));
        for _ in 0..2 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(main),
                    ..Default::default()
                },
                |ui| {
                    background.show_background_in_bounds(ui.ctx(), main);
                    foreground.show_in_bounds(ui.ctx(), floating);
                },
            );
            output.textures_delta.clear();
        }
        for picker in [&background, &foreground] {
            assert!(ctx.memory(|m| m.area_rect(egui::Id::new(("scrollback_picker", picker.session_id)))).is_some());
        }
        let query_id = egui::Id::new(("scrollback_picker_query", foreground.session_id));
        assert!(ctx.memory(|m| m.has_focus(query_id)));
    }

    #[test]
    fn picker_positions_use_the_owner_bounds_and_clamp_cursor() {
        use crate::picker::config::Position;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let bounds = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(600.0, 500.0));
        let size = Some(egui::vec2(560.0, 180.0));
        let anchor =
            |position, row| super::picker_anchor(position, row, 20.0, bounds, screen, size);
        assert_eq!(
            anchor(Position::Top, None),
            (egui::Align2::CENTER_TOP, egui::vec2(-100.0, 110.0))
        );
        assert_eq!(
            anchor(Position::Center, None),
            (egui::Align2::CENTER_CENTER, egui::vec2(-100.0, -50.0))
        );
        assert_eq!(
            anchor(Position::Bottom, None),
            (egui::Align2::CENTER_BOTTOM, egui::vec2(-100.0, -210.0))
        );
        assert_eq!(
            anchor(Position::Cursor, Some(0)),
            (egui::Align2::CENTER_TOP, egui::vec2(-100.0, 130.0))
        );
        assert_eq!(
            anchor(Position::Cursor, Some(14)),
            (egui::Align2::CENTER_TOP, egui::vec2(-100.0, 410.0))
        );
        assert_eq!(
            anchor(Position::Cursor, Some(16)),
            (egui::Align2::CENTER_BOTTOM, egui::vec2(-100.0, -360.0))
        );
        assert_eq!(anchor(Position::Cursor, Some(40)).1.y, -210.0);
        assert_eq!(
            anchor(Position::Cursor, None),
            anchor(Position::Bottom, None)
        );
    }

    #[test]
    fn item_picker_accepts_insert_value_instead_of_display_text() {
        let ctx = egui::Context::default();
        let mut picker = super::Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            crate::picker::config::Options {
                position: crate::picker::config::Position::Bottom,
                sort: crate::picker::config::Sort::Score,
                source: crate::picker::config::Source::Items(crate::picker::config::ItemsOptions {
                    values: vec![crate::picker::protocol::Item {
                        text: "shown".into(),
                        value: "inserted".into(),
                    }],
                    provider: None,
                }),
                callback: None,
                enter_action: crate::picker::config::EnterAction::SendInput,
                params: [("query".to_string(), "sho".to_string())].into(),
                preview: None,
                preview_visible: true,
            },
        );
        assert_eq!(picker.query, "sho");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while picker.results.is_empty() {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                picker.show(ui.ctx(), 24.0);
            });
            output.textures_delta.clear();
            assert!(
                std::time::Instant::now() < deadline,
                "item results were not published"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Copy],
                ..Default::default()
            },
            |ui| assert!(picker.show(ui.ctx(), 24.0).is_none()),
        );
        output.textures_delta.clear();
        assert!(
            output
                .platform_output
                .commands
                .contains(&egui::OutputCommand::CopyText("shown".into()))
        );
        assert_eq!(picker.query, "sho");
        assert!(picker.copied_until.is_some());
        let mut accepted = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| accepted = picker.show(ui.ctx(), 24.0),
        );
        output.textures_delta.clear();
        assert!(matches!(accepted, Some(super::Action::SendInput(text)) if text == "inserted"));
    }

    #[test]
    #[cfg(windows)]
    fn filesystem_picker_streams_and_accepts_without_a_terminal_snapshot() {
        let root =
            std::env::temp_dir().join(format!("ghostty-picker-session-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file with spaces.txt");
        std::fs::write(&file, "").unwrap();
        let ctx = egui::Context::default();
        let mut picker = super::Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            crate::picker::config::Options {
                position: crate::picker::config::Position::Bottom,
                sort: crate::picker::config::Sort::Score,
                source: crate::picker::config::Source::FileSystem(
                    crate::picker::config::FileOptions {
                        roots: vec![root.clone()],
                        max_depth: 1,
                        files_only: true,
                        directories_only: false,
                    },
                ),
                callback: None,
                enter_action: crate::picker::config::EnterAction::SendInput,
                params: Default::default(),
                preview: None,
                preview_visible: true,
            },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                picker.show(ui.ctx(), 24.0);
            });
            output.textures_delta.clear();
            if picker.done && !picker.results.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "filesystem results were not published"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(picker.source.is_filesystem() && picker.generation.is_none());
        picker.selected = picker
            .results
            .iter()
            .position(|result| std::path::Path::new(&result.path) == file.as_path())
            .expect("filesystem picker must include the file");
        let mut accepted = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                if let Some(action) = picker.show(ui.ctx(), 24.0) {
                    accepted = Some(action);
                }
            },
        );
        output.textures_delta.clear();
        assert!(
            matches!(accepted, Some(super::Action::SendInput(text)) if std::path::Path::new(&text) == file.as_path())
        );
        drop(picker);
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn picker_stays_within_floating_pane_bounds() {
        let ctx = egui::Context::default();
        let mut picker = super::Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            crate::picker::config::Options {
                source: crate::picker::config::Source::Items(crate::picker::config::ItemsOptions {
                    values: Vec::new(),
                    provider: None,
                }),
                ..Default::default()
            },
        );
        let bounds = egui::Rect::from_min_size(egui::pos2(200.0, 100.0), egui::vec2(600.0, 400.0));
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    picker.show_in_bounds(ui.ctx(), bounds);
                },
            );
            output.textures_delta.clear();
        }
        let panel = ctx
            .memory(|m| m.area_rect(egui::Id::new(("scrollback_picker", picker.session_id))))
            .unwrap();
        assert!((panel.center().x - bounds.center().x).abs() < 1.0);
        assert!((panel.bottom() - (bounds.bottom() - 10.0)).abs() < 1.0);
        assert!(panel.left() >= bounds.left() && panel.right() <= bounds.right());
    }

    #[test]
    fn position_highlights_preserve_text_and_utf8_boundaries() {
        let font = egui::FontId::monospace(14.0);
        let bold = egui::FontId::new(14.0, egui::FontFamily::Name("bold".into()));
        for (text, query, expected) in [("alpha-beta", "ab", "ab"), ("café", "é", "é")] {
            let job = super::result_layout(text, query, &font, &bold, false);
            assert_eq!(job.text, text);
            let highlighted: String = job
                .sections
                .iter()
                .filter(|section| section.format.color == crate::colors::palette().match_highlight)
                .map(|section| &job.text[section.byte_range.start.0..section.byte_range.end.0])
                .collect();
            assert_eq!(highlighted, expected);
            assert!(
                job.sections
                    .iter()
                    .filter(
                        |section| section.format.color == crate::colors::palette().match_highlight
                    )
                    .all(|section| section.format.font_id == bold)
            );
        }
        let job = super::result_layout("plain", "", &font, &bold, false);
        assert!(
            job.sections
                .iter()
                .all(|section| section.format.color == crate::colors::palette().text)
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
            cursor_row: None,
            position: crate::picker::config::Position::Bottom,
            generation: Some(1),
            from_copy: false,
            typography: crate::picker::font::Typography::default(),
            preview: None,
            preview_visible: true,
            presented_once: false,
            callback: None,
            request_params: Default::default(),
            enter_action: crate::picker::config::EnterAction::Navigate,
            session_id: 123,
            source: PickerSource::test_scrollback(
                store.clone(),
                FuzzySearchSession::new(123, store.clone(), String::new(), tx),
            ),
            updates: Arc::new(std::sync::Mutex::new(None)),
            query: String::new(),
            submitted_query: String::new(),
            results_query: String::new(),
            query_revision: 0,
            results_revision: None,
            results: Vec::new(),
            matched: 0,
            total: 0,
            selected: 0,
            visible_start: 0,
            copied_until: None,
            done: false,
            error: Arc::new(std::sync::Mutex::new(None)),
        };
        let query_id = egui::Id::new(("scrollback_picker_query", 123u64));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 24.0);
        });
        output.textures_delta.clear();
        // The Area keeps its height stable while results stream in.
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 24.0);
        });
        output.textures_delta.clear();
        assert!(ctx.memory(|m| m.has_focus(query_id)));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            picker.show(ui.ctx(), 24.0);
        });
        output.textures_delta.clear();
        let panel = ctx
            .memory(|m| m.area_rect(egui::Id::new(("scrollback_picker", 123u64))))
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
            assert_eq!(
                ctx.memory(|m| m.area_rect(egui::Id::new(("scrollback_picker", 123u64))))
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
        let range = SnapshotCellRange {
            start: SnapshotCell { x: 4, y: 19 },
            end: SnapshotCell { x: 9, y: 19 },
        };
        let mut publisher =
            PublishingStreamingItemStoreWithPayload::new(picker.source.store().clone());
        publisher.add_item(b"chosen", range);
        publisher.complete();
        picker.results = vec![SearchResult {
            node_index: 0,
            score: 1,
            path: "chosen".into(),
        }];
        picker.selected = 0;
        picker.visible_start = 0;
        picker.results_revision = Some(picker.query_revision);
        for enter_action in [
            crate::picker::config::EnterAction::Navigate,
            crate::picker::config::EnterAction::SendInput,
        ] {
            picker.enter_action = enter_action;
            let input = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            };
            let mut accepted = None;
            let mut output = ctx.run_ui(input, |ui| {
                if let Some(action) = picker.show(ui.ctx(), 24.0) {
                    accepted = Some(action);
                }
            });
            output.textures_delta.clear();
            match (enter_action, accepted) {
                (crate::picker::config::EnterAction::Navigate, Some(Action::Accept(cell))) => {
                    assert_eq!(cell, range.start)
                }
                (crate::picker::config::EnterAction::SendInput, Some(Action::SendInput(text))) => {
                    assert_eq!(text, "chosen")
                }
                _ => panic!("configured Enter action was not used"),
            }
        }
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
    fn unique_scrollback_keeps_newest_unicode_match_and_its_cell_range() {
        let regex = regex::Regex::new(r"\S+").unwrap();
        let newest = [
            (
                9,
                TextRow {
                    text: "猫 x ".into(),
                    columns: vec![0, 0, 0, 2, 3, 4],
                    wraps_to_next: true,
                },
            ),
            (
                10,
                TextRow {
                    text: "猫".into(),
                    columns: vec![0, 0, 0],
                    wraps_to_next: false,
                },
            ),
        ];
        let older = [(
            8,
            TextRow {
                text: "猫 X".into(),
                columns: vec![0, 0, 0, 2, 3],
                wraps_to_next: false,
            },
        )];
        let mut seen = Some(std::collections::HashSet::new());
        let items = newest_logical_items(&newest, &regex, &mut seen);
        assert_eq!(
            items.iter().map(|item| item.0.as_str()).collect::<Vec<_>>(),
            ["猫", "x"]
        );
        assert_eq!(items[0].1.start, SnapshotCell { x: 0, y: 10 });
        assert_eq!(items[0].1.end, SnapshotCell { x: 0, y: 10 });
        let older_items = newest_logical_items(&older, &regex, &mut seen);
        assert_eq!(older_items.len(), 1);
        assert_eq!(older_items[0].0, "X");

        let mut seen = None;
        assert_eq!(newest_logical_items(&newest, &regex, &mut seen).len(), 3);
        assert_eq!(newest_logical_items(&older, &regex, &mut seen).len(), 2);
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
        let matches = logical_items(&rows, &regex::Regex::new("界f|oo").unwrap());
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].0, "界f");
        assert_eq!(matches[0].1.start, SnapshotCell { x: 2, y: 3 });
        assert_eq!(matches[0].1.end, SnapshotCell { x: 0, y: 4 });
        assert_eq!(matches[1].0, "oo");
        assert_eq!(matches[1].1.start, SnapshotCell { x: 1, y: 4 });
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

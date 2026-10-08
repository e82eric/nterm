pub(crate) mod command;
pub(crate) mod config;
pub(crate) mod files;
pub(crate) mod font;
pub(crate) mod pipe;
pub(crate) mod preview;
#[cfg(test)]
mod shared_tests;
mod source;
pub(crate) mod wire;

use crate::ghostty_ffi::{self, Host, SnapshotCell, SnapshotCellRange};
use nfm_search_core::search::SearchResult;
use source::PickerSource;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

type Items = nfm_search_core::store::StreamingItemSnapshotWithPayload<SnapshotCellRange>;
const RESULT_ROWS: usize = 7;

/// Ghostty owns source readers, terminal snapshots and Lua callbacks.
/// The shared NFM control owns query input, streaming search and picker drawing.
pub(crate) struct Picker {
    context: egui::Context,
    keys: Vec<config::PickerKey>,
    shared: nfm_egui::Picker<source::Snapshot>,
    pub(crate) cursor_row: Option<u32>,
    position: config::Position,
    pub(crate) generation: Option<u64>,
    pub(crate) from_copy: bool,
    pub(crate) typography: font::Typography,
    command_preview: Option<preview::CommandPreview>,
    snapshot_preview: Option<Arc<preview::SnapshotPreview>>,
    callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    request_params: std::collections::BTreeMap<String, String>,
    enter_action: config::EnterAction,
    source: PickerSource,
    split_parent: Option<(PickerSource, String)>,
    split_parent_presentation: Option<(config::Position, usize, usize, bool)>,
    pub(crate) builtin_pickers: config::BuiltinPickers,
    result_rows: usize,
    preview_rows: usize,
    line_split: bool,
    host: usize,
    snapshot_rows: u64,
    snapshot_columns: usize,
    #[cfg(test)]
    session_id: u64,
    error: Arc<std::sync::Mutex<Option<String>>>,
    pipe_session: Option<Arc<pipe::Session>>,
    pipe_items: Vec<pipe::Item>,
    pipe_publisher:
        Option<nfm_search_core::store::PublishingStreamingItemStoreWithPayload<SnapshotCellRange>>,
}

pub(crate) enum Action {
    Execute(crate::actions::AppAction),
    Custom(
        std::rc::Rc<crate::lua_callbacks::Callback>,
        String,
        bool,
        Option<SnapshotCellRange>,
        String,
        std::collections::BTreeMap<String, String>,
    ),
    Cancel,
    Navigate(SnapshotCell),
    SendInput(String),
}
impl Picker {
    pub(crate) fn is_closed(&self) -> bool {
        self.shared.is_closed()
    }
    pub(crate) fn surrender_focus(&self) {
        self.context.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        self.context.request_repaint();
    }
    pub(crate) fn attach_pipe(
        &mut self,
        ctx: &egui::Context,
        session: Arc<pipe::Session>,
        stream: Option<&pipe::StreamOptions>,
    ) {
        self.pipe_session = Some(session.clone());
        if let Some(options) = stream {
            self.pipe_publisher = Some(self.source.begin_pipe_stream());
            self.shared
                .replace_source(ctx, self.source.provider(), &options.initial_query);
            self.result_rows = options.result_rows.unwrap_or(7);
            self.preview_rows = options.preview_rows.unwrap_or(14);
            self.shared.set_presentation(
                self.result_rows,
                self.preview_rows,
                options.preview_visible,
            );
        }
    }
    pub(crate) fn bind_pipe_search_wake(
        &self,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
        window_id: egui_winit::winit::window::WindowId,
    ) {
        let Some(session) = self.pipe_session.clone() else {
            return;
        };
        self.shared
            .set_search_wake_callback(Arc::new(move |timings| {
                if session.done() {
                    return;
                }
                if timings.first_matches_ready.is_some() {
                    let _ = proxy.send_event(crate::app::UserEvent::PickerWake { window_id });
                }
            }));
    }
    pub(crate) fn pipe_event(&mut self, ctx: &egui::Context, event: pipe::Event) {
        let Some(session) = &self.pipe_session else {
            return;
        };
        match event {
            pipe::Event::Items {
                sequence,
                items,
                session: sender,
            } if Arc::ptr_eq(session, &sender) && !sender.done() => {
                if let Some(publisher) = &mut self.pipe_publisher {
                    for item in items {
                        publisher.add_item(item.text.as_bytes(), SnapshotCellRange::default());
                        self.pipe_items.push(item);
                    }
                    publisher.publish();
                    sender.reply(serde_json::json!({"type":"ack","request_id":sender.id,"sequence":sequence}));
                    ctx.request_repaint();
                }
            }
            pipe::Event::End { session: sender }
                if Arc::ptr_eq(session, &sender) && !sender.done() =>
            {
                if let Some(publisher) = &mut self.pipe_publisher {
                    publisher.complete();
                }
                sender.reply(serde_json::json!({"type":"input_complete","request_id":sender.id}));
                ctx.request_repaint();
            }
            _ => {}
        }
    }
    pub(crate) fn pipe_cancelled(&self) -> bool {
        self.pipe_session
            .as_ref()
            .is_some_and(|s| s.cancelled.load(Ordering::Acquire))
    }
    pub(crate) fn is_command_palette(&self) -> bool {
        self.source.is_command_palette()
    }
    pub(crate) fn label(&self) -> &'static str {
        if self.line_split {
            "LINE SPLIT PICKER"
        } else {
            self.source.label()
        }
    }
    pub(crate) fn start(
        host: *mut Host,
        generation: Option<u64>,
        from_copy: bool,
        rows: u64,
        ctx: egui::Context,
        mut options: config::Options,
    ) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let session_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let initial_query = options
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
        let (source, error) =
            PickerSource::start(host, generation, rows, ctx.clone(), &mut options);
        let preview_rows = options
            .preview
            .as_ref()
            .map_or(12, |preview| match preview {
                config::Preview::Scrollback { rows } => *rows,
                _ => 12,
            });
        let mut shared = nfm_egui::Picker::new(
            ("ghostty-picker", session_id),
            &ctx,
            source.provider(),
            nfm_egui::PickerConfig {
                title: source.label().into(),
                initial_query,
                result_rows: RESULT_ROWS,
                accept_on_click: matches!(source.label(), "GHOSTTY THEMES" | "ERROR HISTORY"),
                preview_rows: options.preview.as_ref().map_or(12, |p| match p {
                    config::Preview::Scrollback { rows } => *rows,
                    _ => 12,
                }),
                preview_visible: options.preview_visible,
                sort: options.sort.mode(),
                ..Default::default()
            },
        );
        let snapshot_preview =
            if matches!(options.preview, Some(config::Preview::Scrollback { .. })) {
                let reader = Arc::new(preview::SnapshotPreview::new(host, generation));
                let provider = preview::scrollback_provider(reader.clone(), rows, snapshot_columns);
                shared.set_preview_provider(&ctx, Arc::new(provider));
                Some(reader)
            } else {
                None
            };
        let complete_preview = match &options.preview {
            Some(config::Preview::Text { text }) => Some((text.clone(), true)),
            Some(config::Preview::Ansi { text }) => Some((text.clone(), false)),
            _ => None,
        };
        let nfm_preview = snapshot_preview.is_some() || complete_preview.is_some();
        if let Some((text, plain)) = complete_preview {
            let value = source.preview_value_resolver();
            shared.set_preview_provider(
                &ctx,
                Arc::new(nfm_egui::byte_preview::BytePreviewProvider::new(
                    move |selection, _| {
                        let text = text.replace("{item}", &value(selection));
                        let text = if plain {
                            text.chars()
                                .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
                                .collect::<String>()
                        } else {
                            text
                        };
                        Ok(nfm_egui::byte_preview::ByteDocument::complete(
                            text.into_bytes(),
                        ))
                    },
                )),
            );
        }
        shared.set_external_preview(options.preview.is_some() && !nfm_preview);
        let mut help = Vec::new();
        if source.is_scrollback() {
            help.push(("Ctrl+B".into(), "Split selected line".into()));
        }
        help.extend(options.keys.iter().map(|binding| {
            (
                binding.trigger.label(binding.mods),
                binding.description.clone(),
            )
        }));
        shared.set_keybinding_help(help);
        let command_preview = match options.preview {
            Some(source @ config::Preview::Command { .. }) => {
                Some(preview::CommandPreview::new(source))
            }
            _ => None,
        };
        Self {
            context: ctx.clone(),
            keys: options.keys,
            pipe_session: None,
            pipe_items: Vec::new(),
            pipe_publisher: None,
            shared,
            cursor_row: None,
            position: options.position,
            generation,
            from_copy,
            typography: font::Typography::default(),
            snapshot_preview,
            command_preview,
            callback: options.callback,
            request_params: options.params,
            enter_action: options.enter_action,
            source,
            split_parent: None,
            split_parent_presentation: None,
            builtin_pickers: config::BuiltinPickers::default(),
            result_rows: RESULT_ROWS,
            preview_rows,
            line_split: false,
            host: host as usize,
            snapshot_rows: rows,
            snapshot_columns,
            #[cfg(test)]
            session_id,
            error,
        }
    }
    pub(crate) fn set_cursor_scroll_padding(&mut self, rows: u32) {
        self.shared.set_cursor_scroll_padding(rows);
    }

    fn appearance(&self, ctx: &egui::Context) -> nfm_egui::Appearance {
        let p = crate::colors::palette();
        nfm_egui::Appearance {
            palette: nfm_egui::Palette {
                background: p.panel_background,
                text: p.text,
                muted: p.muted_text,
                selection_background: p.selection_background,
                selection_text: p.selection_text,
                match_highlight: p.picker_match_highlight.unwrap_or(p.match_highlight),
                divider: p.border,
                error: p.error,
                accent: p.accent,
                copy_cursor_background: p.copy_cursor_background,
                success: p.success,
                search_match_background: Some(p.search_match_background),
                search_match_text: Some(p.search_match_text),
                search_active_background: Some(p.match_highlight),
                search_active_text: Some(p.background),
                yank_background: Some(p.yank_background),
                yank_text: Some(p.yank_text),
            },
            typography: nfm_egui::Typography {
                normal: self.typography.font.clone(),
                bold: self.typography.matched_font.clone(),
                row_height: self.typography.row_height,
                cell_width: ctx
                    .fonts_mut(|fonts| fonts.glyph_width(&self.typography.font, 'M'))
                    .max(1.0),
            },
        }
    }
    fn placement(&self, bounds: egui::Rect) -> nfm_egui::Placement {
        let mut placement = nfm_egui::Placement::new(bounds);
        placement.position = match self.position {
            config::Position::Top => nfm_egui::Position::Top,
            config::Position::Center => nfm_egui::Position::Center,
            config::Position::Bottom => nfm_egui::Position::Bottom,
            config::Position::Cursor if self.cursor_row.is_some() => nfm_egui::Position::Cursor,
            config::Position::Cursor => nfm_egui::Position::Bottom,
        };
        placement.anchor = self.cursor_row.map(|row| {
            egui::pos2(
                bounds.center().x,
                bounds.top() + (row as f32 + 1.0) * self.typography.row_height,
            )
        });
        placement
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
    fn selected_range(&self, candidate: &source::Candidate) -> Option<SnapshotCellRange> {
        if self.source.is_scrollback() {
            candidate.range
        } else {
            None
        }
    }
    fn show_in_bounds_mode(
        &mut self,
        ctx: &egui::Context,
        bounds: egui::Rect,
        interactive: bool,
    ) -> Option<Action> {
        if let Some(session) = &self.pipe_session {
            if let Some(message) = self.error.lock().unwrap().clone() {
                session.finish(
                    "error",
                    serde_json::json!({"code":"source_failed","message":message}),
                );
                return Some(Action::Cancel);
            }
        }
        let appearance = self.appearance(ctx);
        self.shared.set_bindings(if self.pipe_session.is_none() {
            self.keys
                .iter()
                .enumerate()
                .map(|(action, binding)| nfm_egui::PickerBinding {
                    trigger: match binding.trigger {
                        config::BindingTrigger::Key(key) => nfm_egui::PickerTrigger::Key(key),
                        config::BindingTrigger::DoubleClick => nfm_egui::PickerTrigger::DoubleClick,
                    },
                    modifiers: binding.mods,
                    action,
                })
                .collect()
        } else {
            Vec::new()
        });
        if interactive && !self.shared.keybinding_help_visible() && self.source.is_scrollback() {
            if self.split_parent.is_some()
                && ctx.input_mut(|input| {
                    input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft)
                })
            {
                let (source, query) = self.split_parent.take().expect("line split parent");
                self.shared.replace_source(ctx, source.provider(), query);
                self.source = source;
                self.line_split = false;
                if let Some((position, results, previews, visible)) =
                    self.split_parent_presentation.take()
                {
                    self.position = position;
                    self.result_rows = results;
                    self.preview_rows = previews;
                    self.shared.set_presentation(results, previews, visible);
                }
                self.refresh_scrollback_preview(ctx);
                self.set_keybinding_help(vec![("Ctrl+B".into(), "Split selected line".into())]);
            } else if !self.line_split
                && self.pipe_session.is_none()
                && ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::B))
            {
                if let Some(selection) = self.shared.selection() {
                    if let (Some(generation), Some(range)) = (self.generation, selection.item.range)
                    {
                        match PickerSource::split_line(
                            self.host as *mut Host,
                            generation,
                            range.start.y,
                            self.snapshot_rows,
                            self.snapshot_columns,
                        ) {
                            Ok(source) => self.enter_line_split(ctx, source),
                            Err(error) => *self.error.lock().unwrap() = Some(error),
                        }
                    }
                }
            }
        }
        self.shared.set_error(self.error.lock().unwrap().clone());
        let output = self
            .shared
            .show(ctx, self.placement(bounds), &appearance, !interactive);
        let selection = output.selection();
        if let Some(preview) = &mut self.command_preview {
            if output.preview_rect.is_some() && !self.shared.is_closed() {
                if let Some(selection) = selection {
                    let value = self
                        .pipe_items
                        .get(selection.item.node_index)
                        .map(|item| {
                            item.value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| item.value.to_string())
                        })
                        .unwrap_or_else(|| {
                            self.source.accepted_value(&SearchResult {
                                node_index: selection.item.node_index,
                                score: 0,
                                path: selection.text.clone(),
                            })
                        });
                    preview.prepare(
                        ctx,
                        &mut self.shared,
                        selection,
                        &value,
                        self.source.is_filesystem(),
                        &self.request_params,
                    );
                }
            }
        }
        match output.event {
            Some(nfm_egui::PickerEvent::Cancelled) => Some(Action::Cancel),
            Some(nfm_egui::PickerEvent::CopyRequested(selection)) => {
                ctx.copy_text(selection.text);
                self.shared.notify_copied(ctx);
                None
            }
            Some(nfm_egui::PickerEvent::Accepted(selection)) => self.accept(selection),
            Some(nfm_egui::PickerEvent::CustomAction { action, selection }) => self
                .keys
                .get(action)
                .and_then(|binding| self.invoke_callback(binding.action.clone(), selection)),
            None => None,
        }
    }
    fn invoke_callback(
        &self,
        callback: std::rc::Rc<crate::lua_callbacks::Callback>,
        selection: nfm_egui::Selection<source::Candidate>,
    ) -> Option<Action> {
        let result = SearchResult {
            node_index: selection.item.node_index,
            score: 0,
            path: selection.text.clone(),
        };
        Some(Action::Custom(
            callback,
            selection.text,
            self.source.is_filesystem(),
            self.selected_range(&selection.item),
            self.source.accepted_value(&result),
            self.request_params.clone(),
        ))
    }
    fn accept(&self, selection: nfm_egui::Selection<source::Candidate>) -> Option<Action> {
        if let Some(session) = &self.pipe_session {
            let index = selection.item.node_index;
            let value = self
                .pipe_items
                .get(index)
                .map(|item| item.value.clone())
                .unwrap_or_else(|| {
                    serde_json::Value::String(self.source.accepted_value(&SearchResult {
                        node_index: index,
                        score: 0,
                        path: selection.text.clone(),
                    }))
                });
            let id = self
                .pipe_items
                .get(index)
                .map(|item| item.id.clone())
                .unwrap_or_else(|| index.to_string());
            session.finish("accepted",serde_json::json!({"selected":[{"id":id,"text":selection.text,"value":value}],"query":self.shared.query()}));
            return Some(Action::Cancel);
        }
        let result = SearchResult {
            node_index: selection.item.node_index,
            score: 0,
            path: selection.text,
        };
        if let Some(action) = self.source.application_action(&result) {
            return Some(Action::Execute(action));
        }
        let value = self.source.accepted_value(&result);
        let range = self.selected_range(&selection.item);
        if let Some(callback) = &self.callback
            && !self.line_split
        {
            return Some(Action::Custom(
                callback.clone(),
                result.path,
                self.source.is_filesystem(),
                range,
                value,
                self.request_params.clone(),
            ));
        }
        match if self.line_split {
            config::EnterAction::SendInput
        } else {
            self.enter_action
        } {
            config::EnterAction::Navigate => range.map(|r| Action::Navigate(r.start)),
            config::EnterAction::SendInput => Some(Action::SendInput(value)),
        }
    }
    fn enter_line_split(&mut self, ctx: &egui::Context, source: PickerSource) {
        self.split_parent_presentation = Some((
            self.position,
            self.result_rows,
            self.preview_rows,
            self.shared.preview_visible(),
        ));
        let query = self.shared.query().to_owned();
        self.shared.replace_source(ctx, source.provider(), "");
        let parent = std::mem::replace(&mut self.source, source);
        self.split_parent = Some((parent, query));
        self.line_split = true;
        self.apply_line_break_presentation();
        self.refresh_scrollback_preview(ctx);
        self.set_keybinding_help(vec![(
            "Alt+Left".into(),
            "Return to scrollback results".into(),
        )]);
    }
    fn set_keybinding_help(&mut self, mut help: Vec<(String, String)>) {
        help.extend(self.keys.iter().map(|binding| {
            (
                binding.trigger.label(binding.mods),
                binding.description.clone(),
            )
        }));
        self.shared.set_keybinding_help(help);
    }
    fn apply_line_break_presentation(&mut self) {
        let settings = self.builtin_pickers.line_break;
        // Built-in defaults are independent of the parent scrollback picker.
        self.position = settings.position.unwrap_or(config::Position::Bottom);
        self.result_rows = settings.result_rows.unwrap_or(RESULT_ROWS);
        self.preview_rows = settings.preview_rows.unwrap_or(12);
        self.shared.set_presentation(
            self.result_rows,
            self.preview_rows,
            settings.preview_visible.unwrap_or(true),
        );
    }
    fn refresh_scrollback_preview(&mut self, ctx: &egui::Context) {
        let reader = self.snapshot_preview.get_or_insert_with(|| {
            Arc::new(preview::SnapshotPreview::new(
                self.host as *mut Host,
                self.generation,
            ))
        });
        let provider =
            preview::scrollback_provider(reader.clone(), self.snapshot_rows, self.snapshot_columns);
        self.shared.set_preview_provider(ctx, Arc::new(provider));
        self.shared.set_external_preview(false);
    }

    pub(crate) fn start_line_split(
        host: *mut Host,
        generation: u64,
        view: ghostty_ffi::SnapshotView,
        row: u64,
        ctx: egui::Context,
        builtin_pickers: config::BuiltinPickers,
    ) -> Result<Self, String> {
        let source = PickerSource::split_line(
            host,
            generation,
            row,
            view.total_rows,
            view.columns as usize,
        )?;
        // An empty source avoids starting a whole-scrollback scan. The completed
        // split source uses the same frozen snapshot and preview lifetime.
        let mut picker = Self::start(
            host,
            Some(generation),
            true,
            view.total_rows,
            ctx.clone(),
            config::Options {
                keys: Vec::new(),
                source: config::Source::CommandPalette(vec![]),
                preview: Some(config::Preview::Scrollback { rows: 12 }),
                enter_action: config::EnterAction::SendInput,
                ..Default::default()
            },
        );
        picker.shared.replace_source(&ctx, source.provider(), "");
        picker.source = source;
        picker.line_split = true;
        picker.builtin_pickers = builtin_pickers;
        picker.apply_line_break_presentation();
        Ok(picker)
    }
}

impl Drop for Picker {
    fn drop(&mut self) {
        if let Some(session) = &self.pipe_session {
            session.finish("cancelled", serde_json::json!({"reason":"picker_closed"}));
        }
        if let Some(reader) = &self.snapshot_preview {
            reader.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfm_egui::copy_mode::TextRow;
    use nfm_search_core::{
        fuzzy_search_session::FuzzySearchSession, snapshot_store::SnapshotStore,
        store::PublishingStreamingItemStoreWithPayload,
    };
    use source::{logical_item, logical_items, newest_logical_items};
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

use super::{Items, command, config};
use crate::ghostty_ffi::{self, Host, SnapshotCell, SnapshotCellRange};
use nfm_egui::copy_mode::TextRow;
use nfm_picker_sources::structured::{
    StructuredSchema, StructuredStreamingSnapshot, StructuredStreamingStore,
};
use nfm_search_core::fuzzy_search_session::SearchSnapshotProvider;
use nfm_search_core::search::SearchResult;
use nfm_search_core::snapshot_store::SnapshotStore;
use nfm_search_core::store::ItemsSource;
use nfm_search_core::store::PublishingStreamingItemStoreWithPayload;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

type DocumentItems = nfm_egui::styled_document::DocumentItems<SnapshotCellRange>;
fn complete_document_source(store: &SnapshotStore<DocumentItems>) {
    if store.snapshot().is_none() {
        let document = nfm_egui::styled_document::DocumentBuilder::default().finish();
        store.publish(nfm_egui::styled_document::DocumentItemStore::new(document).snapshot());
    }
    store.complete();
}
enum Session {
    Streaming,
    Document {
        store: Arc<SnapshotStore<nfm_egui::styled_document::DocumentItems<SnapshotCellRange>>>,
    },
    Structured {
        snapshot: Arc<StructuredStreamingSnapshot>,
    },
    /// A command with `columns`, streamed into structured rows.
    StructuredCommand {
        store: Arc<SnapshotStore<StructuredStreamingSnapshot>>,
    },
    FileSystem {
        store: Arc<nfm_file_system::walker_search_store::FileSystemSearchStore>,
        scan: nfm_file_system::walker::FileWalkerScan,
    },
}
impl Session {
    fn stop(&self) {
        if let Self::FileSystem { scan, .. } = self {
            scan.cancel();
        }
    }
}

pub(super) struct Runtime {
    session: Session,
    store: Arc<SnapshotStore<Items>>,
    cancelled: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    document: Arc<std::sync::OnceLock<Arc<nfm_egui::styled_document::StyledDocument>>>,
    logical_origin: ghostty_ffi::SnapshotLogicalPosition,
}

pub(super) enum PickerSource {
    CommandPalette {
        runtime: Runtime,
        entries: Vec<crate::actions::Entry>,
    },
    FileSystem(Runtime),
    Scrollback(Runtime),
    Items {
        runtime: Runtime,
        values: Vec<String>,
    },
    Command {
        runtime: Runtime,
        values: Option<command::StreamValues>,
    },
}

impl PickerSource {
    pub(super) fn document(
        &self,
    ) -> Arc<std::sync::OnceLock<Arc<nfm_egui::styled_document::StyledDocument>>> {
        self.runtime().document.clone()
    }
    pub(super) fn logical_range(
        &self,
        range: SnapshotCellRange,
    ) -> Option<ghostty_ffi::SnapshotLogicalRange> {
        let runtime = self.runtime();
        let document = runtime.document.get()?;
        let convert = |cell: SnapshotCell| {
            let position = document.source_position(cell.y as usize, cell.x)?;
            Some(ghostty_ffi::SnapshotLogicalPosition {
                line: runtime
                    .logical_origin
                    .line
                    .checked_add(position.line as u64)?,
                byte: (position.byte as u64).checked_add(if position.line == 0 {
                    runtime.logical_origin.byte
                } else {
                    0
                })?,
            })
        };
        Some(ghostty_ffi::SnapshotLogicalRange {
            start: convert(range.start)?,
            end: convert(range.end)?,
        })
    }
    pub(super) fn begin_pipe_stream(
        &mut self,
    ) -> PublishingStreamingItemStoreWithPayload<SnapshotCellRange> {
        let Self::Items { runtime, .. } = self else {
            panic!("pipe stream requires Items source");
        };
        runtime.store = Arc::new(SnapshotStore::new_append_only());
        PublishingStreamingItemStoreWithPayload::new(runtime.store.clone())
    }
    pub(super) fn split_line(
        host: *mut Host,
        generation: u64,
        row: u64,
        total_rows: u64,
        columns: usize,
    ) -> Result<Self, String> {
        let start = row.saturating_sub(10);
        let end = row.saturating_add(11).min(total_rows);
        let mut lines = Vec::new();
        let mut document = nfm_egui::styled_document::DocumentBuilder::default()
            .with_source_origin(start as usize);
        let mut origin = ghostty_ffi::SnapshotLogicalPosition::default();
        if !unsafe {
            ghostty_ffi::nterm_host_snapshot_logical_position(
                host,
                generation,
                SnapshotCell { x: 0, y: start },
                &mut origin,
            )
        } {
            return Err("Unable to locate line-split document".into());
        }
        for y in start..end {
            let (text, styles, _) =
                read_styled_row(host, generation, y).ok_or("Unable to read line-split snapshot")?;
            document.push_row(&text.text, &text.columns, text.wraps_to_next, &styles)?;
            lines.push(split_line_cells(y, &text, columns));
        }
        let target = usize::try_from(row - start).map_err(|_| "Invalid line-split row")?;
        let mut source =
            Self::split_candidates(nfm_picker_sources::line_split::split(&lines, target, &[]));
        let _ = source.runtime().document.set(document.finish());
        if let Self::Scrollback(runtime) = &mut source {
            runtime.logical_origin = origin;
        }
        Ok(source)
    }

    pub(super) fn split_candidates(
        candidates: Vec<nfm_picker_sources::line_split::Candidate>,
    ) -> Self {
        let store = Arc::new(SnapshotStore::new());
        let mut publisher = PublishingStreamingItemStoreWithPayload::new(store.clone());
        for candidate in candidates {
            let range = SnapshotCellRange {
                start: SnapshotCell {
                    x: candidate.columns.start as u32,
                    y: candidate.row,
                },
                end: SnapshotCell {
                    x: candidate.columns.end.saturating_sub(1) as u32,
                    y: candidate.row,
                },
            };
            publisher.add_item(candidate.text.as_bytes(), range);
        }
        publisher.complete();
        Self::Scrollback(Runtime {
            session: Session::Streaming,
            store,
            cancelled: Arc::new(AtomicBool::new(false)),
            reader: None,
            document: Default::default(),
            logical_origin: Default::default(),
        })
    }
    pub(super) fn start(
        host: *mut Host,
        generation: Option<u64>,
        rows: u64,
        ctx: egui::Context,
        options: &mut config::Options,
        prompt_patterns: &crate::config::prompts::Patterns,
    ) -> (Self, Arc<std::sync::Mutex<Option<String>>>) {
        let store = Arc::new(
            if matches!(
                &options.source,
                config::Source::Command(_) | config::Source::TextFile(_)
            ) {
                SnapshotStore::new_append_only()
            } else {
                SnapshotStore::new()
            },
        );
        let error = Arc::new(std::sync::Mutex::new(None));
        if let crate::picker::config::Source::Items(items) = &mut options.source {
            if let Some(provider) = &items.provider {
                match provider.read() {
                    Ok(values) => items.values = values,
                    Err(message) => {
                        crate::error_log::error(
                            "picker provider",
                            "Unable to load items",
                            &message,
                        );
                        *error.lock().unwrap() = Some(message);
                    }
                }
            }
        }
        let item_values = if let crate::picker::config::Source::Items(items) = &options.source {
            items.values.iter().map(|item| item.value.clone()).collect()
        } else {
            Vec::new()
        };
        let command_values = match &options.source {
            crate::picker::config::Source::Command(config)
                if config.line_continuation.is_some() || config.columns.is_some() =>
            {
                Some(Arc::default())
            }
            crate::picker::config::Source::TextFile(config)
                if config.line_continuation.is_some() =>
            {
                Some(Arc::default())
            }
            _ => None,
        };
        if let crate::picker::config::Source::FileSystem(files) = &mut options.source {
            if let Err(message) = crate::picker::files::resolve_roots(files) {
                crate::error_log::error("picker filesystem", "Unable to resolve roots", &message);
                *error.lock().unwrap() = Some(message);
            }
        }
        let session = match &options.source {
            crate::picker::config::Source::FileSystem(files) => {
                let shared =
                    Arc::new(nfm_file_system::walker_search_store::FileSystemSearchStore::new());
                let scan = nfm_file_system::walker::start_scan(
                    nfm_file_system::walker::ScanOptions {
                        roots: files.roots.clone(),
                        max_depth: files.max_depth,
                        files_only: files.files_only,
                        directories_only: files.directories_only,
                        relative: files.relative,
                    },
                    shared.clone(),
                );
                Session::FileSystem {
                    store: shared,
                    scan,
                }
            }
            crate::picker::config::Source::Themes | crate::picker::config::Source::ErrorLog => {
                unreachable!("theme source must be resolved before starting")
            }
            crate::picker::config::Source::CommandPalette(entries)
            | crate::picker::config::Source::Workspaces(entries) => {
                let columns = if matches!(options.source, config::Source::Workspaces(_)) {
                    vec!["Workspace".into(), "Layout / panes".into()]
                } else {
                    vec!["Action".into(), "Shortcut".into()]
                };
                let schema = StructuredSchema::new(columns).expect("valid command palette schema");
                let mut structured = StructuredStreamingStore::new(schema);
                for entry in entries {
                    structured
                        .add_record(&csv::StringRecord::from(vec![
                            entry.label.as_str(),
                            entry.shortcut.as_deref().unwrap_or_default(),
                        ]))
                        .expect("palette row matches schema");
                }
                let snapshot = structured.snapshot();
                Session::Structured { snapshot }
            }
            crate::picker::config::Source::Scrollback { .. } => Session::Document {
                store: Arc::new(SnapshotStore::new_append_only()),
            },
            crate::picker::config::Source::Command(config) if config.columns.is_some() => {
                Session::StructuredCommand {
                    store: Arc::new(SnapshotStore::new()),
                }
            }
            crate::picker::config::Source::Items(_)
            | crate::picker::config::Source::Command(_)
            | crate::picker::config::Source::TextFile(_) => Session::Streaming,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let document = Arc::new(std::sync::OnceLock::new());
        let reader = if let (
            crate::picker::config::Source::Command(config),
            Session::StructuredCommand { store: rows },
        ) = (&options.source, &session)
        {
            crate::picker::command::start_structured(
                config.clone(),
                config
                    .columns
                    .clone()
                    .expect("structured command has columns"),
                options.params.clone(),
                rows.clone(),
                command_values
                    .clone()
                    .expect("structured command records values"),
                cancelled.clone(),
                error.clone(),
                ctx.clone(),
            )
        } else if let crate::picker::config::Source::Command(config) = &options.source {
            crate::picker::command::start(
                config.clone(),
                options.params.clone(),
                store.clone(),
                cancelled.clone(),
                error.clone(),
                ctx.clone(),
                command_values.clone(),
            )
        } else if let crate::picker::config::Source::TextFile(config) = &options.source {
            crate::picker::command::start_file(
                config.clone(),
                &options.params,
                store.clone(),
                cancelled.clone(),
                error.clone(),
                ctx.clone(),
                command_values.clone(),
            )
        } else if let crate::picker::config::Source::Scrollback {
            regex,
            unique,
            prompts,
        } = &options.source
        {
            let Session::Document {
                store: document_items,
            } = &session
            else {
                unreachable!()
            };
            let document_items = document_items.clone();
            let regex = regex.clone();
            let prompts = prompts.then(|| prompt_patterns.clone());
            let mut seen = unique.then(std::collections::HashSet::new);
            let generation = generation.expect("scrollback picker requires a snapshot");
            let (cancelled, error) = (cancelled.clone(), error.clone());
            let host = host as usize;
            let failure_error = error.clone();
            let failure_ctx = ctx.clone();
            let failure_store = document_items.clone();
            let loaded_document = document.clone();
            crate::worker::spawn(
                "picker scrollback worker",
                move || {
                    // Export the document once. Subsequent search and preview
                    // reads use Rust storage without holding terminal locks.
                    let mut builder = nfm_egui::styled_document::DocumentBuilder::default();
                    // OSC 133 marks for each row, kept for listing prompts.
                    let mut row_prompts = Vec::new();
                    for row in 0..rows {
                        if cancelled.load(Ordering::Acquire) {
                            complete_document_source(&document_items);
                            return;
                        }
                        let captured = read_styled_row(host as *mut Host, generation, row)
                            .ok_or_else(|| "Unable to read scrollback snapshot".to_owned())
                            .and_then(|(text, styles, prompt)| {
                                if prompts.is_some() {
                                    row_prompts.push(prompt);
                                }
                                builder.push_row(
                                    &text.text,
                                    &text.columns,
                                    text.wraps_to_next,
                                    &styles,
                                )
                            });
                        if let Err(message) = captured {
                            *error.lock().unwrap() = Some(message);
                            complete_document_source(&document_items);
                            ctx.request_repaint();
                            return;
                        }
                    }
                    let document = builder.finish();
                    let _ = loaded_document.set(document.clone());
                    let mut publisher =
                        nfm_egui::styled_document::DocumentItemStore::new(document.clone());
                    let read = |row| {
                        if cancelled.load(Ordering::Acquire) {
                            return None;
                        }
                        let result = document.source_row(row as usize);
                        if result.is_none() && error.lock().unwrap().is_none() {
                            crate::error_log::error(
                                "picker",
                                "Unable to read scrollback snapshot",
                                "The terminal snapshot could not be read",
                            );
                            *error.lock().unwrap() =
                                Some("Unable to read scrollback snapshot".into());
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
                        let items = match &prompts {
                            Some(patterns) => prompt_item(&lines, &row_prompts, patterns)
                                .filter(|(text, _)| {
                                    seen.as_mut().is_none_or(|seen| seen.insert(text.clone()))
                                })
                                .into_iter()
                                .collect(),
                            None => newest_logical_items(&lines, &regex, &mut seen),
                        };
                        for (text, range) in items {
                            if cancelled.load(Ordering::Acquire) {
                                break;
                            }
                            let offset = document
                                .source_byte_offset(range.start.y as usize, range.start.x)
                                .expect("captured candidate position");
                            publisher
                                .add(offset, text.len(), range)
                                .expect("captured candidate range");
                            batch += 1;
                            if batch >= 128 {
                                document_items.publish(publisher.snapshot());
                                batch = 0;
                                ctx.request_repaint();
                            }
                        }
                        bottom = start;
                    }
                    document_items.publish(publisher.snapshot());
                    document_items.complete();
                    ctx.request_repaint();
                },
                move |message| {
                    crate::error_log::error("picker", "Scrollback worker failed", &message);
                    *failure_error
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(message);
                    complete_document_source(&failure_store);
                    failure_ctx.request_repaint();
                },
            )
        } else {
            if let crate::picker::config::Source::Items(items) = &options.source {
                let mut publisher = PublishingStreamingItemStoreWithPayload::new(store.clone());
                for item in &items.values {
                    publisher.add_item(item.text.as_bytes(), SnapshotCellRange::default());
                }
                publisher.complete();
                ctx.request_repaint();
            }
            None
        };

        let runtime = Runtime {
            session,
            store,
            cancelled,
            reader,
            document,
            logical_origin: Default::default(),
        };
        let source = match &options.source {
            config::Source::Themes | crate::picker::config::Source::ErrorLog => {
                unreachable!("theme source must be resolved before starting")
            }
            config::Source::CommandPalette(entries) | config::Source::Workspaces(entries) => {
                Self::CommandPalette {
                    runtime,
                    entries: entries.clone(),
                }
            }
            config::Source::FileSystem(_) => Self::FileSystem(runtime),
            config::Source::Scrollback { .. } => Self::Scrollback(runtime),
            config::Source::Items(_) => Self::Items {
                runtime,
                values: item_values,
            },
            config::Source::Command(_) | config::Source::TextFile(_) => Self::Command {
                runtime,
                values: command_values,
            },
        };
        (source, error)
    }

    pub(super) fn stop_scrollback_reader(&mut self) {
        if let Self::Scrollback(runtime) = self {
            runtime.cancelled.store(true, Ordering::Release);
            if let Some(reader) = runtime.reader.take() {
                let _ = reader.join();
            }
        }
    }

    fn runtime(&self) -> &Runtime {
        match self {
            Self::FileSystem(runtime)
            | Self::Scrollback(runtime)
            | Self::Items { runtime, .. }
            | Self::CommandPalette { runtime, .. }
            | Self::Command { runtime, .. } => runtime,
        }
    }
    pub(super) fn is_scrollback(&self) -> bool {
        matches!(self, Self::Scrollback(_))
    }
    pub(super) fn is_filesystem(&self) -> bool {
        matches!(self, Self::FileSystem(_))
    }
    pub(super) fn is_items(&self) -> bool {
        matches!(self, Self::Items { .. })
    }
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::CommandPalette { entries, .. }
                if entries.first().is_some_and(|entry| {
                    matches!(
                        entry.action,
                        crate::actions::AppAction::SelectWorkspaceId(_)
                    )
                }) =>
            {
                "WORKSPACES"
            }
            Self::CommandPalette { entries, .. }
                if entries.first().is_some_and(|entry| {
                    matches!(
                        entry.action,
                        crate::actions::AppAction::MovePaneToWorkspaceId { .. }
                    )
                }) =>
            {
                "MOVE PANE TO WORKSPACE"
            }
            Self::FileSystem(_) => "FILESYSTEM PICKER",
            Self::Scrollback(_) => "SCROLLBACK PICKER",
            Self::Items { .. } => "ITEMS PICKER",
            Self::Command { .. } => "COMMAND PICKER",
            Self::CommandPalette { entries, .. }
                if entries.first().is_some_and(|entry| {
                    matches!(entry.action, crate::actions::AppAction::ApplyTheme(_))
                }) =>
            {
                "GHOSTTY THEMES"
            }
            Self::CommandPalette { .. } => "COMMAND PALETTE",
        }
    }
    pub(super) fn provider(&self) -> Arc<Provider> {
        Arc::new(match &self.runtime().session {
            Session::Streaming => Provider::Streaming(self.runtime().store.clone()),
            Session::Document { store } => Provider::Document(store.clone()),
            Session::Structured { snapshot } => Provider::Structured(snapshot.clone()),
            Session::StructuredCommand { store } => Provider::StructuredStream(store.clone()),
            Session::FileSystem { store, .. } => Provider::FileSystem(store.clone()),
        })
    }
    pub(super) fn accepted_value(&self, result: &SearchResult) -> String {
        let value = match self {
            Self::CommandPalette { entries, .. } => entries
                .get(result.node_index as usize)
                .map(|entry| entry.id.clone()),
            Self::Items { values, .. } => values.get(result.node_index as usize).cloned(),
            Self::Command {
                values: Some(values),
                ..
            } => values.lock().unwrap().get(&result.node_index).cloned(),
            _ => None,
        };
        value.unwrap_or_else(|| result.path.clone())
    }

    pub(super) fn preview_value_resolver(
        &self,
    ) -> impl Fn(&nfm_egui::Selection<Candidate>) -> String + Send + Sync + 'static {
        let values = match self {
            Self::Items { values, .. } => Some(values.clone()),
            Self::CommandPalette { entries, .. } => {
                Some(entries.iter().map(|e| e.id.clone()).collect())
            }
            _ => None,
        };
        let command_values = match self {
            Self::Command { values, .. } => values.clone(),
            _ => None,
        };
        move |selection| {
            values
                .as_ref()
                .and_then(|v| v.get(selection.item.node_index).cloned())
                .or_else(|| {
                    command_values
                        .as_ref()
                        .and_then(|v| v.lock().unwrap().get(&selection.item.node_index).cloned())
                })
                .unwrap_or_else(|| selection.text.clone())
        }
    }

    pub(super) fn is_command_palette(&self) -> bool {
        matches!(self, Self::CommandPalette { .. })
    }

    pub(super) fn application_action(
        &self,
        result: &SearchResult,
    ) -> Option<crate::actions::AppAction> {
        if let Self::CommandPalette { entries, .. } = self {
            entries
                .get(result.node_index as usize)
                .map(|entry| entry.action.clone())
        } else {
            None
        }
    }
    #[cfg(test)]
    pub(super) fn test_scrollback(store: Arc<SnapshotStore<Items>>) -> Self {
        Self::Scrollback(Runtime {
            store,
            session: Session::Streaming,
            cancelled: Arc::new(AtomicBool::new(false)),
            reader: None,
            document: Default::default(),
            logical_origin: Default::default(),
        })
    }
}

impl Drop for PickerSource {
    fn drop(&mut self) {
        let join_reader = matches!(self, Self::Scrollback(_));
        let runtime = match self {
            Self::FileSystem(runtime)
            | Self::Scrollback(runtime)
            | Self::Items { runtime, .. }
            | Self::CommandPalette { runtime, .. }
            | Self::Command { runtime, .. } => runtime,
        };
        runtime.cancelled.store(true, Ordering::Release);
        runtime.session.stop();
        // Native snapshot pages must outlive their scrollback reader.
        // Command readers access only Rust-owned storage.
        if let Some(reader) = runtime.reader.take() {
            if join_reader {
                let _ = reader.join();
            }
        }
    }
}

pub(super) fn split_line_cells(
    row: u64,
    text: &TextRow,
    columns: usize,
) -> nfm_picker_sources::line_split::Line {
    use unicode_segmentation::UnicodeSegmentation;
    let graphemes = text.text.grapheme_indices(true).collect::<Vec<_>>();
    let cells = graphemes
        .iter()
        .filter_map(|&(byte, grapheme)| {
            let start = *text.columns.get(byte)? as usize;
            // Point-map entries locate grapheme starts, not their ends. A
            // following exported blank or newline can be far to the right.
            // Limit every grapheme to its own display width, including when
            // it is followed by whitespace rather than being last in the text.
            let end = (start + unicode_width::UnicodeWidthStr::width(grapheme).max(1)).min(columns);
            Some(nfm_picker_sources::line_split::Cell {
                text: grapheme.into(),
                columns: start..end.max(start + 1),
            })
        })
        .collect();
    nfm_picker_sources::line_split::Line {
        row,
        cells,
        wraps_to_next: text.wraps_to_next,
    }
}

fn read_styled_row(
    host: *mut Host,
    generation: u64,
    row: u64,
) -> Option<(
    TextRow,
    Vec<nfm_egui::styled_document::ColumnStyleRun>,
    ghostty_ffi::RowPrompt,
)> {
    use nfm_egui::styled_document::{ColumnStyleRun, Style};
    // The native row reader caches its snapshot anchor. A UI-thread line split
    // may read while the original scrollback source is still publishing rows.
    static ROW_READ: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _read = ROW_READ.lock().unwrap_or_else(|error| error.into_inner());
    let data = ghostty_ffi::read_snapshot_row(host, generation, row, true)?;
    let mut text = TextRow {
        text: String::from_utf8(data.text).ok()?,
        columns: data.columns,
        wraps_to_next: data.wraps_to_next,
    };
    trim_trailing_spaces(&mut text);
    let color = |value: u32| {
        (value != u32::MAX)
            .then(|| egui::Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8))
    };
    let styles = data
        .style_runs
        .iter()
        .map(|run| ColumnStyleRun {
            columns: run.start..run.end,
            style: Style {
                foreground: color(run.foreground),
                background: color(run.background),
                underline_color: color(run.underline_color),
                flags: run.flags,
                underline: run.underline != 0,
            },
        })
        .collect();
    Some((text, styles, data.prompt))
}

/// Drops the spaces that end a row which does not wrap into the next one.
/// Programs such as cargo on Windows clear a line by writing spaces up to the
/// terminal width; kept, they wrap a preview narrower than the terminal into
/// a blank second row. `push_row` restores trailing cells that have a
/// background, so only unstyled blanks are lost.
fn trim_trailing_spaces(row: &mut TextRow) {
    if row.wraps_to_next {
        return;
    }
    let len = row.text.trim_end_matches(' ').len();
    row.text.truncate(len);
    row.columns.truncate(len);
}

pub(super) fn logical_items(
    rows: &[(u64, TextRow)],
    regex: &regex::Regex,
) -> Vec<(String, SnapshotCellRange)> {
    let mut text = String::new();
    let mut points = Vec::new();
    for (y, row) in rows {
        if row.text.len() != row.columns.len() {
            return Vec::new();
        }
        text.push_str(&row.text);
        points.extend(row.columns.iter().map(|&x| SnapshotCell { x, y: *y }));
    }
    regex
        .find_iter(&text)
        .filter(|matched| !matched.as_str().trim().is_empty())
        .map(|matched| {
            (
                matched.as_str().to_owned(),
                SnapshotCellRange {
                    start: points[matched.start()],
                    end: points[matched.end() - 1],
                },
            )
        })
        .collect()
}

pub(super) fn newest_logical_items(
    rows: &[(u64, TextRow)],
    regex: &regex::Regex,
    seen: &mut Option<std::collections::HashSet<String>>,
) -> Vec<(String, SnapshotCellRange)> {
    logical_items(rows, regex)
        .into_iter()
        .rev()
        .filter(|(text, _)| seen.as_mut().is_none_or(|seen| seen.insert(text.clone())))
        .collect()
}

/// The command typed at a prompt in a logical line, with its cells. A line is
/// a prompt when OSC 133 marks it or a prompt pattern matches; `row_prompts`
/// holds the marks by snapshot row.
pub(super) fn prompt_item(
    rows: &[(u64, TextRow)],
    row_prompts: &[ghostty_ffi::RowPrompt],
    patterns: &crate::config::prompts::Patterns,
) -> Option<(String, SnapshotCellRange)> {
    let mut text = String::new();
    let mut points = Vec::new();
    for (y, row) in rows {
        if row.text.len() != row.columns.len() {
            return None;
        }
        text.push_str(&row.text);
        points.extend(row.columns.iter().map(|&x| SnapshotCell { x, y: *y }));
    }
    let line_rows: Vec<_> = rows
        .iter()
        .map(|(y, row)| crate::config::prompts::LineRow {
            columns: &row.columns,
            prompt: row_prompts.get(*y as usize).copied().unwrap_or_default(),
        })
        .collect();
    let start = patterns.command_start(&text, &line_rows)?;
    let typed = &text[start..];
    let command = typed.trim();
    if command.is_empty() {
        return None;
    }
    let begin = start + (typed.len() - typed.trim_start().len());
    Some((
        command.to_owned(),
        SnapshotCellRange {
            start: points[begin],
            end: points[begin + command.len() - 1],
        },
    ))
}

#[cfg(test)]
pub(super) fn logical_item(rows: &[(u64, TextRow)]) -> Option<(String, SnapshotCellRange)> {
    let crate::picker::config::Source::Scrollback { regex, .. } =
        crate::picker::config::Options::default().source
    else {
        unreachable!()
    };
    logical_items(rows, &regex).into_iter().next()
}

/// Owned snapshots retain original indices and scrollback cell ranges.
pub(super) enum Snapshot {
    Streaming(Arc<Items>),
    Document(Arc<nfm_egui::styled_document::DocumentItems<SnapshotCellRange>>),
    Structured(Arc<StructuredStreamingSnapshot>),
    FileSystem(Arc<nfm_file_system::walker::PublishedSnapshot>),
}
#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub node_index: usize,
    pub range: Option<SnapshotCellRange>,
}
macro_rules! delegate_snapshot {
    ($self:ident, $method:ident $(, $arg:expr)*) => {
        match $self {
            Snapshot::Streaming(s) => ItemsSource::$method(s.as_ref(), $($arg),*),
            Snapshot::Document(s) => ItemsSource::$method(s.as_ref(), $($arg),*),
            Snapshot::Structured(s) => ItemsSource::$method(s.as_ref(), $($arg),*),
            Snapshot::FileSystem(s) => ItemsSource::$method(s.as_ref(), $($arg),*),
        }
    };
}
impl ItemsSource for Snapshot {
    type Item = Candidate;
    fn version(&self) -> u64 {
        delegate_snapshot!(self, version)
    }
    fn len(&self) -> usize {
        delegate_snapshot!(self, len)
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn get_string<'a>(
        &'a self,
        index: usize,
        stack: &'a mut [u8],
        heap: &'a mut Vec<u8>,
    ) -> &'a [u8] {
        delegate_snapshot!(self, get_string, index, stack, heap)
    }
    fn get_string_lossy(&self, index: usize, out: &mut Vec<u8>) -> String {
        delegate_snapshot!(self, get_string_lossy, index, out)
    }
    fn item(&self, index: usize) -> Option<Candidate> {
        if index >= self.len() {
            return None;
        }
        Some(Candidate {
            node_index: index,
            range: match self {
                Self::Streaming(s) => Some(*s.payload(index)),
                Self::Document(s) => Some(*s.payload(index)),
                _ => None,
            },
        })
    }
    fn create_search_plan(&self, query: &str) -> Box<dyn nfm_search_core::store::SearchPlan + '_> {
        delegate_snapshot!(self, create_search_plan, query)
    }
    fn header(&self, query: &str) -> Option<String> {
        delegate_snapshot!(self, header, query)
    }
    fn display_text(&self, index: usize, query: &str) -> Option<String> {
        delegate_snapshot!(self, display_text, index, query)
    }
    fn completions(
        &self,
        input: &str,
        cursor: usize,
    ) -> Vec<nfm_search_core::store::SearchCompletion> {
        delegate_snapshot!(self, completions, input, cursor)
    }
    fn effective_query(&self, input: &str) -> String {
        delegate_snapshot!(self, effective_query, input)
    }
}
pub(super) enum Provider {
    Streaming(Arc<SnapshotStore<Items>>),
    Document(Arc<SnapshotStore<nfm_egui::styled_document::DocumentItems<SnapshotCellRange>>>),
    Structured(Arc<StructuredStreamingSnapshot>),
    StructuredStream(Arc<SnapshotStore<StructuredStreamingSnapshot>>),
    FileSystem(Arc<nfm_file_system::walker_search_store::FileSystemSearchStore>),
}

#[cfg(test)]
mod document_source_tests {
    use super::*;
    #[test]
    fn prompt_items_use_osc_input_marks_then_patterns() {
        let patterns = crate::config::prompts::Patterns::default();
        let row = |text: &str, wraps_to_next| TextRow {
            text: text.into(),
            columns: (0..text.len() as u32).collect(),
            wraps_to_next,
        };
        let marked = |input_column| ghostty_ffi::RowPrompt {
            primary: true,
            marked: true,
            input_column,
        };
        // Rows 0-1: a wrapped command at a custom prompt only OSC 133 finds.
        // Row 2: a debugger prompt found by its pattern. Row 3: a marked
        // prompt with nothing typed.
        let prompts = [marked(Some(3)), Default::default(), Default::default(), marked(None)];
        let item = |rows: &[(u64, TextRow)]| prompt_item(rows, &prompts, &patterns);
        let (text, range) = item(&[(0, row("λ git log", true)), (1, row(" -3", false))]).unwrap();
        assert_eq!(text, "git log -3");
        assert_eq!((range.start, range.end), (
            SnapshotCell { x: 3, y: 0 },
            SnapshotCell { x: 2, y: 1 },
        ));
        assert_eq!(item(&[(2, row("0:000> k", false))]).unwrap().0, "k");
        assert!(item(&[(3, row("PS C:\\> ", false))]).is_none());
        assert!(item(&[(2, row("plain output", false))]).is_none());
    }
    #[test]
    fn failed_document_load_publishes_an_empty_completed_source() {
        let store = SnapshotStore::<DocumentItems>::new_append_only();
        complete_document_source(&store);
        assert!(store.is_done());
        assert!(store.snapshot().unwrap().is_empty());
    }
    #[test]
    fn windowed_document_positions_include_logical_origin_and_utf8_bytes() {
        let mut source = PickerSource::test_scrollback(Arc::new(SnapshotStore::new()));
        let mut builder =
            nfm_egui::styled_document::DocumentBuilder::default().with_source_origin(40);
        builder.push_row("a界", &[0, 1, 1, 1], true, &[]).unwrap();
        builder.push_row("bc", &[0, 1], false, &[]).unwrap();
        builder.push_row("next", &[0, 1, 2, 3], false, &[]).unwrap();
        assert!(source.document().set(builder.finish()).is_ok());
        if let PickerSource::Scrollback(runtime) = &mut source {
            runtime.logical_origin = ghostty_ffi::SnapshotLogicalPosition { line: 12, byte: 7 };
        }
        let range = source
            .logical_range(SnapshotCellRange {
                start: SnapshotCell { x: 1, y: 41 },
                end: SnapshotCell { x: 2, y: 42 },
            })
            .unwrap();
        assert_eq!((range.start.line, range.start.byte), (12, 12));
        assert_eq!((range.end.line, range.end.byte), (13, 2));
    }

    #[test]
    fn rows_ending_a_line_drop_trailing_spaces_but_wrapped_rows_keep_them() {
        let row = |text: &str, wraps_to_next| TextRow {
            text: text.into(),
            columns: (0..text.len() as u32).collect(),
            wraps_to_next,
        };
        let mut padded = row("Compiling x   ", false);
        trim_trailing_spaces(&mut padded);
        assert_eq!(padded.text, "Compiling x");
        assert_eq!(padded.columns, (0..11).collect::<Vec<_>>());
        // A space before a soft wrap separates words of one logical line.
        let mut wrapped = row("word ", true);
        trim_trailing_spaces(&mut wrapped);
        assert_eq!(wrapped.text, "word ");
        let mut blank = row("   ", false);
        trim_trailing_spaces(&mut blank);
        assert!(blank.text.is_empty() && blank.columns.is_empty());
    }

    #[test]
    fn document_source_reuses_text_and_returns_original_terminal_ranges() {
        let mut builder = nfm_egui::styled_document::DocumentBuilder::default();
        builder.push_row("abc", &[0, 1, 2], true, &[]).unwrap();
        builder.push_row("def", &[0, 1, 2], false, &[]).unwrap();
        let document = builder.finish();
        let rows = (0..2)
            .map(|y| (y, document.source_row(y as usize).unwrap()))
            .collect::<Vec<_>>();
        let matches = logical_items(&rows, &regex::Regex::new("bcde").unwrap());
        let mut items = nfm_egui::styled_document::DocumentItemStore::new(document.clone());
        let (text, range) = &matches[0];
        items
            .add(
                document
                    .source_byte_offset(range.start.y as usize, range.start.x)
                    .unwrap(),
                text.len(),
                *range,
            )
            .unwrap();
        let store = Arc::new(SnapshotStore::new_append_only());
        store.publish(items.snapshot());
        store.complete();
        let provider = Provider::Document(store);
        let snapshot = provider.snapshot().unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.get_string_lossy(0, &mut Vec::new()), "bcde");
        let candidate = snapshot.item(0).unwrap();
        assert_eq!(candidate.range, Some(*range));
        assert_eq!(range.start, SnapshotCell { x: 1, y: 0 });
        assert_eq!(range.end, SnapshotCell { x: 1, y: 1 });
        assert!(provider.is_append_only());
        assert!(provider.is_done());
    }
}
impl SearchSnapshotProvider<Snapshot> for Provider {
    fn snapshot(&self) -> Option<Arc<Snapshot>> {
        match self {
            Self::Streaming(s) => s.snapshot().map(|s| Arc::new(Snapshot::Streaming(s))),
            Self::Document(s) => s.snapshot().map(|s| Arc::new(Snapshot::Document(s))),
            Self::Structured(s) => Some(Arc::new(Snapshot::Structured(s.clone()))),
            Self::StructuredStream(s) => s.snapshot().map(|s| Arc::new(Snapshot::Structured(s))),
            Self::FileSystem(s) => s.snapshot().map(|s| Arc::new(Snapshot::FileSystem(s))),
        }
    }
    fn snapshot_version(&self) -> u64 {
        match self {
            Self::Streaming(s) => s.snapshot_version(),
            Self::Document(s) => s.snapshot_version(),
            Self::Structured(s) => s.version(),
            Self::StructuredStream(s) => s.snapshot_version(),
            Self::FileSystem(s) => s.snapshot_version(),
        }
    }
    fn is_done(&self) -> bool {
        match self {
            Self::Streaming(s) => s.is_done(),
            Self::Document(s) => s.is_done(),
            Self::Structured(_) => true,
            Self::StructuredStream(s) => s.is_done(),
            Self::FileSystem(s) => s.is_done(),
        }
    }
    fn is_append_only(&self) -> bool {
        // Structured rows realign as column widths grow, so re-search them.
        !matches!(self, Self::Structured(_) | Self::StructuredStream(_))
    }
    fn subscribe_updates(&self, wake: crossbeam_channel::Sender<()>) -> bool {
        match self {
            Self::Streaming(s) => s.subscribe_updates(wake),
            Self::Document(s) => s.subscribe_updates(wake),
            Self::Structured(_) => true,
            Self::StructuredStream(s) => s.subscribe_updates(wake),
            Self::FileSystem(s) => s.subscribe_updates(wake),
        }
    }
}

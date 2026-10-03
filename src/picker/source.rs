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
enum Session {
    Streaming,
    Structured {
        snapshot: Arc<StructuredStreamingSnapshot>,
    },
    #[cfg(not(windows))]
    UnsupportedFileSystem,
    #[cfg(windows)]
    FileSystem {
        store: Arc<nfm_file_system::walker_search_store::FileSystemSearchStore>,
        scan: nfm_file_system::walker::FileWalkerScan,
    },
}
impl Session {
    fn stop(&self) {
        #[cfg(windows)]
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
}

pub(super) enum PickerSource {
    CommandPalette {
        runtime: Runtime,
        entries: Vec<crate::actions::Entry>,
        error_history: bool,
    },
    FileSystem(Runtime),
    Scrollback(Runtime),
    Items {
        runtime: Runtime,
        values: Vec<String>,
    },
    Command {
        runtime: Runtime,
        values: Option<command::CommandValues>,
    },
}

impl PickerSource {
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
        for y in start..end {
            let text = read_row(host, generation, y).ok_or("Unable to read line-split snapshot")?;
            lines.push(split_line_cells(y, &text, columns));
        }
        let target = usize::try_from(row - start).map_err(|_| "Invalid line-split row")?;
        let prompt =
            regex::Regex::new(r"^(?:PS [^>]*>|\d+:\d+>|›)\s*").expect("valid prompt regex");
        let prompts = lines
            .get(target)
            .and_then(|line| {
                let text: String = line.cells.iter().map(|cell| cell.text.as_str()).collect();
                prompt.find(&text).map(|matched| {
                    let mut bytes = 0;
                    let mut end = 0;
                    for cell in &line.cells {
                        if bytes >= matched.end() {
                            break;
                        }
                        bytes += cell.text.len();
                        end = cell.columns.end;
                    }
                    0..end
                })
            })
            .into_iter()
            .collect::<Vec<_>>();
        Ok(Self::split_candidates(
            nfm_picker_sources::line_split::split(&lines, target, &prompts),
        ))
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
        })
    }
    pub(super) fn start(
        host: *mut Host,
        generation: Option<u64>,
        rows: u64,
        ctx: egui::Context,
        options: &mut config::Options,
    ) -> (Self, Arc<std::sync::Mutex<Option<String>>>) {
        let store = Arc::new(if matches!(&options.source, config::Source::Command(_)) {
            SnapshotStore::new_append_only()
        } else {
            SnapshotStore::new()
        });
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
            #[cfg(windows)]
            crate::picker::config::Source::FileSystem(files) => {
                let shared =
                    Arc::new(nfm_file_system::walker_search_store::FileSystemSearchStore::new());
                let scan = nfm_file_system::walker::start_scan(
                    nfm_file_system::walker::ScanOptions {
                        roots: files.roots.clone(),
                        max_depth: files.max_depth,
                        files_only: files.files_only,
                        directories_only: files.directories_only,
                    },
                    shared.clone(),
                );
                Session::FileSystem {
                    store: shared,
                    scan,
                }
            }
            #[cfg(not(windows))]
            crate::picker::config::Source::FileSystem(_) => {
                *error.lock().unwrap() =
                    Some("Filesystem pickers require NFM support for this platform".into());
                Session::UnsupportedFileSystem
            }
            crate::picker::config::Source::Themes | crate::picker::config::Source::ErrorLog => {
                unreachable!("theme source must be resolved before starting")
            }
            crate::picker::config::Source::CommandPalette(entries) => {
                let schema = StructuredSchema::new(vec!["Action".into(), "Shortcut".into()])
                    .expect("valid command palette schema");
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
            crate::picker::config::Source::Scrollback { .. }
            | crate::picker::config::Source::Items(_)
            | crate::picker::config::Source::Command(_) => Session::Streaming,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let reader = if let crate::picker::config::Source::Command(config) = &options.source {
            crate::picker::command::start(
                config.clone(),
                options.params.clone(),
                store.clone(),
                cancelled.clone(),
                error.clone(),
                ctx.clone(),
                command_values.clone(),
            )
        } else if let crate::picker::config::Source::Scrollback { regex, unique } = &options.source
        {
            let regex = regex.clone();
            let mut seen = unique.then(std::collections::HashSet::new);
            let generation = generation.expect("scrollback picker requires a snapshot");
            let (store, cancelled, error) = (store.clone(), cancelled.clone(), error.clone());
            let host = host as usize;
            let failure_error = error.clone();
            let failure_ctx = ctx.clone();
            let failure_store = store.clone();
            crate::worker::spawn(
                "picker scrollback worker",
                move || {
                    let mut publisher = PublishingStreamingItemStoreWithPayload::new(store);
                    let read = |row| {
                        if cancelled.load(Ordering::Acquire) {
                            return None;
                        }
                        let result = read_row(host as *mut Host, generation, row);
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
                        for (text, range) in newest_logical_items(&lines, &regex, &mut seen) {
                            if cancelled.load(Ordering::Acquire) {
                                break;
                            }
                            publisher.add_item(text.as_bytes(), range);
                            batch += 1;
                            if batch >= 128 {
                                publisher.publish();
                                batch = 0;
                                ctx.request_repaint();
                            }
                        }
                        bottom = start;
                    }
                    publisher.complete();
                    ctx.request_repaint();
                },
                move |message| {
                    crate::error_log::error("picker", "Scrollback worker failed", &message);
                    *failure_error
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(message);
                    PublishingStreamingItemStoreWithPayload::new(failure_store.clone()).complete();
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
        };
        let source = match &options.source {
            config::Source::Themes | crate::picker::config::Source::ErrorLog => {
                unreachable!("theme source must be resolved before starting")
            }
            config::Source::CommandPalette(entries) => Self::CommandPalette {
                runtime,
                entries: entries.clone(),
                error_history: options.params.contains_key("__error_history"),
            },
            config::Source::FileSystem(_) => Self::FileSystem(runtime),
            config::Source::Scrollback { .. } => Self::Scrollback(runtime),
            config::Source::Items(_) => Self::Items {
                runtime,
                values: item_values,
            },
            config::Source::Command(_) => Self::Command {
                runtime,
                values: command_values,
            },
        };
        (source, error)
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
    pub(super) fn label(&self) -> &'static str {
        match self {
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
            Self::CommandPalette {
                error_history: true,
                ..
            } => "ERROR HISTORY",
            Self::CommandPalette { .. } => "COMMAND PALETTE",
        }
    }
    pub(super) fn provider(&self) -> Arc<Provider> {
        Arc::new(match &self.runtime().session {
            Session::Streaming => Provider::Streaming(self.runtime().store.clone()),
            Session::Structured { snapshot } => Provider::Structured(snapshot.clone()),
            #[cfg(windows)]
            Session::FileSystem { store, .. } => Provider::FileSystem(store.clone()),
            #[cfg(not(windows))]
            Session::UnsupportedFileSystem => Provider::Streaming(self.runtime().store.clone()),
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

fn read_row(host: *mut Host, generation: u64, row: u64) -> Option<TextRow> {
    // The native row reader caches its snapshot anchor. A UI-thread line split
    // may read while the original scrollback source is still publishing rows.
    static ROW_READ: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _read = ROW_READ.lock().unwrap_or_else(|error| error.into_inner());
    let mut result = ghostty_ffi::SnapshotTextRow::default();
    if !unsafe {
        ghostty_ffi::ghostty_egui_host_snapshot_text_row(host, generation, row, &mut result)
    } {
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
    unsafe { ghostty_ffi::ghostty_egui_host_snapshot_text_row_free(&mut result) };
    Some(TextRow {
        text: String::from_utf8(text).ok()?,
        columns,
        wraps_to_next,
    })
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
    Structured(Arc<StructuredStreamingSnapshot>),
    #[cfg(windows)]
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
            Snapshot::Structured(s) => ItemsSource::$method(s.as_ref(), $($arg),*),
            #[cfg(windows)]
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
    Structured(Arc<StructuredStreamingSnapshot>),
    #[cfg(windows)]
    FileSystem(Arc<nfm_file_system::walker_search_store::FileSystemSearchStore>),
}
impl SearchSnapshotProvider<Snapshot> for Provider {
    fn snapshot(&self) -> Option<Arc<Snapshot>> {
        match self {
            Self::Streaming(s) => s.snapshot().map(|s| Arc::new(Snapshot::Streaming(s))),
            Self::Structured(s) => Some(Arc::new(Snapshot::Structured(s.clone()))),
            #[cfg(windows)]
            Self::FileSystem(s) => s.snapshot().map(|s| Arc::new(Snapshot::FileSystem(s))),
        }
    }
    fn snapshot_version(&self) -> u64 {
        match self {
            Self::Streaming(s) => s.snapshot_version(),
            Self::Structured(s) => s.version(),
            #[cfg(windows)]
            Self::FileSystem(s) => s.snapshot_version(),
        }
    }
    fn is_done(&self) -> bool {
        match self {
            Self::Streaming(s) => s.is_done(),
            Self::Structured(_) => true,
            #[cfg(windows)]
            Self::FileSystem(s) => s.is_done(),
        }
    }
    fn is_append_only(&self) -> bool {
        !matches!(self, Self::Structured(_))
    }
    fn subscribe_updates(&self, wake: crossbeam_channel::Sender<()>) -> bool {
        match self {
            Self::Streaming(s) => s.subscribe_updates(wake),
            Self::Structured(_) => true,
            #[cfg(windows)]
            Self::FileSystem(s) => s.subscribe_updates(wake),
        }
    }
}

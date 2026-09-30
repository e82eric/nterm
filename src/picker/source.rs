use super::{Items, command, config};
use crate::copy_mode::TextRow;
use crate::ghostty_ffi::{self, Host, SnapshotCell, SnapshotCellRange};
use nfm_picker_sources::structured::{
    StructuredSchema, StructuredStreamingSnapshot, StructuredStreamingStore,
};
use nfm_search_core::fuzzy_search_session::{FuzzySearchSession, FuzzySearchUpdate};
use nfm_search_core::search::SearchResult;
use nfm_search_core::snapshot_store::SnapshotStore;
use nfm_search_core::store::ItemsSource;
use nfm_search_core::store::PublishingStreamingItemStoreWithPayload;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
enum Session {
    Streaming(FuzzySearchSession<Items, SnapshotStore<Items>>),
    Structured {
        session: FuzzySearchSession<
            StructuredStreamingSnapshot,
            SnapshotStore<StructuredStreamingSnapshot>,
        >,
        snapshot: Arc<StructuredStreamingSnapshot>,
    },
    #[cfg(not(windows))]
    UnsupportedFileSystem,
    #[cfg(windows)]
    FileSystem {
        session: FuzzySearchSession<
            nfm_file_system::walker::PublishedSnapshot,
            nfm_file_system::walker_search_store::FileSystemSearchStore,
        >,
        scan: nfm_file_system::walker::FileWalkerScan,
    },
}

impl Session {
    fn set_query(&self, query: String) {
        match self {
            Self::Streaming(session) => session.set_query(query),
            Self::Structured { session, .. } => session.set_query(query),
            #[cfg(not(windows))]
            Self::UnsupportedFileSystem => {}
            #[cfg(windows)]
            Self::FileSystem { session, .. } => session.set_query(query),
        }
    }
    fn stop(&self) {
        match self {
            Self::Streaming(session) => session.stop(),
            Self::Structured { session, .. } => session.stop(),
            #[cfg(not(windows))]
            Self::UnsupportedFileSystem => {}
            #[cfg(windows)]
            Self::FileSystem { session, scan } => {
                scan.cancel();
                session.stop();
            }
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
    pub(super) fn start(
        host: *mut Host,
        generation: Option<u64>,
        rows: u64,
        ctx: egui::Context,
        options: &mut config::Options,
        session_id: u64,
        initial_query: String,
    ) -> (
        Self,
        Arc<std::sync::Mutex<Option<FuzzySearchUpdate>>>,
        Arc<std::sync::Mutex<Option<String>>>,
    ) {
        let store = Arc::new(SnapshotStore::new());
        let (tx, updates_rx) = crossbeam_channel::unbounded();
        let error = Arc::new(std::sync::Mutex::new(None));
        if let crate::picker::config::Source::Items(items) = &mut options.source {
            if let Some(provider) = &items.provider {
                match provider.read() {
                    Ok(values) => items.values = values,
                    Err(message) => *error.lock().unwrap() = Some(message),
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
                let session = FuzzySearchSession::new_with_sort(
                    session_id,
                    shared,
                    initial_query.clone(),
                    tx,
                    options.sort.mode(),
                );
                session.start();
                Session::FileSystem { session, scan }
            }
            #[cfg(not(windows))]
            crate::picker::config::Source::FileSystem(_) => {
                *error.lock().unwrap() =
                    Some("Filesystem pickers require NFM support for this platform".into());
                Session::UnsupportedFileSystem
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
                let shared = Arc::new(SnapshotStore::new());
                shared.publish(snapshot.clone());
                shared.complete();
                let session = FuzzySearchSession::new_with_sort(
                    session_id,
                    shared,
                    initial_query.clone(),
                    tx,
                    options.sort.mode(),
                );
                session.start();
                Session::Structured { session, snapshot }
            }
            crate::picker::config::Source::Scrollback { .. }
            | crate::picker::config::Source::Items(_)
            | crate::picker::config::Source::Command(_) => {
                let session = FuzzySearchSession::new_with_sort(
                    session_id,
                    store.clone(),
                    initial_query.clone(),
                    tx,
                    options.sort.mode(),
                );
                session.start();
                Session::Streaming(session)
            }
        };
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
        let reader = if let crate::picker::config::Source::Command(config) = &options.source {
            Some(crate::picker::command::start(
                config.clone(),
                options.params.clone(),
                store.clone(),
                cancelled.clone(),
                error.clone(),
                ctx.clone(),
                command_values.clone(),
            ))
        } else if let crate::picker::config::Source::Scrollback { regex, unique } = &options.source
        {
            let regex = regex.clone();
            let mut seen = unique.then(std::collections::HashSet::new);
            let generation = generation.expect("scrollback picker requires a snapshot");
            let (store, cancelled, error) = (store.clone(), cancelled.clone(), error.clone());
            let host = host as usize;
            Some(thread::spawn(move || {
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
            }))
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
            config::Source::CommandPalette(entries) => Self::CommandPalette {
                runtime,
                entries: entries.clone(),
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
        (source, updates, error)
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
    pub(super) fn is_filesystem(&self) -> bool {
        matches!(self, Self::FileSystem(_))
    }
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::FileSystem(_) => "FILESYSTEM PICKER",
            Self::Scrollback(_) => "SCROLLBACK PICKER",
            Self::Items { .. } => "ITEMS PICKER",
            Self::Command { .. } => "COMMAND PICKER",
            Self::CommandPalette { .. } => "COMMAND PALETTE",
        }
    }
    pub(super) fn set_query(&self, query: String) {
        self.runtime().session.set_query(query);
    }
    pub(super) fn effective_query(&self, query: &str) -> String {
        match &self.runtime().session {
            Session::Structured { snapshot, .. } => snapshot.effective_query(query),
            _ => query.to_owned(),
        }
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
    pub(super) fn range(&self, result: &SearchResult) -> Option<SnapshotCellRange> {
        if let Self::Scrollback(runtime) = self {
            runtime
                .store
                .snapshot()
                .map(|snapshot| *snapshot.payload(result.node_index))
        } else {
            None
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
    pub(super) fn test_scrollback(
        store: Arc<SnapshotStore<Items>>,
        session: FuzzySearchSession<Items, SnapshotStore<Items>>,
    ) -> Self {
        Self::Scrollback(Runtime {
            store,
            session: Session::Streaming(session),
            cancelled: Arc::new(AtomicBool::new(false)),
            reader: None,
        })
    }
    #[cfg(test)]
    pub(super) fn store(&self) -> &Arc<SnapshotStore<Items>> {
        &self.runtime().store
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

fn read_row(host: *mut Host, generation: u64, row: u64) -> Option<TextRow> {
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

use super::{Items, command, config};
use crate::copy_mode::TextRow;
use crate::ghostty_ffi::{self, Host, SnapshotCell, SnapshotCellRange};
use nfm_search_core::fuzzy_search_session::{FuzzySearchSession, FuzzySearchUpdate};
use nfm_search_core::search::SearchResult;
use nfm_search_core::snapshot_store::SnapshotStore;
use nfm_search_core::store::PublishingStreamingItemStoreWithPayload;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
enum Session {
    Streaming(FuzzySearchSession<Items, SnapshotStore<Items>>),
    #[cfg(not(windows))]
    UnsupportedFileSystem,
    #[cfg(windows)]
    FileSystem(nfm_file_system::file_system_search::FileSystemSearch),
}

impl Session {
    fn set_query(&self, query: String) {
        match self {
            Self::Streaming(session) => session.set_query(query),
            #[cfg(not(windows))]
            Self::UnsupportedFileSystem => {}
            #[cfg(windows)]
            Self::FileSystem(session) => session.set_query(query),
        }
    }
    fn stop(&self) {
        match self {
            Self::Streaming(session) => session.stop(),
            #[cfg(not(windows))]
            Self::UnsupportedFileSystem => {}
            #[cfg(windows)]
            Self::FileSystem(session) => session.stop(),
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
            crate::picker::config::Source::FileSystem(files) => Session::FileSystem(
                nfm_file_system::file_system_search::FileSystemSearch::start(
                    session_id,
                    nfm_file_system::walker::ScanOptions {
                        roots: files.roots.clone(),
                        max_depth: files.max_depth,
                        files_only: files.files_only,
                        directories_only: files.directories_only,
                    },
                    initial_query.clone(),
                    tx,
                ),
            ),
            #[cfg(not(windows))]
            crate::picker::config::Source::FileSystem(_) => {
                *error.lock().unwrap() =
                    Some("Filesystem pickers require NFM support for this platform".into());
                Session::UnsupportedFileSystem
            }
            crate::picker::config::Source::Scrollback { .. }
            | crate::picker::config::Source::Items(_)
            | crate::picker::config::Source::Command(_) => {
                let session =
                    FuzzySearchSession::new(session_id, store.clone(), initial_query.clone(), tx);
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
        }
    }
    pub(super) fn set_query(&self, query: String) {
        self.runtime().session.set_query(query);
    }
    pub(super) fn accepted_value(&self, result: &SearchResult) -> String {
        let value = match self {
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

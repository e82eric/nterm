use crate::ffi::SnapshotTextRange;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread;

#[derive(Default)]
pub(crate) struct SearchEvents {
    pub(crate) open: Option<String>,
    pub(crate) close: bool,
}

#[derive(Default)]
pub(crate) struct SearchUi {
    pub(crate) active: bool,
    pub(crate) query: String,
    pub(crate) snapshot_generation: Option<u64>,
    pub(crate) text: Arc<str>,
    pub(crate) matches: Vec<SnapshotTextRange>,
    pub(crate) selected: Option<usize>,
    pub(crate) focus_requested: bool,
    pub(crate) query_revision: u64,
}

impl SearchUi {
    pub(crate) fn apply_result(&mut self, matches: Vec<SnapshotTextRange>) {
        self.matches = matches;
        self.selected = (!self.matches.is_empty()).then_some(0);
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

    pub(crate) fn selected_range(&self) -> Option<&SnapshotTextRange> {
        self.selected.and_then(|index| self.matches.get(index))
    }

    pub(crate) fn match_label(&self) -> String {
        match self.selected {
            Some(selected) => format!("{}/{}", selected + 1, self.matches.len()),
            None => format!("0/{}", self.matches.len()),
        }
    }
}

pub(crate) struct SearchResult {
    pub(crate) snapshot_generation: u64,
    pub(crate) query_revision: u64,
    pub(crate) matches: Vec<SnapshotTextRange>,
}

struct SearchRequest {
    snapshot_generation: u64,
    query_revision: u64,
    query: String,
    text: Arc<str>,
}

enum SearchCommand {
    Search(SearchRequest),
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
        text: Arc<str>,
    ) -> u64 {
        let query_revision = self.advance_query_revision();
        let _ = self.commands.send(SearchCommand::Search(SearchRequest {
            snapshot_generation,
            query_revision,
            query,
            text,
        }));
        query_revision
    }

    pub(crate) fn cancel(&mut self) {
        self.advance_query_revision();
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
    while let Ok(command) = commands.recv() {
        let mut request = match command {
            SearchCommand::Search(request) => request,
            SearchCommand::Shutdown => return,
        };

        // Collapse queued edits before starting work. A query revision check
        // inside the byte scan also abandons an in-progress search after a
        // newer edit.
        loop {
            match commands.try_recv() {
                Ok(SearchCommand::Search(newer)) => request = newer,
                Ok(SearchCommand::Shutdown) => return,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        let query_revision = request.query_revision;
        if latest_query_revision.load(Ordering::Acquire) != query_revision {
            continue;
        }
        let Some(matches) = find_search_matches_cancellable(&request.text, &request.query, || {
            latest_query_revision.load(Ordering::Acquire) != query_revision
        }) else {
            continue;
        };
        if latest_query_revision.load(Ordering::Acquire) != query_revision {
            continue;
        }
        if results
            .send(SearchResult {
                snapshot_generation: request.snapshot_generation,
                query_revision,
                matches,
            })
            .is_err()
        {
            return;
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
fn find_search_matches(text: &str, query: &str) -> Vec<SnapshotTextRange> {
    find_search_matches_cancellable(text, query, || false).unwrap_or_default()
}

fn find_search_matches_cancellable(
    text: &str,
    query: &str,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<SnapshotTextRange>> {
    let mut result = Vec::new();
    if query.is_empty() || query.len() > text.len() {
        return Some(result);
    }

    let text_bytes = text.as_bytes();
    let query_bytes = query.as_bytes();
    let mut offset = 0;
    while offset + query_bytes.len() <= text_bytes.len() {
        if offset & 0xfff == 0 && cancelled() {
            return None;
        }
        let end = offset + query_bytes.len();
        if text.is_char_boundary(offset)
            && text.is_char_boundary(end)
            && text_bytes[offset..end].eq_ignore_ascii_case(query_bytes)
        {
            result.push(SnapshotTextRange {
                start_byte: offset as u64,
                end_byte: end as u64,
            });
            offset = end;
        } else {
            offset += 1;
        }
    }
    // The snapshot text is oldest-to-newest. Search navigation starts at the
    // newest match and walks backward through scrollback.
    result.reverse();
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::{SearchWorker, SnapshotTextRange, find_search_matches};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn matches_are_newest_to_oldest() {
        let text = "hit old\nhit hit new";
        assert_eq!(
            find_search_matches(text, "hit"),
            vec![
                SnapshotTextRange {
                    start_byte: 12,
                    end_byte: 15,
                },
                SnapshotTextRange {
                    start_byte: 8,
                    end_byte: 11,
                },
                SnapshotTextRange {
                    start_byte: 0,
                    end_byte: 3,
                },
            ]
        );
    }

    #[test]
    fn refined_query_selects_newest_remaining_match() {
        let matches = find_search_matches("ab old\na new", "ab");
        assert_eq!(matches[0].start_byte, 0);
    }

    #[test]
    fn matching_is_ascii_case_insensitive_like_ghostty_search() {
        let matches = find_search_matches("LastWriteTime\n.winapp .wt .WTD", "W");
        assert_eq!(matches.len(), 4);
        assert_eq!(matches[0].start_byte, 27);
        assert_eq!(matches[3].start_byte, 4);
    }

    #[test]
    fn match_ranges_are_utf8_byte_offsets() {
        let matches = find_search_matches("a界 café", "界");
        assert_eq!(
            matches,
            vec![SnapshotTextRange {
                start_byte: 1,
                end_byte: 4,
            }]
        );
    }

    #[test]
    fn worker_tags_results_with_snapshot_generation_and_query_revision() {
        let mut worker = SearchWorker::new(egui::Context::default());
        let text: Arc<str> = Arc::from("old\nnew match");
        let query_revision = worker.submit(42, "match".to_owned(), text);
        let result = worker
            .results
            .recv_timeout(Duration::from_secs(2))
            .expect("search worker result");

        assert_eq!(result.snapshot_generation, 42);
        assert_eq!(result.query_revision, query_revision);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].start_byte, 8);
        assert_eq!(result.matches[0].end_byte, 13);
    }
}

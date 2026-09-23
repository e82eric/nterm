use crate::ffi::SnapshotRange;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use unicode_width::UnicodeWidthStr;

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
    pub(crate) columns: u32,
    pub(crate) rows: Arc<[String]>,
    pub(crate) matches: Vec<SnapshotRange>,
    pub(crate) selected: Option<usize>,
    pub(crate) focus_requested: bool,
    pub(crate) query_revision: u64,
}

impl SearchUi {
    pub(crate) fn apply_result(&mut self, matches: Vec<SnapshotRange>) {
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

    pub(crate) fn selected_range(&self) -> Option<&SnapshotRange> {
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
    pub(crate) matches: Vec<SnapshotRange>,
}

struct SearchRequest {
    snapshot_generation: u64,
    query_revision: u64,
    query: String,
    columns: u32,
    rows: Arc<[String]>,
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
        columns: u32,
        rows: Arc<[String]>,
    ) -> u64 {
        let query_revision = self.advance_query_revision();
        let _ = self.commands.send(SearchCommand::Search(SearchRequest {
            snapshot_generation,
            query_revision,
            query,
            columns,
            rows,
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
        // inside the row loop also abandons an in-progress search after a
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
        let Some(matches) =
            find_search_matches_cancellable(&request.rows, &request.query, request.columns, || {
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
fn find_search_matches(rows: &[String], query: &str, columns: u32) -> Vec<SnapshotRange> {
    find_search_matches_cancellable(rows, query, columns, || false).unwrap_or_default()
}

fn find_search_matches_cancellable(
    rows: &[String],
    query: &str,
    columns: u32,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<SnapshotRange>> {
    let mut result = Vec::new();
    if query.is_empty() || columns == 0 {
        return Some(result);
    }

    // Ghostty search navigation is newest-to-oldest. Screen row zero is the
    // oldest scrollback row, so collect rows and matches in reverse order.
    for y in (0..rows.len()).rev() {
        if cancelled() {
            return None;
        }
        let row = &rows[y];
        let row_matches = ascii_case_insensitive_matches(row, query);
        for byte in row_matches.into_iter().rev() {
            let matched = &row[byte..byte + query.len()];
            let start_x = UnicodeWidthStr::width(&row[..byte]);
            let width = UnicodeWidthStr::width(matched);
            if width == 0 || start_x >= columns as usize {
                continue;
            }
            result.push(SnapshotRange {
                start_x: start_x as u32,
                start_y: y as u32,
                end_x: (start_x + width - 1).min(columns as usize - 1) as u32,
                end_y: y as u32,
            });
        }
    }
    Some(result)
}

fn ascii_case_insensitive_matches(haystack: &str, needle: &str) -> Vec<usize> {
    let mut result = Vec::new();
    if needle.is_empty() || needle.len() > haystack.len() {
        return result;
    }

    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut offset = 0;
    while offset + needle_bytes.len() <= haystack_bytes.len() {
        let end = offset + needle_bytes.len();
        if haystack.is_char_boundary(offset)
            && haystack.is_char_boundary(end)
            && haystack_bytes[offset..end].eq_ignore_ascii_case(needle_bytes)
        {
            result.push(offset);
            offset = end;
        } else {
            offset += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{SearchWorker, SnapshotRange, find_search_matches};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn matches_are_newest_to_oldest() {
        let rows = vec!["hit old".to_owned(), "hit hit new".to_owned()];
        assert_eq!(
            find_search_matches(&rows, "hit", 20),
            vec![
                SnapshotRange {
                    start_x: 4,
                    start_y: 1,
                    end_x: 6,
                    end_y: 1,
                },
                SnapshotRange {
                    start_x: 0,
                    start_y: 1,
                    end_x: 2,
                    end_y: 1,
                },
                SnapshotRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 2,
                    end_y: 0,
                },
            ]
        );
    }

    #[test]
    fn refined_query_selects_newest_remaining_match() {
        let rows = vec!["ab old".to_owned(), "a new".to_owned()];
        let matches = find_search_matches(&rows, "ab", 20);
        assert_eq!(matches[0].start_y, 0);
    }

    #[test]
    fn matching_is_ascii_case_insensitive_like_ghostty_search() {
        let rows = vec!["LastWriteTime".to_owned(), ".winapp .wt .WTD".to_owned()];
        let matches = find_search_matches(&rows, "W", 20);
        assert_eq!(matches.len(), 4);
        assert_eq!(matches[0].start_y, 1);
        assert_eq!(matches[0].start_x, 13);
        assert_eq!(matches[3].start_y, 0);
    }

    #[test]
    fn worker_tags_results_with_snapshot_generation_and_query_revision() {
        let mut worker = SearchWorker::new(egui::Context::default());
        let rows: Arc<[String]> = vec!["old".to_owned(), "new match".to_owned()].into();
        let query_revision = worker.submit(42, "match".to_owned(), 20, rows);
        let result = worker
            .results
            .recv_timeout(Duration::from_secs(2))
            .expect("search worker result");

        assert_eq!(result.snapshot_generation, 42);
        assert_eq!(result.query_revision, query_revision);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].start_y, 1);
        assert_eq!(result.matches[0].start_x, 4);
    }
}

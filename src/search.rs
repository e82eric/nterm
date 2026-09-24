use crate::ffi::{self, SnapshotCell, SnapshotCellRange, SnapshotTextChunk};
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
    pub(crate) cursor: usize,
    pub(crate) matches: Vec<SnapshotCellRange>,
    pub(crate) selected: Option<usize>,
    pub(crate) focus_requested: bool,
    pub(crate) query_revision: u64,
}

impl SearchUi {
    pub(crate) fn apply_result(&mut self, matches: Vec<SnapshotCellRange>) {
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

    pub(crate) fn selected_range(&self) -> Option<&SnapshotCellRange> {
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
    pub(crate) matches: Vec<SnapshotCellRange>,
}

struct SearchRequest {
    snapshot_generation: u64,
    query_revision: u64,
    query: String,
    cursor: usize,
}

enum SearchCommand {
    Search(SearchRequest),
    Barrier(mpsc::Sender<()>),
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

    pub(crate) fn submit(&mut self, snapshot_generation: u64, query: String, cursor: usize) -> u64 {
        let query_revision = self.advance_query_revision();
        let _ = self.commands.send(SearchCommand::Search(SearchRequest {
            snapshot_generation,
            query_revision,
            query,
            cursor,
        }));
        query_revision
    }

    pub(crate) fn cancel(&mut self) {
        self.advance_query_revision();
    }

    /// Wait until the worker has stopped using the previous snapshot cursor.
    pub(crate) fn cancel_and_wait(&mut self) {
        self.cancel();
        let (tx, rx) = mpsc::channel();
        if self.commands.send(SearchCommand::Barrier(tx)).is_ok() {
            let _ = rx.recv();
        }
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
    'requests: while let Ok(command) = commands.recv() {
        let mut request = match command {
            SearchCommand::Search(request) => request,
            SearchCommand::Barrier(done) => {
                let _ = done.send(());
                continue;
            }
            SearchCommand::Shutdown => return,
        };

        // Collapse queued edits before starting work. A query revision check
        // inside the byte scan also abandons an in-progress search after a
        // newer edit.
        loop {
            match commands.try_recv() {
                Ok(SearchCommand::Search(newer)) => request = newer,
                Ok(SearchCommand::Barrier(done)) => {
                    let _ = done.send(());
                    continue 'requests;
                }
                Ok(SearchCommand::Shutdown) => return,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        let query_revision = request.query_revision;
        if latest_query_revision.load(Ordering::Acquire) != query_revision {
            continue;
        }
        let Some(matches) = find_search_matches_cancellable(request.cursor, &request.query, || {
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

fn find_search_matches_cancellable(
    cursor: usize,
    query: &str,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<SnapshotCellRange>> {
    let mut scan = StreamScan::new(query);
    if query.is_empty() {
        return Some(Vec::new());
    }
    if !unsafe { ffi::ghostty_egui_host_snapshot_cursor_reset(cursor as *mut _) } {
        return None;
    }
    loop {
        if cancelled() {
            return None;
        }
        let mut chunk = SnapshotTextChunk::default();
        match unsafe { ffi::ghostty_egui_host_snapshot_cursor_next(cursor as *mut _, &mut chunk) } {
            0 => break,
            1 => {}
            _ => return None,
        }
        let bytes = unsafe { std::slice::from_raw_parts(chunk.text, chunk.len) };
        let points = unsafe { std::slice::from_raw_parts(chunk.points, chunk.len) };
        scan.feed(bytes, points, chunk.row_base, chunk.start_y, &cancelled)?;
    }
    scan.matches.reverse();
    Some(scan.matches)
}

struct StreamScan {
    query: Vec<u8>,
    tail_bytes: Vec<u8>,
    tail_cells: Vec<SnapshotCell>,
    byte_base: u64,
    next_allowed: u64,
    matches: Vec<SnapshotCellRange>,
}

impl StreamScan {
    fn new(query: &str) -> Self {
        Self {
            query: query.as_bytes().to_vec(),
            tail_bytes: Vec::new(),
            tail_cells: Vec::new(),
            byte_base: 0,
            next_allowed: 0,
            matches: Vec::new(),
        }
    }

    fn feed(
        &mut self,
        chunk: &[u8],
        points: &[ffi::SnapshotTextPoint],
        row_base: u64,
        start_y: u16,
        cancelled: &impl Fn() -> bool,
    ) -> Option<()> {
        if chunk.len() != points.len() {
            return None;
        }
        let tail_len = self.tail_bytes.len();
        let first = self.byte_base - tail_len as u64;
        let mut bytes = std::mem::take(&mut self.tail_bytes);
        let old_cells = std::mem::take(&mut self.tail_cells);
        bytes.extend_from_slice(chunk);
        let cell_at = |i: usize| -> Option<SnapshotCell> {
            if i < tail_len {
                return old_cells.get(i).copied();
            }
            let point = points.get(i - tail_len)?;
            Some(SnapshotCell {
                x: u32::from(point.x),
                y: row_base + u64::from(point.y.checked_sub(u32::from(start_y))?),
            })
        };
        let q = self.query.len();
        if q > 0 && bytes.len() >= q {
            for i in 0..=bytes.len() - q {
                if i & 0xfff == 0 && cancelled() {
                    return None;
                }
                let at = first + i as u64;
                if at < self.next_allowed || bytes[i] & 0xc0 == 0x80 {
                    continue;
                }
                let end = i + q;
                if end < bytes.len() && bytes[end] & 0xc0 == 0x80 {
                    continue;
                }
                if bytes[i..end].eq_ignore_ascii_case(&self.query) {
                    self.matches.push(SnapshotCellRange {
                        start: cell_at(i)?,
                        end: cell_at(end - 1)?,
                    });
                    self.next_allowed = at + q as u64;
                }
            }
        }
        self.byte_base += chunk.len() as u64;
        let keep = bytes.len().min(q.saturating_sub(1));
        let mut next_cells = Vec::with_capacity(keep);
        for i in bytes.len() - keep..bytes.len() {
            next_cells.push(cell_at(i)?);
        }
        let tail_start = bytes.len() - keep;
        bytes.copy_within(tail_start.., 0);
        bytes.truncate(keep);
        self.tail_bytes = bytes;
        self.tail_cells = next_cells;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::StreamScan;
    use crate::ffi::{SnapshotCell, SnapshotCellRange, SnapshotTextPoint};

    fn feed(scan: &mut StreamScan, text: &str, row_base: u64) {
        let points: Vec<_> = (0..text.len())
            .map(|x| SnapshotTextPoint { x: x as u16, y: 0 })
            .collect();
        scan.feed(text.as_bytes(), &points, row_base, 0, &|| false)
            .unwrap();
    }

    #[test]
    fn matches_cross_chunks_and_keep_cell_locations() {
        let mut scan = StreamScan::new("界x");
        feed(&mut scan, "a界", 0);
        feed(&mut scan, "x and 界x", 1);
        assert_eq!(
            scan.matches,
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
    fn ascii_search_is_case_insensitive_and_nonoverlapping() {
        let mut scan = StreamScan::new("aa");
        feed(&mut scan, "aAaA", 0);
        assert_eq!(scan.matches.len(), 2);
        assert_eq!(scan.matches[0].start.x, 0);
        assert_eq!(scan.matches[1].start.x, 2);
    }

    #[test]
    fn utf8_match_can_span_chunks_without_losing_its_cell() {
        let mut scan = StreamScan::new("界");
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
        assert_eq!(
            scan.matches,
            vec![SnapshotCellRange {
                start: SnapshotCell { x: 1, y: 0 },
                end: SnapshotCell { x: 1, y: 0 },
            }]
        );
    }
}

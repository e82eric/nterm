//! Pickers the daemon runs for programs in its shells. A program asks for
//! one by name; the daemon has what it lists and searches it, on its own
//! machine, and the UI attached to the program's pane shows the rows it is
//! sent and says what is typed and chosen. The UI is sent nothing but what
//! to show: what was chosen goes from here to the program.
//!
//! A program's first message is `Control::Pick`. What it says after that,
//! and is answered, is JSON in `Frame::Shell`:
//!
//! - `{"type":"open","request_id":ID,"picker":NAME,"params":{...}}` asks
//!   for a picker, and is answered `{"type":"opened"}` once a UI has been
//!   asked to show it;
//! - `{"type":"cancel"}`, or leaving, ends it;
//! - `{"type":"result","request_id":ID,"status":...}` is how it ended:
//!   `accepted` with `selected` and `query`, `cancelled`, or `error` with
//!   `code` and `message`.

use crate::log::log;
use nfm_file_system::{
    walker::{FileWalkerScan, PublishedSnapshot, ScanOptions, start_scan},
    walker_search_store::FileSystemSearchStore,
};
use nfm_search_core::{
    fuzzy_search_session::{FuzzySearchSession, FuzzySearchUpdate, SearchSnapshotProvider},
    search::SearchSortMode,
    store::{ItemsSource, SearchPlan},
};
use nterm_session::protocol::{Control, Frame, PaneId, PickerRow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc::Sender,
    },
};

/// Encoded frames on their way to a UI or a program, as `server` has them.
type Outbox = Sender<Vec<u8>>;

/// The most rows a UI is answered with, whatever it asks for.
const MOST_ROWS: usize = 1000;
/// How many of the answers a picker has sent it can still say the rows of:
/// a row is chosen from an answer the UI was shown, not always the last.
const KEPT_ANSWERS: usize = 8;
/// The largest request a program can make.
const MAX_REQUEST: usize = 1 << 20;

/// A search runs on as many threads as it is let, and the daemon's first
/// work is its shells: searches are left all the processors but two, so
/// that a shell's output is passed on while one runs.
/// Done when the first picker is opened: a daemon that runs none has no
/// such threads.
fn limit_search_threads() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let processors = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = processors.saturating_sub(2).max(1);
        // Refused only if something has searched already, which nothing has.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|n| format!("picker search {n}"))
            .build_global();
    });
}

/// What a program asks for.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Said {
    Open {
        #[serde(default)]
        request_id: Value,
        picker: String,
        #[serde(default)]
        params: BTreeMap<String, String>,
    },
    Cancel,
    #[serde(other)]
    Other,
}

/// A picker the daemon has: what it lists.
struct Definition {
    label: &'static str,
    scan: ScanOptions,
    query: String,
}

/// The pickers there are. `files` and `directories` list what is under the
/// folders `roots` names, a JSON array, or under `cwd` with none; `query`
/// is what the picker starts with.
fn define(name: &str, params: &BTreeMap<String, String>) -> Result<Definition, String> {
    let (label, directories_only) = match name {
        "files" => ("Files", false),
        "directories" => ("Directories", true),
        _ => return Err(format!("no picker named {name}")),
    };
    let mut roots: Vec<PathBuf> = match params.get("roots") {
        Some(roots) => serde_json::from_str::<Vec<String>>(roots)
            .map_err(|error| format!("roots is not a list of folders: {error}"))?
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        None => Vec::new(),
    };
    let cwd = params.get("cwd").map(PathBuf::from);
    if roots.is_empty() {
        roots.push(cwd.clone().ok_or("neither roots nor cwd was given")?);
    }
    for root in &mut roots {
        if root.is_relative() {
            let cwd = cwd.as_ref().ok_or_else(|| format!("{} is not a whole path", root.display()))?;
            *root = cwd.join(&*root);
        }
        if !root.is_dir() {
            return Err(format!("{} is not a folder", root.display()));
        }
    }
    // Under one folder a row says where it is from there; under several
    // that would not say which.
    let relative = roots.len() == 1;
    Ok(Definition {
        label,
        scan: ScanOptions {
            roots,
            max_depth: 0,
            directories_only,
            files_only: !directories_only,
            relative,
        },
        query: params.get("query").cloned().unwrap_or_default(),
    })
}

/// What a walk has found so far, as a picker searches it: all but the
/// folders the walk was given, which are entries of it too.
struct Files(Arc<PublishedSnapshot>);
impl ItemsSource for Files {
    type Item = String;
    fn version(&self) -> u64 {
        self.0.version()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn get_string<'a>(
        &'a self,
        index: usize,
        stack_buffer: &'a mut [u8],
        heap_buffer: &'a mut Vec<u8>,
    ) -> &'a [u8] {
        self.0.get_string(index, stack_buffer, heap_buffer)
    }
    fn get_string_lossy(&self, node_index: usize, out: &mut Vec<u8>) -> String {
        self.0.get_string_lossy(node_index, out)
    }
    fn item(&self, node_index: usize) -> Option<String> {
        let path = self.0.absolute_path(node_index)?;
        Some(path.to_string_lossy().into_owned())
    }
    fn create_search_plan(&self, query: &str) -> Box<dyn SearchPlan + '_> {
        Box::new(Inside { plan: self.0.create_search_plan(query), found: &self.0 })
    }
}
struct Inside<'a> {
    plan: Box<dyn SearchPlan + 'a>,
    found: &'a PublishedSnapshot,
}
impl SearchPlan for Inside<'_> {
    fn fuzzy_query(&self) -> &str {
        self.plan.fuzzy_query()
    }
    fn filters_items(&self) -> bool {
        true
    }
    fn includes(&self, index: usize) -> bool {
        !self.found.is_root(index) && self.plan.includes(index)
    }
    fn compare(&self, left: usize, right: usize) -> std::cmp::Ordering {
        self.plan.compare(left, right)
    }
    fn has_custom_sort(&self) -> bool {
        self.plan.has_custom_sort()
    }
    fn display_text(&self, index: usize) -> Option<String> {
        self.plan.display_text(index)
    }
}
/// A walk's store, as what a search of `Files` reads.
struct Walk(Arc<FileSystemSearchStore>);
impl SearchSnapshotProvider<Files> for Walk {
    fn snapshot(&self) -> Option<Arc<Files>> {
        Some(Arc::new(Files(SearchSnapshotProvider::snapshot(&*self.0)?)))
    }
    fn snapshot_version(&self) -> u64 {
        self.0.snapshot_version()
    }
    fn is_done(&self) -> bool {
        self.0.is_done()
    }
    fn subscribe_updates(&self, wake: crossbeam_channel::Sender<()>) -> bool {
        self.0.subscribe_updates(wake)
    }
    fn skipped_items(&self) -> usize {
        self.0.skipped_items()
    }
}

/// When a UI is sent what was found. An answer to a query not yet answered
/// goes at once. More found for a query already answered, as while a walk
/// goes on, goes only once the UI has said it took the last it was sent:
/// until then the newest waits, in place of any that waited before it. So
/// no more is queued for a UI than it takes, however slowly it does.
struct Flow<T> {
    /// Counts what was sent, from one.
    serial: u64,
    /// The revision last asked for.
    asked: Option<u64>,
    /// Whether that one has been answered.
    answered: bool,
    /// The serial of what was sent and not yet said to be taken.
    unseen: Option<u64>,
    waiting: Option<T>,
}
impl<T> Flow<T> {
    fn new() -> Self {
        Self { serial: 0, asked: None, answered: false, unseen: None, waiting: None }
    }
    fn asked(&mut self, revision: u64) {
        self.asked = Some(revision);
        self.answered = false;
        self.waiting = None;
    }
    /// What was found for `revision`: to send now with the serial given, or
    /// nothing to send yet, or ever for a query that is no longer the one.
    fn found(&mut self, revision: u64, what: T) -> Option<(u64, T)> {
        if self.asked != Some(revision) {
            return None;
        }
        if self.answered && self.unseen.is_some() {
            self.waiting = Some(what);
            return None;
        }
        self.answered = true;
        Some(self.send(what))
    }
    /// The UI took what was sent with `serial`.
    fn seen(&mut self, serial: u64) -> Option<(u64, T)> {
        if self.unseen != Some(serial) {
            return None;
        }
        self.unseen = None;
        let waiting = self.waiting.take()?;
        Some(self.send(waiting))
    }
    fn send(&mut self, what: T) -> (u64, T) {
        self.serial += 1;
        self.unseen = Some(self.serial);
        (self.serial, what)
    }
}

/// What was found for a query, before it has a serial.
struct Answer {
    update: FuzzySearchUpdate,
    source: Arc<Files>,
}

struct Picker {
    number: u32,
    pane: PaneId,
    /// The UI that shows it: its client number and the way to it.
    ui: u64,
    ui_outbox: Outbox,
    /// The way to the program that asked.
    program: Outbox,
    request_id: Value,
    store: Arc<FileSystemSearchStore>,
    scan: FileWalkerScan,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    search: Option<FuzzySearchSession<Files, Walk>>,
    /// The revision of each query the search was given, in order: the
    /// search counts them from none, and says which it found for.
    revisions: Vec<u64>,
    rows: usize,
    flow: Option<Flow<Answer>>,
    /// What the last answers sent were of, by serial.
    sent: VecDeque<(u64, Arc<Files>)>,
    query: String,
}

/// How a picker ended.
enum Outcome {
    Accepted { row: u64, text: String, value: String },
    Cancelled(&'static str),
    Error { code: &'static str, message: String },
}

/// The pickers under way.
#[derive(Default)]
pub(crate) struct Pickers {
    next: AtomicU32,
    open: Mutex<HashMap<u32, Arc<Picker>>>,
}

/// Where a program's picker is to be shown.
pub(crate) struct Showing {
    pub pane: PaneId,
    pub ui: u64,
    pub ui_outbox: Outbox,
    pub program: Outbox,
}

fn reply(to: &Outbox, value: Value) {
    let _ = to.send(Frame::Shell { channel: 0, bytes: value.to_string().into_bytes() }.encode());
}
fn result(to: &Outbox, request_id: &Value, status: &str, extra: Value) {
    let mut value = json!({"type": "result", "request_id": request_id, "status": status});
    if let (Some(to), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        to.extend(extra.clone());
    }
    reply(to, value);
}

impl Pickers {
    /// A program's message. `mine` is the picker it has open, if it has.
    /// Returns false when the conversation is over.
    pub(crate) fn said(&self, bytes: &[u8], mine: &mut Option<u32>, showing: impl FnOnce() -> Result<Showing, String>, program: &Outbox) -> bool {
        let said = if bytes.len() > MAX_REQUEST { None } else { serde_json::from_slice(bytes).ok() };
        match said {
            Some(Said::Open { request_id, picker, params }) if mine.is_none() => {
                let refuse = |code: &'static str, message: String| {
                    log(format_args!("picker {picker} refused: {message}"));
                    result(program, &request_id, "error", json!({"code": code, "message": message}));
                    false
                };
                let definition = match define(&picker, &params) {
                    Ok(definition) => definition,
                    Err(message) => return refuse("bad_request", message),
                };
                let showing = match showing() {
                    Ok(showing) => showing,
                    Err(message) => return refuse("no_window", message),
                };
                *mine = Some(self.open(definition, request_id, showing));
                reply(program, json!({"type": "opened"}));
                true
            }
            Some(Said::Cancel) => {
                if let Some(number) = *mine {
                    self.end(number, Outcome::Cancelled("cancelled"), true);
                }
                false
            }
            // Not understood, or a second picker asked for: left unanswered,
            // as a message from a newer program would be.
            _ => true,
        }
    }

    fn open(&self, definition: Definition, request_id: Value, showing: Showing) -> u32 {
        limit_search_threads();
        let number = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let store = Arc::new(FileSystemSearchStore::new());
        log(format_args!(
            "picker {number}: {} under {:?} for pane {}",
            definition.label, definition.scan.roots, showing.pane
        ));
        let scan = start_scan(definition.scan, store.clone());
        let picker = Arc::new(Picker {
            number,
            pane: showing.pane,
            ui: showing.ui,
            ui_outbox: showing.ui_outbox,
            program: showing.program,
            request_id,
            store,
            scan,
            state: Mutex::new(State { flow: Some(Flow::new()), ..Default::default() }),
        });
        self.open.lock().unwrap().insert(number, picker.clone());
        picker.tell_ui(Control::PickerOpen {
            picker: number,
            pane: picker.pane,
            label: definition.label.to_owned(),
            query: definition.query,
        });
        number
    }

    /// The picker `number`, if UI `ui` is the one that shows it.
    fn shown_by(&self, number: u32, ui: u64) -> Option<Arc<Picker>> {
        self.open.lock().unwrap().get(&number).filter(|picker| picker.ui == ui).cloned()
    }

    /// What the UI `ui` said of a picker. False for a message that is not
    /// about pickers.
    pub(crate) fn from_ui(&self, ui: u64, control: &Control) -> bool {
        match control {
            Control::PickerQuery { picker, revision, query, rows } => {
                if let Some(picker) = self.shown_by(*picker, ui) {
                    picker.query(*revision, query.clone(), *rows as usize);
                }
            }
            Control::PickerSeen { picker, serial } => {
                if let Some(picker) = self.shown_by(*picker, ui) {
                    picker.seen(*serial);
                }
            }
            Control::PickerAccept { picker, serial, row } => {
                if let Some(found) = self.shown_by(*picker, ui) {
                    let outcome = found.chosen(*serial, *row);
                    self.end(*picker, outcome, false);
                }
            }
            Control::PickerClose { picker, refused } => {
                if self.shown_by(*picker, ui).is_some() {
                    let outcome = match refused {
                        Some(message) => Outcome::Error { code: "refused", message: message.clone() },
                        None => Outcome::Cancelled("picker_closed"),
                    };
                    self.end(*picker, outcome, false);
                }
            }
            _ => return false,
        }
        true
    }

    /// End every picker `which` picks: its UI has left its pane or gone, or
    /// its pane's shell has ended. The UI is told unless it has gone.
    pub(crate) fn end_where(&self, which: impl Fn(PaneId, u64) -> bool, tell_ui: bool) {
        let ended: Vec<u32> = {
            let open = self.open.lock().unwrap();
            open.values().filter(|p| which(p.pane, p.ui)).map(|p| p.number).collect()
        };
        for number in ended {
            self.end(number, Outcome::Cancelled("window_left"), tell_ui);
        }
    }

    /// The program that asked for `number` has gone.
    pub(crate) fn program_gone(&self, number: u32) {
        self.end(number, Outcome::Cancelled("program_left"), true);
    }

    fn end(&self, number: u32, outcome: Outcome, tell_ui: bool) {
        let Some(picker) = self.open.lock().unwrap().remove(&number) else { return };
        picker.scan.cancel();
        let query = {
            let mut state = picker.state.lock().unwrap();
            if let Some(search) = state.search.take() {
                search.stop();
            }
            state.flow = None;
            state.sent.clear();
            std::mem::take(&mut state.query)
        };
        let how = match &outcome {
            Outcome::Accepted { .. } => "accepted".to_owned(),
            Outcome::Cancelled(reason) => format!("cancelled ({reason})"),
            Outcome::Error { message, .. } => format!("failed: {message}"),
        };
        log(format_args!("picker {number}: {how}"));
        let message = match &outcome {
            Outcome::Error { message, .. } => Some(message.clone()),
            _ => None,
        };
        match outcome {
            Outcome::Accepted { row, text, value } => result(
                &picker.program,
                &picker.request_id,
                "accepted",
                json!({"selected": [{"id": row.to_string(), "text": text, "value": value}], "query": query}),
            ),
            Outcome::Cancelled(reason) => {
                result(&picker.program, &picker.request_id, "cancelled", json!({"reason": reason}))
            }
            Outcome::Error { code, message } => result(
                &picker.program,
                &picker.request_id,
                "error",
                json!({"code": code, "message": message}),
            ),
        }
        // Write what was queued for the program, then close its connection.
        let _ = picker.program.send(Vec::new());
        if tell_ui {
            picker.tell_ui(Control::PickerClosed { picker: number, message });
        }
    }
}

impl Picker {
    fn tell_ui(&self, control: Control) {
        let _ = self.ui_outbox.send(Frame::Control(control).encode());
    }

    fn query(self: &Arc<Self>, revision: u64, query: String, rows: usize) {
        let mut state = self.state.lock().unwrap();
        let Some(flow) = &mut state.flow else { return };
        flow.asked(revision);
        state.rows = rows.clamp(1, MOST_ROWS);
        state.query = query.clone();
        state.revisions.push(revision);
        if let Some(search) = &state.search {
            search.set_query(query);
            return;
        }
        let picker = Arc::downgrade(self);
        let search = FuzzySearchSession::new_with_snapshot_observer(
            u64::from(self.number),
            Arc::new(Walk(self.store.clone())),
            query,
            SearchSortMode::Score,
            move |update, source| {
                if let Some(picker) = picker.upgrade() {
                    picker.found(Answer { update, source });
                }
            },
        );
        search.start();
        state.search = Some(search);
    }

    /// The search found something, for the query it was given the
    /// `generation`th time.
    fn found(&self, answer: Answer) {
        let mut state = self.state.lock().unwrap();
        let Some(revision) = state.revisions.get(answer.update.generation as usize).copied() else {
            return;
        };
        let Some(flow) = &mut state.flow else { return };
        if let Some((serial, answer)) = flow.found(revision, answer) {
            self.send(&mut state, serial, revision, answer);
        }
    }

    fn seen(&self, serial: u64) {
        let mut state = self.state.lock().unwrap();
        let Some(flow) = &mut state.flow else { return };
        let Some(revision) = flow.asked else { return };
        if let Some((serial, answer)) = flow.seen(serial) {
            self.send(&mut state, serial, revision, answer);
        }
    }

    fn send(&self, state: &mut State, serial: u64, revision: u64, answer: Answer) {
        let Answer { update, source } = answer;
        let rows = update
            .results
            .into_iter()
            .take(state.rows)
            .map(|result| PickerRow { index: result.node_index as u64, text: result.path })
            .collect();
        state.sent.push_back((serial, source.clone()));
        while state.sent.len() > KEPT_ANSWERS {
            state.sent.pop_front();
        }
        self.tell_ui(Control::PickerRows {
            picker: self.number,
            serial,
            revision,
            matched_against: source.effective_query(&update.query),
            query: update.query,
            header: None,
            rows,
            matched: update.matched as u64,
            total: update.total as u64,
            skipped: self.store.skipped_items() as u64,
            done: update.source_done,
            version: update.source_version,
        });
    }

    /// The row the UI chose, of those sent with `serial`.
    fn chosen(&self, serial: u64, row: u64) -> Outcome {
        let state = self.state.lock().unwrap();
        let source = state.sent.iter().find(|(sent, _)| *sent == serial).map(|(_, source)| source);
        let chosen = source.and_then(|source| {
            let index = usize::try_from(row).ok().filter(|index| *index < source.len())?;
            let text = source.get_string_lossy(index, &mut Vec::new());
            Some((text, source.item(index)?))
        });
        match chosen {
            Some((text, value)) => Outcome::Accepted { row, text, value },
            None => Outcome::Error {
                code: "no_such_row",
                message: format!("row {row} of answer {serial} is not one the picker has"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_is_answered_at_once_and_more_waits_to_be_taken() {
        let mut flow = Flow::new();
        // Nothing was asked: nothing found is for anything.
        assert_eq!(flow.found(0, "early"), None);
        flow.asked(0);
        assert_eq!(flow.found(0, "first"), Some((1, "first")));
        // More for the same query waits, the newest in place of the rest.
        assert_eq!(flow.found(0, "second"), None);
        assert_eq!(flow.found(0, "third"), None);
        // Taking something else changes nothing.
        assert_eq!(flow.seen(7), None);
        assert_eq!(flow.seen(1), Some((2, "third")));
        assert_eq!(flow.seen(2), None);
        // With nothing sent and not taken, more goes at once.
        assert_eq!(flow.found(0, "fourth"), Some((3, "fourth")));
    }

    #[test]
    fn a_new_query_does_not_wait_and_an_old_one_is_not_answered() {
        let mut flow = Flow::new();
        flow.asked(0);
        assert_eq!(flow.found(0, "first"), Some((1, "first")));
        assert_eq!(flow.found(0, "waits"), None);
        flow.asked(1);
        // What waited was for the query before, and is not sent.
        assert_eq!(flow.seen(1), None);
        assert_eq!(flow.found(0, "late"), None);
        assert_eq!(flow.found(1, "new"), Some((2, "new")));
        // The first answer to a query goes whether the last was taken or not.
        flow.asked(2);
        assert_eq!(flow.found(2, "newer"), Some((3, "newer")));
        assert_eq!(flow.found(2, "more"), None);
        assert_eq!(flow.seen(3), Some((4, "more")));
    }

    #[test]
    fn a_picker_is_of_files_or_folders_under_roots_or_the_folder_the_program_is_in() {
        let here = std::env::temp_dir();
        let params = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        let cwd = here.to_string_lossy().into_owned();
        let files = define("files", &params(&[("cwd", &cwd), ("query", "ab")])).unwrap();
        assert_eq!(files.scan.roots, [here.clone()]);
        assert!(files.scan.files_only && !files.scan.directories_only && files.scan.relative);
        assert_eq!(files.query, "ab");

        let roots = serde_json::to_string(&[&cwd, "."]).unwrap();
        let folders = define("directories", &params(&[("cwd", &cwd), ("roots", &roots)])).unwrap();
        assert_eq!(folders.scan.roots, [here.clone(), here.join(".")]);
        assert!(folders.scan.directories_only && !folders.scan.relative);

        for (name, given) in [
            ("themes", params(&[("cwd", &cwd)])),
            ("files", params(&[])),
            ("files", params(&[("roots", "[\"relative\"]")])),
            ("files", params(&[("roots", "not json"), ("cwd", &cwd)])),
            ("files", params(&[("cwd", "Z:\\no\\such\\folder\\anywhere")])),
        ] {
            assert!(define(name, &given).is_err(), "{name} with {given:?} was taken");
        }
    }
}

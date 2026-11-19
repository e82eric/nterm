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
//!   for a picker, and is answered `{"type":"opened","request_id":ID}` once a UI has been
//!   asked to show it;
//! - `{"type":"items","request_id":ID,"sequence":N,"items":[{"id":..,
//!   "text":..,"value":..}]}` gives the picker `items` rows, and is
//!   answered `{"type":"ack","request_id":ID,"sequence":N}`;
//!   `{"type":"end_items","request_id":ID}` says there are no more, and is
//!   answered `{"type":"input_complete","request_id":ID}`;
//! - `{"type":"cancel"}`, or leaving, ends it;
//! - `{"type":"result","request_id":ID,"status":...}` is how it ended:
//!   `accepted` with `selected` and `query`, `cancelled`, or `error` with
//!   `code` and `message`.

use crate::configured::{Configured, Defined};
use crate::previewing::{Previewer, Shows};
use crate::log::log;
use nfm_file_system::{
    walker::{FileWalkerScan, PublishedSnapshot, ScanOptions, start_scan},
    walker_search_store::FileSystemSearchStore,
};
use nfm_picker_sources::{
    command::{self, CommandSourceOptions, CommandSpec},
    file::{self, FileSourceOptions},
    records::{StreamControl, StreamSink, StreamValues},
};
use nfm_search_core::{
    fuzzy_search_session::{FuzzySearchSession, FuzzySearchUpdate, SearchSnapshotProvider},
    search::SearchSortMode,
    snapshot_store::SnapshotStore,
    store::{
        ItemsSource, PublishingStreamingItemStoreWithPayload, SearchPlan, StreamingItemSnapshotWithPayload,
    },
};
use nterm_session::protocol::{Control, Frame, PaneId, PickerBinding, PickerRow, PickerStep, PreviewKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
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
/// The most rows a program can give a picker: more are left out and counted.
const MOST_GIVEN: usize = 100_000;
/// The most characters of the name a program gives a picker of its rows.
const MOST_LABEL: usize = 64;
/// The most lines a picker reads of a file or of what a command prints.
const MOST_LINES: usize = 1_000_000;

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
    Items {
        #[serde(default)]
        request_id: Value,
        #[serde(default)]
        sequence: Value,
        items: Vec<Item>,
    },
    EndItems {
        #[serde(default)]
        request_id: Value,
    },
    Cancel,
    #[serde(other)]
    Other,
}

/// One of the rows a program gives a picker of its own rows.
#[derive(Deserialize)]
struct Item {
    /// The program's name for it, which it is told of the one chosen.
    #[serde(default)]
    id: Option<String>,
    /// What the row shows, and is searched by.
    text: String,
    /// What the program is given when it is chosen: `text` when there is
    /// none.
    #[serde(default)]
    value: Option<Value>,
    /// What a preview of the machine's file may show for it, such as
    /// PowerShell's tooltip for a completion: its function is given it as
    /// `item.description`.
    #[serde(default)]
    description: Option<String>,
}

/// A picker the daemon has: what it lists.
struct Definition {
    label: String,
    lists: Lists,
    query: String,
    sort: SearchSortMode,
    /// Whether its rows have previews, and if so whether it opens showing
    /// the one of the selected row.
    preview: Option<bool>,
    /// Whether it is shown at the pane's cursor.
    at_cursor: bool,
    /// What says what shows for a row, for a picker of the machine's file.
    resolves: Option<Resolves>,
}
/// What a picker's rows are.
enum Lists {
    /// The files or the folders a walk finds.
    Walk(ScanOptions),
    /// The lines of a file: of what was typed at a shell, the newest first.
    File(FileSourceOptions),
    /// The lines a command prints, as a search of files does.
    Command(CommandSourceOptions),
    /// The lines a command prints, each split into columns of these names
    /// at `delimiter`.
    Table { command: CommandSourceOptions, names: Vec<String>, delimiter: String },
    /// Rows that are written in the file of the pickers.
    Fixed(Vec<crate::configured::Item>),
    /// What the program that asked gives, as `items`.
    Given,
}


/// The most of a file that is sent as its preview.
const MOST_PREVIEW: usize = 256 * 1024;
/// The most entries of a folder that are.
const MOST_LISTED: usize = 1000;
/// The most of a picture that is: one larger is not sent.
const MOST_PICTURE: u64 = 10 * 1024 * 1024;
/// The files that are pictures, by how their names end.
const PICTURES: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"];

/// What shows for a row: the start of a file, or what is in a folder.
#[derive(Debug, Default, PartialEq)]
struct Shown {
    text: String,
    /// The file's name, for one shown as it is.
    name: Option<String>,
    truncated: bool,
    error: Option<String>,
    /// The file itself, where it is a picture, in place of `text`.
    picture: Option<Vec<u8>>,
}
impl Shown {
    fn of(path: &Path) -> Self {
        let shown = if path.is_dir() { Self::folder(path) } else { Self::file(path) };
        shown.unwrap_or_else(|error| Self { error: Some(error.to_string()), ..Default::default() })
    }

    fn file(path: &Path) -> std::io::Result<Self> {
        use std::io::Read;
        let file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        // A picture is sent as it is, for the UI to draw.
        let extension = path.extension().map(|extension| extension.to_string_lossy().to_lowercase());
        if extension.is_some_and(|extension| PICTURES.contains(&extension.as_str())) {
            if size > MOST_PICTURE {
                return Ok(Self { text: format!("A picture too large to show: {size} bytes"), ..Default::default() });
            }
            let mut bytes = Vec::new();
            (&file).take(MOST_PICTURE).read_to_end(&mut bytes)?;
            if !bytes.is_empty() {
                return Ok(Self { picture: Some(bytes), ..Default::default() });
            }
            return Ok(Self { text: "An empty file".into(), ..Default::default() });
        }
        let mut bytes = Vec::new();
        file.take(MOST_PREVIEW as u64).read_to_end(&mut bytes)?;
        // A file with a zero byte near its start is not one to read as text.
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            return Ok(Self { text: format!("Not text: {size} bytes"), ..Default::default() });
        }
        let truncated = size > bytes.len() as u64;
        if truncated {
            // Not half a line, nor half a character.
            let whole = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1);
            bytes.truncate(whole);
        }
        Ok(Self {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            name: path.file_name().map(|name| name.to_string_lossy().into_owned()),
            truncated,
            error: None,
            picture: None,
        })
    }

    /// Folders first, each with a `\` after it, then files, by name.
    fn folder(path: &Path) -> std::io::Result<Self> {
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in std::fs::read_dir(path)? {
            if entries.len() == MOST_LISTED {
                truncated = true;
                break;
            }
            let entry = entry?;
            let folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
            entries.push((!folder, entry.file_name().to_string_lossy().into_owned()));
        }
        entries.sort_by(|a, b| (a.0, a.1.to_lowercase()).cmp(&(b.0, b.1.to_lowercase())));
        let lines: Vec<String> = entries
            .into_iter()
            .map(|(file, name)| if file { name } else { format!("{name}\\") })
            .collect();
        let text = if lines.is_empty() { "Empty folder".to_owned() } else { lines.join("\n") };
        Ok(Self { text, truncated, ..Default::default() })
    }
}

/// `path` with a `~` it starts with, alone or before a separator, taken to
/// be this machine's home folder.
fn in_home(path: PathBuf) -> Result<PathBuf, String> {
    let Some(text) = path.to_str() else { return Ok(path) };
    let rest = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => rest.trim_start_matches(['/', '\\']),
        _ => return Ok(path),
    };
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    let home = PathBuf::from(home.ok_or("there is no home folder to read ~ as")?);
    Ok(if rest.is_empty() { home } else { home.join(rest) })
}

/// The one picker the daemon has of its own, `items`: what the program
/// that asked gives after it, called `label`. It starts with `query`, and is
/// shown at the pane's cursor when `position` is `cursor`. Any other is the
/// machine's file's to say: see `configured`.
fn define(name: &str, params: &BTreeMap<String, String>) -> Result<Definition, String> {
    if name != "items" {
        return Err(format!("no picker named {name}"));
    }
    let given = params.get("label").map(|label| label.trim()).filter(|label| !label.is_empty());
    Ok(Definition {
        label: given.unwrap_or("Items").chars().take(MOST_LABEL).collect(),
        lists: Lists::Given,
        query: params.get("query").cloned().unwrap_or_default(),
        sort: SearchSortMode::Score,
        preview: None,
        at_cursor: params.get("position").is_some_and(|position| position == "cursor"),
        resolves: None,
    })
}

/// The folders under `roots`, no more than `depth` levels down or all of
/// them with 0, each as its whole path: what a UI asks for to start a
/// workspace in (see `Control::FoldersAsk`). A root that starts with `~` is
/// in this machine's home folder.
fn search_of_folders(roots: &[String], depth: i32) -> Result<Definition, String> {
    if roots.is_empty() {
        return Err("no folders to look under were given".into());
    }
    let mut whole = Vec::with_capacity(roots.len());
    for root in roots {
        let root = in_home(PathBuf::from(root))?;
        if root.is_relative() || !root.is_dir() {
            return Err(format!("{} is not a folder", root.display()));
        }
        whole.push(root);
    }
    Ok(Definition {
        label: "Directories".into(),
        lists: Lists::Walk(ScanOptions {
            roots: whole,
            max_depth: depth.max(0),
            directories_only: true,
            files_only: false,
            relative: false,
        }),
        query: String::new(),
        sort: SearchSortMode::Score,
        preview: Some(false),
        at_cursor: false,
        resolves: None,
    })
}

/// The names of the pickers the daemon has of its own. The file of the
/// machine's pickers has none of these names: see `configured`.
pub(crate) const BUILT_IN: [&str; 1] = ["items"];

/// What is given for `{name}`, where `text` is that and no more.
fn given<'a>(text: &'a str, params: &'a BTreeMap<String, String>) -> Result<&'a str, String> {
    match text.strip_prefix('{').and_then(|text| text.strip_suffix('}')) {
        Some(name) => {
            params.get(name).map(String::as_str).ok_or_else(|| format!("it was not opened with {name}"))
        }
        None => Ok(text),
    }
}

/// A picker of the machine's file, opened with `params`. They fill what
/// the file leaves to be given, as `{name}`, and say where: they are never
/// which program is run, and each is one argument whole, whatever is in it.
fn configured(
    name: &str,
    defined: Defined,
    file: Option<PathBuf>,
    params: &BTreeMap<String, String>,
) -> Result<Definition, String> {
    use crate::configured::{Lists as Of, Position, Sort};
    let cwd = params.get("cwd").map(PathBuf::from);
    if let Some(cwd) = cwd.as_ref().filter(|cwd| cwd.is_relative() || !cwd.is_dir()) {
        return Err(format!("{} is not a folder", cwd.display()));
    }
    let Defined { presentation, lists, preview: resolved, action: acts, keys, context } = defined;
    let mut preview = None;
    let lists = match lists {
        Of::Command { args, cwd: folder, line_continuation, success_exit_codes, unique, columns, delimiter, .. } => {
            let mut filled = Vec::with_capacity(args.len());
            for argument in &args {
                let argument = given(argument, params)?;
                if argument.contains('\0') {
                    return Err("an argument has a NUL in it".into());
                }
                filled.push(argument.to_owned());
            }
            let mut command = CommandSpec::new(filled.remove(0));
            command.arguments = filled;
            // Where it was opened, or where the file says, in that order.
            command.working_directory = cwd.or(folder.map(PathBuf::from));
            if let Some(folder) = command.working_directory.as_ref().filter(|folder| !folder.is_dir()) {
                return Err(format!("{} is not a folder", folder.display()));
            }
            command.output_limit = 16 * 1024;
            let mut options = CommandSourceOptions::new(command);
            options.line_continuation = line_continuation;
            options.unique = unique.unwrap_or(false);
            if let Some(codes) = success_exit_codes {
                options.success_exit_codes = codes;
            }
            // With columns, each line is split into them and they show
            // lined up, under their names.
            match columns {
                None if delimiter.is_some() => return Err("delimiter is for a picker with columns".into()),
                None => Lists::Command(options),
                Some(names) => {
                    if names.is_empty() || names.len() > 32 {
                        return Err("it has from 1 to 32 columns".into());
                    }
                    if options.line_continuation.is_some() || options.unique {
                        return Err("columns do not go with line_continuation or unique".into());
                    }
                    nfm_picker_sources::structured::StructuredSchema::new(names.clone())
                        .map_err(|error| format!("columns: {error}"))?;
                    let delimiter = delimiter.unwrap_or_else(|| "\t".to_owned());
                    if delimiter.is_empty() {
                        return Err("delimiter is empty".into());
                    }
                    Lists::Table { command: options, names, delimiter }
                }
            }
        }
        Of::File { path, reverse, line_continuation, unique, .. } => {
            let mut path = PathBuf::from(given(&path, params)?);
            if path.is_relative() {
                path = cwd.ok_or_else(|| format!("{} is not a whole path", path.display()))?.join(path);
            }
            if !path.is_file() {
                return Err(format!("{} is not a file", path.display()));
            }
            let mut options = FileSourceOptions::new(path);
            options.reverse = reverse;
            options.line_continuation = line_continuation;
            options.unique = unique.unwrap_or(false);
            Lists::File(options)
        }
        Of::Items { items: Some(items), .. } => Lists::Fixed(items),
        // None written: the program that opens it gives them.
        Of::Items { items: None, .. } => Lists::Given,
        Of::FileSystem { roots, max_depth, files_only, directories_only, relative, .. } => {
            let mut roots: Vec<PathBuf> = match params.get("roots") {
                Some(roots) => serde_json::from_str::<Vec<String>>(roots)
                    .map_err(|error| format!("roots is not a list of folders: {error}"))?
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
                None => roots.into_iter().map(PathBuf::from).collect(),
            };
            if roots.is_empty() {
                roots.push(cwd.ok_or("it has no roots, and was opened with no cwd")?);
            }
            if let Some(root) = roots.iter().find(|root| root.is_relative() || !root.is_dir()) {
                return Err(format!("{} is not a folder", root.display()));
            }
            if max_depth.is_some_and(|depth| depth < 0) {
                return Err("max_depth is less than nothing".into());
            }
            // Its rows are files and folders: they show as those of the
            // daemon's own pickers of them do.
            // Whether it opens showing them is the file's to say, as of
            // any picker of the file.
            preview = Some(presentation.preview_visible);
            Lists::Walk(ScanOptions {
                roots,
                max_depth: max_depth.unwrap_or(i32::MAX),
                directories_only,
                files_only,
                relative,
            })
        }
    };
    // Where the file says what shows for a row, every row has a preview,
    // and it is the file's and not the daemon's own.
    let resolves = match file {
        Some(file) if resolved || acts || !keys.is_empty() => {
            if resolved {
                // The file says whether it opens showing them: what a
                // window would do with a picker of its own does not.
                preview = Some(presentation.preview_visible);
            }
            Some(Resolves { file, name: name.to_owned(), params: params.clone(), preview: resolved, action: acts, keys, context })
        }
        _ => None,
    };
    Ok(Definition {
        label: name.chars().take(MOST_LABEL).collect(),
        lists,
        query: params.get("query").cloned().or(presentation.initial_query).unwrap_or_default(),
        sort: match presentation.sort {
            Sort::Score => SearchSortMode::Score,
            Sort::SourceOrder => SearchSortMode::SourceOrder,
        },
        preview,
        at_cursor: presentation.position == Position::Cursor
            || params.get("position").is_some_and(|position| position == "cursor"),
        resolves,
    })
}

/// Rows that are lines of text: of a file, or what a program gave.
type Lines = StreamingItemSnapshotWithPayload<()>;
/// Rows of columns: see `Lists::Table`.
type Table = nfm_picker_sources::structured::StructuredStreamingSnapshot;

/// What a picker lists so far, as it is searched.
enum Listed {
    /// What a walk has found: all but the folders the walk was given, which
    /// are entries of it too.
    Files(Arc<PublishedSnapshot>),
    Lines(Arc<Lines>),
    /// Lines split into columns, which show lined up under their names.
    Table(Arc<Table>),
}
impl ItemsSource for Listed {
    type Item = String;
    fn version(&self) -> u64 {
        match self {
            Self::Files(found) => found.version(),
            Self::Lines(lines) => lines.version(),
            Self::Table(table) => table.version(),
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::Files(found) => found.len(),
            Self::Lines(lines) => lines.len(),
            Self::Table(table) => table.len(),
        }
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn get_string<'a>(
        &'a self,
        index: usize,
        stack_buffer: &'a mut [u8],
        heap_buffer: &'a mut Vec<u8>,
    ) -> &'a [u8] {
        match self {
            Self::Files(found) => found.get_string(index, stack_buffer, heap_buffer),
            Self::Lines(lines) => lines.get_string(index, stack_buffer, heap_buffer),
            Self::Table(table) => table.get_string(index, stack_buffer, heap_buffer),
        }
    }
    fn get_string_lossy(&self, node_index: usize, out: &mut Vec<u8>) -> String {
        match self {
            Self::Files(found) => found.get_string_lossy(node_index, out),
            Self::Lines(lines) => lines.get_string_lossy(node_index, out),
            Self::Table(table) => table.get_string_lossy(node_index, out),
        }
    }
    /// What a row is: the whole path of a file, and for a line what it
    /// shows.
    fn item(&self, node_index: usize) -> Option<String> {
        match self {
            Self::Files(found) => Some(found.absolute_path(node_index)?.to_string_lossy().into_owned()),
            Self::Lines(lines) => {
                (node_index < lines.len()).then(|| lines.get_string_lossy(node_index, &mut Vec::new()))
            }
            Self::Table(table) => (node_index < table.len()).then(|| table.value(node_index)),
        }
    }
    // A table's rows show lined up under its columns' names, and it reads
    // a query its own way: a column may be named in it.
    fn header(&self, query: &str) -> Option<String> {
        match self {
            Self::Table(table) => ItemsSource::header(&**table, query),
            _ => None,
        }
    }
    fn display_text(&self, node_index: usize, query: &str) -> Option<String> {
        match self {
            Self::Table(table) => ItemsSource::display_text(&**table, node_index, query),
            _ => None,
        }
    }
    fn effective_query(&self, input: &str) -> String {
        match self {
            Self::Files(found) => found.effective_query(input),
            Self::Lines(lines) => lines.effective_query(input),
            Self::Table(table) => table.effective_query(input),
        }
    }
    fn create_search_plan(&self, query: &str) -> Box<dyn SearchPlan + '_> {
        match self {
            Self::Files(found) => Box::new(Inside { plan: found.create_search_plan(query), found }),
            Self::Lines(lines) => lines.create_search_plan(query),
            Self::Table(table) => table.create_search_plan(query),
        }
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
/// Where what a picker lists is added to, as what a search of `Listed`
/// reads.
enum Found {
    Walk(Arc<FileSystemSearchStore>),
    Lines(Arc<SnapshotStore<Lines>>),
    Table(Arc<SnapshotStore<Table>>),
}
impl SearchSnapshotProvider<Listed> for Found {
    fn snapshot(&self) -> Option<Arc<Listed>> {
        Some(Arc::new(match self {
            Self::Walk(store) => Listed::Files(SearchSnapshotProvider::snapshot(&**store)?),
            Self::Lines(store) => Listed::Lines(SearchSnapshotProvider::snapshot(&**store)?),
            Self::Table(store) => Listed::Table(SearchSnapshotProvider::snapshot(&**store)?),
        }))
    }
    fn snapshot_version(&self) -> u64 {
        match self {
            Self::Walk(store) => SearchSnapshotProvider::snapshot_version(&**store),
            Self::Lines(store) => SearchSnapshotProvider::snapshot_version(&**store),
            Self::Table(store) => SearchSnapshotProvider::snapshot_version(&**store),
        }
    }
    fn is_done(&self) -> bool {
        match self {
            Self::Walk(store) => SearchSnapshotProvider::is_done(&**store),
            Self::Lines(store) => SearchSnapshotProvider::is_done(&**store),
            Self::Table(store) => SearchSnapshotProvider::is_done(&**store),
        }
    }
    fn subscribe_updates(&self, wake: crossbeam_channel::Sender<()>) -> bool {
        match self {
            Self::Walk(store) => store.subscribe_updates(wake),
            Self::Lines(store) => store.subscribe_updates(wake),
            Self::Table(store) => store.subscribe_updates(wake),
        }
    }
    fn skipped_items(&self) -> usize {
        match self {
            Self::Walk(store) => store.skipped_items(),
            Self::Lines(store) => store.skipped_items(),
            Self::Table(store) => store.skipped_items(),
        }
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
#[derive(Clone)]
struct Answer {
    update: Arc<FuzzySearchUpdate>,
    source: Arc<Listed>,
}

/// What a picker lists, and how that is added to and stopped.
enum Source {
    Walk { store: Arc<FileSystemSearchStore>, scan: FileWalkerScan },
    /// Lines read as they come, of a file or of what a command prints, on
    /// a thread that stops when `cancelled`.
    /// `whole` has, by row, what a row is where that is not what it shows:
    /// a command of several lines, which shows as one.
    Read { store: Arc<SnapshotStore<Lines>>, cancelled: Arc<AtomicBool>, whole: StreamValues },
    /// Lines a command prints, split into columns. `whole` has each line
    /// as it was printed, which is what its row is.
    Table { store: Arc<SnapshotStore<Table>>, cancelled: Arc<AtomicBool>, whole: StreamValues },
    /// What the program gives.
    Given { store: Arc<SnapshotStore<Lines>>, given: Mutex<Given> },
}
/// The rows a program has given.
struct Given {
    /// Where they are added, until the program says it has no more.
    adding: Option<PublishingStreamingItemStoreWithPayload<()>>,
    /// By row: the program's name for it, what it is, and what it said of it.
    rows: Vec<(String, Value, Option<String>)>,
}
impl Source {
    fn found(&self) -> Found {
        match self {
            Self::Walk { store, .. } => Found::Walk(store.clone()),
            Self::Read { store, .. } | Self::Given { store, .. } => Found::Lines(store.clone()),
            Self::Table { store, .. } => Found::Table(store.clone()),
        }
    }
    fn skipped(&self) -> usize {
        self.found().skipped_items()
    }
    fn stop(&self) {
        match self {
            Self::Walk { scan, .. } => scan.cancel(),
            Self::Read { cancelled, .. } | Self::Table { cancelled, .. } => cancelled.store(true, Ordering::Release),
            Self::Given { given, .. } => given.lock().unwrap().adding = None,
        }
    }
}

/// A row of a picker: what a program is told of the one chosen.
struct Row {
    id: String,
    text: String,
    value: Value,
    /// What the program that gave it said of it.
    description: Option<String>,
}

struct Picker {
    number: u32,
    /// The pane it is over: none for a search of folders a UI asked for.
    pane: Option<PaneId>,
    /// The UI that shows it: its client number and the way to it.
    ui: u64,
    ui_outbox: Outbox,
    /// Who asked for it, and is told how it ended.
    asker: Asker,
    source: Source,
    sort: SearchSortMode,
    /// The way to the thread that reads previews, once one was asked for.
    previews: Mutex<Option<std::sync::mpsc::Sender<(u64, Wanted)>>>,
    /// The function of the machine's file that says what shows for a row.
    resolves: Option<Resolves>,
    /// The preview the UI last asked for, by its number for it: what is
    /// being found for one before it is stopped. The most there is once
    /// the picker has ended.
    wanted: Arc<AtomicU64>,
    /// The pickers under way, of which this is one: its own thread ends it.
    open: std::sync::Weak<Mutex<HashMap<u32, Arc<Picker>>>>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    search: Option<FuzzySearchSession<Listed, Found>>,
    /// The revision of each query the search was given, in order: the
    /// search counts them from none, and says which it found for.
    revisions: Vec<u64>,
    rows: usize,
    flow: Option<Flow<Answer>>,
    /// What the last answers sent were of, by serial.
    sent: VecDeque<(u64, Arc<Listed>)>,
    query: String,
    /// What the search last found for that query.
    last: Option<Answer>,
}

/// Who asked for a picker.
enum Asker {
    /// A program in the pane's shell: the way to it, and its name for what
    /// it asked.
    Program { to: Outbox, request_id: Value },
    /// The UI that shows it, as when its user asked with a key.
    Ui,
}

/// How a picker ended.
enum Outcome {
    Accepted(Row),
    Cancelled(&'static str),
    Error { code: &'static str, message: String },
    /// The picker's own file did what is done with the row chosen.
    Acted,
}

/// What the thread that finds a picker's previews is asked for.
enum Wanted {
    /// A file or a folder.
    File(PathBuf),
    /// What the picker's file says shows for a row: what the row shows as,
    /// what it is, and what the program that gave it said of it.
    Row { text: String, value: String, description: Option<String> },
    /// A row to do what the picker's file says is done with one.
    Act { text: String, value: String, description: Option<String> },
    /// A row one of the picker's own keys was pressed on, and which key.
    Key { key: usize, text: String, value: String, description: Option<String> },
}

/// Where the function is that says what shows for a row of a picker of the
/// machine's file, and what the picker was opened with: see `previewing`.
#[derive(Clone)]
struct Resolves {
    file: PathBuf,
    name: String,
    params: BTreeMap<String, String>,
    /// Whether the file says what shows for a row, and what is done with
    /// the one chosen.
    preview: bool,
    action: bool,
    keys: Vec<PickerBinding>,
    /// What a picker that is a function was called with.
    context: Option<serde_json::Value>,
}

/// To a UI: there is nothing to show for the row, and why.
fn said(picker: u32, request: u64, error: String) -> Control {
    Control::PickerPreviewed {
        picker,
        request,
        text: String::new(),
        name: None,
        truncated: false,
        line: None,
        syntax: None,
        extension: None,
        error: Some(error),
    }
}

/// To a UI: `text` shows for the row, as the file called `name` or plain.
fn text_of(picker: u32, request: u64, text: String, name: Option<String>) -> Control {
    Control::PickerPreviewed {
        picker,
        request,
        text,
        name,
        truncated: false,
        line: None,
        syntax: None,
        extension: None,
        error: None,
    }
}

/// The pickers under way.
pub(crate) struct Pickers {
    next: AtomicU32,
    open: Arc<Mutex<HashMap<u32, Arc<Picker>>>>,
    /// The pickers of the file of the machine the daemon is on.
    configured: Configured,
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
    pub(crate) fn new(file: Option<PathBuf>) -> Self {
        Self { next: AtomicU32::default(), open: Arc::default(), configured: Configured::new(file) }
    }

    /// The picker called `name`, opened with `params`: one of the daemon's
    /// own, or one of the machine's file.
    fn define(
        &self,
        name: &str,
        params: &BTreeMap<String, String>,
        at_cursor: &dyn Fn() -> crate::vt::AtCursor,
    ) -> Result<Definition, String> {
        if BUILT_IN.contains(&name) {
            return define(name, params);
        }
        // What a picker that is a function is told of the pane: what is at
        // the shell's cursor, which is this daemon's to read, and what was
        // at a window's own cursor over what it copies from, which the
        // window gave.
        let context = || {
            let at = at_cursor();
            let given = |name: &str| params.get(name).cloned().unwrap_or_default();
            json!({
                "line": at.line,
                "line_before_cursor": at.before,
                "cursor_column": at.column,
                "recent_lines": at.recent,
                "cwd": params.get("cwd"),
                "copy_line": given("copy_line"),
                "copy_line_before_cursor": given("copy_line_before_cursor"),
            })
        };
        match self.configured.picker_for(name, &context)? {
            Some(defined) => configured(name, defined, self.configured.file(), params).map_err(|why| format!("{name}: {why}")),
            None => Err(format!("no picker named {name}")),
        }
    }

    /// A program's message. `mine` is the picker it has open, if it has.
    /// Returns false when the conversation is over.
    pub(crate) fn said(&self, bytes: &[u8], mine: &mut Option<u32>, showing: impl FnOnce() -> Result<Showing, String>, program: &Outbox, at_cursor: &dyn Fn() -> crate::vt::AtCursor) -> bool {
        let said = if bytes.len() > MAX_REQUEST { None } else { serde_json::from_slice(bytes).ok() };
        match said {
            Some(Said::Open { request_id, picker, params }) if mine.is_none() => {
                let refuse = |code: &'static str, message: String| {
                    log(format_args!("picker {picker} refused: {message}"));
                    result(program, &request_id, "error", json!({"code": code, "message": message}));
                    false
                };
                let definition = match self.define(&picker, &params, at_cursor) {
                    Ok(definition) => definition,
                    Err(message) => return refuse("bad_request", message),
                };
                let showing = match showing() {
                    Ok(showing) => showing,
                    Err(message) => return refuse("no_window", message),
                };
                let opened = json!({"type": "opened", "request_id": request_id});
                let asker = Asker::Program { to: showing.program, request_id };
                *mine = Some(self.open(definition, asker, None, Some(showing.pane), showing.ui, showing.ui_outbox));
                reply(program, opened);
                true
            }
            // Each lot of rows is answered, so that a program sends no more
            // at a time than are taken; also when they are not wanted.
            Some(Said::Items { request_id, sequence, items }) => {
                if let Some(number) = *mine {
                    self.given(number, items);
                }
                reply(program, json!({"type": "ack", "request_id": request_id, "sequence": sequence}));
                true
            }
            Some(Said::EndItems { request_id }) => {
                if let Some(number) = *mine {
                    self.given_all(number);
                }
                reply(program, json!({"type": "input_complete", "request_id": request_id}));
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

    fn open(&self, definition: Definition, asker: Asker, ask: Option<u64>, pane: Option<PaneId>, ui: u64, ui_outbox: Outbox) -> u32 {
        limit_search_threads();
        let number = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let of = match &definition.lists {
            Lists::Walk(scan) => format!("under {:?}", scan.roots),
            Lists::File(file) => format!("of {}", file.path.display()),
            Lists::Command(command) | Lists::Table { command, .. } => format!(
                "of what {} prints in {}",
                command.command.program.display(),
                command.command.working_directory.as_deref().unwrap_or(Path::new(".")).display()
            ),
            Lists::Given => "of what its program gives".to_owned(),
            Lists::Fixed(rows) => format!("of {} rows", rows.len()),
        };
        let over = pane.map(|pane| format!("pane {pane}")).unwrap_or_else(|| "no pane".to_owned());
        log(format_args!("picker {number}: {} {of} for {over}", definition.label));
        // What reads a file or runs a command, begun once the picker is one
        // of those open: it may fail at once, and that ends the picker.
        let mut begin: Option<Box<dyn FnOnce()>> = None;
        let source = match definition.lists {
            Lists::Walk(scan) => {
                let store = Arc::new(FileSystemSearchStore::new_append_only());
                let scan = start_scan(scan, store.clone());
                Source::Walk { store, scan }
            }
            Lists::File(options) => {
                let (source, sink, control) = self.read(number);
                begin = Some(Box::new(move || {
                    let failed = control.failed.clone();
                    if let Err(message) = file::start(options, sink, control) {
                        failed(&message);
                    }
                }));
                source
            }
            Lists::Command(command) => {
                let (source, sink, control) = self.read(number);
                begin = Some(Box::new(move || {
                    let failed = control.failed.clone();
                    if let Err(message) = command::start(command, sink, control) {
                        failed(&message);
                    }
                }));
                source
            }
            Lists::Table { command, names, delimiter } => {
                use nfm_picker_sources::structured::{StructuredSchema, StructuredStreamingStore};
                let store: Arc<SnapshotStore<Table>> = Arc::new(SnapshotStore::new());
                let cancelled = Arc::new(AtomicBool::new(false));
                let whole = StreamValues::default();
                let (open, to, stop, lines) = (self.open.clone(), store.clone(), cancelled.clone(), whole.clone());
                begin = Some(Box::new(move || {
                    let read = move || {
                        // The names were looked at when the picker was defined.
                        let Ok(schema) = StructuredSchema::new(names.clone()) else { return };
                        let mut rows = StructuredStreamingStore::new(schema);
                        // A line is a row: its columns, and the line as it
                        // was printed, which is what the row is.
                        let add = |rows: &mut StructuredStreamingStore, line: &[u8]| {
                            let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
                            if line.is_empty() || rows.len() >= MOST_LINES {
                                return;
                            }
                            let mut fields: Vec<&str> = line.splitn(names.len(), delimiter.as_str()).collect();
                            fields.resize(names.len(), "");
                            if rows.add_record(&csv::StringRecord::from(fields)).is_ok() {
                                lines.lock().unwrap().insert(rows.len() - 1, line);
                            }
                        };
                        let mut pending = Vec::new();
                        let output = nfm_preview_command::run_streaming(
                            &command.command,
                            || stop.load(Ordering::Acquire),
                            |stream, bytes| {
                                if stream != nfm_preview_command::Stream::Stdout {
                                    return true;
                                }
                                pending.extend_from_slice(bytes);
                                let mut start = 0;
                                while let Some(end) = pending[start..].iter().position(|&byte| byte == b'\n') {
                                    add(&mut rows, &pending[start..start + end]);
                                    start += end + 1;
                                }
                                pending.drain(..start);
                                if start > 0 {
                                    to.publish(rows.snapshot());
                                }
                                rows.len() < MOST_LINES
                            },
                        );
                        let stopped = stop.load(Ordering::Acquire);
                        if !stopped {
                            add(&mut rows, &std::mem::take(&mut pending));
                        }
                        to.publish(rows.snapshot());
                        to.complete();
                        let failed = match output {
                            _ if stopped || rows.len() >= MOST_LINES => None,
                            Ok(output) if output.status.code().is_some_and(|code| command.success_exit_codes.contains(&code)) => None,
                            Ok(output) => Some(format!(
                                "{} ended with {}: {}",
                                command.command.program.display(),
                                output.status,
                                String::from_utf8_lossy(&output.stderr).trim()
                            )),
                            Err(message) => Some(message),
                        };
                        if let Some(message) = failed {
                            log(format_args!("picker {number}: what it lists was not read: {message}"));
                            // As of lines: with rows it goes on with those.
                            if rows.len() == 0 {
                                end(&open, number, Outcome::Error { code: "source_failed", message }, true);
                            }
                        }
                    };
                    let _ = std::thread::Builder::new().name(format!("picker {number} table")).spawn(read);
                }));
                Source::Table { store, cancelled, whole }
            }
            Lists::Fixed(items) => {
                let store = Arc::new(SnapshotStore::new_append_only());
                let mut adding = PublishingStreamingItemStoreWithPayload::new(store.clone());
                let mut rows = Vec::with_capacity(items.len());
                for item in items {
                    // A row is one line, whatever is in what it shows.
                    let shows: String =
                        item.text.chars().map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c }).collect();
                    adding.add_item(shows.as_bytes(), ());
                    rows.push((rows.len().to_string(), Value::String(item.value), None));
                }
                adding.publish();
                adding.complete();
                Source::Given { store, given: Mutex::new(Given { adding: None, rows }) }
            }
            Lists::Given => {
                let store = Arc::new(SnapshotStore::new_append_only());
                let adding = Some(PublishingStreamingItemStoreWithPayload::new(store.clone()));
                Source::Given { store, given: Mutex::new(Given { adding, rows: Vec::new() }) }
            }
        };
        let picker = Arc::new(Picker {
            number,
            pane,
            ui,
            ui_outbox,
            asker,
            source,
            sort: definition.sort,
            previews: Mutex::default(),
            resolves: definition.resolves,
            wanted: Arc::default(),
            open: Arc::downgrade(&self.open),
            state: Mutex::new(State { flow: Some(Flow::new()), ..Default::default() }),
        });
        self.open.lock().unwrap().insert(number, picker.clone());
        picker.tell_ui(Control::PickerOpen {
            picker: number,
            pane: picker.pane,
            label: definition.label,
            query: definition.query,
            preview: definition.preview,
            at_cursor: definition.at_cursor,
            keys: picker.resolves.as_ref().map(|resolves| resolves.keys.clone()).unwrap_or_default(),
            ask,
        });
        if let Some(begin) = begin {
            begin();
        }
        number
    }

    /// A picker of lines that are read as they come, of a file or of what a
    /// command prints: what it lists, where the lines are put, and how the
    /// reading is stopped and says it failed.
    fn read(&self, number: u32) -> (Source, StreamSink<()>, StreamControl) {
        let store = Arc::new(SnapshotStore::new_append_only());
        let cancelled = Arc::new(AtomicBool::new(false));
        let whole = StreamValues::default();
        let sink = StreamSink {
            store: store.clone(),
            payload: (),
            values: Some(whole.clone()),
            error: Arc::default(),
        };
        let (open, listed) = (self.open.clone(), store.clone());
        let failed = move |message: &str| {
            log(format_args!("picker {number}: what it lists was not read: {message}"));
            // With rows it goes on with those it has: a search that could
            // not read one file has still found what is in the rest. With
            // none there is nothing to choose, and it ends saying why.
            if listed.snapshot().is_none_or(|rows| rows.is_empty()) {
                let outcome = Outcome::Error { code: "source_failed", message: message.to_owned() };
                end(&open, number, outcome, true);
            }
        };
        let (most, stop) = (store.clone(), cancelled.clone());
        let changed = move || {
            if most.snapshot().is_some_and(|rows| rows.len() >= MOST_LINES) {
                stop.store(true, Ordering::Release);
            }
        };
        let control = StreamControl {
            cancelled: cancelled.clone(),
            changed: Arc::new(changed),
            failed: Arc::new(failed),
        };
        (Source::Read { store, cancelled, whole }, sink, control)
    }

    /// The UI `ui` asks for a picker over `pane` for itself, with its own
    /// number `ask` for it. `shows` is whether it is the UI that shows the
    /// pane. It is told the picker is open as the UI of any picker is, with
    /// its number, or that it is refused.
    pub(crate) fn asked(
        &self,
        ui: u64,
        ui_outbox: &Outbox,
        ask: u64,
        pane: PaneId,
        name: &str,
        params: &BTreeMap<String, String>,
        shows: bool,
        at_cursor: &dyn Fn() -> crate::vt::AtCursor,
    ) {
        let definition = if shows { self.define(name, params, at_cursor) } else { Err(format!("pane {pane} is not shown here")) };
        let definition = match definition {
            // It has no program to give it rows.
            Ok(Definition { lists: Lists::Given, .. }) => Err(format!("{name} is a picker for a program")),
            other => other,
        };
        match definition {
            Ok(definition) => {
                self.open(definition, Asker::Ui, Some(ask), Some(pane), ui, ui_outbox.clone());
            }
            Err(message) => {
                log(format_args!("picker {name} refused: {message}"));
                let refused = Control::PickerRefused { ask, message };
                let _ = ui_outbox.send(Frame::Control(refused).encode());
            }
        }
    }

    /// The UI `ui` asks for a search of folders over no pane, with its own
    /// number `ask` for it: see `Control::FoldersAsk`. It lists whole
    /// paths, and it ends when the UI closes it or goes.
    pub(crate) fn folders(&self, ui: u64, ui_outbox: &Outbox, ask: u64, roots: &[String], depth: i32) {
        match search_of_folders(roots, depth) {
            Ok(definition) => {
                self.open(definition, Asker::Ui, Some(ask), None, ui, ui_outbox.clone());
            }
            Err(message) => {
                log(format_args!("a search of folders was refused: {message}"));
                let _ = ui_outbox.send(Frame::Control(Control::PickerRefused { ask, message }).encode());
            }
        }
    }

    /// The program that asked for `number` gives it rows. They are not
    /// taken by a picker that lists something else, or one that has ended.
    fn given(&self, number: u32, items: Vec<Item>) {
        let Some(picker) = self.open.lock().unwrap().get(&number).cloned() else { return };
        let Source::Given { store, given } = &picker.source else { return };
        let mut given = given.lock().unwrap();
        let Given { adding: Some(adding), rows } = &mut *given else { return };
        let mut left_out = 0;
        for item in items {
            if rows.len() >= MOST_GIVEN {
                left_out += 1;
                continue;
            }
            // A row is one line, whatever is in what it shows.
            let shows: String =
                item.text.chars().map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c }).collect();
            adding.add_item(shows.as_bytes(), ());
            let id = item.id.unwrap_or_else(|| rows.len().to_string());
            rows.push((id, item.value.unwrap_or(Value::String(item.text)), item.description));
        }
        adding.publish();
        if left_out > 0 {
            store.add_skipped(left_out);
        }
    }

    /// The program has given `number` all its rows.
    fn given_all(&self, number: u32) {
        let Some(picker) = self.open.lock().unwrap().get(&number).cloned() else { return };
        let Source::Given { given, .. } = &picker.source else { return };
        if let Some(mut adding) = given.lock().unwrap().adding.take() {
            adding.complete();
        }
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
                    // Where its file says what is done with a row, and a UI
                    // asked for it, that is done: a program that asked is
                    // given the row.
                    let acts = found.resolves.as_ref().is_some_and(|resolves| resolves.action);
                    if acts && matches!(found.asker, Asker::Ui) && found.act(*serial, *row) {
                        return true;
                    }
                    let outcome = found.chosen(*serial, *row);
                    self.end(*picker, outcome, false);
                }
            }
            Control::PickerKey { picker, serial, row, key } => {
                if let Some(picker) = self.shown_by(*picker, ui) {
                    picker.key(*serial, *row, *key);
                }
            }
            Control::PickerPreview { picker, request, serial, row } => {
                if let Some(picker) = self.shown_by(*picker, ui) {
                    picker.preview(*request, *serial, *row);
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
    pub(crate) fn end_where(&self, which: impl Fn(Option<PaneId>, u64) -> bool, tell_ui: bool) {
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
        end(&self.open, number, outcome, tell_ui);
    }
}

/// End picker `number` of those `open`, if it is still one of them: its
/// program is told how, and its UI unless that is what ended it.
fn end(open: &Mutex<HashMap<u32, Arc<Picker>>>, number: u32, outcome: Outcome, tell_ui: bool) {
    let Some(picker) = open.lock().unwrap().remove(&number) else { return };
    picker.source.stop();
    picker.wanted.store(u64::MAX, Ordering::Release);
    let query = {
        let mut state = picker.state.lock().unwrap();
        if let Some(search) = state.search.take() {
            search.stop();
        }
        state.flow = None;
        state.last = None;
        state.sent.clear();
        std::mem::take(&mut state.query)
    };
    let how = match &outcome {
        Outcome::Accepted(_) => "accepted".to_owned(),
        Outcome::Acted => "accepted, and its file acted on the row".to_owned(),
        Outcome::Cancelled(reason) => format!("cancelled ({reason})"),
        Outcome::Error { message, .. } => format!("failed: {message}"),
    };
    log(format_args!("picker {number}: {how}"));
    let message = match &outcome {
        Outcome::Error { message, .. } => Some(message.clone()),
        _ => None,
    };
    match (&picker.asker, outcome) {
        // The UI asked for it itself: it is told what the row it chose is,
        // and knows of any other end as the UI of any picker does.
        (Asker::Ui, Outcome::Accepted(row)) => {
            let value = match row.value {
                Value::String(value) => value,
                other => other.to_string(),
            };
            picker.tell_ui(Control::PickerChosen { picker: number, text: row.text, value, query });
        }
        (Asker::Ui, _) => {}
        // A program is given the row, and does with it what it will.
        (Asker::Program { .. }, Outcome::Acted) => {}
        (Asker::Program { to, request_id }, outcome) => {
            match outcome {
                Outcome::Accepted(row) => result(
                    to,
                    request_id,
                    "accepted",
                    json!({"selected": [{"id": row.id, "text": row.text, "value": row.value}], "query": query}),
                ),
                Outcome::Cancelled(reason) => result(to, request_id, "cancelled", json!({"reason": reason})),
                Outcome::Acted => {}
                Outcome::Error { code, message } => {
                    result(to, request_id, "error", json!({"code": code, "message": message}))
                }
            }
            // Write what was queued for the program, then close its
            // connection.
            let _ = to.send(Vec::new());
        }
    }
    if tell_ui {
        picker.tell_ui(Control::PickerClosed { picker: number, message });
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
        if state.search.is_some() && state.query == query {
            // Asked for again, as when the UI has room for more rows. The
            // search finds nothing new for a query it has, and counts it
            // as the same one: what it last found is the answer.
            if let Some(last) = state.revisions.last_mut() {
                *last = revision;
            }
            let found = state.last.clone().and_then(|last| state.flow.as_mut()?.found(revision, last));
            if let Some((serial, answer)) = found {
                self.send(&mut state, serial, revision, answer);
            }
            return;
        }
        state.query = query.clone();
        state.revisions.push(revision);
        state.last = None;
        if let Some(search) = &state.search {
            search.set_query(query);
            return;
        }
        let picker = Arc::downgrade(self);
        let search = FuzzySearchSession::new_with_snapshot_observer(
            u64::from(self.number),
            Arc::new(self.source.found()),
            query,
            self.sort,
            move |update, source| {
                if let Some(picker) = picker.upgrade() {
                    picker.found(Answer { update: Arc::new(update), source });
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
        // Only of the query the search has now: an answer to one before it
        // is not what that is asked for again with.
        if answer.update.generation as usize + 1 == state.revisions.len() {
            state.last = Some(answer.clone());
        }
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
            .iter()
            .take(state.rows)
            .map(|result| PickerRow {
                index: result.node_index as u64,
                text: source.display_text(result.node_index, &update.query).unwrap_or_else(|| result.path.clone()),
            })
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
            query: update.query.clone(),
            header: source.header(&update.query),
            rows,
            matched: update.matched as u64,
            total: update.total as u64,
            skipped: self.source.skipped() as u64,
            done: update.source_done,
            version: update.source_version,
            append_only: true,
        });
    }

    /// Row `row` of those sent with `serial`.
    fn row(&self, serial: u64, row: u64) -> Option<Row> {
        let (index, text, shown) = {
            let state = self.state.lock().unwrap();
            let source = state.sent.iter().find(|(sent, _)| *sent == serial).map(|(_, source)| source)?;
            let index = usize::try_from(row).ok().filter(|index| *index < source.len())?;
            (index, source.get_string_lossy(index, &mut Vec::new()), source.item(index)?)
        };
        Some(match &self.source {
            Source::Walk { .. } => Row { id: row.to_string(), text, value: Value::String(shown), description: None },
            Source::Read { whole, .. } | Source::Table { whole, .. } => {
                let whole = whole.lock().unwrap().get(&index).cloned();
                Row { id: row.to_string(), text, value: Value::String(whole.unwrap_or(shown)), description: None }
            }
            Source::Given { given, .. } => {
                let (id, value, description) = given.lock().unwrap().rows.get(index)?.clone();
                Row { id, text, value, description }
            }
        })
    }

    /// The UI asks what shows for a row. It is found on a thread of the
    /// picker's own, so that a slow folder or program holds up nothing else
    /// the UI says, and only the last asked for is. Where the picker's file
    /// says what shows, its function is run; otherwise only a row that is a
    /// file or a folder has anything to show.
    fn preview(&self, request: u64, serial: u64, row: u64) {
        let found = self.row(serial, row);
        let wanted = match (&self.resolves, &self.source, found) {
            (Some(resolves), _, Some(Row { text, value, description, .. })) if resolves.preview => {
                let value = match value {
                    Value::String(value) => value,
                    other => other.to_string(),
                };
                Some(Wanted::Row { text, value, description })
            }
            // A file or a folder.
            (_, Source::Walk { .. }, Some(Row { value: Value::String(path), .. })) => {
                Some(Wanted::File(PathBuf::from(path)))
            }
            _ => None,
        };
        let Some(wanted) = wanted else {
            let error = format!("row {row} of answer {serial} is not one the picker can show");
            return self.tell_ui(said(self.number, request, error));
        };
        // What is being found for a row before this one is stopped.
        self.wanted.store(request, Ordering::Release);
        self.ask(request, wanted);
    }

    /// Give the thread that runs the picker's file, and reads what shows
    /// for its rows, something to do. It is started by the first.
    fn ask(&self, request: u64, wanted: Wanted) {
        let mut previews = self.previews.lock().unwrap();
        let asked = previews.get_or_insert_with(|| {
            let (asked, asking) = std::sync::mpsc::channel::<(u64, Wanted)>();
            let (number, ui) = (self.number, self.ui_outbox.clone());
            let (resolves, latest) = (self.resolves.clone(), self.wanted.clone());
            let files = matches!(self.source, Source::Walk { .. });
            let open = self.open.clone();
            let read = move || {
                // The picker's Lua, made when a row is first rested on or
                // acted on.
                let make = || match &resolves {
                    Some(resolves) => {
                        let made = Previewer::called(
                            &resolves.file,
                            &resolves.name,
                            resolves.params.clone(),
                            files,
                            latest.clone(),
                            resolves.context.as_ref(),
                        );
                        if let Err(error) = &made {
                            log(format_args!("picker {number}: its file cannot be run: {error}"));
                        }
                        made
                    }
                    None => Err("the picker is not one of the machine's file".to_owned()),
                };
                let mut previewer: Option<Result<Previewer, String>> = None;
                let mut waiting = std::collections::VecDeque::new();
                loop {
                    if waiting.is_empty() {
                        let Ok(first) = asking.recv() else { break };
                        waiting.push_back(first);
                    }
                    // Of the previews asked for only the last is found. A
                    // row to act on is never passed over.
                    while let Ok(newer) = asking.try_recv() {
                        if !matches!(waiting.back(), Some((_, Wanted::Act { .. } | Wanted::Key { .. }))) {
                            waiting.pop_back();
                        }
                        waiting.push_back(newer);
                    }
                    let Some((request, wanted)) = waiting.pop_front() else { continue };
                    let previewed = match (wanted, &resolves) {
                        (Wanted::Act { text, value, description }, _) => {
                            let acted = match previewer.get_or_insert_with(&make) {
                                Ok(previewer) => previewer.act(&text, &value, description.as_deref(), None),
                                Err(error) => Err(error.clone()),
                            };
                            // What it asked for is the UI's to do, and the
                            // picker is over: a row was chosen.
                            let outcome = match acted {
                                Ok(acted) => {
                                    let steps = acted.steps;
                                    let _ = ui.send(Frame::Control(Control::PickerActed { picker: number, steps }).encode());
                                    Outcome::Acted
                                }
                                Err(message) => Outcome::Error { code: "action_failed", message },
                            };
                            if let Some(open) = open.upgrade() {
                                end(&open, number, outcome, true);
                            }
                            break;
                        }
                        (Wanted::Key { key, text, value, description }, _) => {
                            let acted = match previewer.get_or_insert_with(&make) {
                                Ok(previewer) => previewer.act(&text, &value, description.as_deref(), Some(key)),
                                Err(error) => Err(error.clone()),
                            };
                            // A key leaves the picker open, unless its
                            // function closed it or asked for another.
                            let (steps, ends) = match acted {
                                Ok(acted) => {
                                    let another = acted.steps.iter().any(|step| matches!(step, PickerStep::Picker { .. }));
                                    (acted.steps, acted.close || another)
                                }
                                Err(message) => {
                                    let title = Some("The picker's key failed".to_owned());
                                    (vec![PickerStep::Toast { title, message, duration_ms: Some(6000) }], false)
                                }
                            };
                            let _ = ui.send(Frame::Control(Control::PickerActed { picker: number, steps }).encode());
                            if ends {
                                if let Some(open) = open.upgrade() {
                                    end(&open, number, Outcome::Cancelled("closed_by_key"), true);
                                }
                                break;
                            }
                            continue;
                        }
                        (Wanted::Row { text, value, description }, Some(_)) => {
                            let previewer = previewer.get_or_insert_with(&make);
                            let shows = match previewer {
                                Ok(previewer) => previewer.resolve(request, &text, &value, description.as_deref()),
                                Err(error) => Some(Err(error.clone())),
                            };
                            // Stopped for another row: that one is next.
                            let Some(shows) = shows else { continue };
                            match shows {
                                Ok(Shows::Nothing) => Frame::Control(text_of(number, request, String::new(), None)),
                                Ok(Shows::Text { text, name, extension, syntax, line }) => {
                                    Frame::Control(Control::PickerPreviewed {
                                        picker: number,
                                        request,
                                        text,
                                        name,
                                        truncated: false,
                                        line,
                                        syntax,
                                        extension,
                                        error: None,
                                    })
                                }
                                Ok(Shows::Ansi { bytes, line }) => {
                                    Frame::Previewed { picker: number, request, kind: PreviewKind::Ansi, line, bytes }
                                }
                                Ok(Shows::Image { bytes }) => Frame::Previewed {
                                    picker: number,
                                    request,
                                    kind: PreviewKind::Image,
                                    line: None,
                                    bytes,
                                },
                                Err(error) => Frame::Control(said(number, request, error)),
                            }
                        }
                        (Wanted::File(path), _) => {
                            let shown = Shown::of(&path);
                            match shown.picture {
                                Some(bytes) => Frame::Previewed {
                                    picker: number,
                                    request,
                                    kind: PreviewKind::Image,
                                    line: None,
                                    bytes,
                                },
                                None => Frame::Control(Control::PickerPreviewed {
                                    picker: number,
                                    request,
                                    text: shown.text,
                                    name: shown.name,
                                    truncated: shown.truncated,
                                    line: None,
                                    syntax: None,
                                    extension: None,
                                    error: shown.error,
                                }),
                            }
                        }
                        (Wanted::Row { .. }, None) => continue,
                    };
                    if ui.send(previewed.encode()).is_err() {
                        break;
                    }
                }
            };
            let _ = std::thread::Builder::new().name(format!("picker {number} previews")).spawn(read);
            asked
        });
        let _ = asked.send((request, wanted));
    }

    /// The UI chose a row of a picker whose file says what is done with
    /// one: the file's function is run for it, on the picker's own thread,
    /// which then ends the picker. False for a row there is not.
    fn act(&self, serial: u64, row: u64) -> bool {
        let Some(Row { text, value, description, .. }) = self.row(serial, row) else { return false };
        let value = match value {
            Value::String(value) => value,
            other => other.to_string(),
        };
        // No preview is wanted of a picker a row was chosen of.
        self.wanted.store(u64::MAX, Ordering::Release);
        self.ask(0, Wanted::Act { text, value, description });
        true
    }

    /// The UI pressed one of the picker's own keys on a row: the function
    /// the file has for it is run, on the picker's own thread.
    fn key(&self, serial: u64, row: u64, key: u32) {
        let has = self.resolves.as_ref().is_some_and(|resolves| (key as usize) < resolves.keys.len());
        let Some(Row { text, value, description, .. }) = self.row(serial, row).filter(|_| has) else { return };
        let value = match value {
            Value::String(value) => value,
            other => other.to_string(),
        };
        self.ask(0, Wanted::Key { key: key as usize, text, value, description });
    }

    /// The row the UI chose, of those sent with `serial`.
    fn chosen(&self, serial: u64, row: u64) -> Outcome {
        match self.row(serial, row) {
            Some(row) => Outcome::Accepted(row),
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
    fn a_path_from_a_tilde_is_in_the_home_folder() {
        let home = PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap());
        let read = |path: &str| in_home(PathBuf::from(path)).unwrap();
        assert_eq!(read("~"), home);
        assert_eq!(read("~/src"), home.join("src"));
        assert_eq!(read("~\\src"), home.join("src"));
        // A tilde that is part of a name is left alone.
        assert_eq!(read("~src"), PathBuf::from("~src"));
        assert_eq!(read("C:/src/~"), PathBuf::from("C:/src/~"));
    }

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

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn a_ui_searches_folders_under_roots_and_is_given_whole_paths() {
        let here = std::env::temp_dir();
        let root = here.to_string_lossy().into_owned();
        let folders = search_of_folders(&[root.clone(), "~".into()], 2).unwrap();
        let Lists::Walk(scan) = &folders.lists else { panic!("not a walk") };
        let home = in_home(PathBuf::from("~")).unwrap();
        assert_eq!(scan.roots, [here.clone(), home]);
        assert_eq!((scan.max_depth, scan.directories_only, scan.files_only, scan.relative), (2, true, false, false));
        assert_eq!((folders.preview, folders.at_cursor), (Some(false), false));
        for roots in [vec![], vec![".".to_owned()], vec![r"Z:\no\such\folder\anywhere".to_owned()]] {
            assert!(search_of_folders(&roots, 0).is_err(), "{roots:?} was taken");
        }
    }

    #[test]
    fn the_daemon_has_a_picker_of_what_its_program_gives_and_no_other() {
        let items = define("items", &params(&[("query", "git"), ("position", "cursor")])).unwrap();
        assert!(matches!(items.lists, Lists::Given) && matches!(items.sort, SearchSortMode::Score));
        assert_eq!((items.label.as_str(), items.query.as_str(), items.preview, items.at_cursor), ("Items", "git", None, true));
        let long = "x".repeat(MOST_LABEL + 10);
        assert_eq!(define("items", &params(&[("label", " Completions ")])).unwrap().label, "Completions");
        assert_eq!(define("items", &params(&[("label", &long)])).unwrap().label.len(), MOST_LABEL);
        // Files, folders, the lines of a file and what a program prints are
        // the machine's file's to say: the daemon has none of its own.
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        for name in ["files", "directories", "history", "ripgrep"] {
            let asked = define(name, &params(&[("cwd", &cwd), ("pattern", "fn main")]));
            assert_eq!(asked.err(), Some(format!("no picker named {name}")));
        }
    }

    #[test]
    fn a_row_shows_as_the_start_of_its_file_or_what_is_in_its_folder() {
        let root = std::env::temp_dir().join(format!("nterm-shown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("b.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("A.txt"), "caf\u{e9}").unwrap();
        std::fs::write(root.join("data.bin"), [1u8, 0, 2, 3]).unwrap();
        // Lines of ten bytes, to past the most that is sent.
        let long = "123456789\n".repeat(MOST_PREVIEW / 10 + 5);
        std::fs::write(root.join("long.log"), format!("{long}tail with no end")).unwrap();

        let file = Shown::of(&root.join("b.rs"));
        let whole = Shown { text: "fn main() {}\n".into(), name: Some("b.rs".into()), ..Default::default() };
        assert_eq!(file, whole);
        assert_eq!(Shown::of(&root.join("A.txt")).text, "caf\u{e9}");

        // Cut at the end of a line, and said to be cut.
        let cut = Shown::of(&root.join("long.log"));
        assert!(cut.truncated && cut.text.len() <= MOST_PREVIEW);
        assert!(cut.text.ends_with("123456789\n") && cut.text.len() > MOST_PREVIEW - 10);

        let binary = Shown::of(&root.join("data.bin"));
        assert_eq!((binary.text.as_str(), &binary.name), ("Not text: 4 bytes", &None));

        // Folders first, then files, neither by the case of its name.
        let folder = Shown::of(&root);
        assert_eq!(folder.text, "sub\\\nA.txt\nb.rs\ndata.bin\nlong.log");
        assert_eq!((&folder.name, folder.truncated), (&None, false));
        assert_eq!(Shown::of(&root.join("sub")).text, "Empty folder");

        // A picture is the file itself, whatever the case of its name, and
        // one too large to send, or with nothing in it, says so.
        std::fs::write(root.join("Dot.PNG"), [137u8, 80, 78, 71, 0, 1]).unwrap();
        let picture = Shown::of(&root.join("Dot.PNG"));
        assert_eq!((picture.picture.as_deref(), picture.text.as_str()), (Some(&[137u8, 80, 78, 71, 0, 1][..]), ""));
        let large = std::fs::File::create(root.join("large.jpg")).unwrap();
        large.set_len(MOST_PICTURE + 1).unwrap();
        let large = Shown::of(&root.join("large.jpg"));
        assert!(large.picture.is_none() && large.text.starts_with("A picture too large to show"), "{}", large.text);
        std::fs::write(root.join("none.gif"), []).unwrap();
        assert_eq!(Shown::of(&root.join("none.gif")).text, "An empty file");
        let missing = Shown::of(&root.join("none.txt"));
        assert!(missing.error.is_some() && missing.text.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

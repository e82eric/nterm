//! The UI's end of the session daemon.
//!
//! With `terminal.session_daemon` on, a pane's shell runs in the daemon. The
//! pane's surface has no process of its own: it is a mirror that is given the
//! daemon's screen and then the shell's output, and that sends back only what
//! the user did. The daemon's terminal answers the program's queries.
//!
//! One connection serves every pane. It is made by a thread of its own, and
//! can be made again after it fails or is lost. While it lasts, one thread
//! reads it and hands each frame to the pane it names, and one writes what
//! the others queue for it, so that none of them waits on the pipe.
//!
//! Closing a pane ends its shell. Closing a window, or leaving nterm, does
//! not: the shells stay in the daemon, and the next nterm opens a pane on
//! each of them.

use crate::ghostty_ffi::{self, Host, HostSettings, HostSpawn, SurfaceSize};
use nterm_session::{
    DEFAULT_SESSION, GHOSTTY_REVISION,
    pipe::{self, Connection},
    protocol::{
        Control, CursorStyle, Frame, Identity, PROTOCOL_VERSION, PaneId, PaneInfo, Size,
        TerminalSettings,
    },
};
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, c_void},
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// How long copy mode, search and the pickers wait for a pane's history.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(5);

/// How long a launch waits for the connection before it goes on without.
const LAUNCH_WAIT: Duration = Duration::from_secs(10);

/// How long leaving nterm waits for what was queued to be written.
const FLUSH_WAIT: Duration = Duration::from_secs(2);

/// Input typed before the shell exists is kept, up to this much.
const QUEUED_INPUT_MAX: usize = 1 << 16;

static ENABLED: AtomicBool = AtomicBool::new(false);

// Defined by the pane, which is what the surface tells of a title or a
// directory. `userdata` is the pane's `WakeState`.
unsafe extern "C" {
    fn nterm_title(userdata: *mut c_void, title: *const std::ffi::c_char);
    fn nterm_pwd(userdata: *mut c_void, pwd: *const std::ffi::c_char);
}

/// What a pane being made does for a shell.
#[derive(Clone, Debug, Default)]
pub(crate) enum Start {
    /// Start one.
    #[default]
    Shell,
    /// Take this one, which the daemon is running with nothing showing it.
    Adopt(Box<Adopt>),
}

/// A shell to take, as a listing gave it.
#[derive(Clone, Debug)]
pub(crate) struct Adopt {
    pub(crate) info: PaneInfo,
    /// The picker endpoint and token in the shell's environment, which the
    /// UI that started it left, so that `npick` run from it reaches the
    /// pane showing it now.
    pub(crate) picker: Option<(String, String)>,
    /// The connection whose listing this came from.
    daemon: u64,
}

/// What the daemon holds, as it answered one request: every pane, and
/// everything UIs have left with it. It is not kept up to date; whoever
/// asked takes it, and asks again for a newer one.
#[derive(Clone, Debug, Default)]
pub(crate) struct Listing {
    pub(crate) panes: Vec<PaneInfo>,
    pub(crate) blobs: Vec<(String, String)>,
    /// The connection that answered. Its pane numbers mean nothing to
    /// another daemon.
    daemon: u64,
}

impl Listing {
    /// What a pane needs to take the shell `pane`, if it is waiting.
    pub(crate) fn adopt(&self, pane: PaneId) -> Option<Adopt> {
        let info = self.panes.iter().find(|info| info.pane == pane && waiting(info))?.clone();
        let key = picker_key(pane);
        let picker = self
            .blobs
            .iter()
            .find(|(name, _)| *name == key)
            .and_then(|(_, value)| serde_json::from_str(value).ok());
        Some(Adopt { info, picker, daemon: self.daemon })
    }
}

/// A listing asked for and not yet taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Asked {
    /// The connection it was asked on.
    daemon: u64,
    request: u32,
}

/// Where a listing asked for is.
pub(crate) enum Answer {
    Waiting,
    Ready(Listing),
    /// The connection it was asked on is gone; it will not come.
    Gone,
}

/// Whether a pane's shell is running with nothing showing it.
pub(crate) fn waiting(pane: &PaneInfo) -> bool {
    pane.exited.is_none() && pane.attached == 0
}

fn picker_key(pane: PaneId) -> String {
    format!("picker/{pane}")
}

/// Keep a pane's picker endpoint and token for the next UI.
pub(crate) fn save_picker_identity(pane: PaneId, endpoint: &str, token: &str) {
    if let Ok(value) = serde_json::to_string(&(endpoint, token)) {
        set_blob(picker_key(pane), value);
    }
}

/// Keep something in the daemon for the next UI. Does nothing without a
/// connection.
pub(crate) fn set_blob(key: String, value: String) {
    if let Some(daemon) = daemon() {
        daemon.control(Control::BlobSet { key, value });
    }
}

pub(crate) fn remove_blob(key: String) {
    if let Some(daemon) = daemon() {
        daemon.control(Control::BlobRemove { key });
    }
}

/// Ask the daemon what it holds. `None` without a connection. The answer
/// is collected with `take_listing`, or given up with `forget_listing`.
pub(crate) fn request_listing() -> Option<Asked> {
    let daemon = daemon()?;
    let request = daemon.next_request.fetch_add(1, Ordering::Relaxed);
    daemon.listings.lock().unwrap().insert(request, Parked::default());
    daemon.control(Control::List { request });
    daemon.control(Control::BlobList { request, prefix: String::new() });
    Some(Asked { daemon: daemon.number, request })
}

/// The answer to `asked`, once. Taking it leaves nothing behind.
pub(crate) fn take_listing(asked: Asked) -> Answer {
    let Some(daemon) = daemon().filter(|daemon| daemon.number == asked.daemon) else {
        return Answer::Gone;
    };
    let mut listings = daemon.listings.lock().unwrap();
    match listings.get(&asked.request) {
        None => Answer::Gone,
        Some(parked) if !parked.complete() => Answer::Waiting,
        Some(_) => {
            let mut listing = listings.remove(&asked.request).unwrap().listing;
            listing.daemon = daemon.number;
            Answer::Ready(listing)
        }
    }
}

/// Nobody wants the answer to `asked` any more.
pub(crate) fn forget_listing(asked: Asked) {
    if let Some(daemon) = daemon().filter(|daemon| daemon.number == asked.daemon) {
        daemon.listings.lock().unwrap().remove(&asked.request);
    }
}

/// A listing on its way: the daemon answers for the panes and for the
/// blobs separately.
#[derive(Default)]
struct Parked {
    listing: Listing,
    panes: bool,
    blobs: bool,
}

impl Parked {
    fn complete(&self) -> bool {
        self.panes && self.blobs
    }
}


/// Called with the configuration before the first pane is made.
pub(crate) fn configure(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
}

/// `NTERM_SESSION_DAEMON=1` or `0` overrides the configuration.
fn enabled() -> bool {
    #[cfg(test)]
    if tests::TARGET.lock().unwrap().is_some() {
        return true;
    }
    match std::env::var("NTERM_SESSION_DAEMON").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => ENABLED.load(Ordering::Acquire),
    }
}

fn report(summary: &str, details: &str) {
    crate::error_log::error("session", summary, details);
}

/// Where the connection to the daemon stands.
enum Conn {
    Idle,
    Connecting,
    Connected(Arc<Daemon>),
    Unavailable,
    Lost,
}

/// The connection, whatever state it is in. There is one for the process.
struct Client {
    state: Mutex<Conn>,
    /// Told of every change of `state`.
    changed: Condvar,
    /// Counts the connections made, to tell one from the next.
    connections: Mutex<u64>,
    /// The UI, to be woken when something it may be waiting for arrives.
    ui: Mutex<Option<egui::Context>>,
}

static CLIENT: Client = Client {
    state: Mutex::new(Conn::Idle),
    changed: Condvar::new(),
    connections: Mutex::new(0),
    ui: Mutex::new(None),
};

impl Client {
    fn set(&self, state: Conn) {
        *self.state.lock().unwrap() = state;
        self.changed.notify_all();
        self.wake_ui();
    }

    fn wake_ui(&self) {
        if let Some(ui) = &*self.ui.lock().unwrap() {
            ui.request_repaint();
        }
    }
}

/// The UI to wake when the connection changes or a listing arrives.
pub(crate) fn set_ui(ui: &egui::Context) {
    *CLIENT.ui.lock().unwrap() = Some(ui.clone());
}

/// Start connecting, unless that is under way or done. Returns at once; the
/// UI is woken when it has succeeded or failed. After a failure or a loss
/// this tries again.
pub(crate) fn connect() {
    if !enabled() {
        return;
    }
    {
        let mut state = CLIENT.state.lock().unwrap();
        if matches!(*state, Conn::Connecting | Conn::Connected(_)) {
            return;
        }
        *state = Conn::Connecting;
    }
    CLIENT.changed.notify_all();
    let started = std::thread::Builder::new().name("session-connect".into()).spawn(|| {
        let number = {
            let mut connections = CLIENT.connections.lock().unwrap();
            *connections += 1;
            *connections
        };
        CLIENT.set(match open(number) {
            Ok(daemon) => Conn::Connected(daemon),
            Err(error) => {
                report(
                    "Session daemon unavailable",
                    &format!("{error}\n\nPanes run their own shells."),
                );
                Conn::Unavailable
            }
        });
    });
    if let Err(error) = started {
        report("Session daemon unavailable", &format!("{error}\n\nPanes run their own shells."));
        CLIENT.set(Conn::Unavailable);
    }
}

/// At launch: connect, wait until that has succeeded or failed, and take
/// what the daemon held when it did. `None` without a connection.
///
/// This is the one place the UI thread waits for the connection. It is
/// here because a window cannot yet be shown before its first pane, and
/// the first pane depends on the answer.
pub(crate) fn launch_listing() -> Option<Listing> {
    connect();
    let deadline = Instant::now() + LAUNCH_WAIT;
    let mut state = CLIENT.state.lock().unwrap();
    while matches!(*state, Conn::Connecting) {
        let left = deadline.checked_duration_since(Instant::now())?;
        state = CLIENT.changed.wait_timeout(state, left).unwrap().0;
    }
    match &*state {
        Conn::Connected(daemon) => daemon.at_connect.lock().unwrap().take(),
        _ => None,
    }
}

/// Leaving nterm: wait, briefly, until what was queued for the daemon has
/// been written. A shell's fate is in that queue.
pub(crate) fn flush() {
    let Some(daemon) = daemon() else { return };
    let (done, written) = mpsc::channel();
    if daemon.queue(Out::Flush(done)) {
        let _ = written.recv_timeout(FLUSH_WAIT);
    }
}

/// The connection, while there is one.
fn daemon() -> Option<Arc<Daemon>> {
    if !enabled() {
        return None;
    }
    match &*CLIENT.state.lock().unwrap() {
        Conn::Connected(daemon) => Some(daemon.clone()),
        _ => None,
    }
}

/// Something for the writer thread.
enum Out {
    Frame(Frame),
    /// Answer when everything before this has been written.
    Flush(mpsc::Sender<()>),
}

/// One connection to one daemon, and what is known of the daemon over it.
/// A pane's link belongs to the connection it was made on: pane numbers
/// mean nothing to another daemon.
struct Daemon {
    number: u64,
    connection: Connection,
    /// The writer thread's queue; `None` once the connection is gone.
    outbox: Mutex<Option<mpsc::Sender<Out>>>,
    panes: Mutex<HashMap<PaneId, Arc<Link>>>,
    /// Panes whose `Spawn` has not been answered, by request.
    spawning: Mutex<HashMap<u32, Arc<Link>>>,
    /// Who waits for each snapshot asked for, by request.
    snapshots: Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>,
    /// Listings asked for, by request, until they are taken.
    listings: Mutex<HashMap<u32, Parked>>,
    /// What the daemon held when the connection was made, until the launch
    /// takes it.
    at_connect: Mutex<Option<Listing>>,
    next_request: AtomicU32,
    /// The waiting shells that panes of this process have taken.
    taken: Mutex<HashSet<PaneId>>,
    /// What the daemon was last told.
    settings: Mutex<TerminalSettings>,
    gone: AtomicBool,
}

impl Daemon {
    /// Queue something for the writer thread. Nothing is written here, so
    /// no caller waits on the pipe. False once the connection is gone.
    fn queue(&self, out: Out) -> bool {
        match &*self.outbox.lock().unwrap() {
            Some(outbox) => outbox.send(out).is_ok(),
            None => false,
        }
    }

    fn send(&self, frame: Frame) {
        self.queue(Out::Frame(frame));
    }

    fn control(&self, control: Control) {
        self.send(Frame::Control(control));
    }

    /// The pane's link, without holding the table while it is used.
    fn pane(&self, pane: PaneId) -> Option<Arc<Link>> {
        self.panes.lock().unwrap().get(&pane).cloned()
    }
}

/// The session to connect to and the daemon program to start for it.
fn target() -> io::Result<(String, std::path::PathBuf)> {
    #[cfg(test)]
    if let Some(target) = tests::TARGET.lock().unwrap().clone() {
        return Ok(target);
    }
    let program = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "nterm-daemon.exe"
    } else {
        "nterm-daemon"
    });
    Ok((DEFAULT_SESSION.to_owned(), program))
}

/// Make connection `number`: find or start the daemon, greet it, and ask
/// what it holds. Runs on the connecting thread.
fn open(number: u64) -> io::Result<Arc<Daemon>> {
    let (session, program) = target()?;
    let other = |message: String| io::Error::other(message);
    // A daemon from another build cannot be trusted to speak the same
    // snapshot format. One with no panes is replaced; one with panes is left
    // alone and not used.
    for attempt in 0..2 {
        let connection = pipe::connect_or_start(&session, &program)?;
        let mut reader = io::BufReader::with_capacity(1 << 16, connection.reader()?);
        let mut writer = connection.writer()?;
        Frame::Control(Control::Hello {
            protocol: PROTOCOL_VERSION,
            ghostty: GHOSTTY_REVISION.to_owned(),
        })
        .write(&mut writer)?;
        let Frame::Control(Control::Welcome { protocol, ghostty, .. }) = Frame::read(&mut reader)?
        else {
            return Err(other("the daemon did not answer the greeting".into()));
        };
        Frame::Control(Control::List { request: 0 }).write(&mut writer)?;
        Frame::Control(Control::BlobList { request: 0, prefix: String::new() }).write(&mut writer)?;
        let mut held = Parked::default();
        held.listing.daemon = number;
        while !held.complete() {
            match Frame::read(&mut reader)? {
                Frame::Control(Control::Panes { panes, .. }) => {
                    held.listing.panes = panes;
                    held.panes = true;
                }
                Frame::Control(Control::Blobs { blobs, .. }) => {
                    held.listing.blobs = blobs;
                    held.blobs = true;
                }
                Frame::Control(Control::Error { message, .. }) => return Err(other(message)),
                _ => {}
            }
        }
        if protocol != PROTOCOL_VERSION || ghostty != GHOSTTY_REVISION {
            let panes = held.listing.panes.len();
            if panes == 0 && attempt == 0 {
                Frame::Control(Control::Shutdown).write(&mut writer)?;
                // Wait for it to go, so the next connect starts a new one.
                while Frame::read(&mut reader).is_ok() {}
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            return Err(other(format!(
                "a daemon from another build is running with {panes} panes \
                 (protocol {protocol}, ghostty {ghostty}; this build has \
                 protocol {PROTOCOL_VERSION}, ghostty {GHOSTTY_REVISION})"
            )));
        }
        let (outbox, queued) = mpsc::channel();
        let daemon = Arc::new(Daemon {
            number,
            connection,
            outbox: Mutex::new(Some(outbox)),
            panes: Mutex::default(),
            spawning: Mutex::default(),
            snapshots: Mutex::default(),
            listings: Mutex::default(),
            at_connect: Mutex::new(Some(held.listing)),
            next_request: AtomicU32::new(1),
            taken: Mutex::default(),
            settings: Mutex::default(),
            gone: AtomicBool::new(false),
        });
        let writing = daemon.clone();
        std::thread::Builder::new()
            .name("session-write".into())
            .spawn(move || write(&writing, writer, queued))?;
        let reading = daemon.clone();
        std::thread::Builder::new()
            .name("session".into())
            .spawn(move || read(&reading, reader))?;
        return Ok(daemon);
    }
    unreachable!("the second attempt returns")
}

/// The writer thread: the only one that writes to the pipe. Ends when the
/// queue is closed, or when a write fails, which closes the connection so
/// that the reader ends too.
fn write(daemon: &Daemon, mut writer: impl io::Write, queued: mpsc::Receiver<Out>) {
    for out in queued {
        match out {
            Out::Frame(frame) => {
                if frame.write(&mut writer).is_err() {
                    daemon.connection.close();
                    break;
                }
            }
            Out::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

/// The reader thread.
fn read(daemon: &Arc<Daemon>, mut reader: impl io::Read) {
    let error = loop {
        let frame = match Frame::read(&mut reader) {
            Ok(frame) => frame,
            Err(error) => break error,
        };
        match frame {
            Frame::Output { pane, bytes } => {
                if let Some(link) = daemon.pane(pane) {
                    link.with_host(|host| unsafe {
                        ghostty_ffi::nterm_host_output(host, bytes.as_ptr(), bytes.len());
                    });
                }
            }
            Frame::Screen { pane, bytes } => {
                if let Some(link) = daemon.pane(pane) {
                    link.with_host(|host| unsafe {
                        ghostty_ffi::nterm_host_resync(host, bytes.as_ptr(), bytes.len());
                    });
                }
            }
            Frame::Snapshot { request, bytes } => {
                if let Some(waiting) = daemon.snapshots.lock().unwrap().remove(&request) {
                    let _ = waiting.send(bytes);
                }
            }
            Frame::Control(Control::Panes { request, panes }) => {
                if let Some(parked) = daemon.listings.lock().unwrap().get_mut(&request) {
                    parked.listing.panes = panes;
                    parked.panes = true;
                }
                CLIENT.wake_ui();
            }
            Frame::Control(Control::Blobs { request, blobs }) => {
                if let Some(parked) = daemon.listings.lock().unwrap().get_mut(&request) {
                    parked.listing.blobs = blobs;
                    parked.blobs = true;
                }
                CLIENT.wake_ui();
            }
            Frame::Control(Control::Spawned { request, info }) => {
                let link = daemon.spawning.lock().unwrap().remove(&request);
                if let Some(link) = link {
                    link.spawned(&info);
                }
            }
            Frame::Control(Control::Exited { pane, .. }) => {
                if let Some(link) = daemon.pane(pane) {
                    link.close_pane();
                }
            }
            Frame::Control(Control::Error { request, message }) => {
                // Dropping the sender tells whoever waits for a snapshot.
                if let Some(id) = request {
                    daemon.snapshots.lock().unwrap().remove(&id);
                    daemon.listings.lock().unwrap().remove(&id);
                }
                let failed = request.and_then(|id| daemon.spawning.lock().unwrap().remove(&id));
                match failed {
                    Some(link) => {
                        report("The shell could not be started", &message);
                        link.close_pane();
                    }
                    None => report("Session daemon error", &message),
                }
            }
            _ => {}
        }
    };
    // The daemon is gone and its shells with it. Closing the queue ends
    // the writer thread.
    daemon.gone.store(true, Ordering::Release);
    *daemon.outbox.lock().unwrap() = None;
    daemon.connection.close();
    daemon.snapshots.lock().unwrap().clear();
    daemon.listings.lock().unwrap().clear();
    let mut links: Vec<Arc<Link>> = daemon.panes.lock().unwrap().drain().map(|(_, l)| l).collect();
    links.extend(daemon.spawning.lock().unwrap().drain().map(|(_, link)| link));
    if !links.is_empty() {
        report("The session daemon went away", &format!("{error}\n\nIts panes are closed."));
    }
    // Only if this is still the connection in use: a later one is not lost.
    {
        let mut state = CLIENT.state.lock().unwrap();
        if matches!(&*state, Conn::Connected(current) if current.number == daemon.number) {
            *state = Conn::Lost;
        }
    }
    CLIENT.changed.notify_all();
    CLIENT.wake_ui();
    for link in links {
        link.close_pane();
    }
}

/// What the surface would have run, as the fork describes it.
struct SpawnSpec {
    command: String,
    cwd: Option<String>,
    env: Vec<(String, String)>,
}

#[derive(Default)]
struct State {
    /// The daemon's pane, once known.
    pane: Option<PaneId>,
    /// A running pane to attach to instead of starting a shell.
    adopt: Option<PaneInfo>,
    spec: Option<SpawnSpec>,
    /// Whether the daemon has been asked to start or attach the pane.
    started: bool,
    /// The grid at the last layout, to tell when it has settled.
    laid_out: Option<(u16, u16)>,
    /// The size the daemon should have; sent once the pane is known.
    size: Option<Size>,
    sent_size: Option<Size>,
    queued_input: Vec<u8>,
    /// The shell's pid and creation time.
    shell: (u32, u64),
    closed: bool,
    /// The surface is going but the shell is to stay.
    keep: bool,
}

/// One pane's tie to the daemon. The surface's callbacks arrive here from
/// its IO thread; frames arrive from the session thread.
pub(crate) struct Link {
    daemon: Arc<Daemon>,
    /// The waiting shell this pane took, which no other pane may take while
    /// this link lasts.
    adopted: Option<PaneId>,
    /// The pane's `WakeState`, for telling the UI the shell has ended.
    wake: usize,
    /// The surface, or zero before it exists and after it is destroyed. Held
    /// while the surface is fed, so it cannot be destroyed underneath.
    host: Mutex<usize>,
    state: Mutex<State>,
}

impl Link {
    fn with_host(&self, feed: impl FnOnce(*mut Host)) {
        let host = self.host.lock().unwrap();
        if *host != 0 {
            feed(*host as *mut Host);
        }
    }

    /// A link for a pane whose shell is to run in the daemon. The pane's
    /// surface is then created with the link's `backend`, which makes it a
    /// mirror of the daemon's pane. `wake` is the pane's `WakeState`.
    ///
    /// `Ok(None)` when the daemon is off or cannot be reached: the surface
    /// runs its own shell. `Err` when a shell was to be taken and is not
    /// there to take; starting another in its place would pass a new shell
    /// off as the one asked for.
    pub(crate) fn prepare(wake: *mut c_void, start: &Start) -> Result<Option<Arc<Link>>, String> {
        let daemon = daemon();
        let adopt = match (start, &daemon) {
            (Start::Shell, _) => None,
            (Start::Adopt(_), None) => {
                return Err("the session daemon cannot be reached".into());
            }
            (Start::Adopt(adopt), Some(daemon)) => {
                let pane = adopt.info.pane;
                if adopt.daemon != daemon.number {
                    return Err(format!("shell {pane} was in a daemon that has gone"));
                }
                // A listing can be older than another pane's taking of it.
                if !daemon.taken.lock().unwrap().insert(pane) {
                    return Err(format!("shell {pane} is already shown"));
                }
                Some(adopt.info.clone())
            }
        };
        let Some(daemon) = daemon else { return Ok(None) };
        let link = Arc::new(Link {
            daemon,
            adopted: adopt.as_ref().map(|info| info.pane),
            wake: wake as usize,
            host: Mutex::new(0),
            state: Mutex::new(State { adopt, ..State::default() }),
        });
        Ok(Some(link))
    }

    /// What the surface is created with: the callbacks that make it this
    /// link's mirror. The surface copies it. The callbacks borrow the
    /// pane's reference to the link, so the pane keeps that reference until
    /// the surface is destroyed.
    pub(crate) fn backend(self: &Arc<Self>) -> ghostty_ffi::HostBackend {
        ghostty_ffi::HostBackend {
            userdata: Arc::as_ptr(self).cast_mut().cast(),
            write: on_write,
            resize: on_resize,
            spawn: on_spawn,
            settings: on_settings,
        }
    }

    /// The surface exists.
    pub(crate) fn attach(&self, host: *mut Host) {
        *self.host.lock().unwrap() = host as usize;
    }

    /// Called at every layout. The shell is started, or the pane attached,
    /// once the grid has been the same for two layouts in a row, so that it
    /// starts at the size it will keep. Returns true while still waiting.
    pub(crate) fn layout(self: &Arc<Self>, surface: SurfaceSize) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.started || state.closed {
            return false;
        }
        let grid = (surface.columns, surface.rows);
        if grid.0 == 0 || grid.1 == 0 || state.laid_out != Some(grid) {
            state.laid_out = Some(grid);
            return true;
        }
        let size = Size {
            cols: surface.columns,
            rows: surface.rows,
            cell_width: surface.cell_width_px,
            cell_height: surface.cell_height_px,
        };
        state.started = true;
        state.size = Some(size);
        if let Some(pane) = state.adopt.take() {
            drop(state);
            self.spawned(&pane);
            return false;
        }
        let Some(spec) = state.spec.take() else {
            drop(state);
            report("The shell could not be started", "The terminal did not describe it.");
            self.close_pane();
            return false;
        };
        drop(state);
        let request = self.daemon.next_request.fetch_add(1, Ordering::Relaxed);
        self.daemon.spawning.lock().unwrap().insert(request, self.clone());
        self.daemon.control(Control::Spawn {
            request,
            command: spec.command,
            cwd: spec.cwd,
            env: spec.env,
            size,
        });
        false
    }

    /// The daemon's pane is known: attach to it at the size wanted now.
    fn spawned(self: &Arc<Self>, info: &PaneInfo) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            drop(state);
            self.daemon.control(Control::Close { pane: info.pane });
            return;
        }
        state.pane = Some(info.pane);
        state.shell = (info.pid, info.creation_time);
        // What the shell told the terminal before this UI was there.
        let wake = self.wake as *mut c_void;
        for (text, tell) in [
            (&info.title, nterm_title as unsafe extern "C" fn(*mut c_void, *const std::ffi::c_char)),
            (&info.pwd, nterm_pwd),
        ] {
            if let Some(text) = text.as_deref().and_then(|text| std::ffi::CString::new(text).ok()) {
                unsafe { tell(wake, text.as_ptr()) };
            }
        }
        let size = state.size.unwrap_or(info.size);
        state.sent_size = Some(size);
        let queued = std::mem::take(&mut state.queued_input);
        // Registered before the attach is sent, so its screen finds us.
        self.daemon.panes.lock().unwrap().insert(info.pane, self.clone());
        self.daemon.control(Control::Attach { pane: info.pane, size });
        if !queued.is_empty() {
            self.daemon.send(Frame::Input { pane: info.pane, bytes: queued });
        }
    }

    /// The daemon's pane: known at once for a shell taken over, and once the
    /// daemon has answered for one this pane started.
    pub(crate) fn pane_id(&self) -> Option<PaneId> {
        let state = self.state.lock().unwrap();
        state.pane.or(state.adopt.as_ref().map(|pane| pane.pane))
    }

    /// The shell's pid and creation time, or zeros while unknown.
    pub(crate) fn shell(&self) -> (u32, u64) {
        self.state.lock().unwrap().shell
    }

    /// The pane's whole terminal as the daemon holds it, history included.
    /// The surface keeps only its screen, so anything that reads history
    /// asks here. Waits for the answer. `Ok(None)` before the shell has
    /// started, when there is no history; `Err` when the daemon did not
    /// answer.
    pub(crate) fn snapshot(&self) -> Result<Option<Vec<u8>>, ()> {
        let Some(pane) = self.state.lock().unwrap().pane else {
            return Ok(None);
        };
        if self.daemon.gone.load(Ordering::Acquire) {
            return Err(());
        }
        let request = self.daemon.next_request.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.daemon.snapshots.lock().unwrap().insert(request, sender);
        self.daemon.control(Control::Snapshot { request, pane });
        let bytes = receiver.recv_timeout(SNAPSHOT_WAIT).ok();
        if bytes.is_none() {
            self.daemon.snapshots.lock().unwrap().remove(&request);
        }
        bytes.map(Some).ok_or(())
    }

    /// Tell the UI this pane's shell is gone.
    fn close_pane(&self) {
        let host = self.host.lock().unwrap();
        if *host != 0 {
            crate::terminal::close_pane_from_session(self.wake as *mut c_void);
        } else {
            // The surface does not exist yet; it is closed once it does.
            self.state.lock().unwrap().closed = true;
        }
    }

    /// Whether the pane should close as soon as its surface exists.
    pub(crate) fn closed_early(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    /// The pane's window is closing, not the pane: its shell stays in the
    /// daemon when the surface is destroyed.
    pub(crate) fn keep_shell(&self) {
        self.state.lock().unwrap().keep = true;
    }

    /// The surface is about to be destroyed: stop feeding it, and end the
    /// shell unless it is to be kept.
    pub(crate) fn detach(&self) {
        *self.host.lock().unwrap() = 0;
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        let keep = state.keep;
        if let Some(pane) = state.pane.take() {
            drop(state);
            self.daemon.panes.lock().unwrap().remove(&pane);
            self.daemon.control(if keep {
                Control::Detach { pane }
            } else {
                // Nothing will run `npick` from it again.
                remove_blob(picker_key(pane));
                Control::Close { pane }
            });
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Some(pane) = self.adopted {
            self.daemon.taken.lock().unwrap().remove(&pane);
        }
    }
}

fn link<'a>(userdata: *mut c_void) -> &'a Link {
    unsafe { &*userdata.cast::<Link>() }
}

/// Bytes for the shell: what the user did, encoded by the surface.
extern "C" fn on_write(userdata: *mut c_void, bytes: *const u8, len: usize) {
    crate::native_error::ffi_boundary("send input to the session daemon", || {
        let link = link(userdata);
        let bytes = unsafe { std::slice::from_raw_parts(bytes, len) };
        let mut state = link.state.lock().unwrap();
        match state.pane {
            Some(pane) => {
                drop(state);
                link.daemon.send(Frame::Input { pane, bytes: bytes.to_vec() });
            }
            None if state.queued_input.len() + bytes.len() <= QUEUED_INPUT_MAX => {
                state.queued_input.extend_from_slice(bytes);
            }
            None => {}
        }
    });
}

/// The surface reflowed to a new size.
extern "C" fn on_resize(userdata: *mut c_void, cols: u16, rows: u16, width: u32, height: u32) {
    crate::native_error::ffi_boundary("send a resize to the session daemon", || {
        if cols == 0 || rows == 0 {
            return;
        }
        let link = link(userdata);
        let size = Size {
            cols,
            rows,
            cell_width: width / cols as u32,
            cell_height: height / rows as u32,
        };
        let mut state = link.state.lock().unwrap();
        // Before the pane starts, the layout decides its size.
        if !state.started {
            return;
        }
        state.size = Some(size);
        let Some(pane) = state.pane else { return };
        let grid = |size: Size| (size.cols, size.rows);
        if state.sent_size.map(grid) == Some(grid(size)) {
            return;
        }
        state.sent_size = Some(size);
        drop(state);
        link.daemon.control(Control::Resize { pane, size });
    });
}

extern "C" fn on_spawn(userdata: *mut c_void, spawn: *const HostSpawn) {
    crate::native_error::ffi_boundary("describe a shell for the session daemon", || {
        let link = link(userdata);
        let spawn = unsafe { &*spawn };
        let text = |ptr| unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        let env = if spawn.env_len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(spawn.env, spawn.env_len) }
        };
        link.state.lock().unwrap().spec = Some(SpawnSpec {
            command: text(spawn.command),
            cwd: (!spawn.cwd.is_null()).then(|| text(spawn.cwd)),
            env: env.iter().map(|var| (text(var.key), text(var.value))).collect(),
        });
    });
}

/// The surface's terminal settings, at creation and on every change. The
/// daemon's terminals must match or the two screens drift apart.
extern "C" fn on_settings(userdata: *mut c_void, settings: *const HostSettings) {
    crate::native_error::ffi_boundary("send terminal settings to the session daemon", || {
        let link = link(userdata);
        let from = unsafe { &*settings };
        let text = |bytes: &[u8]| {
            let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        };
        let mut last = link.daemon.settings.lock().unwrap();
        let mut settings = TerminalSettings {
            foreground: Some(from.foreground),
            background: Some(from.background),
            cursor: from.has_cursor.then_some(from.cursor),
            palette: from.palette.to_vec(),
            cursor_style: Some(match from.cursor_style {
                0 => CursorStyle::Bar,
                2 => CursorStyle::Underline,
                3 => CursorStyle::BlockHollow,
                _ => CursorStyle::Block,
            }),
            cursor_blink: (from.cursor_blink >= 0).then_some(from.cursor_blink > 0),
            image_storage_limit: Some(from.image_storage_limit),
            dark: Some(from.dark),
            identity: Some(Identity {
                xtversion: text(&from.xtversion),
                enquiry: text(&from.enquiry),
                conformance: from.da_conformance,
                features: from.da_features[..(from.da_features_len as usize).min(4)].to_vec(),
                secondary: from.da_secondary,
            }),
            // Known only when a surface is created; kept across changes.
            scrollback_bytes: last.scrollback_bytes,
            scrollback_lines: last.scrollback_lines,
            grapheme_cluster: last.grapheme_cluster,
        };
        if from.has_creation {
            settings.scrollback_bytes = Some(from.scrollback_bytes);
            settings.scrollback_lines = Some(from.scrollback_lines);
            settings.grapheme_cluster = Some(from.grapheme_cluster);
        }
        if *last != settings {
            *last = settings.clone();
            link.daemon.control(Control::Settings { settings });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session and daemon program a test connects to, in place of the
    /// user's own. Setting it also turns the session daemon on.
    pub(super) static TARGET: Mutex<Option<(String, std::path::PathBuf)>> = Mutex::new(None);

    const WAIT: Duration = Duration::from_secs(20);

    fn until<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn listing() -> Listing {
        let asked = request_listing().expect("connected");
        until("a listing", || match take_listing(asked) {
            Answer::Ready(listing) => Some(listing),
            Answer::Waiting => None,
            Answer::Gone => panic!("the listing was lost"),
        })
    }

    /// One test, because there is one connection for the process: it is
    /// made off the calling thread, a listing is parked until taken and
    /// taken once, a lost daemon is noticed, and connecting again reaches
    /// a new one.
    #[test]
    #[cfg(windows)]
    fn connects_parks_listings_and_connects_again() {
        // The daemon binary is built beside the tests' directory.
        let program = std::env::current_exe().unwrap();
        let program = program.parent().unwrap().parent().unwrap().join("nterm-daemon.exe");
        assert!(program.exists(), "build nterm-daemon first: {}", program.display());
        let session = format!("test-client-{}", std::process::id());
        *TARGET.lock().unwrap() = Some((session, program));

        // The launch waits for the connection and takes what the daemon held.
        let held = launch_listing().expect("the daemon was started");
        assert!(held.panes.is_empty() && held.blobs.is_empty());
        assert!(matches!(&*CLIENT.state.lock().unwrap(), Conn::Connected(_)));
        // It is taken once.
        assert!(launch_listing().is_none());
        let first = daemon().unwrap().number;

        // What is queued is written in order: the blob is there to be listed.
        set_blob("picker/7".into(), r#"["endpoint","token"]"#.into());
        let asked = request_listing().unwrap();
        let listed = until("a listing", || match take_listing(asked) {
            Answer::Ready(listing) => Some(listing),
            _ => None,
        });
        assert_eq!(listed.blobs, vec![("picker/7".to_owned(), r#"["endpoint","token"]"#.to_owned())]);
        // Taking it left nothing behind.
        assert!(matches!(take_listing(asked), Answer::Gone));
        // No shell 7 is waiting, so there is nothing to take.
        assert!(listed.adopt(7).is_none());

        // The daemon goes away: the connection is lost, and what was asked
        // of it will not come.
        let asked = request_listing().unwrap();
        daemon().unwrap().control(Control::Shutdown);
        until("the loss", || matches!(&*CLIENT.state.lock().unwrap(), Conn::Lost).then_some(()));
        assert!(daemon().is_none());
        assert!(request_listing().is_none());
        assert!(matches!(take_listing(asked), Answer::Gone));

        // Connecting again starts a new daemon, which holds nothing.
        connect();
        until("the new connection", || daemon().map(|_| ()));
        assert_ne!(daemon().unwrap().number, first);
        assert!(matches!(take_listing(asked), Answer::Gone));
        assert!(listing().blobs.is_empty());

        // Leaving waits for the queue to be written.
        daemon().unwrap().control(Control::Shutdown);
        flush();
        until("the end", || daemon().is_none().then_some(()));
        *TARGET.lock().unwrap() = None;
    }
}

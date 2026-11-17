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

use crate::{
    ghostty_ffi::{self, Host, HostSettings, HostSpawn, SurfaceSize},
    picker::pipe::{
        Event,
        relay::{self, Request},
    },
};
use crossbeam_channel::{Receiver, Sender};
use nterm_session::{
    DEFAULT_SESSION, GHOSTTY_REVISION,
    pipe::{self, Connection},
    protocol::{
        Control, CursorStyle, Frame, Identity, PROTOCOL_VERSION, PaneId, PaneInfo, SessionInfo,
        Size, TerminalSettings,
    },
};
use std::{
    collections::HashMap,
    ffi::{CStr, c_void},
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

/// How long copy mode, search and the pickers wait for a pane's history.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(5);

/// How long a launch waits for the connection before it goes on without.

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
    /// The connection whose listing this came from.
    daemon: u64,
}

/// The sessions the daemon holds, as it gave them once. The daemon gives
/// them again whenever one changes, and `sessions` has the last.
#[derive(Clone, Debug, Default)]
pub(crate) struct Listing {
    pub(crate) sessions: Vec<SessionInfo>,
    /// Counts what the connection has given: a list with another number is
    /// another list.
    serial: u64,
    /// The connection that gave it. Its pane numbers mean nothing to
    /// another daemon.
    daemon: u64,
}

impl Listing {
    #[cfg(test)]
    pub(crate) fn of(sessions: Vec<SessionInfo>) -> Self {
        Self { sessions, ..Self::default() }
    }

    /// What tells this list from every other the process is given.
    pub(crate) fn stamp(&self) -> (u64, u64) {
        (self.daemon, self.serial)
    }

    /// What a pane needs to take the shell `pane`, if it is waiting.
    pub(crate) fn adopt(&self, pane: PaneId) -> Option<Adopt> {
        let mut panes = self.sessions.iter().flat_map(|session| &session.panes);
        let info = panes.find(|info| info.pane == pane && waiting(info))?.clone();
        Some(Adopt { info, daemon: self.daemon })
    }
}

/// Whether a pane's shell is running with nothing showing it.
pub(crate) fn waiting(pane: &PaneInfo) -> bool {
    pane.exited.is_none() && pane.attached == 0
}


/// Tell the daemon of a workspace: the panes in it, and how they are laid
/// out, for whichever UI shows it next. Does nothing without a connection.
pub(crate) fn set_session(id: String, panes: Vec<PaneId>, layout: String) {
    if let Some(daemon) = daemon() {
        daemon.control(Control::SessionSet { id, panes, layout });
    }
}

pub(crate) fn remove_session(id: String) {
    if let Some(daemon) = daemon() {
        daemon.control(Control::SessionRemove { id });
    }
}

/// The sessions the daemon holds, as it last gave them. `None` without a
/// connection.
pub(crate) fn sessions() -> Option<Arc<Listing>> {
    Some(daemon()?.sessions.lock().unwrap().clone())
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
    Unavailable(String),
    Lost,
}

/// The connection, whatever state it is in. There is one for the process.
struct Client {
    state: Mutex<Conn>,
    /// Told of every change of `state`.
    changed: Condvar,
    /// Counts the connections made, to tell one from the next.
    connections: Mutex<u64>,
    /// Wakes the UI when something it may be waiting for arrives.
    ui: Mutex<Option<Box<dyn Fn() + Send>>>,
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
        if let Some(wake) = &*self.ui.lock().unwrap() {
            wake();
        }
    }
}

/// How to wake the UI when the connection changes or a listing arrives.
/// It is called from the session's threads, so it must draw every window
/// whichever thread asks. A repaint asked of the egui context does not: off
/// the UI thread it is asked of the root viewport, which no window is, and
/// egui passes on only the first such request until the root is next run.
pub(crate) fn set_ui(wake: impl Fn() + Send + 'static) {
    *CLIENT.ui.lock().unwrap() = Some(Box::new(wake));
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
                let error = error.to_string();
                report("Session daemon unavailable", &error);
                Conn::Unavailable(error)
            }
        });
    });
    if let Err(error) = started {
        let error = error.to_string();
        report("Session daemon unavailable", &error);
        CLIENT.set(Conn::Unavailable(error));
    }
}

/// Where the connection to the daemon stands, for a window to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Shells are not kept in a daemon: each pane runs its own.
    Off,
    /// `connect` has not been called.
    Idle,
    Connecting,
    Connected,
    /// The daemon could not be reached or started, and why.
    Unavailable(String),
    /// The daemon was connected and has gone.
    Lost,
}

pub(crate) fn status() -> Status {
    if !enabled() {
        return Status::Off;
    }
    match &*CLIENT.state.lock().unwrap() {
        Conn::Idle => Status::Idle,
        Conn::Connecting => Status::Connecting,
        Conn::Connected(_) => Status::Connected,
        Conn::Unavailable(error) => Status::Unavailable(error.clone()),
        Conn::Lost => Status::Lost,
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
    /// Links waiting for the daemon to answer a `Spawn` or an `Attach`, by
    /// request.
    starting: Mutex<HashMap<u32, Arc<Link>>>,
    /// The programs in shells that are talking to panes here, by the
    /// daemon's number for each conversation.
    programs: Mutex<HashMap<u32, Program>>,
    /// Who waits for each snapshot asked for, by request.
    snapshots: Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>,
    /// The sessions the daemon holds, as it last gave them.
    sessions: Mutex<Arc<Listing>>,
    next_request: AtomicU32,
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
        // The daemon says what sessions it holds as soon as it has greeted.
        let sessions = loop {
            match Frame::read(&mut reader)? {
                Frame::Control(Control::Sessions { sessions }) => break sessions,
                Frame::Control(Control::Error { message, .. }) => return Err(other(message)),
                _ => {}
            }
        };
        if protocol != PROTOCOL_VERSION || ghostty != GHOSTTY_REVISION {
            let panes: usize = sessions.iter().map(|session| session.panes.len()).sum();
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
            starting: Mutex::default(),
            programs: Mutex::default(),
            snapshots: Mutex::default(),
            sessions: Mutex::new(Arc::new(Listing { sessions, serial: 0, daemon: number })),
            next_request: AtomicU32::new(1),
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

/// A program in a pane's shell that is talking to the pane.
struct Program {
    link: Arc<Link>,
    /// What it asked, once it has.
    request: Option<Request>,
}

impl Daemon {
    /// A conversation is over. `tell` when the daemon does not know: the
    /// picker answered, or the program broke the protocol.
    fn end_program(&self, channel: u32, tell: bool) {
        let Some(program) = self.programs.lock().unwrap().remove(&channel) else { return };
        if let Some(request) = &program.request {
            request.gone();
        }
        if tell {
            self.control(Control::ShellClosed { channel });
        }
        program.link.programs.fetch_sub(1, Ordering::AcqRel);
        program.link.wake_pane();
    }
}

/// A message from a program in a shell.
fn said(daemon: &Arc<Daemon>, channel: u32, bytes: &[u8]) {
    let mut programs = daemon.programs.lock().unwrap();
    let Some(program) = programs.get_mut(&channel) else { return };
    let event = match &mut program.request {
        Some(request) => request.accept(bytes),
        None => Request::open(bytes).map(|(request, event, replies)| {
            program.request = Some(request);
            answer(daemon.clone(), channel, replies);
            Some(event)
        }),
    };
    match event {
        Ok(Some(event)) => {
            let _ = program.link.requests.0.send(event);
            program.link.wake_pane();
        }
        Ok(None) => {}
        Err(_) => {
            drop(programs);
            daemon.end_program(channel, true);
        }
    }
}

/// Pass a picker's replies to the program that asked, as they come. They
/// come from the UI thread and from the picker's own.
fn answer(daemon: Arc<Daemon>, channel: u32, replies: Receiver<serde_json::Value>) {
    let pass = move || {
        for reply in replies {
            let Ok(bytes) = serde_json::to_vec(&reply) else { break };
            daemon.send(Frame::Shell { channel, bytes });
            if relay::last(&reply) {
                break;
            }
        }
        // Answered, or dropped with no answer: either way it is over.
        daemon.end_program(channel, true);
    };
    let _ = std::thread::Builder::new().name("picker replies".into()).spawn(pass);
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
            Frame::Control(Control::Sessions { sessions }) => {
                let mut held = daemon.sessions.lock().unwrap();
                // The daemon says again what it said at the greeting.
                if held.sessions != sessions {
                    let serial = held.serial + 1;
                    *held = Arc::new(Listing { sessions, serial, daemon: daemon.number });
                    drop(held);
                    CLIENT.wake_ui();
                }
            }
            Frame::Control(Control::Spawned { request, info }) => {
                let link = daemon.starting.lock().unwrap().remove(&request);
                if let Some(link) = link {
                    link.spawned(&info);
                }
            }
            Frame::Control(Control::Attached { request, .. }) => {
                let link = daemon.starting.lock().unwrap().remove(&request);
                if let Some(link) = link {
                    link.attached();
                }
            }
            Frame::Control(Control::ShellOpen { channel, pane }) => match daemon.pane(pane) {
                Some(link) => {
                    link.programs.fetch_add(1, Ordering::AcqRel);
                    link.wake_pane();
                    let program = Program { link, request: None };
                    daemon.programs.lock().unwrap().insert(channel, program);
                }
                // The pane went as the program came.
                None => daemon.control(Control::ShellClosed { channel }),
            },
            Frame::Shell { channel, bytes } => said(daemon, channel, &bytes),
            Frame::Control(Control::ShellClosed { channel }) => daemon.end_program(channel, false),
            Frame::Control(Control::Exited { pane, .. }) => {
                if let Some(link) = daemon.pane(pane) {
                    link.close_pane();
                }
            }
            Frame::Control(Control::Error { request, message }) => {
                // Dropping the sender tells whoever waits for a snapshot.
                if let Some(id) = request {
                    daemon.snapshots.lock().unwrap().remove(&id);
                }
                let failed = request.and_then(|id| daemon.starting.lock().unwrap().remove(&id));
                match failed {
                    Some(link) => link.refused("The shell could not be shown", &message),
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
    let programs: Vec<u32> = daemon.programs.lock().unwrap().keys().copied().collect();
    for channel in programs {
        daemon.end_program(channel, false);
    }
    let mut links: Vec<Arc<Link>> = daemon.panes.lock().unwrap().drain().map(|(_, l)| l).collect();
    links.extend(daemon.starting.lock().unwrap().drain().map(|(_, link)| link));
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

/// What a link does once its surface has a size.
enum Want {
    /// Start what the surface would have run, once the fork has described
    /// it.
    Spawn(Option<SpawnSpec>),
    /// Take a shell the daemon is running.
    Adopt(PaneInfo),
}

/// Where a link is. It moves down this list, or from anywhere to `Closed`.
enum Phase {
    /// The surface is being made and laid out. `grid` is its grid at the
    /// last layout, to tell when it has settled.
    Sizing { want: Want, grid: Option<(u16, u16)> },
    /// The daemon has been asked to start the shell.
    Spawning,
    /// The daemon has been asked to attach this link to `pane`, at the size
    /// `sent`.
    Attaching { pane: PaneId, sent: Size },
    /// `pane` is this link's: its output comes here and input goes to it.
    Attached { pane: PaneId, sent: Size },
    /// Over: the pane was closed, or the daemon refused or went away.
    Closed,
}

struct State {
    phase: Phase,
    /// The size the surface has now, for the daemon to match.
    size: Option<Size>,
    /// Input typed before the shell is this link's.
    queued_input: Vec<u8>,
}

impl State {
    /// The daemon's pane, once the daemon has been asked for it.
    fn pane(&self) -> Option<PaneId> {
        match self.phase {
            Phase::Attaching { pane, .. } | Phase::Attached { pane, .. } => Some(pane),
            _ => None,
        }
    }
}

/// What becomes of the shell when its pane goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// It ends with the pane.
    Close,
    /// It stays in the daemon, with nothing showing it.
    Keep,
}

/// One pane's tie to the daemon. The surface's callbacks arrive here from
/// its IO thread; frames arrive from the session thread; the pane calls
/// from the UI thread.
pub(crate) struct Link {
    daemon: Arc<Daemon>,
    /// Whether the shell was taken, not started by this pane.
    adopted: bool,
    /// What programs in the shell ask of the pane, for the pane to take.
    requests: (Sender<Event>, Receiver<Event>),
    /// How many programs in the shell are talking to the pane.
    programs: AtomicUsize,
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
    /// `Ok(None)` when shells are not kept in a daemon: the surface runs
    /// its own. `Err` when they are and the daemon cannot be reached, or
    /// when the shell to be taken cannot be. A shell of the surface's own
    /// in its place would not outlive the window, and nothing would say so.
    pub(crate) fn prepare(wake: *mut c_void, start: &Start) -> Result<Option<Arc<Link>>, String> {
        let daemon = daemon();
        let want = match (start, &daemon) {
            (Start::Shell, _) => Want::Spawn(None),
            (Start::Adopt(_), None) => {
                return Err("the session daemon cannot be reached".into());
            }
            (Start::Adopt(adopt), Some(daemon)) => {
                if adopt.daemon != daemon.number {
                    let pane = adopt.info.pane;
                    return Err(format!("shell {pane} was in a daemon that has gone"));
                }
                Want::Adopt(adopt.info.clone())
            }
        };
        let Some(daemon) = daemon else {
            return match enabled() {
                true => Err("the session daemon cannot be reached".into()),
                false => Ok(None),
            };
        };
        let link = Arc::new(Link {
            daemon,
            adopted: matches!(want, Want::Adopt(_)),
            requests: crossbeam_channel::unbounded(),
            programs: AtomicUsize::new(0),
            wake: wake as usize,
            host: Mutex::new(0),
            state: Mutex::new(State {
                phase: Phase::Sizing { want, grid: None },
                size: None,
                queued_input: Vec::new(),
            }),
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

    /// Called at every layout while `Sizing`. Once the grid has been the
    /// same for two layouts in a row, the daemon is asked to start the
    /// shell, or to attach to the one being taken, so that it starts at the
    /// size it will keep. Returns true while still waiting for that.
    pub(crate) fn layout(self: &Arc<Self>, surface: SurfaceSize) -> bool {
        let mut state = self.state.lock().unwrap();
        let Phase::Sizing { want, grid } = &mut state.phase else {
            return false;
        };
        let now = (surface.columns, surface.rows);
        if now.0 == 0 || now.1 == 0 || *grid != Some(now) {
            *grid = Some(now);
            return true;
        }
        let size = Size {
            cols: surface.columns,
            rows: surface.rows,
            cell_width: surface.cell_width_px,
            cell_height: surface.cell_height_px,
        };
        let want = std::mem::replace(want, Want::Spawn(None));
        state.size = Some(size);
        match want {
            Want::Adopt(info) => self.ask_attach(state, &info),
            Want::Spawn(Some(spec)) => {
                state.phase = Phase::Spawning;
                drop(state);
                self.ask(|request| Control::Spawn {
                    request,
                    command: spec.command,
                    cwd: spec.cwd,
                    env: spec.env,
                    size,
                });
            }
            Want::Spawn(None) => {
                drop(state);
                self.refused("The shell could not be started", "The terminal did not describe it.");
            }
        }
        false
    }

    /// Ask the daemon for something it answers by request number, with
    /// this link waiting for the answer. If the daemon has gone there will
    /// be none, and the pane closes.
    fn ask(self: &Arc<Self>, control: impl FnOnce(u32) -> Control) {
        let request = self.daemon.next_request.fetch_add(1, Ordering::Relaxed);
        self.daemon.starting.lock().unwrap().insert(request, self.clone());
        self.daemon.control(control(request));
        // Checked after the link is where the reader looks when the daemon
        // goes, so one of the two sees it.
        if self.daemon.gone.load(Ordering::Acquire)
            && self.daemon.starting.lock().unwrap().remove(&request).is_some()
        {
            self.refused("The shell could not be started", "The session daemon has gone.");
        }
    }

    /// The daemon's pane is known: ask to be attached to it, alone, at the
    /// size wanted now.
    fn ask_attach(self: &Arc<Self>, mut state: std::sync::MutexGuard<'_, State>, info: &PaneInfo) {
        let pane = info.pane;
        let size = state.size.unwrap_or(info.size);
        state.phase = Phase::Attaching { pane, sent: size };
        drop(state);
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
        // Registered before the attach is sent, so its screen finds us.
        self.daemon.panes.lock().unwrap().insert(pane, self.clone());
        self.ask(|request| Control::Attach { request, pane, size, exclusive: true });
    }

    /// The daemon has started the shell this link asked for.
    fn spawned(self: &Arc<Self>, info: &PaneInfo) {
        let state = self.state.lock().unwrap();
        if !matches!(state.phase, Phase::Spawning) {
            // The pane was closed while the shell was being started.
            drop(state);
            self.daemon.control(Control::Close { pane: info.pane });
            return;
        }
        self.ask_attach(state, info);
    }

    /// The daemon has attached this link to its pane: the shell is ours.
    fn attached(&self) {
        let mut state = self.state.lock().unwrap();
        let Phase::Attaching { pane, sent } = state.phase else {
            // Closed meanwhile; `detach` has dealt with the pane.
            return;
        };
        state.phase = Phase::Attached { pane, sent };
        let queued = std::mem::take(&mut state.queued_input);
        drop(state);
        if !queued.is_empty() {
            self.daemon.send(Frame::Input { pane, bytes: queued });
        }
    }

    /// The daemon would not start or attach the shell, or cannot. The pane
    /// closes. A shell this pane started ends with it, as nothing shows
    /// it; one it meant to take is not this link's to end.
    fn refused(&self, summary: &str, details: &str) {
        report(summary, details);
        let phase = std::mem::replace(&mut self.state.lock().unwrap().phase, Phase::Closed);
        if let Phase::Attaching { pane, .. } = phase {
            self.daemon.panes.lock().unwrap().remove(&pane);
            if !self.adopted {
                self.daemon.control(Control::Close { pane });
            }
        }
        self.close_pane();
    }

    /// The daemon's pane: known at once for a shell taken over, and once
    /// the daemon has answered for one this pane started.
    pub(crate) fn pane_id(&self) -> Option<PaneId> {
        let state = self.state.lock().unwrap();
        match &state.phase {
            Phase::Sizing { want: Want::Adopt(info), .. } => Some(info.pane),
            _ => state.pane(),
        }
    }

    /// The pane's whole terminal as the daemon holds it, history included.
    /// The surface keeps only its screen, so anything that reads history
    /// asks here. Waits for the answer. `Ok(None)` before the shell has
    /// started, when there is no history; `Err` when the daemon did not
    /// answer.
    pub(crate) fn snapshot(&self) -> Result<Option<Vec<u8>>, ()> {
        let Some(pane) = self.state.lock().unwrap().pane() else {
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

    /// Have the pane's window draw: a program in the shell has asked
    /// something, or has gone.
    fn wake_pane(&self) {
        if *self.host.lock().unwrap() != 0 {
            crate::terminal::wake_pane_from_session(self.wake as *mut c_void);
        }
    }

    /// What a program in the shell asks next, if anything.
    pub(crate) fn request(&self) -> Option<Event> {
        self.requests.1.try_recv().ok()
    }

    /// Whether a program in the shell is talking to the pane.
    pub(crate) fn has_program(&self) -> bool {
        self.programs.load(Ordering::Acquire) > 0
    }

    /// Tell the UI this pane's shell is gone. The pane then closes, which
    /// comes back here as `detach`.
    fn close_pane(&self) {
        if *self.host.lock().unwrap() != 0 {
            crate::terminal::close_pane_from_session(self.wake as *mut c_void);
        }
    }

    /// The surface is about to be destroyed: stop feeding it, and settle
    /// what becomes of the shell.
    pub(crate) fn detach(&self, end: End) {
        *self.host.lock().unwrap() = 0;
        let phase = std::mem::replace(&mut self.state.lock().unwrap().phase, Phase::Closed);
        let (pane, ours) = match phase {
            Phase::Attached { pane, .. } => (pane, true),
            // The daemon has not said the shell is this link's. One this
            // pane started is; one it meant to take may be another's.
            Phase::Attaching { pane, .. } => (pane, !self.adopted),
            _ => return,
        };
        self.daemon.panes.lock().unwrap().remove(&pane);
        // The daemon ends its side of these when the pane is let go.
        let programs: Vec<u32> = {
            let programs = self.daemon.programs.lock().unwrap();
            let mine = programs.iter().filter(|(_, p)| std::ptr::eq(&*p.link, self));
            mine.map(|(channel, _)| *channel).collect()
        };
        for channel in programs {
            self.daemon.end_program(channel, false);
        }
        self.daemon.control(if end == End::Close && ours {
            Control::Close { pane }
        } else {
            Control::Detach { pane }
        });
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
        match state.phase {
            Phase::Attached { pane, .. } => {
                drop(state);
                link.daemon.send(Frame::Input { pane, bytes: bytes.to_vec() });
            }
            Phase::Closed => {}
            _ if state.queued_input.len() + bytes.len() <= QUEUED_INPUT_MAX => {
                state.queued_input.extend_from_slice(bytes);
            }
            _ => {}
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
        // While sizing, the layout decides the size the shell starts at.
        if matches!(state.phase, Phase::Sizing { .. } | Phase::Closed) {
            return;
        }
        state.size = Some(size);
        let (Phase::Attaching { pane, sent } | Phase::Attached { pane, sent }) = &mut state.phase
        else {
            // Spawning: the attach that follows is sent this size.
            return;
        };
        if (sent.cols, sent.rows) == (size.cols, size.rows) {
            return;
        }
        *sent = size;
        let pane = *pane;
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
        let spec = SpawnSpec {
            command: text(spawn.command),
            cwd: (!spawn.cwd.is_null()).then(|| text(spawn.cwd)),
            env: env.iter().map(|var| (text(var.key), text(var.value))).collect(),
        };
        // A link that takes a running shell has no use for it.
        if let Phase::Sizing { want: Want::Spawn(wanted), .. } =
            &mut link.state.lock().unwrap().phase
        {
            *wanted = Some(spec);
        }
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
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// One test, because there is one connection for the process: it is
    /// made off the calling thread, the sessions are there as soon as it is
    /// and follow what the daemon is told, a lost daemon is noticed, and
    /// connecting again reaches a new one.
    #[test]
    #[cfg(windows)]
    fn connects_follows_the_sessions_and_connects_again() {
        // The daemon binary is built beside the tests' directory.
        let program = std::env::current_exe().unwrap();
        let program = program.parent().unwrap().parent().unwrap().join("nterm-daemon.exe");
        assert!(program.exists(), "build nterm-daemon first: {}", program.display());
        let session = format!("test-client-{}", std::process::id());
        *TARGET.lock().unwrap() = Some((session, program));

        // Once connected, what the daemon holds is known.
        assert_eq!(status(), Status::Idle);
        assert!(sessions().is_none());
        connect();
        until("the connection", || (status() == Status::Connected).then_some(()));
        let held = sessions().expect("the daemon was started");
        assert!(held.sessions.is_empty());
        let first = daemon().unwrap().number;

        // A shell is started, and the list follows without being asked for.
        let daemon_now = daemon().unwrap();
        daemon_now.control(Control::Spawn {
            request: 9,
            command: "cmd.exe /d /q /k".into(),
            cwd: None,
            env: vec![],
            size: Size { cols: 80, rows: 24, cell_width: 9, cell_height: 18 },
        });
        let listed = until("the shell", || sessions().filter(|held| !held.sessions.is_empty()));
        assert_ne!(listed.stamp(), held.stamp());
        let pane = listed.sessions[0].panes[0].pane;
        assert_eq!(listed.sessions[0].id, "");
        // Nothing shows it, so it is there to take; no other shell is.
        assert!(listed.adopt(pane).is_some());
        assert!(listed.adopt(pane + 1).is_none());

        // What is queued is written in order: the session is the shell's.
        set_session("one".into(), vec![pane], "a layout".into());
        let listed = until("the session", || {
            sessions().filter(|held| held.sessions.iter().any(|session| session.id == "one"))
        });
        assert_eq!(listed.sessions[0].layout, "a layout");
        remove_session("one".into());
        until("its removal", || sessions().filter(|held| held.sessions[0].id.is_empty()));

        // The daemon goes away: the connection is lost, and nothing is
        // known of its sessions.
        daemon_now.control(Control::Shutdown);
        until("the loss", || (status() == Status::Lost).then_some(()));
        assert!(daemon().is_none());
        assert!(sessions().is_none());

        // Connecting again starts a new daemon, which holds nothing.
        connect();
        until("the new connection", || daemon().map(|_| ()));
        assert_ne!(daemon().unwrap().number, first);
        let held = sessions().unwrap();
        assert!(held.sessions.is_empty());
        assert_ne!(held.stamp(), listed.stamp());

        // Leaving waits for the queue to be written.
        daemon().unwrap().control(Control::Shutdown);
        flush();
        until("the end", || daemon().is_none().then_some(()));
        *TARGET.lock().unwrap() = None;
    }
}

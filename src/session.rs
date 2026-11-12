//! The UI's end of the session daemon.
//!
//! With `terminal.session_daemon` on, a pane's shell runs in the daemon. The
//! pane's surface has no process of its own: it is a mirror that is given the
//! daemon's screen and then the shell's output, and that sends back only what
//! the user did. The daemon's terminal answers the program's queries.
//!
//! One connection serves every pane. A thread reads it and hands each frame
//! to the pane it names.
//!
//! Until layouts are restored, leaving nterm closes its panes as it always
//! has. A UI that is killed leaves them running, and the next one's first
//! pane attaches to the oldest of them.

use crate::ghostty_ffi::{self, Host, HostSettings, HostSpawn, SurfaceSize};
use nterm_session::{
    DEFAULT_SESSION, GHOSTTY_REVISION,
    pipe::{self, Connection, Writer},
    protocol::{
        Control, CursorStyle, Frame, Identity, PROTOCOL_VERSION, PaneId, PaneInfo, Size,
        TerminalSettings,
    },
};
use std::{
    collections::HashMap,
    ffi::{CStr, c_void},
    io,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::Duration,
};

/// How long copy mode, search and the pickers wait for a pane's history.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(5);

/// Input typed before the shell exists is kept, up to this much.
const QUEUED_INPUT_MAX: usize = 1 << 16;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Called with the configuration before the first pane is made.
pub(crate) fn configure(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
}

/// `NTERM_SESSION_DAEMON=1` or `0` overrides the configuration.
fn enabled() -> bool {
    match std::env::var("NTERM_SESSION_DAEMON").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => ENABLED.load(Ordering::Acquire),
    }
}

fn report(summary: &str, details: &str) {
    crate::error_log::error("session", summary, details);
}

struct Client {
    connection: Connection,
    writer: Mutex<Writer>,
    panes: Mutex<HashMap<PaneId, Arc<Link>>>,
    /// Panes whose `Spawn` has not been answered, by request.
    spawning: Mutex<HashMap<u32, Arc<Link>>>,
    /// Who waits for each snapshot asked for, by request.
    snapshots: Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>,
    next_request: AtomicU32,
    /// Running panes nothing was attached to when this UI connected.
    adoptable: Mutex<Vec<PaneInfo>>,
    /// What the daemon was last told.
    settings: Mutex<TerminalSettings>,
    lost: AtomicBool,
}

impl Client {
    fn send(&self, frame: &Frame) {
        if frame.write(&mut *self.writer.lock().unwrap()).is_err() {
            self.connection.close();
        }
    }

    fn control(&self, control: Control) {
        self.send(&Frame::Control(control));
    }

    /// The pane's link, without holding the table while it is used.
    fn pane(&self, pane: PaneId) -> Option<Arc<Link>> {
        self.panes.lock().unwrap().get(&pane).cloned()
    }
}

/// The connection, made on first use. `None` when the daemon is off or could
/// not be reached; panes then run their own shells.
fn client() -> Option<&'static Arc<Client>> {
    static CLIENT: OnceLock<Option<Arc<Client>>> = OnceLock::new();
    if !enabled() {
        return None;
    }
    CLIENT
        .get_or_init(|| match connect() {
            Ok(client) => Some(client),
            Err(error) => {
                report(
                    "Session daemon unavailable",
                    &format!("{error}\n\nPanes run their own shells for this run."),
                );
                None
            }
        })
        .as_ref()
}

fn connect() -> io::Result<Arc<Client>> {
    let daemon = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "nterm-daemon.exe"
    } else {
        "nterm-daemon"
    });
    let other = |message: String| io::Error::other(message);
    // A daemon from another build cannot be trusted to speak the same
    // snapshot format. One with no panes is replaced; one with panes is left
    // alone and not used.
    for attempt in 0..2 {
        let connection = pipe::connect_or_start(DEFAULT_SESSION, &daemon)?;
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
        let panes = loop {
            match Frame::read(&mut reader)? {
                Frame::Control(Control::Panes { panes, .. }) => break panes,
                Frame::Control(Control::Error { message, .. }) => return Err(other(message)),
                _ => {}
            }
        };
        if protocol != PROTOCOL_VERSION || ghostty != GHOSTTY_REVISION {
            if panes.is_empty() && attempt == 0 {
                Frame::Control(Control::Shutdown).write(&mut writer)?;
                // Wait for it to go, so the next connect starts a new one.
                while Frame::read(&mut reader).is_ok() {}
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            return Err(other(format!(
                "a daemon from another build is running with {} panes \
                 (protocol {protocol}, ghostty {ghostty}; this build has \
                 protocol {PROTOCOL_VERSION}, ghostty {GHOSTTY_REVISION})",
                panes.len()
            )));
        }
        let mut adoptable: Vec<PaneInfo> = panes
            .into_iter()
            .filter(|pane| pane.exited.is_none() && pane.attached == 0)
            .collect();
        adoptable.sort_by_key(|pane| std::cmp::Reverse(pane.pane));
        let client = Arc::new(Client {
            connection,
            writer: Mutex::new(writer),
            panes: Mutex::default(),
            spawning: Mutex::default(),
            snapshots: Mutex::default(),
            next_request: AtomicU32::new(1),
            adoptable: Mutex::new(adoptable),
            settings: Mutex::default(),
            lost: AtomicBool::new(false),
        });
        let reading = client.clone();
        std::thread::Builder::new()
            .name("session".into())
            .spawn(move || read(&reading, reader))?;
        return Ok(client);
    }
    unreachable!("the second attempt returns")
}

fn read(client: &Arc<Client>, mut reader: impl io::Read) {
    let error = loop {
        let frame = match Frame::read(&mut reader) {
            Ok(frame) => frame,
            Err(error) => break error,
        };
        match frame {
            Frame::Output { pane, bytes } => {
                if let Some(link) = client.pane(pane) {
                    link.with_host(|host| unsafe {
                        ghostty_ffi::nterm_host_output(host, bytes.as_ptr(), bytes.len());
                    });
                }
            }
            Frame::Screen { pane, bytes } => {
                if let Some(link) = client.pane(pane) {
                    link.with_host(|host| unsafe {
                        ghostty_ffi::nterm_host_resync(host, bytes.as_ptr(), bytes.len());
                    });
                }
            }
            Frame::Snapshot { request, bytes } => {
                if let Some(waiting) = client.snapshots.lock().unwrap().remove(&request) {
                    let _ = waiting.send(bytes);
                }
            }
            Frame::Control(Control::Spawned { request, info }) => {
                let link = client.spawning.lock().unwrap().remove(&request);
                if let Some(link) = link {
                    link.spawned(&info);
                }
            }
            Frame::Control(Control::Exited { pane, .. }) => {
                if let Some(link) = client.pane(pane) {
                    link.close_pane();
                }
            }
            Frame::Control(Control::Error { request, message }) => {
                // Dropping the sender tells whoever waits for a snapshot.
                if let Some(id) = request {
                    client.snapshots.lock().unwrap().remove(&id);
                }
                let failed = request.and_then(|id| client.spawning.lock().unwrap().remove(&id));
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
    // The daemon is gone and its shells with it.
    client.lost.store(true, Ordering::Release);
    client.snapshots.lock().unwrap().clear();
    let mut links: Vec<Arc<Link>> = client.panes.lock().unwrap().drain().map(|(_, l)| l).collect();
    links.extend(client.spawning.lock().unwrap().drain().map(|(_, link)| link));
    if !links.is_empty() {
        report("The session daemon went away", &format!("{error}\n\nIts panes are closed."));
    }
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
}

/// One pane's tie to the daemon. The surface's callbacks arrive here from
/// its IO thread; frames arrive from the session thread.
pub(crate) struct Link {
    client: Arc<Client>,
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

    /// Make the next surface created on this thread a mirror of a daemon
    /// pane. `wake` is the pane's `WakeState`. With `adopt`, a pane left
    /// running by an earlier UI is taken instead of starting a shell.
    /// `None` leaves the surface to run its own shell.
    pub(crate) fn prepare(wake: *mut c_void, adopt: bool) -> Option<Arc<Link>> {
        let client = client()?;
        if client.lost.load(Ordering::Acquire) {
            return None;
        }
        let adopt = if adopt { client.adoptable.lock().unwrap().pop() } else { None };
        let link = Arc::new(Link {
            client: client.clone(),
            wake: wake as usize,
            host: Mutex::new(0),
            state: Mutex::new(State { adopt, ..State::default() }),
        });
        // The surface's callbacks hold this reference until `release`.
        let userdata = Arc::into_raw(link.clone()).cast_mut().cast::<c_void>();
        let ok = unsafe {
            ghostty_ffi::nterm_host_backend_next(
                userdata,
                on_write,
                on_resize,
                on_spawn,
                on_settings,
                std::ptr::null(),
                0,
            )
        };
        if !ok {
            unsafe { drop(Arc::from_raw(userdata.cast::<Link>())) };
            return None;
        }
        Some(link)
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
        let request = self.client.next_request.fetch_add(1, Ordering::Relaxed);
        self.client.spawning.lock().unwrap().insert(request, self.clone());
        self.client.control(Control::Spawn {
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
            self.client.control(Control::Close { pane: info.pane });
            return;
        }
        state.pane = Some(info.pane);
        state.shell = (info.pid, info.creation_time);
        let size = state.size.unwrap_or(info.size);
        state.sent_size = Some(size);
        let queued = std::mem::take(&mut state.queued_input);
        // Registered before the attach is sent, so its screen finds us.
        self.client.panes.lock().unwrap().insert(info.pane, self.clone());
        self.client.control(Control::Attach { pane: info.pane, size });
        if !queued.is_empty() {
            self.client.send(&Frame::Input { pane: info.pane, bytes: queued });
        }
    }

    /// The shell's pid and creation time, or zeros while unknown.
    pub(crate) fn shell(&self) -> (u32, u64) {
        self.state.lock().unwrap().shell
    }

    /// The pane's whole terminal as the daemon holds it, history included.
    /// The surface keeps only its screen, so anything that reads history
    /// asks here. Waits for the answer; `None` when there is none to be had.
    pub(crate) fn snapshot(&self) -> Option<Vec<u8>> {
        let pane = self.state.lock().unwrap().pane?;
        if self.client.lost.load(Ordering::Acquire) {
            return None;
        }
        let request = self.client.next_request.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.client.snapshots.lock().unwrap().insert(request, sender);
        self.client.control(Control::Snapshot { request, pane });
        let bytes = receiver.recv_timeout(SNAPSHOT_WAIT).ok();
        if bytes.is_none() {
            self.client.snapshots.lock().unwrap().remove(&request);
        }
        bytes
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

    /// The surface is about to be destroyed: stop feeding it and end the
    /// shell.
    pub(crate) fn detach(&self) {
        *self.host.lock().unwrap() = 0;
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        if let Some(pane) = state.pane.take() {
            drop(state);
            self.client.panes.lock().unwrap().remove(&pane);
            self.client.control(Control::Close { pane });
        }
    }

    /// The surface is destroyed and will make no more callbacks.
    pub(crate) fn release(self: &Arc<Self>) {
        unsafe { Arc::decrement_strong_count(Arc::as_ptr(self)) };
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
                link.client.send(&Frame::Input { pane, bytes: bytes.to_vec() });
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
        link.client.control(Control::Resize { pane, size });
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
        let mut last = link.client.settings.lock().unwrap();
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
            link.client.control(Control::Settings { settings });
        }
    });
}

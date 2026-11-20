//! The daemon proper: panes that outlive the UIs attached to them.
//!
//! Threads, per pane: one reads the shell's output into the pane's terminal
//! and forwards it; one writes to the shell; one waits for the shell to end.
//! Per connection: one reads the UI's messages; one writes to the UI.
//!
//! Nothing that can block on the shell or on a UI happens under a pane's
//! lock. Writes to either go through a queue with its own thread.

use crate::{
    conpty,
    log::{log, trace},
    picker::{Pickers, Showing},
    vt::Terminal,
};
use nterm_session::{
    GHOSTTY_REVISION,
    pipe::{Connection, Listener},
    shell::{DAEMON_VARIABLE, PANE_VARIABLE, PIPE_VARIABLE, SHARE_VARIABLE},
    protocol::{
        Control, Frame, PROTOCOL_VERSION, PaneId, PaneInfo, SessionInfo, Size, TerminalSettings,
    },
};
use std::{
    collections::{BTreeMap, HashMap},
    io::{self, BufReader, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    time::{Duration, Instant},
};

pub struct Options {
    pub session: String,
    /// How long to stay with no panes and no UI before exiting.
    pub idle: Duration,
    /// The file of the pickers of this machine, in place of looking for
    /// it: see `configured`.
    pub pickers: Option<std::path::PathBuf>,
}

/// Encoded frames on their way to one UI, or to one program in a shell.
type Outbox = Sender<Vec<u8>>;

/// Queued in place of a frame: write what was queued before it, then close
/// the connection. No frame is empty.
const CLOSE: &[u8] = &[];

struct Session {
    name: String,
    panes: Mutex<HashMap<PaneId, Arc<Pane>>>,
    next_pane: AtomicU32,
    next_client: AtomicU64,
    /// What the UI last sent; new panes start from it.
    settings: Mutex<TerminalSettings>,
    /// The UIs' sessions, by the names the UIs gave them.
    sessions: Mutex<BTreeMap<String, Saved>>,
    /// Sent to when the sessions, or anything a UI is told of them, may have
    /// changed: see `tell_sessions`.
    sessions_changed: Sender<()>,
    clients: Mutex<HashMap<u64, Outbox>>,
    /// The conversations under way between programs in shells and UIs.
    channels: Mutex<HashMap<u32, Channel>>,
    next_channel: AtomicU32,
    /// The pickers the daemon is running for programs in shells.
    pickers: Pickers,
}

/// A program in a pane's shell talking to the UI attached to that pane. The
/// daemon passes their messages on unread. It lasts until either ends it, or
/// the UI leaves the pane.
struct Channel {
    pane: PaneId,
    /// The UI's client number and the way to it.
    ui: u64,
    ui_outbox: Outbox,
    /// The way to the program.
    shell: Outbox,
}

/// One workspace of a UI: the panes in it, and the UI's own account of how
/// they are laid out, which the daemon keeps and does not read.
struct Saved {
    panes: Vec<PaneId>,
    layout: String,
}

struct Pane {
    id: PaneId,
    command: String,
    process: conpty::Process,
    /// Bytes for the shell: the UI's input and the terminal's replies, in the
    /// order they were queued.
    input: Sender<Vec<u8>>,
    state: Mutex<State>,
}

struct State {
    terminal: Terminal,
    /// Gone once the shell has ended.
    console: Option<conpty::Console>,
    size: Size,
    /// The UI that shows this pane and gets its output. A pane is shown by
    /// one UI at a time.
    shown: Option<(u64, Outbox)>,
    exited: Option<u32>,
    /// The shell's output has ended. That it has before the shell did is
    /// what is left when the pseudoconsole goes on its own.
    silent: bool,
    /// Forget the pane as soon as its shell has ended.
    closing: bool,
}

impl Pane {
    fn info(&self) -> PaneInfo {
        self.info_of(&mut self.state.lock().unwrap())
    }

    /// The pane's info, for a caller that holds its state.
    fn info_of(&self, state: &mut State) -> PaneInfo {
        PaneInfo {
            title: state.terminal.title(),
            pwd: state.terminal.pwd(),
            pane: self.id,
            pid: self.process.pid,
            creation_time: self.process.creation_time,
            command: self.command.clone(),
            size: state.size,
            exited: state.exited,
            attached: state.shown.is_some(),
        }
    }
}

impl State {
    /// Send a frame to the UI that shows the pane. One that cannot be sent
    /// to has gone, and no longer shows it.
    fn send(&mut self, frame: Vec<u8>) {
        self.shown.take_if(|(_, outbox)| outbox.send(frame).is_err());
    }

    /// Reflow the terminal, then tell the pseudoconsole, as the UI's own
    /// terminal did when it owned the shell.
    fn resize(&mut self, pane: &Pane, size: Size) {
        if size.cols == 0 || size.rows == 0 || size == self.size || self.exited.is_some() {
            return;
        }
        let grid_changed = (size.cols, size.rows) != (self.size.cols, self.size.rows);
        self.size = size;
        let replies = self.terminal.resize(size);
        if !replies.is_empty() {
            let _ = pane.input.send(replies);
        }
        if grid_changed && let Some(console) = &self.console {
            console.resize(size.cols, size.rows);
        }
    }

    /// The pane's screen, for a UI to start from when it takes the pane
    /// or to look at. `pictures` for the pixels of the pictures on it.
    fn send_screen(&mut self, pane: PaneId, to: &Outbox, pictures: bool) -> bool {
        let frame = Frame::Screen { pane, bytes: self.terminal.screen(pictures) }.encode();
        to.send(frame).is_ok()
    }
}

impl Session {
    fn pane(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().unwrap().get(&id).cloned()
    }

    /// The folder pane `id`'s shell last reported, if there is one on this
    /// machine: where a shell started beside it starts.
    fn folder_of(&self, id: PaneId) -> Option<String> {
        let reported = self.pane(id)?.state.lock().unwrap().terminal.pwd()?;
        let folder = nterm_session::reported_folder(&reported);
        std::path::Path::new(&folder).is_dir().then_some(folder)
    }

    fn broadcast(&self, control: Control) {
        let frame = Frame::Control(control).encode();
        for outbox in self.clients.lock().unwrap().values() {
            let _ = outbox.send(frame.clone());
        }
    }

    /// The sessions may have changed; the UIs are told if they have. Nothing
    /// is worked out here, so this is safe under any lock.
    fn sessions_changed(&self) {
        let _ = self.sessions_changed.send(());
    }

    /// Every session with the shells of it that are running, and last the
    /// running shells in none. A session with no running shell is forgotten
    /// here: nothing could show it again.
    fn sessions(&self) -> Vec<SessionInfo> {
        let mut panes: Vec<Arc<Pane>> = self.panes.lock().unwrap().values().cloned().collect();
        panes.sort_by_key(|pane| pane.id);
        let mut running: BTreeMap<PaneId, PaneInfo> = panes
            .iter()
            .map(|pane| (pane.id, pane.info()))
            .filter(|(_, info)| info.exited.is_none())
            .collect();
        let mut sessions = self.sessions.lock().unwrap();
        let mut listed = Vec::new();
        sessions.retain(|id, saved| {
            let panes: Vec<PaneInfo> =
                saved.panes.iter().filter_map(|pane| running.remove(pane)).collect();
            let kept = !panes.is_empty();
            if kept {
                listed.push(SessionInfo { id: id.clone(), panes, layout: saved.layout.clone() });
            }
            kept
        });
        if !running.is_empty() {
            let panes = running.into_values().collect();
            listed.push(SessionInfo { id: String::new(), panes, layout: String::new() });
        }
        listed
    }

    /// End the conversations `which` picks. Each program is told and its
    /// connection closed; the UI is told too unless it is the one that has
    /// gone.
    fn end_channels(&self, which: impl Fn(u32, &Channel) -> bool, tell_ui: bool) {
        let ended: Vec<(u32, Channel)> = {
            let mut channels = self.channels.lock().unwrap();
            let numbers: Vec<u32> =
                channels.iter().filter(|(n, c)| which(**n, c)).map(|(n, _)| *n).collect();
            numbers.into_iter().filter_map(|n| Some((n, channels.remove(&n)?))).collect()
        };
        for (number, channel) in ended {
            let _ = channel.shell.send(Frame::Control(Control::ShellClosed { channel: 0 }).encode());
            let _ = channel.shell.send(CLOSE.to_vec());
            if tell_ui {
                let closed = Control::ShellClosed { channel: number };
                let _ = channel.ui_outbox.send(Frame::Control(closed).encode());
            }
        }
    }

    fn spawn(
        self: &Arc<Self>,
        command: String,
        cwd: Option<String>,
        mut env: Vec<(String, String)>,
        size: Size,
    ) -> Result<Arc<Pane>, String> {
        if size.cols == 0 || size.rows == 0 {
            return Err("a pane needs a size".into());
        }
        // The settings are in place before the shell can write a byte.
        let settings = self.settings.lock().unwrap().clone();
        let terminal = Terminal::new(size, &settings)?;
        // The shell is told which pane it is, so the number is taken before
        // it is started. One whose shell does not start is not used again.
        let id = self.next_pane.fetch_add(1, Ordering::Relaxed);
        // With none given the shell has the daemon's own, and still does.
        if env.is_empty() {
            env = std::env::vars_os()
                .map(|(key, value)| {
                    (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned())
                })
                .collect();
        }
        // A profile loads code from the share folder, so its path is only
        // ever this daemon's own: one a UI names, from wherever, is dropped.
        let ours = [DAEMON_VARIABLE, PANE_VARIABLE, PIPE_VARIABLE, SHARE_VARIABLE];
        env.retain(|(key, _)| !ours.iter().any(|ours| key.eq_ignore_ascii_case(ours)));
        if let Some(share) = crate::configured::installed().into_iter().find(|share| share.is_dir()) {
            env.push((SHARE_VARIABLE.into(), share.to_string_lossy().into_owned()));
        }
        if let Ok(pipe) = nterm_session::pipe::name(&self.name) {
            env.push((PIPE_VARIABLE.into(), pipe));
        }
        env.push((DAEMON_VARIABLE.into(), self.name.clone()));
        env.push((PANE_VARIABLE.into(), id.to_string()));
        // With none given it is the user's own, not the daemon's, which is
        // where the daemon keeps its log. A window on another machine gives
        // none: it knows no directory of this one.
        let cwd = cwd.or_else(|| std::env::var("USERPROFILE").ok());
        let spawned = conpty::spawn(&command, cwd.as_deref(), &env, size.cols, size.rows)
            .map_err(|error| error.to_string())?;
        let conpty::Spawned { console, output, input, process } = spawned;

        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        let pane = Arc::new(Pane {
            id,
            command,
            process,
            input: input_tx,
            state: Mutex::new(State {
                terminal,
                console: Some(console),
                size,
                shown: None,
                exited: None,
                silent: false,
                closing: false,
            }),
        });
        self.panes.lock().unwrap().insert(id, pane.clone());
        self.sessions_changed();
        log(format_args!(
            "pane {id}: pid {} {}x{} {:?}",
            pane.process.pid, size.cols, size.rows, pane.command
        ));

        let thread = |name: &str| std::thread::Builder::new().name(format!("pane {id} {name}"));
        thread("input")
            .spawn(move || {
                for bytes in input_rx {
                    trace(id, "<", &bytes);
                    if let Err(code) = input.write(&bytes) {
                        // Nothing more can be given to the shell. Said, for
                        // this is how a pseudoconsole that went on its own
                        // is first met.
                        log(format_args!("pane {id}: input not taken (error {code})"));
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        let reader = {
            let pane = pane.clone();
            thread("output")
                .spawn(move || {
                    let mut buffer = vec![0u8; 65536];
                    loop {
                        let count = match output.read(&mut buffer) {
                            Ok(count) if count > 0 => count,
                            end => {
                                let mut state = pane.state.lock().unwrap();
                                state.silent = true;
                                // The pseudoconsole is closed here once the
                                // shell has ended. Output that ends while it
                                // is still open ended some other way.
                                if state.console.is_some() {
                                    log(format_args!(
                                        "pane {}: output ended (error {}) with its shell still running",
                                        pane.id,
                                        end.err().unwrap_or(0)
                                    ));
                                }
                                break;
                            }
                        };
                        let bytes = &buffer[..count];
                        trace(pane.id, ">", bytes);
                        let mut state = pane.state.lock().unwrap();
                        // This terminal answers the program's queries whether
                        // or not a UI is attached.
                        let replies = state.terminal.feed(bytes);
                        if !replies.is_empty() {
                            let _ = pane.input.send(replies);
                        }
                        if state.shown.is_some() {
                            state.send(Frame::encode_output(pane.id, bytes));
                        }
                    }
                })
                .map_err(|error| error.to_string())?
        };

        let session = self.clone();
        let waiting = pane.clone();
        thread("wait")
            .spawn(move || {
                let pane = waiting;
                let code = pane.process.wait();
                // Closing the pseudoconsole flushes what the shell last
                // wrote and ends the reader. Not under the lock: it blocks
                // until the reader, which takes the lock, has drained it.
                let (console, silent) = {
                    let mut state = pane.state.lock().unwrap();
                    (state.console.take(), state.silent)
                };
                drop(console);
                let _ = reader.join();
                let closing = {
                    let mut state = pane.state.lock().unwrap();
                    state.exited = Some(code);
                    state.closing
                };
                log(format_args!(
                    "pane {}: exited with {code}{}",
                    pane.id,
                    if silent { ", after its output had ended" } else { "" }
                ));
                // With no UI connected there is nobody to close it, and it
                // would keep the daemon from ever being idle.
                if closing || session.clients.lock().unwrap().is_empty() {
                    session.panes.lock().unwrap().remove(&pane.id);
                }
                session.end_channels(|_, channel| channel.pane == pane.id, true);
                session.pickers.end_where(|of, _| of == Some(pane.id), true);
                session.broadcast(Control::Exited { pane: pane.id, code });
                session.sessions_changed();
            })
            .map_err(|error| error.to_string())?;
        Ok(pane)
    }

    fn shutdown(&self) -> ! {
        log(format_args!("shutdown requested"));
        for pane in self.panes.lock().unwrap().values() {
            pane.process.terminate();
        }
        std::process::exit(0);
    }
}

/// Serve the session until told to shut down or left idle.
pub fn run(options: Options) -> io::Result<()> {
    let mut listener = Listener::bind(&options.session)?;    let (sessions_changed, changes) = mpsc::channel();
    let session = Arc::new(Session {
        sessions: Mutex::default(),
        sessions_changed,
        name: options.session,
        panes: Mutex::default(),
        next_pane: AtomicU32::new(1),
        next_client: AtomicU64::new(1),
        settings: Mutex::default(),
        clients: Mutex::default(),
        channels: Mutex::default(),
        next_channel: AtomicU32::new(1),
        pickers: Pickers::new(options.pickers.clone()),
    });
    log(format_args!(
        "serving session {:?}, pid {}, ghostty {GHOSTTY_REVISION}, conpty {}",
        session.name,
        std::process::id(),
        conpty::source()
    ));

    {
        let session = session.clone();
        let idle = options.idle;
        std::thread::spawn(move || {
            let mut since = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let busy = !session.panes.lock().unwrap().is_empty()
                    || !session.clients.lock().unwrap().is_empty();
                if busy {
                    since = Instant::now();
                } else if since.elapsed() >= idle {
                    log(format_args!("no panes and no UI; exiting"));
                    std::process::exit(0);
                }
            }
        });
    }

    {
        // One thread tells the UIs of the sessions, so that what they are
        // told is in order and a burst of changes is one message.
        let session = session.clone();
        std::thread::Builder::new().name("sessions".into()).spawn(move || {
            let mut told = Vec::new();
            loop {
                // A shell changes its title or its directory without anything
                // being asked of the daemon, so look now and then as well.
                if let Err(mpsc::RecvTimeoutError::Disconnected) =
                    changes.recv_timeout(Duration::from_secs(1))
                {
                    return;
                }
                while changes.try_recv().is_ok() {}
                if session.clients.lock().unwrap().is_empty() {
                    continue;
                }
                let sessions = session.sessions();
                let frame = Frame::Control(Control::Sessions { sessions }).encode();
                if frame != told {
                    for outbox in session.clients.lock().unwrap().values() {
                        let _ = outbox.send(frame.clone());
                    }
                    told = frame;
                }
            }
        })?;
    }

    loop {
        match listener.accept() {
            Ok(connection) => {
                let session = session.clone();
                std::thread::spawn(move || serve(&session, connection));
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                log(format_args!("client refused: {error}"));
            }
            Err(error) => {
                log(format_args!("accept failed: {error}"));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn serve(session: &Arc<Session>, connection: Connection) {
    let id = session.next_client.fetch_add(1, Ordering::Relaxed);
    let began = std::time::Instant::now();
    let (outbox, queued) = mpsc::channel::<Vec<u8>>();
    // Why the writer closed the connection, when it did: the reader then
    // fails only because of that.
    let closed = Arc::new(Mutex::new(None::<String>));
    let writer = {
        let connection = connection.clone();
        let closed = closed.clone();
        std::thread::spawn(move || {
            let Ok(mut writer) = connection.writer() else { return };
            for frame in queued {
                if frame == CLOSE {
                    // Asked for: what came before it has been written.
                    *closed.lock().unwrap() = Some("the daemon closed the connection".into());
                    connection.close();
                    break;
                }
                if let Err(error) = writer.write_all(&frame) {
                    // Wake the reading side; this UI is gone.
                    *closed.lock().unwrap() = Some(format!("writing to it failed: {error}"));
                    connection.close();
                    break;
                }
            }
        })
    };

    let result = converse(session, &connection, id, &outbox);
    let why = match (closed.lock().unwrap().take(), result) {
        (Some(closed), _) => closed,
        (None, Ok(())) => "it closed the connection".to_owned(),
        (None, Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
            format!("what it sent could not be read: {error}")
        }
        (None, Err(error)) => format!("reading from it failed: {error}"),
    };
    log(format_args!(
        "client {id} (pid {}) gone after {}: {why}",
        connection.peer_pid(),
        crate::log::duration(began.elapsed()),
    ));

    let last = {
        let mut clients = session.clients.lock().unwrap();
        clients.remove(&id);
        clients.is_empty()
    };
    let panes: Vec<_> = session.panes.lock().unwrap().values().cloned().collect();
    for pane in panes {
        let mut state = pane.state.lock().unwrap();
        state.shown.take_if(|(client, _)| *client == id);
        // The last UI has gone: an ended shell it left has nobody to close
        // it. Running ones wait for the next UI.
        if last && state.exited.is_some() {
            drop(state);
            session.panes.lock().unwrap().remove(&pane.id);
        }
    }
    // After it has left every pane, so no program finds it there.
    session.end_channels(|_, channel| channel.ui == id, false);
    session.pickers.end_where(|_, ui| ui == id, false);
    // What it showed is waiting now.
    session.sessions_changed();
    // With every sender gone the writer sends what is queued and ends.
    drop(outbox);
    let _ = writer.join();
    connection.close();
}

/// The first of two variables the environment has: Windows's name, then
/// another system's. Empty when it has neither.
fn either(name: &str, other: &str) -> String {
    std::env::var(name).or_else(|_| std::env::var(other)).unwrap_or_default()
}

fn converse(
    session: &Arc<Session>,
    connection: &Connection,
    id: u64,
    outbox: &Outbox,
) -> io::Result<()> {
    let mut reader = BufReader::with_capacity(1 << 16, connection.reader()?);
    let send = |control: Control| {
        let _ = outbox.send(Frame::Control(control).encode());
    };
    let fail = |request: Option<u32>, message: String| {
        send(Control::Error { request, message });
    };

    let first = match Frame::read(&mut reader) {
        Ok(frame) => frame,
        // Something only checked that the daemon is there.
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Frame::Control(Control::Shell { pane }) = first {
        return relay(session, connection, &mut reader, pane, outbox);
    }
    if let Frame::Control(Control::Pick { pane }) = first {
        return pick(session, connection, &mut reader, pane, outbox);
    }
    let Frame::Control(Control::Hello { protocol, ghostty }) = first else {
        fail(None, "expected hello".into());
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no hello"));
    };
    send(Control::Welcome {
        protocol: PROTOCOL_VERSION,
        ghostty: GHOSTTY_REVISION.to_owned(),
        pid: std::process::id(),
        session: session.name.clone(),
        files: true,
        machine: either("COMPUTERNAME", "HOSTNAME"),
        user: either("USERNAME", "USER"),
    });
    if protocol != PROTOCOL_VERSION {
        fail(None, format!("protocol {protocol} is not supported; this daemon speaks {PROTOCOL_VERSION}"));
        return Err(io::Error::new(io::ErrorKind::InvalidData, "protocol version"));
    }
    log(format_args!(
        "client {id} (pid {}) connected, ghostty {ghostty}",
        connection.peer_pid()
    ));
    session.clients.lock().unwrap().insert(id, outbox.clone());
    // A UI starts from what there is, and hears of changes from then on.
    send(Control::Sessions { sessions: session.sessions() });

    loop {
        let frame = match Frame::read(&mut reader) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        let control = match frame {
            Frame::Input { pane, bytes } => {
                if let Some(pane) = session.pane(pane) {
                    let _ = pane.input.send(bytes);
                }
                continue;
            }
            Frame::Control(control) => control,
            // Kept away from this thread, so the UI's typing is not held
            // up while a large file is written.
            Frame::File { request, extension, bytes } => {
                let outbox = outbox.clone();
                let keep = move || {
                    let answer = match crate::files::keep(&crate::files::directory(), &extension, &bytes) {
                        Ok(path) => {
                            log(format_args!("client {id}: kept {} bytes as {}", bytes.len(), path.display()));
                            Control::FileKept { request, path: path.to_string_lossy().into_owned() }
                        }
                        Err(message) => {
                            log(format_args!("client {id}: file not kept: {message}"));
                            Control::Error { request: Some(request), message }
                        }
                    };
                    let _ = outbox.send(Frame::Control(answer).encode());
                };
                if let Err(error) = std::thread::Builder::new().name("keep file".into()).spawn(keep) {
                    let message = format!("the file could not be kept: {error}");
                    fail(Some(request), message);
                }
                continue;
            }
            // Frames only the daemon sends.
            Frame::Shell { channel, bytes } => {
                let channels = session.channels.lock().unwrap();
                if let Some(to) = channels.get(&channel).filter(|c| c.ui == id) {
                    let _ = to.shell.send(Frame::Shell { channel: 0, bytes }.encode());
                }
                continue;
            }
            Frame::Output { .. }
            | Frame::Screen { .. }
            | Frame::Resized { .. }
            | Frame::Previewed { .. }
            | Frame::Snapshot { .. } => continue,
        };
        let missing = |request: Option<u32>, pane: PaneId| {
            fail(request, format!("no pane {pane}"));
        };
        if session.pickers.from_ui(id, &control) {
            continue;
        }
        if let Control::FoldersAsk { ask, roots, depth } = &control {
            session.pickers.folders(id, outbox, *ask, roots, *depth);
            continue;
        }
        if let Control::PickerAsk { ask, pane, picker, params } = &control {
            let shows = session.pane(*pane).is_some_and(|pane| {
                pane.state.lock().unwrap().shown.as_ref().is_some_and(|(shown, _)| *shown == id)
            });
            let of = session.pane(*pane);
            let at_cursor = || match &of {
                Some(pane) => pane.state.lock().unwrap().terminal.at_cursor(),
                None => Default::default(),
            };
            session.pickers.asked(id, outbox, *ask, *pane, picker, params, shows, &at_cursor);
            continue;
        }
        match control {
            Control::Settings { settings } => {
                *session.settings.lock().unwrap() = settings.clone();
                let panes: Vec<_> = session.panes.lock().unwrap().values().cloned().collect();
                for pane in panes {
                    let refused = pane.state.lock().unwrap().terminal.apply(&settings);
                    if !refused.is_empty() {
                        log(format_args!("pane {}: settings refused: {refused:?}", pane.id));
                    }
                }
            }
            Control::Spawn { request, command, cwd, beside, env, size } => {
                let cwd = beside.and_then(|pane| session.folder_of(pane)).or(cwd);
                match session.spawn(command, cwd, env, size) {
                    Ok(pane) => send(Control::Spawned { request, info: pane.info() }),
                    Err(message) => fail(Some(request), message),
                }
            }
            Control::Attach { request, pane: pane_id, size } => {
                let Some(pane) = session.pane(pane_id) else {
                    missing(Some(request), pane_id);
                    continue;
                };
                // The check, the resize, the screen and the subscription
                // under one lock: of two UIs that ask for a pane one is
                // refused, and the output that follows starts exactly
                // where the screen ends.
                let mut state = pane.state.lock().unwrap();
                if state.shown.as_ref().is_some_and(|(client, _)| *client != id) {
                    drop(state);
                    fail(Some(request), format!("pane {pane_id} is already shown"));
                    continue;
                }
                log(format_args!(
                    "pane {pane_id}: client {id} attaches at {}x{}, was {}x{}",
                    size.cols, size.rows, state.size.cols, state.size.rows
                ));
                state.resize(&pane, size);
                let mut info = pane.info_of(&mut state);
                info.attached = true;
                send(Control::Attached { request, info });
                state.shown =
                    state.send_screen(pane_id, outbox, true).then(|| (id, outbox.clone()));
                if let Some(code) = state.exited {
                    send(Control::Exited { pane: pane_id, code });
                }
                session.sessions_changed();
            }
            Control::Peek { pane: pane_id } => {
                // A pane that has gone is not an error to one only looking.
                // It asks again and again, so it is not sent pixels.
                if let Some(pane) = session.pane(pane_id) {
                    pane.state.lock().unwrap().send_screen(pane_id, outbox, false);
                }
            }
            Control::Detach { pane: pane_id } => {
                if let Some(pane) = session.pane(pane_id) {
                    pane.state.lock().unwrap().shown.take_if(|(client, _)| *client == id);
                }
                // After it has left the pane, so no program finds it there.
                session.end_channels(|_, c| c.pane == pane_id && c.ui == id, false);
                session.pickers.end_where(|pane, ui| pane == Some(pane_id) && ui == id, true);
                session.sessions_changed();
            }
            Control::SessionSet { id: name, panes, layout } => {
                if name.is_empty() {
                    continue;
                }
                let mut sessions = session.sessions.lock().unwrap();
                // A pane is in the session that named it last: the UI that
                // shows it knows where it is.
                for (other, saved) in sessions.iter_mut() {
                    if *other != name {
                        saved.panes.retain(|pane| !panes.contains(pane));
                    }
                }
                sessions.retain(|_, saved| !saved.panes.is_empty());
                sessions.insert(name, Saved { panes, layout });
                drop(sessions);
                session.sessions_changed();
            }
            Control::Resize { pane: pane_id, size } => {
                let Some(pane) = session.pane(pane_id) else {
                    missing(None, pane_id);
                    continue;
                };
                let mut state = pane.state.lock().unwrap();
                if (size.cols, size.rows) != (state.size.cols, state.size.rows) {
                    log(format_args!(
                        "pane {pane_id}: resized to {}x{}, was {}x{}",
                        size.cols, size.rows, state.size.cols, state.size.rows
                    ));
                }
                state.resize(&pane, size);
                // Between the output before the resize and the output after,
                // so the UI resizes its copy at the same point and the copy
                // stays the same as this.
                let (cols, rows) = (state.size.cols, state.size.rows);
                state.send(Frame::Resized { pane: pane_id, cols, rows }.encode());
            }
            Control::Snapshot { request, pane: pane_id } => {
                let Some(pane) = session.pane(pane_id) else {
                    missing(Some(request), pane_id);
                    continue;
                };
                let bytes = pane.state.lock().unwrap().terminal.snapshot();
                let _ = outbox.send(Frame::Snapshot { request, bytes }.encode());
            }
            Control::Close { pane: pane_id } => {
                let Some(pane) = session.pane(pane_id) else { continue };
                // An exit that follows is this client's doing, and the log
                // of a lost shell says so.
                log(format_args!("pane {pane_id}: client {id} asks to close it"));
                let mut state = pane.state.lock().unwrap();
                if state.exited.is_some() {
                    drop(state);
                    session.panes.lock().unwrap().remove(&pane_id);
                } else {
                    state.closing = true;
                    pane.process.terminate();
                }
            }
            Control::List { request } => {
                let mut panes: Vec<_> =
                    session.panes.lock().unwrap().values().cloned().collect();
                panes.sort_by_key(|pane| pane.id);
                let panes = panes.iter().map(|pane| pane.info()).collect();
                send(Control::Panes { request, panes });
            }
            Control::ShellClosed { channel } => {
                session.end_channels(|number, c| number == channel && c.ui == id, false);
            }
            Control::Shutdown => session.shutdown(),
            // A second hello, the daemon's own messages, or something newer.
            _ => {}
        }
    }
}

/// A program in a pane's shell. What it sends goes to the UI showing the
/// pane, until one of them is done or the UI leaves the pane.
fn relay(
    session: &Arc<Session>,
    connection: &Connection,
    reader: &mut impl io::Read,
    pane_id: PaneId,
    outbox: &Outbox,
) -> io::Result<()> {
    let refuse = |message: String| {
        log(format_args!("program {} refused for pane {pane_id}: {message}", connection.peer_pid()));
        let _ = outbox.send(Frame::Control(Control::Error { request: None, message }).encode());
        Ok(())
    };
    let Some(pane) = session.pane(pane_id) else {
        return refuse(format!("no pane {pane_id}"));
    };
    let number = session.next_channel.fetch_add(1, Ordering::Relaxed);
    let ui = {
        // Under the pane's lock, as leaving a pane is: a UI that leaves
        // either is not found here or finds the channel to end.
        let state = pane.state.lock().unwrap();
        let Some((ui, ui_outbox)) = state.shown.clone() else {
            drop(state);
            return refuse("no window is showing this pane".into());
        };
        session.channels.lock().unwrap().insert(
            number,
            Channel { pane: pane_id, ui, ui_outbox: ui_outbox.clone(), shell: outbox.clone() },
        );
        ui_outbox
    };
    let _ = ui.send(Frame::Control(Control::ShellOpen { channel: number, pane: pane_id }).encode());
    loop {
        match Frame::read(reader) {
            Ok(Frame::Shell { bytes, .. }) => {
                if ui.send(Frame::Shell { channel: number, bytes }.encode()).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // Still there: the program went first, and the UI has to be told.
    if session.channels.lock().unwrap().remove(&number).is_some() {
        let _ = ui.send(Frame::Control(Control::ShellClosed { channel: number }).encode());
    }
    Ok(())
}

/// A program in a pane's shell that wants a picker. The daemon runs it, and
/// the UI showing the pane shows it; see `crate::picker`.
fn pick(
    session: &Arc<Session>,
    connection: &Connection,
    reader: &mut impl io::Read,
    pane_id: PaneId,
    outbox: &Outbox,
) -> io::Result<()> {
    let Some(pane) = session.pane(pane_id) else {
        let message = format!("no pane {pane_id}");
        log(format_args!("program {} refused for pane {pane_id}: {message}", connection.peer_pid()));
        let _ = outbox.send(Frame::Control(Control::Error { request: None, message }).encode());
        return Ok(());
    };
    // The UI that shows the pane, if it is the one a picker was opened for.
    let shown_by = |ui: u64| {
        pane.state.lock().unwrap().shown.as_ref().is_some_and(|(shown, _)| *shown == ui)
    };
    let mut mine = None;
    loop {
        let bytes = match Frame::read(reader) {
            Ok(Frame::Shell { bytes, .. }) => bytes,
            Ok(_) => continue,
            Err(_) => break,
        };
        let had = mine;
        let mut ui = None;
        let showing = || {
            let (shown, ui_outbox) =
                pane.state.lock().unwrap().shown.clone().ok_or("no window is showing this pane")?;
            ui = Some(shown);
            Ok(Showing { pane: pane_id, ui: shown, ui_outbox, program: outbox.clone() })
        };
        let at_cursor = || pane.state.lock().unwrap().terminal.at_cursor();
        let goes_on = session.pickers.said(&bytes, &mut mine, showing, outbox, &at_cursor);
        // A UI that left the pane while the picker was being opened did not
        // find it to end.
        if had.is_none()
            && mine.is_some()
            && let Some(ui) = ui
            && !shown_by(ui)
        {
            session.pickers.end_where(|of, by| of == Some(pane_id) && by == ui, true);
        }
        if !goes_on {
            break;
        }
    }
    if let Some(number) = mine {
        session.pickers.program_gone(number);
    }
    Ok(())
}

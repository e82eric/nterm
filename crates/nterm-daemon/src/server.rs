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
    vt::Terminal,
};
use nterm_session::{
    GHOSTTY_REVISION,
    pipe::{Connection, Listener},
    shell::{DAEMON_VARIABLE, PANE_VARIABLE, PIPE_VARIABLE},
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

    fn send_screen(&mut self, pane: PaneId, to: &Outbox) -> bool {
        let frame = Frame::Screen { pane, bytes: self.terminal.screen() }.encode();
        to.send(frame).is_ok()
    }
}

impl Session {
    fn pane(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().unwrap().get(&id).cloned()
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
        let ours = [DAEMON_VARIABLE, PANE_VARIABLE, PIPE_VARIABLE];
        env.retain(|(key, _)| !ours.contains(&key.as_str()));
        if let Ok(pipe) = nterm_session::pipe::name(&self.name) {
            env.push((PIPE_VARIABLE.into(), pipe));
        }
        env.push((DAEMON_VARIABLE.into(), self.name.clone()));
        env.push((PANE_VARIABLE.into(), id.to_string()));
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
                    if !input.write(&bytes) {
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
                        let count = output.read(&mut buffer);
                        if count == 0 {
                            break;
                        }
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
                let console = pane.state.lock().unwrap().console.take();
                drop(console);
                let _ = reader.join();
                let closing = {
                    let mut state = pane.state.lock().unwrap();
                    state.exited = Some(code);
                    state.closing
                };
                log(format_args!("pane {}: exited with {code}", pane.id));
                // With no UI connected there is nobody to close it, and it
                // would keep the daemon from ever being idle.
                if closing || session.clients.lock().unwrap().is_empty() {
                    session.panes.lock().unwrap().remove(&pane.id);
                }
                session.end_channels(|_, channel| channel.pane == pane.id, true);
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
    let mut listener = Listener::bind(&options.session)?;
    let (sessions_changed, changes) = mpsc::channel();
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
    let (outbox, queued) = mpsc::channel::<Vec<u8>>();
    let writer = {
        let connection = connection.clone();
        std::thread::spawn(move || {
            let Ok(mut writer) = connection.writer() else { return };
            for frame in queued {
                if frame == CLOSE {
                    // Asked for: what came before it has been written.
                    connection.close();
                    break;
                }
                if writer.write_all(&frame).is_err() {
                    // Wake the reading side; this UI is gone.
                    connection.close();
                    break;
                }
            }
        })
    };

    let result = converse(session, &connection, id, &outbox);
    log(format_args!(
        "client {id} (pid {}) gone: {}",
        connection.peer_pid(),
        result.err().map_or("closed".to_owned(), |error| error.to_string())
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
    // What it showed is waiting now.
    session.sessions_changed();
    // With every sender gone the writer sends what is queued and ends.
    drop(outbox);
    let _ = writer.join();
    connection.close();
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
    let Frame::Control(Control::Hello { protocol, ghostty }) = first else {
        fail(None, "expected hello".into());
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no hello"));
    };
    send(Control::Welcome {
        protocol: PROTOCOL_VERSION,
        ghostty: GHOSTTY_REVISION.to_owned(),
        pid: std::process::id(),
        session: session.name.clone(),
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
            | Frame::Snapshot { .. } => continue,
        };
        let missing = |request: Option<u32>, pane: PaneId| {
            fail(request, format!("no pane {pane}"));
        };
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
            Control::Spawn { request, command, cwd, env, size } => {
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
                    state.send_screen(pane_id, outbox).then(|| (id, outbox.clone()));
                if let Some(code) = state.exited {
                    send(Control::Exited { pane: pane_id, code });
                }
                session.sessions_changed();
            }
            Control::Detach { pane: pane_id } => {
                if let Some(pane) = session.pane(pane_id) {
                    pane.state.lock().unwrap().shown.take_if(|(client, _)| *client == id);
                }
                // After it has left the pane, so no program finds it there.
                session.end_channels(|_, c| c.pane == pane_id && c.ui == id, false);
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

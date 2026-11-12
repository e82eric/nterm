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
    protocol::{Control, Frame, PROTOCOL_VERSION, PaneId, PaneInfo, Size, TerminalSettings},
};
use std::{
    collections::HashMap,
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

/// Encoded frames on their way to one UI.
type Outbox = Sender<Vec<u8>>;

struct Session {
    name: String,
    panes: Mutex<HashMap<PaneId, Arc<Pane>>>,
    next_pane: AtomicU32,
    next_client: AtomicU64,
    /// What the UI last sent; new panes start from it.
    settings: Mutex<TerminalSettings>,
    blobs: Mutex<HashMap<String, String>>,
    clients: Mutex<HashMap<u64, Outbox>>,
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
    /// The UIs that get this pane's output.
    subscribers: Vec<(u64, Outbox)>,
    exited: Option<u32>,
    /// Forget the pane as soon as its shell has ended.
    closing: bool,
}

impl Pane {
    fn info(&self) -> PaneInfo {
        let state = self.state.lock().unwrap();
        PaneInfo {
            pane: self.id,
            pid: self.process.pid,
            creation_time: self.process.creation_time,
            command: self.command.clone(),
            size: state.size,
            exited: state.exited,
            attached: state.subscribers.len() as u32,
        }
    }
}

impl State {
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

    fn spawn(
        self: &Arc<Self>,
        command: String,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        size: Size,
    ) -> Result<Arc<Pane>, String> {
        if size.cols == 0 || size.rows == 0 {
            return Err("a pane needs a size".into());
        }
        // The settings are in place before the shell can write a byte.
        let settings = self.settings.lock().unwrap().clone();
        let terminal = Terminal::new(size, &settings)?;
        let spawned = conpty::spawn(&command, cwd.as_deref(), &env, size.cols, size.rows)
            .map_err(|error| error.to_string())?;
        let conpty::Spawned { console, output, input, process } = spawned;

        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        let id = self.next_pane.fetch_add(1, Ordering::Relaxed);
        let pane = Arc::new(Pane {
            id,
            command,
            process,
            input: input_tx,
            state: Mutex::new(State {
                terminal,
                console: Some(console),
                size,
                subscribers: Vec::new(),
                exited: None,
                closing: false,
            }),
        });
        self.panes.lock().unwrap().insert(id, pane.clone());
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
                        if !state.subscribers.is_empty() {
                            let frame = Frame::encode_output(pane.id, bytes);
                            state.subscribers.retain(|(_, outbox)| outbox.send(frame.clone()).is_ok());
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
                if closing {
                    session.panes.lock().unwrap().remove(&pane.id);
                }
                session.broadcast(Control::Exited { pane: pane.id, code });
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
    let session = Arc::new(Session {
        name: options.session,
        panes: Mutex::default(),
        next_pane: AtomicU32::new(1),
        next_client: AtomicU64::new(1),
        settings: Mutex::default(),
        blobs: Mutex::default(),
        clients: Mutex::default(),
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

    session.clients.lock().unwrap().remove(&id);
    let panes: Vec<_> = session.panes.lock().unwrap().values().cloned().collect();
    for pane in panes {
        pane.state.lock().unwrap().subscribers.retain(|(client, _)| *client != id);
    }
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
            Frame::Output { .. } | Frame::Screen { .. } | Frame::Snapshot { .. } => continue,
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
            Control::Attach { pane: pane_id, size } => {
                let Some(pane) = session.pane(pane_id) else {
                    missing(None, pane_id);
                    continue;
                };
                // Resize, screen and subscription under one lock, so the
                // output that follows starts exactly where the screen ends.
                let mut state = pane.state.lock().unwrap();
                state.resize(&pane, size);
                state.subscribers.retain(|(client, _)| *client != id);
                if state.send_screen(pane_id, outbox) {
                    state.subscribers.push((id, outbox.clone()));
                }
                if let Some(code) = state.exited {
                    send(Control::Exited { pane: pane_id, code });
                }
            }
            Control::Detach { pane } => {
                if let Some(pane) = session.pane(pane) {
                    pane.state.lock().unwrap().subscribers.retain(|(client, _)| *client != id);
                }
            }
            Control::Resize { pane: pane_id, size } => {
                let Some(pane) = session.pane(pane_id) else {
                    missing(None, pane_id);
                    continue;
                };
                let mut state = pane.state.lock().unwrap();
                state.resize(&pane, size);
                // A UI reflows its own copy, and not always from the size
                // this one came from; the daemon's copy replaces it.
                let frame =
                    Frame::Screen { pane: pane_id, bytes: state.terminal.screen() }.encode();
                state.subscribers.retain(|(_, outbox)| outbox.send(frame.clone()).is_ok());
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
            Control::BlobSet { key, value } => {
                session.blobs.lock().unwrap().insert(key, value);
            }
            Control::BlobGet { request, key } => {
                let value = session.blobs.lock().unwrap().get(&key).cloned();
                send(Control::Blob { request, key, value });
            }
            Control::Shutdown => session.shutdown(),
            // A second hello, the daemon's own messages, or something newer.
            _ => {}
        }
    }
}

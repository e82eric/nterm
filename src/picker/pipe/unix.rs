//! Unix domain socket transport. Each pane listens on its own socket in a
//! private per-process directory, and only processes running as the terminal's
//! user may connect. The framing, handshake and request lifecycle match the
//! Windows transport.
//
// TODO(picker-pipe): the framing and request loop below mirror windows.rs.
// Share them once both can be built and tested together.
use super::*;
use std::{
    collections::HashSet,
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

// The tests drive this transport with npick's real client.
#[cfg(test)]
use super::protocol;
#[cfg(test)]
#[path = "../../npick/client.rs"]
mod client;

/// How often blocked reads wake to check for replies, deadlines and shutdown.
const POLL: Duration = Duration::from_millis(50);

struct Shared {
    stop: AtomicBool,
    connected: AtomicBool,
    idle: AtomicBool,
    policy: Mutex<Policy>,
    active: Mutex<Option<(Arc<Session>, Capability)>>,
    context: egui::Context,
    native_wake: Mutex<
        Option<(
            egui_winit::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
            egui_winit::winit::window::WindowId,
        )>,
    >,
}
impl Shared {
    fn wake_ui(&self) {
        if let Some((proxy, window_id)) = self.native_wake.lock().unwrap().as_ref() {
            let _ = proxy.send_event(crate::app::UserEvent::PickerWake {
                window_id: *window_id,
            });
        }
        self.context.request_repaint();
    }
}
enum Capability {
    Named(String),
    Stream,
}
impl Capability {
    fn permitted(&self, policy: &Policy) -> bool {
        policy.enabled
            && match self {
                Self::Named(name) => policy.named.contains(name),
                Self::Stream => policy.allow_stream,
            }
    }
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

/// The user id of the process on the other end, as reported by the kernel.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (result == 0 && len as usize == size_of::<libc::ucred>()).then_some(credentials.uid)
}
#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let (mut uid, mut gid) = (0, 0);
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (result == 0).then_some(uid)
}

/// Create `path` as a directory only the current user can use, or verify that
/// an existing one is. Refuses symlinks and directories owned by anyone else.
fn private_dir(path: &Path) -> Result<(), String> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("unable to create {}: {error}", path.display())),
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect {}: {error}", path.display()))?;
    if !metadata.is_dir() || metadata.uid() != effective_uid() || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "{} must be a directory private to the current user",
            path.display()
        ));
    }
    Ok(())
}

/// Remove socket directories left behind by terminals that no longer run.
fn sweep_stale(base: &Path) {
    let own = std::process::id();
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        if pid as u32 == own || pid <= 0 {
            continue;
        }
        let gone = unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `$XDG_RUNTIME_DIR/ghostty-egui/<pid>`, falling back to a per-user directory
/// under the temporary directory when no runtime directory is set.
fn socket_dir() -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("ghostty-egui"),
        _ => std::env::temp_dir().join(format!("ghostty-egui-{}", effective_uid())),
    };
    private_dir(&base)?;
    static SWEEP: std::sync::Once = std::sync::Once::new();
    SWEEP.call_once(|| sweep_stale(&base));
    let dir = base.join(std::process::id().to_string());
    private_dir(&dir)?;
    Ok(dir)
}

pub(crate) struct Service {
    /// The socket path, exported to the shell as GHOSTTY_PICKER_ENDPOINT.
    pub endpoint: String,
    pub token: String,
    pub events: Receiver<Event>,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}
impl Service {
    pub fn new(context: egui::Context) -> Result<Self, String> {
        let token = super::super::wire::new_token().map_err(|e| e.to_string())?;
        let name = super::super::wire::new_token().map_err(|e| e.to_string())?;
        let path = socket_dir()?.join(format!("pane-{}.sock", &name[..16]));
        let endpoint = path
            .to_str()
            .ok_or("picker socket path is not valid UTF-8")?
            .to_owned();
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("unable to listen on {endpoint}: {error}"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("unable to secure {endpoint}: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            policy: Mutex::new(Policy::default()),
            active: Mutex::new(None),
            context,
            native_wake: Mutex::new(None),
        });
        let (tx, events) = crossbeam_channel::bounded(2);
        let state = shared.clone();
        let name = endpoint.clone();
        let secret = token.clone();
        let thread = std::thread::Builder::new()
            .name("picker socket".into())
            .spawn(move || {
                let uid = effective_uid();
                while !state.stop.load(Ordering::Acquire) {
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(_) => {
                            std::thread::sleep(POLL);
                            continue;
                        }
                    };
                    // One connection at a time, like the Windows pipe's single
                    // instance. Others wait in the backlog.
                    if peer_uid(&stream) != Some(uid)
                        || stream.set_nonblocking(false).is_err()
                        || stream.set_read_timeout(Some(POLL)).is_err()
                        || stream
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .is_err()
                    {
                        continue;
                    }
                    // Failures intentionally do not echo credentials or request data.
                    let _ = serve(&stream, &state, &name, &secret, &tx);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            endpoint,
            token,
            events,
            shared,
            thread: Some(thread),
            path,
        })
    }
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }
    pub fn bind_window(
        &self,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
        window_id: egui_winit::winit::window::WindowId,
    ) {
        *self.shared.native_wake.lock().unwrap() = Some((proxy, window_id));
    }
    pub fn enabled(&self) -> bool {
        self.shared.policy.lock().unwrap().enabled
    }
    /// Apply the current Lua policy. The shell arguments exist for the
    /// Windows ancestry check; the socket's user check replaces it here.
    pub fn update(&self, policy: &Policy, _shell_pid: u32, _expected_creation: u64) {
        *self.shared.policy.lock().unwrap() = policy.clone();
        if let Some((session, capability)) = self.shared.active.lock().unwrap().as_ref() {
            if !capability.permitted(policy) {
                session.cancelled.store(true, Ordering::Release);
                session.finish("cancelled", json!({"reason":"permission_revoked"}));
                self.shared.context.request_repaint();
            }
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
        // Succeeds only after the last pane of this process closes.
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

enum Failure {
    Error,
}
type IoResult<T> = Result<T, Failure>;
impl From<String> for Failure {
    fn from(error: String) -> Self {
        let _ = error;
        Self::Error
    }
}
struct ReplyPump<'a> {
    rx: &'a Receiver<Value>,
    terminal: std::cell::Cell<bool>,
    deadline: std::cell::Cell<Option<Instant>>,
}
fn pump_reply(stream: &UnixStream, state: &Shared, pump: Option<&ReplyPump<'_>>) -> IoResult<()> {
    if let Some(pump) = pump {
        if let Ok(reply) = pump.rx.try_recv() {
            send(stream, &reply, state)?;
            if reply["type"] == "result" {
                pump.terminal.set(true);
                pump.deadline
                    .set(Some(Instant::now() + Duration::from_secs(5)));
            }
        }
    }
    Ok(())
}
/// Read exactly `bytes`, delivering UI replies while waiting. Gives up on
/// shutdown, on disabling while idle, or when a deadline passes.
fn read_exact(
    mut stream: &UnixStream,
    bytes: &mut [u8],
    state: &Shared,
    deadline: Option<Instant>,
    replies: Option<&ReplyPump<'_>>,
) -> IoResult<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        pump_reply(stream, state, replies)?;
        if state.stop.load(Ordering::Acquire)
            || (state.idle.load(Ordering::Acquire) && !state.policy.lock().unwrap().enabled)
            || deadline.is_some_and(|d| Instant::now() >= d)
            || replies.is_some_and(|pump| pump.deadline.get().is_some_and(|d| Instant::now() >= d))
        {
            return Err(Failure::Error);
        }
        match stream.read(&mut bytes[offset..]) {
            Ok(0) => return Err(Failure::Error),
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(Failure::Error),
        }
    }
    Ok(())
}
fn send(mut stream: &UnixStream, value: &Value, state: &Shared) -> IoResult<()> {
    let _ = state;
    let mut bytes = serde_json::to_vec(value).map_err(|_| Failure::Error)?;
    if bytes.len() > MAX_FRAME {
        return Err(Failure::Error);
    }
    let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
    frame.append(&mut bytes);
    // The socket's write timeout bounds a blocked client.
    stream.write_all(&frame).map_err(|_| Failure::Error)
}
fn read(
    stream: &UnixStream,
    state: &Shared,
    deadline: Option<Instant>,
    replies: Option<&ReplyPump<'_>>,
) -> IoResult<Message> {
    let mut header = [0; 4];
    read_exact(stream, &mut header[..1], state, deadline, replies)?;
    let partial_deadline =
        Some(deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(5)));
    read_exact(stream, &mut header[1..], state, partial_deadline, replies)?;
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(Failure::Error);
    }
    let mut bytes = vec![0; len];
    read_exact(
        stream,
        &mut bytes,
        state,
        Some(Instant::now() + Duration::from_secs(5)),
        replies,
    )?;
    parse(&bytes).map_err(|error| {
        let _ = error;
        Failure::Error
    })
}
fn send_reply(stream: &UnixStream, reply: &Value, state: &Shared) -> IoResult<bool> {
    send(stream, reply, state)?;
    let terminal = reply["type"] == "result" || reply["type"] == "error";
    if terminal {
        // Wait for the client's receipt so the result is not lost when the
        // connection is reused or closed.
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        for _ in 0..4 {
            match read(stream, state, deadline, None)? {
                Message::Received { request_id } if reply["request_id"] == request_id => {
                    return Ok(true);
                }
                Message::Items { request_id, .. }
                | Message::EndItems { request_id, .. }
                | Message::Cancel { request_id }
                    if reply["request_id"] == request_id =>
                {
                    continue;
                }
                _ => return Err(Failure::Error),
            }
        }
        return Err(Failure::Error);
    }
    Ok(terminal)
}
fn wait_reply(stream: &UnixStream, state: &Shared, rx: &Receiver<Value>) -> IoResult<bool> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if state.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(Failure::Error);
        }
        if let Ok(reply) = rx.recv_timeout(POLL) {
            return send_reply(stream, &reply, state);
        }
    }
}
fn serve(
    stream: &UnixStream,
    state: &Shared,
    endpoint: &str,
    token: &str,
    events: &Sender<Event>,
) -> IoResult<()> {
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    let nonce = super::super::wire::new_token().map_err(|e| e.to_string())?;
    send(
        stream,
        &json!({"type":"challenge","version":1,"nonce":nonce}),
        state,
    )?;
    let Message::Authenticate {
        version: 1,
        client_nonce,
        proof: provided,
    } = read(stream, state, deadline, None)?
    else {
        return Err(Failure::Error);
    };
    if client_nonce.len() != 64
        || !client_nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || !verify_proof(token, endpoint, &nonce, &client_nonce, &provided)
    {
        return Err(Failure::Error);
    }
    let policy = state.policy.lock().unwrap().clone();
    if !policy.enabled {
        send_reply(
            stream,
            &json!({"type":"error","request_id":"authentication","code":"disabled","message":"Enable picker_pipe.enabled in the active Lua configuration."}),
            state,
        )?;
        return Ok(());
    }
    send(
        stream,
        &json!({"type":"ready","version":1,"persistent":true,"proof":hex(&proof(token,endpoint,&nonce,&client_nonce,"server")),"capabilities":{"named":policy.named,"stream":policy.allow_stream},"limits":{"max_frame_bytes":MAX_FRAME,"max_item_bytes":MAX_ITEM,"max_batch_items":1024,"max_items":MAX_ITEMS,"max_storage_bytes":MAX_STORAGE,"in_flight_batches":1}}),
        state,
    )?;
    struct Connected<'a>(&'a Shared);
    impl Drop for Connected<'_> {
        fn drop(&mut self) {
            self.0.idle.store(false, Ordering::Release);
            self.0.connected.store(false, Ordering::Release);
            self.0.context.request_repaint();
        }
    }
    state.connected.store(true, Ordering::Release);
    state.context.request_repaint();
    let _connected = Connected(state);
    loop {
        state.idle.store(true, Ordering::Release);
        let request = read(stream, state, None, None)?;
        state.idle.store(false, Ordering::Release);
        let request_id = match &request {
            Message::Open { request_id, .. } => request_id.clone(),
            _ => return Err(Failure::Error),
        };
        serve_request(stream, state, events, request)?;
        // The request guard is dropped before confirming that the next open
        // may safely reuse this connection. Result receipt drains in-flight input.
        send(
            stream,
            &json!({"type":"idle","request_id":request_id}),
            state,
        )?;
        match read(
            stream,
            state,
            Some(Instant::now() + Duration::from_secs(5)),
            None,
        )? {
            Message::Received {
                request_id: received,
            } if received == request_id => {}
            _ => return Err(Failure::Error),
        }
    }
}

fn serve_request(
    stream: &UnixStream,
    state: &Shared,
    events: &Sender<Event>,
    request: Message,
) -> IoResult<()> {
    let Message::Open {
        request_id,
        request,
    } = request
    else {
        return Err(Failure::Error);
    };
    if request_id.is_empty() || request_id.len() > 128 {
        return Err(Failure::Error);
    }
    let permission = state.policy.lock().unwrap().authorize(&request);
    if let Err(message) = permission {
        send_reply(
            stream,
            &json!({"type":"result","request_id":request_id,"status":"error","code":"capability_denied","message":message}),
            state,
        )?;
        return Ok(());
    }
    let stream_items = matches!(request, Open::Stream { .. });
    let capability = match &request {
        Open::Named { name, .. } => Capability::Named(name.clone()),
        Open::Stream { .. } => Capability::Stream,
    };
    let (session, rx) = Session::new(request_id);
    *state.active.lock().unwrap() = Some((session.clone(), capability));
    struct Disconnect<'a> {
        session: Arc<Session>,
        context: &'a egui::Context,
        active: &'a Mutex<Option<(Arc<Session>, Capability)>>,
    }
    impl Drop for Disconnect<'_> {
        fn drop(&mut self) {
            self.session.cancelled.store(true, Ordering::Release);
            self.active.lock().unwrap().take();
            self.context.request_repaint();
        }
    }
    let _disconnect = Disconnect {
        session: session.clone(),
        context: &state.context,
        active: &state.active,
    };
    events
        .try_send(Event::Open {
            request,
            session: session.clone(),
        })
        .map_err(|_| Failure::Error)?;
    state.wake_ui();
    if wait_reply(stream, state, &rx)? {
        return Ok(());
    }
    let mut sequence = 0;
    let mut ended = false;
    let mut total = 0;
    let mut count = 0;
    let mut ids = HashSet::new();
    let pump = ReplyPump {
        rx: &rx,
        terminal: std::cell::Cell::new(false),
        deadline: std::cell::Cell::new(None),
    };
    let mut drained = 0;
    loop {
        let message = read(stream, state, pump.deadline.get(), Some(&pump))?;
        pump_reply(stream, state, Some(&pump))?;
        if pump.terminal.get() {
            match message {
                Message::Received { request_id } if request_id == session.id => return Ok(()),
                Message::Items { request_id, .. }
                | Message::EndItems { request_id, .. }
                | Message::Cancel { request_id }
                    if request_id == session.id && drained < 4 =>
                {
                    drained += 1;
                    continue;
                }
                _ => return Err(Failure::Error),
            }
        }
        if session.done() {
            if wait_reply(stream, state, &rx)? {
                return Ok(());
            }
            continue;
        }
        // A config reload can revoke all access or individual capabilities.
        if !state.policy.lock().unwrap().enabled {
            return Err(Failure::Error);
        }
        let event = match message {
            Message::Cancel { request_id } if request_id == session.id => {
                session.cancelled.store(true, Ordering::Release);
                session.finish("cancelled", json!({"reason":"client"}));
                state.context.request_repaint();
                if wait_reply(stream, state, &rx)? {
                    return Ok(());
                }
                continue;
            }
            Message::Items {
                request_id,
                sequence: received,
                items,
            } if stream_items && !ended && request_id == session.id && received == sequence => {
                if items.is_empty() || items.len() > 1024 {
                    return Err(Failure::Error);
                }
                for item in &items {
                    let bytes = item.text.len()
                        + serde_json::to_vec(&item.value)
                            .map_err(|_| Failure::Error)?
                            .len()
                        + item.id.len();
                    if item.id.is_empty()
                        || item.id.len() > 128
                        || bytes > MAX_ITEM
                        || !ids.insert(item.id.clone())
                    {
                        return Err(Failure::Error);
                    }
                    total += bytes;
                    count += 1;
                    if total > MAX_STORAGE || count > MAX_ITEMS {
                        return Err(Failure::Error);
                    }
                }
                sequence += 1;
                Event::Items {
                    sequence: received,
                    items,
                    session: session.clone(),
                }
            }
            Message::EndItems {
                request_id,
                last_sequence,
            } if stream_items
                && !ended
                && request_id == session.id
                && last_sequence == sequence.checked_sub(1) =>
            {
                ended = true;
                Event::End {
                    session: session.clone(),
                }
            }
            _ => return Err(Failure::Error),
        };
        if session.done() {
            if wait_reply(stream, state, &rx)? {
                return Ok(());
            }
            continue;
        }
        events.try_send(event).map_err(|_| Failure::Error)?;
        state.wake_ui();
        if wait_reply(stream, state, &rx)? {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::client::{self, Item, Outcome};
    use super::*;

    fn service(policy: Policy) -> Service {
        let service = Service::new(egui::Context::default()).unwrap();
        service.update(&policy, 0, 0);
        service
    }

    fn enabled() -> Policy {
        Policy {
            enabled: true,
            allow_stream: true,
            named: vec!["files".into()],
        }
    }

    fn next_event(service: &Service) -> Event {
        service
            .events
            .recv_timeout(Duration::from_secs(5))
            .expect("expected a picker event")
    }

    /// Authenticate by hand, for tests that need to misbehave afterwards.
    fn raw_connect(service: &Service) -> UnixStream {
        let mut stream = UnixStream::connect(&service.endpoint).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let read = |stream: &mut UnixStream| -> Value {
            let mut header = [0; 4];
            stream.read_exact(&mut header).unwrap();
            let mut body = vec![0; u32::from_le_bytes(header) as usize];
            stream.read_exact(&mut body).unwrap();
            serde_json::from_slice(&body).unwrap()
        };
        let write = |stream: &mut UnixStream, value: Value| {
            let body = serde_json::to_vec(&value).unwrap();
            stream
                .write_all(&(body.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(&body).unwrap();
        };
        let challenge = read(&mut stream);
        let server = challenge["nonce"].as_str().unwrap().to_owned();
        let nonce = "ab".repeat(32);
        let token_proof = hex(&proof(
            &service.token,
            &service.endpoint,
            &server,
            &nonce,
            "client",
        ));
        write(
            &mut stream,
            json!({"type":"authenticate","version":1,"client_nonce":nonce,"proof":token_proof}),
        );
        assert_eq!(read(&mut stream)["type"], "ready");
        write(
            &mut stream,
            json!({"type":"open","request_id":"r1","request":{"kind":"named","name":"files","params":{}}}),
        );
        stream
    }

    #[test]
    fn socket_is_private_to_the_user_and_removed_with_the_pane() {
        let service = service(Policy::default());
        let path = PathBuf::from(&service.endpoint);
        let socket = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(socket.mode() & 0o777, 0o600);
        let dir = std::fs::symlink_metadata(path.parent().unwrap()).unwrap();
        assert_eq!(dir.mode() & 0o777, 0o700);
        assert_eq!(dir.uid(), effective_uid());
        drop(service);
        assert!(!path.exists());
    }

    #[test]
    fn peer_uid_is_reported_by_the_kernel() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a), Some(effective_uid()));
    }

    #[test]
    fn named_request_returns_the_accepted_value() {
        let service = service(enabled());
        let (endpoint, token) = (service.endpoint.clone(), service.token.clone());
        let request = json!({"kind":"named","name":"files","params":{"cwd":"/tmp"}});
        let client = std::thread::spawn(move || client::run(&endpoint, &token, request, None));
        let Event::Open { request, session } = next_event(&service) else {
            panic!("expected open");
        };
        let Open::Named { name, params } = request else {
            panic!("expected a named request");
        };
        assert_eq!((name.as_str(), params["cwd"].as_str()), ("files", "/tmp"));
        assert!(service.connected());
        session.reply(json!({"type":"opened","request_id":session.id}));
        session.finish(
            "accepted",
            json!({"selected":[{"id":"0","text":"a","value":"/tmp/a"}],"query":""}),
        );
        assert_eq!(
            client.join().unwrap(),
            Ok(Outcome::Accepted(json!("/tmp/a")))
        );
    }

    #[test]
    fn streamed_items_reach_the_picker_and_cancel_returns_cancelled() {
        let service = service(enabled());
        let (endpoint, token) = (service.endpoint.clone(), service.token.clone());
        let (tx, rx) = crossbeam_channel::unbounded();
        for text in ["one", "two", "three"] {
            tx.send(Item {
                text: text.into(),
                value: format!("/{text}"),
            })
            .unwrap();
        }
        drop(tx);
        let request = json!({"kind":"stream","options":{"initial_query":"t"}});
        let client = std::thread::spawn(move || client::run(&endpoint, &token, request, Some(rx)));
        let Event::Open { request, session } = next_event(&service) else {
            panic!("expected open");
        };
        let Open::Stream { options } = request else {
            panic!("expected a stream request");
        };
        assert_eq!(options.initial_query, "t");
        session.reply(json!({"type":"opened","request_id":session.id}));
        let mut received = Vec::new();
        loop {
            match next_event(&service) {
                Event::Items {
                    sequence, items, ..
                } => {
                    received.extend(items.into_iter().map(|item| (item.text, item.value)));
                    session
                        .reply(json!({"type":"ack","request_id":session.id,"sequence":sequence}));
                }
                Event::End { .. } => {
                    session.reply(json!({"type":"input_complete","request_id":session.id}));
                    break;
                }
                Event::Open { .. } => panic!("unexpected second open"),
            }
        }
        assert_eq!(
            received,
            [("one", "/one"), ("two", "/two"), ("three", "/three")]
                .map(|(t, v)| (t.to_owned(), json!(v)))
        );
        session.finish("cancelled", json!({"reason":"user"}));
        assert_eq!(client.join().unwrap(), Ok(Outcome::Cancelled));
    }

    #[test]
    fn items_too_large_once_json_encoded_are_skipped_by_the_client() {
        // Each `"` is 1 byte raw but 2 encoded (`\"`), so these values pass a
        // raw-length check while the server measures them at about twice that.
        let quotes = |n: usize| "\"".repeat(n);
        let too_large = quotes((MAX_ITEM - 2) / 2);
        let largest = quotes((MAX_ITEM - 4) / 2);
        // The server's measure: id + text + JSON-encoded value. Ids are "0".."3".
        let measured = |id: &str, text: &str, value: &str| {
            id.len() + text.len() + serde_json::to_vec(&json!(value)).unwrap().len()
        };
        assert_eq!(measured("1", "b", &too_large), MAX_ITEM + 2);
        assert_eq!(measured("2", "c", &largest), MAX_ITEM);
        assert!(too_large.len() + 2 < MAX_ITEM);
        let service = service(enabled());
        let (endpoint, token) = (service.endpoint.clone(), service.token.clone());
        let (tx, rx) = crossbeam_channel::unbounded();
        for (text, value) in [("a", "a"), ("b", &too_large), ("c", &largest), ("d", "d")] {
            tx.send(Item {
                text: text.into(),
                value: value.to_owned(),
            })
            .unwrap();
        }
        drop(tx);
        let request = json!({"kind":"stream","options":{}});
        let client = std::thread::spawn(move || client::run(&endpoint, &token, request, Some(rx)));
        let Event::Open { session, .. } = next_event(&service) else {
            panic!("expected open");
        };
        session.reply(json!({"type":"opened","request_id":session.id}));
        let mut received = Vec::new();
        loop {
            match next_event(&service) {
                Event::Items {
                    sequence, items, ..
                } => {
                    received.extend(items.into_iter().map(|item| item.text));
                    session
                        .reply(json!({"type":"ack","request_id":session.id,"sequence":sequence}));
                }
                Event::End { .. } => {
                    session.reply(json!({"type":"input_complete","request_id":session.id}));
                    break;
                }
                Event::Open { .. } => panic!("unexpected second open"),
            }
        }
        // The item at exactly the limit is sent; the one a byte over is not,
        // and skipping it keeps the connection and later items.
        assert_eq!(received, ["a", "c", "d"]);
        session.finish("cancelled", json!({"reason":"user"}));
        assert_eq!(client.join().unwrap(), Ok(Outcome::Cancelled));
    }

    #[test]
    fn wrong_token_never_reaches_the_picker() {
        let service = service(enabled());
        let result = client::run(
            &service.endpoint,
            &"0".repeat(64),
            json!({"kind":"named","name":"files","params":{}}),
            None,
        );
        assert!(result.is_err());
        assert!(service.events.try_recv().is_err());
    }

    #[test]
    fn disabled_pipe_explains_how_to_enable_it() {
        let service = service(Policy::default());
        let error = client::run(
            &service.endpoint,
            &service.token,
            json!({"kind":"named","name":"files","params":{}}),
            None,
        )
        .unwrap_err();
        assert!(error.contains("picker_pipe.enabled"), "{error}");
    }

    #[test]
    fn pickers_outside_the_allowlist_are_denied() {
        let service = service(enabled());
        let error = client::run(
            &service.endpoint,
            &service.token,
            json!({"kind":"named","name":"secret","params":{}}),
            None,
        )
        .unwrap_err();
        assert!(error.contains("capability_denied"), "{error}");
        assert!(service.events.try_recv().is_err());
    }

    #[test]
    fn disconnecting_cancels_the_open_picker() {
        let service = service(enabled());
        let stream = raw_connect(&service);
        let Event::Open { session, .. } = next_event(&service) else {
            panic!("expected open");
        };
        session.reply(json!({"type":"opened","request_id":session.id}));
        drop(stream);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.cancelled.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "session was not cancelled");
            std::thread::sleep(POLL);
        }
    }

    #[test]
    fn revoking_the_capability_cancels_the_request() {
        let service = service(enabled());
        let (endpoint, token) = (service.endpoint.clone(), service.token.clone());
        let request = json!({"kind":"named","name":"files","params":{}});
        let client = std::thread::spawn(move || client::run(&endpoint, &token, request, None));
        let Event::Open { session, .. } = next_event(&service) else {
            panic!("expected open");
        };
        session.reply(json!({"type":"opened","request_id":session.id}));
        service.update(
            &Policy {
                named: Vec::new(),
                ..enabled()
            },
            0,
            0,
        );
        assert_eq!(client.join().unwrap(), Ok(Outcome::Cancelled));
        assert!(session.cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn stale_directories_of_exited_terminals_are_removed() {
        let base = std::env::temp_dir().join(format!("npick-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["2147483646", "not-a-pid", &std::process::id().to_string()] {
            std::fs::create_dir_all(base.join(name)).unwrap();
        }
        sweep_stale(&base);
        assert!(!base.join("2147483646").exists());
        assert!(base.join("not-a-pid").exists());
        assert!(base.join(std::process::id().to_string()).exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn shared_directories_are_refused() {
        let dir = std::env::temp_dir().join(format!("npick-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&dir).is_err());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(private_dir(&dir).is_ok());
        std::fs::remove_dir(&dir).unwrap();
    }
}

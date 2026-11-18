//! The named pipe between a UI and the daemon.
//!
//! The pipe is named after the account and only that account may open it,
//! from any of its sign-ins: at the desktop, over SSH, from a scheduled
//! task. Each end then checks that the other runs as the same account at the
//! same elevation (see `identity`): the daemon by impersonating the client,
//! the client by looking at the process that serves the pipe. Handles are
//! never inherited.
//!
//! A connection is one duplex byte stream. One thread may read while another
//! writes; `close` wakes both.

use crate::identity::{self, Handle, Identity, owned, wide};
use crate::relay;
use std::{
    io::{self, Read, Write},
    mem::{size_of, zeroed},
    path::Path,
    ptr::{null, null_mut},
    sync::Arc,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::*,
    Security::*,
    Storage::FileSystem::*,
    System::{IO::*, Pipes::*, Threading::*},
};

/// What a client asks for: read, write, and nothing that would let it serve
/// the pipe itself.
const CLIENT_ACCESS: u32 = 0x0012_0083;

/// The pipe of a session, for this account and elevation.
pub fn name(session: &str) -> io::Result<String> {
    if session.is_empty()
        || !session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a session name is letters, digits, '-' and '_'",
        ));
    }
    let own = identity::own_identity()?;
    // An elevated UI gets its own daemon; the two could not talk anyway.
    let high = if own.integrity >= 0x3000 { "-hi" } else { "" };
    Ok(format!(r"\\.\pipe\nterm-session-{}-{session}{high}", own.user))
}

struct Pipe {
    pipe: Handle,
    /// Signalled to end pending and future reads and writes.
    closed: Handle,
    peer: u32,
}

/// One end of a connection: a pipe, or a relay to one (see `relay`).
#[derive(Clone)]
pub struct Connection(Link);

#[derive(Clone)]
enum Link {
    Pipe(Arc<Pipe>),
    Relay(Arc<relay::Child>),
}

impl Connection {
    fn new(pipe: Handle, peer: u32) -> io::Result<Self> {
        let closed = owned(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        Ok(Self(Link::Pipe(Arc::new(Pipe { pipe, closed, peer }))))
    }

    pub(crate) fn through(relay: Arc<relay::Child>) -> Self {
        Self(Link::Relay(relay))
    }

    /// The process at the other end: the daemon, or the relay to it.
    pub fn peer_pid(&self) -> u32 {
        match &self.0 {
            Link::Pipe(pipe) => pipe.peer,
            Link::Relay(relay) => relay.pid(),
        }
    }

    /// Whether the other end is reached through a relay, and so was not
    /// checked to be this user's daemon on this machine.
    pub fn relayed(&self) -> bool {
        matches!(self.0, Link::Relay(_))
    }

    /// A relay has one reader and one writer; a pipe as many as are asked for.
    pub fn reader(&self) -> io::Result<Reader> {
        Ok(Reader(match &self.0 {
            Link::Pipe(pipe) => ReadEnd::Pipe(Half::new(pipe)?),
            Link::Relay(relay) => ReadEnd::Relay(relay.reader()?),
        }))
    }

    pub fn writer(&self) -> io::Result<Writer> {
        Ok(Writer(match &self.0 {
            Link::Pipe(pipe) => WriteEnd::Pipe(Half::new(pipe)?),
            Link::Relay(relay) => WriteEnd::Relay(relay.writer()?),
        }))
    }

    /// End reads and writes on every clone, now and from now on.
    pub fn close(&self) {
        match &self.0 {
            Link::Pipe(pipe) => unsafe {
                SetEvent(pipe.closed.0);
            },
            Link::Relay(relay) => relay.close(),
        }
    }
}

struct Half {
    inner: Arc<Pipe>,
    /// Signalled when this half's operation completes.
    done: Handle,
}

impl Half {
    fn new(pipe: &Arc<Pipe>) -> io::Result<Self> {
        Ok(Self {
            inner: pipe.clone(),
            done: owned(unsafe { CreateEventW(null(), 1, 0, null()) })?,
        })
    }

    /// One read or write. A read returns 0 when the other end is gone.
    fn transfer(&mut self, buffer: *mut u8, len: usize, write: bool) -> io::Result<usize> {
        let pipe = self.inner.pipe.0;
        let closed = self.inner.closed.0;
        let aborted = || io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed");
        if unsafe { WaitForSingleObject(closed, 0) } == WAIT_OBJECT_0 {
            return Err(aborted());
        }
        let len = len.min(1 << 26) as u32;
        let mut operation: OVERLAPPED = unsafe { zeroed() };
        operation.hEvent = self.done.0;
        let mut count = 0u32;
        let started = unsafe {
            ResetEvent(self.done.0);
            if write {
                WriteFile(pipe, buffer, len, &mut count, &mut operation)
            } else {
                ReadFile(pipe, buffer, len, &mut count, &mut operation)
            }
        };
        let mut error = if started != 0 { 0 } else { unsafe { GetLastError() } };
        if error == ERROR_IO_PENDING {
            let handles = [self.done.0, closed];
            let woke = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
            if woke != WAIT_OBJECT_0 {
                unsafe {
                    CancelIoEx(pipe, &operation);
                }
            }
            // The operation owns `operation` and the buffer until it reports.
            error = if unsafe { GetOverlappedResult(pipe, &operation, &mut count, 1) } != 0 {
                0
            } else {
                unsafe { GetLastError() }
            };
        }
        match error {
            0 => Ok(count as usize),
            ERROR_OPERATION_ABORTED => Err(aborted()),
            ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA if !write => Ok(0),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
}

enum ReadEnd {
    Pipe(Half),
    Relay(relay::ChildReader),
}

pub struct Reader(ReadEnd);
impl Read for Reader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        match &mut self.0 {
            ReadEnd::Pipe(half) => half.transfer(buffer.as_mut_ptr(), buffer.len(), false),
            ReadEnd::Relay(relay) => relay.read(buffer),
        }
    }
}

enum WriteEnd {
    Pipe(Half),
    Relay(relay::ChildWriter),
}

pub struct Writer(WriteEnd);
impl Write for Writer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        match &mut self.0 {
            // The kernel only reads through the pointer on a write.
            WriteEnd::Pipe(half) => half.transfer(buffer.as_ptr().cast_mut(), buffer.len(), true),
            WriteEnd::Relay(relay) => relay.write(buffer),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match &mut self.0 {
            WriteEnd::Pipe(_) => Ok(()),
            WriteEnd::Relay(relay) => relay.flush(),
        }
    }
}

/// The daemon's end: accepts UIs.
pub struct Listener {
    name: Vec<u16>,
    identity: Identity,
    /// The instance the next client connects to. There is always one, so the
    /// name never becomes free while the daemon runs.
    next: Handle,
}

impl Listener {
    /// Claim the session's pipe. `AlreadyExists` means a daemon already
    /// serves it.
    pub fn bind(session: &str) -> io::Result<Self> {
        let name = wide(&name(session)?);
        let identity = identity::own_identity()?;
        let next = instance(&name, &identity, true).map_err(|error| {
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                io::Error::new(io::ErrorKind::AlreadyExists, "the session already has a daemon")
            } else {
                error
            }
        })?;
        Ok(Self { name, identity, next })
    }

    /// Wait for a UI. `PermissionDenied` is a client that was turned away;
    /// the listener is still good.
    pub fn accept(&mut self) -> io::Result<Connection> {
        let event = owned(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        let mut operation: OVERLAPPED = unsafe { zeroed() };
        operation.hEvent = event.0;
        if unsafe { ConnectNamedPipe(self.next.0, &mut operation) } == 0 {
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => {}
                ERROR_IO_PENDING => {
                    let mut count = 0;
                    if unsafe { GetOverlappedResult(self.next.0, &operation, &mut count, 1) } == 0 {
                        // A client that came and went; this instance is spent.
                        let error = io::Error::last_os_error();
                        self.next = instance(&self.name, &self.identity, false)?;
                        return Err(io::Error::new(io::ErrorKind::PermissionDenied, error));
                    }
                }
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        // Not a refused client, whatever the system calls the failure.
        let fresh = instance(&self.name, &self.identity, false)
            .map_err(|error| io::Error::other(format!("cannot create a pipe instance: {error}")))?;
        let pipe = std::mem::replace(&mut self.next, fresh);
        let peer = self.authorize(pipe.0)?;
        Connection::new(pipe, peer)
    }

    fn authorize(&self, pipe: HANDLE) -> io::Result<u32> {
        let denied = |what: &str| io::Error::new(io::ErrorKind::PermissionDenied, what.to_owned());
        if identity::own_identity()? != self.identity {
            return Err(denied("the daemon's security context changed"));
        }
        // What the client is acting as, then what its process is.
        let effective =
            identity::impersonated(|| unsafe { ImpersonateNamedPipeClient(pipe) } != 0)?;
        if effective != self.identity {
            return Err(denied("the client's security context differs from the daemon's"));
        }
        let mut pid = 0;
        if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
            return Err(denied("the client process is unknown"));
        }
        if identity::process_identity(pid)? != effective {
            return Err(denied("the client's process and acting identities differ"));
        }
        Ok(pid)
    }
}

fn instance(name: &[u16], identity: &Identity, first: bool) -> io::Result<Handle> {
    let account = &identity.user;
    // The account gets generic read and write, which includes creating
    // instances: the daemon is checked against this list for every instance
    // after the first and has no SID of its own to hold more. So a client
    // does not trust the pipe's name; it checks who serves it.
    let label = if identity.integrity >= 0x3000 { "HI" } else { "ME" };
    let sddl = wide(&format!("D:P(A;;0x0012019F;;;{account})S:(ML;;NWNR;;;{label})"));
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let claim = if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let pipe = owned(unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | claim,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            1 << 20,
            1 << 20,
            0,
            &attributes,
        )
    });
    unsafe {
        LocalFree(descriptor);
    }
    pipe
}

/// Connect to the session's daemon. `NotFound` means none is running.
pub fn connect(session: &str) -> io::Result<Connection> {
    let name = wide(&name(session)?);
    let deadline = Instant::now() + Duration::from_secs(5);
    let pipe = loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                CLIENT_ACCESS,
                0,
                null(),
                OPEN_EXISTING,
                // The daemon may learn who we are and nothing more.
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break Handle(handle);
        }
        let error = io::Error::last_os_error();
        // Every instance is taken for the moment a client is being admitted.
        if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) || Instant::now() >= deadline {
            return Err(error);
        }
        unsafe {
            WaitNamedPipeW(name.as_ptr(), 250);
        }
    };
    let denied = |what: &str| io::Error::new(io::ErrorKind::PermissionDenied, what.to_owned());
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut pid) } == 0 {
        return Err(denied("the daemon process is unknown"));
    }
    if identity::process_identity(pid)? != identity::own_identity()? {
        return Err(denied("the pipe is served by a process with another security context"));
    }
    Connection::new(pipe, pid)
}

/// Connect, starting `daemon` for the session when nothing serves it.
///
/// The daemon is started with no console and no inherited handles, in its own
/// process group, and outside any job this process is in when the job allows
/// it, so that it outlives the UI.
pub fn connect_or_start(session: &str, daemon: &Path) -> io::Result<Connection> {
    match connect(session) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        other => return other,
    }
    start(session, daemon)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match connect(session) {
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => return other,
        }
    }
}

fn start(session: &str, daemon: &Path) -> io::Result<()> {
    let program = daemon.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the daemon path is not Unicode")
    })?;
    let application = wide(program);
    let mut line = wide(&format!("\"{program}\" --session {session}"));
    let mut startup: STARTUPINFOW = unsafe { zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
    let mut create = |flags| unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            null(),
            null(),
            0,
            flags,
            null(),
            null(),
            &startup,
            &mut process,
        )
    };
    // A job that forbids breaking away refuses the flag outright.
    if create(flags | CREATE_BREAKAWAY_FROM_JOB) == 0 && create(flags) == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        CloseHandle(process.hProcess);
        CloseHandle(process.hThread);
    }
    Ok(())
}

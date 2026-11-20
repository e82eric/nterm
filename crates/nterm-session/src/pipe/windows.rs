//! The named pipe between a UI and the daemon, on Windows.
//!
//! The pipe is named after the account and only that account may open it,
//! from any of its sign-ins: at the desktop, over SSH, from a scheduled
//! task. Each end then checks that the other runs as the same account at the
//! same elevation (see `identity`): the daemon by impersonating the client,
//! the client by looking at the process that serves the pipe. Handles are
//! never inherited.

use crate::identity::{self, Handle, Identity, owned, wide};
use std::{
    io,
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
pub(super) fn name(session: &str) -> io::Result<String> {
    if !crate::is_session_name(session) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, crate::SESSION_NAME_RULE));
    }
    let own = identity::own_identity()?;
    // An elevated UI gets its own daemon; the two could not talk anyway.
    let high = if own.integrity >= 0x3000 { "-hi" } else { "" };
    Ok(format!(r"\\.\pipe\nterm-session-{}-{session}{high}", own.user))
}

/// A connected pipe instance.
pub(super) struct Stream {
    pipe: Handle,
    /// Signalled to end pending and future reads and writes.
    closed: Handle,
    peer: u32,
}

impl Stream {
    fn new(pipe: Handle, peer: u32) -> io::Result<Self> {
        let closed = owned(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        Ok(Self { pipe, closed, peer })
    }

    pub(super) fn peer(&self) -> u32 {
        self.peer
    }

    pub(super) fn close(&self) {
        unsafe {
            SetEvent(self.closed.0);
        }
    }
}

/// A reader or a writer of a stream: each has its own overlapped event.
pub(super) struct Half {
    inner: Arc<Stream>,
    /// Signalled when this half's operation completes.
    done: Handle,
}

impl Half {
    pub(super) fn new(stream: &Arc<Stream>) -> io::Result<Self> {
        Ok(Self {
            inner: stream.clone(),
            done: owned(unsafe { CreateEventW(null(), 1, 0, null()) })?,
        })
    }

    pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.transfer(buffer.as_mut_ptr(), buffer.len(), false)
    }

    pub(super) fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // The kernel only reads through the pointer on a write.
        self.transfer(buffer.as_ptr().cast_mut(), buffer.len(), true)
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

pub(super) struct Listener {
    name: Vec<u16>,
    identity: Identity,
    /// The instance the next client connects to. There is always one, so the
    /// name never becomes free while the daemon runs.
    next: Handle,
}

impl Listener {
    pub(super) fn bind(session: &str) -> io::Result<Self> {
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

    pub(super) fn accept(&mut self) -> io::Result<Stream> {
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
                // A client that came and went before it was waited for. The
                // instance is spent as above: waited on again it would say
                // the same for ever, with every client told the pipe is busy.
                ERROR_NO_DATA => {
                    let error = io::Error::from_raw_os_error(ERROR_NO_DATA as i32);
                    self.next = instance(&self.name, &self.identity, false)?;
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, error));
                }
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        // Not a refused client, whatever the system calls the failure.
        let fresh = instance(&self.name, &self.identity, false)
            .map_err(|error| io::Error::other(format!("cannot create a pipe instance: {error}")))?;
        let pipe = std::mem::replace(&mut self.next, fresh);
        let peer = self.authorize(pipe.0)?;
        Stream::new(pipe, peer)
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

pub(super) fn connect(session: &str) -> io::Result<Stream> {
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
    Stream::new(pipe, pid)
}

/// Start the daemon with no console and no inherited handles, in its own
/// process group, and outside any job this process is in when the job allows
/// it.
pub(super) fn start(session: &str, daemon: &Path) -> io::Result<()> {
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

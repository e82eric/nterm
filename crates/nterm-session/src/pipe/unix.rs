//! The Unix domain socket between a UI and the daemon.
//!
//! A session's socket is `/tmp/nterm-UID/SESSION`, or the same under
//! `$NTERM_TMPDIR`. The folder is the account's alone: it is made with mode
//! 0700, and refused if it belongs to another account, is open to others, or
//! is a link. It is not under `$XDG_RUNTIME_DIR`, which is removed at the
//! account's last logout while the daemon lives on.
//!
//! Each end then checks, with the socket's peer credentials, that the other
//! runs as the same account. Root is an account of its own and gets a daemon
//! of its own, as an elevated UI does on Windows.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::Shutdown,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// The account's folder of sockets, made if it is not there, and checked.
fn folder() -> io::Result<PathBuf> {
    let base = std::env::var_os("NTERM_TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    checked(base.join(format!("nterm-{}", uid())))
}

fn checked(folder: PathBuf) -> io::Result<PathBuf> {
    match fs::DirBuilder::new().mode(0o700).create(&folder) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    let meta = fs::symlink_metadata(&folder)?;
    let denied = |why: &str| {
        io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}", folder.display()))
    };
    if !meta.file_type().is_dir() {
        return Err(denied("not a folder"));
    }
    if meta.uid() != uid() {
        return Err(denied("the folder is another account's"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(denied("the folder is open to other accounts"));
    }
    Ok(folder)
}

fn path(session: &str) -> io::Result<PathBuf> {
    if !crate::is_session_name(session) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, crate::SESSION_NAME_RULE));
    }
    Ok(folder()?.join(session))
}

/// The socket of a session, for this account.
pub(super) fn name(session: &str) -> io::Result<String> {
    path(session)?.into_os_string().into_string().map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "the socket's path is not Unicode")
    })
}

/// Who is at the other end of a socket: its account and its process.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer(socket: &UnixStream) -> io::Result<(u32, u32)> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let got = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    if got != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((credentials.uid, credentials.pid.max(0) as u32))
}

/// Who is at the other end of a socket: its account, and no process.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn peer(socket: &UnixStream) -> io::Result<(u32, u32)> {
    let (mut user, mut group) = (0, 0);
    if unsafe { libc::getpeereid(socket.as_raw_fd(), &mut user, &mut group) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((user, 0))
}

/// A connected socket.
pub(super) struct Stream {
    socket: UnixStream,
    peer: u32,
    closed: AtomicBool,
}

impl Stream {
    pub(super) fn peer(&self) -> u32 {
        self.peer
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        // Wakes a read or a write that is waiting, in any thread.
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    fn aborted(&self) -> Option<io::Error> {
        self.closed
            .load(Ordering::Acquire)
            .then(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))
    }
}

/// A reader or a writer of a stream. A socket needs nothing per half.
pub(super) struct Half {
    inner: Arc<Stream>,
}

impl Half {
    pub(super) fn new(stream: &Arc<Stream>) -> io::Result<Self> {
        Ok(Self { inner: stream.clone() })
    }

    /// 0 when the other end is gone, as for a pipe.
    pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(aborted) = self.inner.aborted() {
            return Err(aborted);
        }
        let read = match (&self.inner.socket).read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => Ok(0),
            other => other,
        };
        match (read, self.inner.aborted()) {
            (Ok(0) | Err(_), Some(aborted)) => Err(aborted),
            (read, _) => read,
        }
    }

    pub(super) fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Some(aborted) = self.inner.aborted() {
            return Err(aborted);
        }
        (&self.inner.socket).write(buffer).map_err(|error| self.inner.aborted().unwrap_or(error))
    }
}

pub(super) struct Listener {
    socket: UnixListener,
    /// Held for as long as the daemon serves the session: a second daemon
    /// cannot take it, and does not remove the socket of the first.
    _lock: File,
}

impl Listener {
    pub(super) fn bind(session: &str) -> io::Result<Self> {
        let path = path(session)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                io::Error::new(io::ErrorKind::AlreadyExists, "the session already has a daemon")
            } else {
                error
            });
        }
        // What is there is a socket that nothing serves: its daemon ended
        // without removing it.
        match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let socket = UnixListener::bind(&path)?;
        Ok(Self { socket, _lock: lock })
    }

    pub(super) fn accept(&mut self) -> io::Result<Stream> {
        let refused = |error: io::Error| io::Error::new(io::ErrorKind::PermissionDenied, error);
        let socket = match self.socket.accept() {
            Ok((socket, _)) => socket,
            // A client that came and went before it was taken.
            Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => {
                return Err(refused(error));
            }
            Err(error) => return Err(error),
        };
        let (account, pid) = peer(&socket).map_err(refused)?;
        if account != uid() {
            return Err(refused(io::Error::other(format!(
                "the client runs as account {account}, not {}",
                uid()
            ))));
        }
        Ok(Stream { socket, peer: pid, closed: AtomicBool::new(false) })
    }
}

pub(super) fn connect(session: &str) -> io::Result<Stream> {
    let path = path(session)?;
    let socket = match UnixStream::connect(&path) {
        Ok(socket) => socket,
        // No socket, or one that nothing serves: either way no daemon.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no daemon serves {}: {error}", path.display()),
            ));
        }
        Err(error) => return Err(error),
    };
    let (account, pid) = peer(&socket)?;
    if account != uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the socket is served by account {account}, not {}", uid()),
        ));
    }
    Ok(Stream { socket, peer: pid, closed: AtomicBool::new(false) })
}

/// Start the daemon in a session of its own, with nothing of this process's
/// for its input and output, so that it outlives this process and whatever
/// terminal or SSH session it is in.
pub(super) fn start(session: &str, daemon: &Path) -> io::Result<()> {
    let mut command = Command::new(daemon);
    command
        .arg("--session")
        .arg(session)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // Waited for, so that a UI that runs for long is left no zombie when
    // the daemon ends. A process that ends first leaves it to init.
    std::thread::Builder::new()
        .name("daemon waiter".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The folder of sockets of these tests, which each use sessions of
    /// their own: tests run in threads of one process and share its
    /// environment, so it is set once.
    fn isolated(_test: &str) -> PathBuf {
        static BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        BASE.get_or_init(|| {
            let base = std::env::temp_dir().join(format!("nterm-pipe-tests-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            fs::create_dir_all(&base).unwrap();
            unsafe { std::env::set_var("NTERM_TMPDIR", &base) };
            base
        })
        .clone()
    }

    #[test]
    fn a_daemon_and_a_client_of_one_account_talk_and_a_second_daemon_is_refused() {
        let _base = isolated("talk");
        let mut listener = Listener::bind("t1").unwrap();
        let second = Listener::bind("t1").map(|_| ()).unwrap_err();
        assert_eq!(second.kind(), io::ErrorKind::AlreadyExists);

        let served = std::thread::spawn(move || {
            let stream = Arc::new(listener.accept().unwrap());
            let mut half = Half::new(&stream).unwrap();
            let mut buffer = [0u8; 5];
            let read = half.read(&mut buffer).unwrap();
            half.write(&buffer[..read]).unwrap();
            assert_eq!(stream.peer(), std::process::id());
        });
        let stream = Arc::new(connect("t1").unwrap());
        assert_eq!(stream.peer(), std::process::id());
        let mut half = Half::new(&stream).unwrap();
        half.write(b"hello").unwrap();
        let mut buffer = [0u8; 5];
        let mut got = 0;
        while got < 5 {
            got += half.read(&mut buffer[got..]).unwrap();
        }
        assert_eq!(&buffer, b"hello");
        served.join().unwrap();
        // The daemon's end has gone.
        assert_eq!(half.read(&mut buffer).unwrap(), 0);
    }

    #[test]
    fn a_socket_nothing_serves_is_no_daemon_and_is_replaced() {
        let base = isolated("stale");
        {
            let _listener = Listener::bind("t2").unwrap();
        }
        // The socket is still there, but its daemon is not.
        assert!(base.join(format!("nterm-{}", uid())).join("t2").exists());
        assert_eq!(connect("t2").map(|_| ()).unwrap_err().kind(), io::ErrorKind::NotFound);
        let _listener = Listener::bind("t2").unwrap();
        connect("t2").unwrap();
    }

    #[test]
    fn closing_ends_a_read_that_is_waiting() {
        let _base = isolated("close");
        let mut listener = Listener::bind("t3").unwrap();
        let client = std::thread::spawn(|| connect("t3").unwrap());
        let stream = Arc::new(listener.accept().unwrap());
        let _client = client.join().unwrap();
        let mut half = Half::new(&stream).unwrap();
        let closer = stream.clone();
        let reading = std::thread::spawn(move || {
            let mut buffer = [0u8; 8];
            half.read(&mut buffer)
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        closer.close();
        let error = reading.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[test]
    fn a_folder_open_to_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let folder = isolated("open").join("open-to-others");
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        let error = checked(folder.clone()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        // A link to a folder that would do is refused as well.
        let good = isolated("open").join("good");
        checked(good.clone()).unwrap();
        let link = isolated("open").join("link");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        assert_eq!(checked(link).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}

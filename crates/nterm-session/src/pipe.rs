//! The local connection between a UI and the daemon: a named pipe on
//! Windows (`windows`), a Unix domain socket elsewhere (`unix`).
//!
//! Only the account the daemon runs as may connect, from any of its
//! sign-ins: at the desktop, over SSH, from a scheduled task. Each end checks
//! that the other runs as that account (on Windows, at the same elevation
//! too). Handles are never inherited.
//!
//! A connection is one duplex byte stream. One thread may read while another
//! writes; `close` wakes both.

use crate::relay;
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as system;
#[cfg(windows)]
use windows as system;

/// What the session's daemon is reached by: a pipe's name, or a socket's
/// path. Given to its shells, for the programs in them.
pub fn name(session: &str) -> io::Result<String> {
    system::name(session)
}

/// One end of a connection: a pipe, or a relay to one (see `relay`).
#[derive(Clone)]
pub struct Connection(Link);

#[derive(Clone)]
enum Link {
    Pipe(Arc<system::Stream>),
    Relay(Arc<relay::Child>),
}

impl Connection {
    fn local(stream: system::Stream) -> Self {
        Self(Link::Pipe(Arc::new(stream)))
    }

    pub(crate) fn through(relay: Arc<relay::Child>) -> Self {
        Self(Link::Relay(relay))
    }

    /// The process at the other end: the daemon, or the relay to it.
    pub fn peer_pid(&self) -> u32 {
        match &self.0 {
            Link::Pipe(pipe) => pipe.peer(),
            Link::Relay(relay) => relay.pid(),
        }
    }

    /// Whether the other end is reached through a relay, and so was not
    /// checked to be this user's daemon on this machine.
    pub fn relayed(&self) -> bool {
        matches!(self.0, Link::Relay(_))
    }

    /// What a relay's program has written to its error output since the
    /// connection was ready: for `ssh`, why the connection ended. Waits up
    /// to `wait` for it to finish saying it. Nothing for a pipe.
    pub fn said(&self, wait: Duration) -> Option<String> {
        match &self.0 {
            Link::Pipe(_) => None,
            Link::Relay(relay) => Some(relay.said(wait, true)),
        }
    }

    /// A relay has one reader and one writer; a pipe as many as are asked for.
    pub fn reader(&self) -> io::Result<Reader> {
        Ok(Reader(match &self.0 {
            Link::Pipe(pipe) => ReadEnd::Pipe(system::Half::new(pipe)?),
            Link::Relay(relay) => ReadEnd::Relay(relay.reader()?),
        }))
    }

    pub fn writer(&self) -> io::Result<Writer> {
        Ok(Writer(match &self.0 {
            Link::Pipe(pipe) => WriteEnd::Pipe(system::Half::new(pipe)?),
            Link::Relay(relay) => WriteEnd::Relay(relay.writer()?),
        }))
    }

    /// End reads and writes on every clone, now and from now on.
    pub fn close(&self) {
        match &self.0 {
            Link::Pipe(pipe) => pipe.close(),
            Link::Relay(relay) => relay.close(),
        }
    }
}

enum ReadEnd {
    Pipe(system::Half),
    Relay(relay::ChildReader),
}

pub struct Reader(ReadEnd);
impl Read for Reader {
    /// 0 when the other end is gone; `ConnectionAborted` once closed here.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        match &mut self.0 {
            ReadEnd::Pipe(half) => half.read(buffer),
            ReadEnd::Relay(relay) => relay.read(buffer),
        }
    }
}

enum WriteEnd {
    Pipe(system::Half),
    Relay(relay::ChildWriter),
}

pub struct Writer(WriteEnd);
impl Write for Writer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        match &mut self.0 {
            WriteEnd::Pipe(half) => half.write(buffer),
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
pub struct Listener(system::Listener);

impl Listener {
    /// Claim the session's pipe. `AlreadyExists` means a daemon already
    /// serves it.
    pub fn bind(session: &str) -> io::Result<Self> {
        system::Listener::bind(session).map(Self)
    }

    /// Wait for a UI. `PermissionDenied` is a client that was turned away;
    /// the listener is still good.
    pub fn accept(&mut self) -> io::Result<Connection> {
        self.0.accept().map(Connection::local)
    }
}

/// Connect to the session's daemon. `NotFound` means none is running.
pub fn connect(session: &str) -> io::Result<Connection> {
    system::connect(session).map(Connection::local)
}

/// Connect, starting `daemon` for the session when nothing serves it.
///
/// The daemon is started detached from this process (on Windows with no
/// console, in its own process group and outside any job that allows it; on
/// Unix in a session of its own), with no inherited handles, so that it
/// outlives the UI.
pub fn connect_or_start(session: &str, daemon: &Path) -> io::Result<Connection> {
    connect_or_start_saying(session, daemon).map(|(connection, _)| connection)
}

/// `connect_or_start`, and whether the daemon had to be started.
pub fn connect_or_start_saying(session: &str, daemon: &Path) -> io::Result<(Connection, bool)> {
    match connect(session) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        other => return other.map(|connection| (connection, false)),
    }
    system::start(session, daemon)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match connect(session) {
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => return other.map(|connection| (connection, true)),
        }
    }
}

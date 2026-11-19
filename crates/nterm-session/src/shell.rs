//! The way a program in a pane's shell reaches the UI showing that pane.
//!
//! The daemon gives each shell its own name and the pane in the environment.
//! A program connects to that daemon's pipe, names the pane, and from then
//! on exchanges messages with the UI; the daemon passes them on unread. The
//! daemon ends the conversation when the UI does, or when the UI goes.

use crate::{
    pipe::{self, Connection, Writer},
    protocol::{Control, Frame, PaneId},
};
use std::{
    io::{self, BufReader},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    time::Duration,
};

/// The daemon that runs a shell, by the name it serves under.
pub const DAEMON_VARIABLE: &str = "NTERM_DAEMON";
/// The pane a shell is in.
pub const PANE_VARIABLE: &str = "NTERM_PANE";
/// The daemon's pipe, whole, for a client that is a script and would
/// otherwise have to work the name out. See `Invoke-NtermPicker.ps1`.
pub const PIPE_VARIABLE: &str = "NTERM_DAEMON_PIPE";
/// The folder installed with the daemon (`share\nterm`: the Lua modules and
/// the PowerShell module), for a profile on the daemon's machine to load
/// from. Only the daemon sets it, and only when the folder is there.
pub const SHARE_VARIABLE: &str = "NTERM_SHARE";

/// What came from the other side.
#[derive(Debug, PartialEq)]
pub enum Event {
    Message(Vec<u8>),
    /// The UI finished, or left the pane.
    Closed,
    /// The daemon would not pass anything on, and why.
    Refused(String),
    /// The daemon went.
    Lost,
}

pub struct Program {
    connection: Connection,
    writer: Writer,
    events: Receiver<Event>,
}

impl Program {
    /// For the shell this process was started from. `None` when it is not
    /// in a pane whose shell the daemon runs.
    pub fn from_environment() -> Option<io::Result<Self>> {
        let session = std::env::var(DAEMON_VARIABLE).ok().filter(|v| !v.is_empty())?;
        let pane = std::env::var(PANE_VARIABLE).ok()?.parse().ok()?;
        Some(Self::connect(&session, pane))
    }

    pub fn connect(session: &str, pane: PaneId) -> io::Result<Self> {
        Self::open(session, Control::Shell { pane })
    }

    /// As `connect`, for a program that wants the daemon to run a picker:
    /// what it sends is for the daemon, and so is what it is answered.
    pub fn pick(session: &str, pane: PaneId) -> io::Result<Self> {
        Self::open(session, Control::Pick { pane })
    }

    fn open(session: &str, first: Control) -> io::Result<Self> {
        let connection = pipe::connect(session)?;
        let mut writer = connection.writer()?;
        Frame::Control(first).write(&mut writer)?;
        let mut reader = BufReader::new(connection.reader()?);
        let (tx, events) = mpsc::channel();
        std::thread::Builder::new().name("nterm session".into()).spawn(move || {
            loop {
                let event = match Frame::read(&mut reader) {
                    Ok(Frame::Shell { bytes, .. }) => Event::Message(bytes),
                    Ok(Frame::Control(Control::ShellClosed { .. })) => Event::Closed,
                    Ok(Frame::Control(Control::Error { message, .. })) => Event::Refused(message),
                    Ok(_) => continue,
                    Err(_) => Event::Lost,
                };
                let last = !matches!(event, Event::Message(_));
                if tx.send(event).is_err() || last {
                    return;
                }
            }
        })?;
        Ok(Self { connection, writer, events })
    }

    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        Frame::Shell { channel: 0, bytes: bytes.to_vec() }.write(&mut self.writer)
    }

    /// The next event, or `None` when none came in `timeout`. After one
    /// that is not a message, every call returns `Lost`.
    pub fn receive(&self, timeout: Duration) -> Option<Event> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Event::Lost),
        }
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        self.connection.close();
    }
}

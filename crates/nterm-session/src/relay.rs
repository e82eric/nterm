//! A connection to a daemon through a relay: a child process whose input and
//! output are the daemon's byte stream.
//!
//! `nterm-daemon --relay` is the relay. It connects to the daemon beside it,
//! writes [`MARKER`] on a line of its own, and from then on copies bytes both
//! ways. Run through `ssh`, that puts a UI on one machine and the daemon on
//! another.
//!
//! Nothing here checks who the daemon is, as `pipe::connect` does: whatever
//! started the child (`ssh`, say) is what vouches for the other end.

use std::{
    io::{self, Read, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

/// What a relay writes, on a line of its own, when the bytes that follow are
/// the daemon's. Anything before it (a login banner, say) is not.
pub const MARKER: &str = "nterm-relay 1";
const MARKER_PREFIX: &str = "nterm-relay ";

/// How long a relay gets to say it is ready.
const READY: Duration = Duration::from_secs(30);
/// How long a relay gets to pass on what it holds once its input has ended.
const LEAVING: Duration = Duration::from_secs(2);
/// No line before the marker is worth more than this.
const LONGEST_LINE: usize = 4096;
/// How much of the child's error output is kept.
const MOST_SAID: usize = 1 << 16;
/// How long a child that has gone gets to finish its error output.
const HEARD_OUT: Duration = Duration::from_secs(1);

/// From `windows-sys`'s `CREATE_NO_WINDOW`: no console window for the child.
#[cfg(windows)]
const NO_WINDOW: u32 = 0x0800_0000;

pub(crate) struct Child {
    process: Mutex<std::process::Child>,
    pid: u32,
    closed: AtomicBool,
    /// Whether the one writer has been given out.
    writing: AtomicBool,
    input: Mutex<Option<ChildStdin>>,
    output: Mutex<Option<ChildStdout>>,
    /// What the child has written to its error output, up to `MOST_SAID`:
    /// `ssh`'s explanations, and the relay's.
    said: Arc<Mutex<Vec<u8>>>,
    /// How much of `said` was there when the connection was ready.
    said_before: AtomicUsize,
    /// Reads the error output until the child closes it.
    listener: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Child {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// What the child wrote to its error output: all of it, or only what
    /// came once the connection was ready. Waits up to `wait` for the child
    /// to close it, as `ssh` explains an end as it goes.
    pub(crate) fn said(&self, wait: Duration, since_ready: bool) -> String {
        let deadline = std::time::Instant::now() + wait;
        loop {
            let finished = self.listener.lock().unwrap().as_ref().is_none_or(|l| l.is_finished());
            if finished || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let said = self.said.lock().unwrap();
        let from = if since_ready { self.said_before.load(Ordering::Acquire) } else { 0 };
        String::from_utf8_lossy(&said[from.min(said.len())..]).trim().to_owned()
    }

    /// End the connection: no more is written, and reads end once the child
    /// has gone.
    ///
    /// The child is not killed at once. What was written just before may
    /// still be on its way through it, and a pipe would have delivered that.
    /// Its input is closed instead, which a relay takes as the UI leaving: it
    /// passes on what it holds and goes. One that has not gone after
    /// `LEAVING` is killed.
    pub(crate) fn close(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let child = self.clone();
        let leave = move || child.leave();
        if std::thread::Builder::new().name("relay leaving".into()).spawn(leave).is_err() {
            self.kill();
        }
    }

    /// Close the child's input and wait for it to go, for `LEAVING` at most.
    fn leave(&self) {
        match self.input.try_lock() {
            Ok(mut input) => drop(input.take()),
            // A write is stuck in the child: there is nothing to wait for.
            Err(_) => return self.kill(),
        }
        let deadline = std::time::Instant::now() + LEAVING;
        while std::time::Instant::now() < deadline {
            if !matches!(self.process.lock().unwrap().try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }

    fn kill(&self) {
        let _ = self.process.lock().unwrap().kill();
    }

    fn aborted(&self) -> Option<io::Error> {
        self.closed
            .load(Ordering::Acquire)
            .then(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))
    }

    pub(crate) fn reader(self: &Arc<Self>) -> io::Result<ChildReader> {
        let output = self.output.lock().unwrap().take();
        let output = output.ok_or_else(|| io::Error::other("a relay has one reader"))?;
        Ok(ChildReader { child: self.clone(), output })
    }

    pub(crate) fn writer(self: &Arc<Self>) -> io::Result<ChildWriter> {
        if self.writing.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other("a relay has one writer"));
        }
        Ok(ChildWriter { child: self.clone() })
    }
}

impl Drop for Child {
    /// The last of the connection going is a close, and as gentle: something
    /// may have been written just before.
    fn drop(&mut self) {
        self.leave();
        let _ = self.process.get_mut().unwrap().wait();
    }
}

pub(crate) struct ChildReader {
    child: Arc<Child>,
    output: ChildStdout,
}

impl Read for ChildReader {
    /// Like a pipe's: 0 when the other end is gone, an error once closed here.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(aborted) = self.child.aborted() {
            return Err(aborted);
        }
        let read = match self.output.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
            other => other,
        };
        match (read, self.child.aborted()) {
            (Ok(0) | Err(_), Some(aborted)) => Err(aborted),
            (read, _) => read,
        }
    }
}

pub(crate) struct ChildWriter {
    child: Arc<Child>,
}

impl Write for ChildWriter {
    /// The child's input stays with the child, so that `close` can end it.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let closed = || io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed");
        let mut input = self.child.input.lock().unwrap();
        match (self.child.aborted(), input.as_mut()) {
            (None, Some(input)) => input.write(buffer),
            _ => Err(closed()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One line of the child's output, without its end; `None` at the end of the
/// output. Read a byte at a time, so that nothing after the line is taken.
fn line(output: &mut ChildStdout) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8];
    loop {
        match output.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) if byte[0] == b'\n' => break,
            // A line too long to be the marker: keep its start and carry on.
            Ok(_) if bytes.len() < LONGEST_LINE => bytes.push(byte[0]),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Start `command` as a relay and wait until it says the daemon is there.
///
/// The error for a relay that ends first carries what it wrote to its error
/// output, which is where `ssh` and the relay explain themselves.
pub fn connect(command: &mut Command) -> io::Result<crate::pipe::Connection> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(command, NO_WINDOW);
    let mut process = command.spawn()?;
    let input = process.stdin.take();
    let mut output = process.stdout.take().expect("piped");
    let mut errors = process.stderr.take().expect("piped");
    // Read for as long as the child runs, or it would stop when the pipe filled.
    let said = Arc::new(Mutex::new(Vec::new()));
    let saying = said.clone();
    let listener = std::thread::Builder::new().name("relay errors".into()).spawn(move || {
        let mut chunk = [0u8; 1024];
        while let Ok(read @ 1..) = errors.read(&mut chunk) {
            let mut said = saying.lock().unwrap();
            if said.len() < MOST_SAID {
                said.extend_from_slice(&chunk[..read]);
            }
        }
    })?;
    let child = Arc::new(Child {
        pid: process.id(),
        process: Mutex::new(process),
        closed: AtomicBool::new(false),
        writing: AtomicBool::new(false),
        input: Mutex::new(input),
        output: Mutex::new(None),
        said,
        said_before: AtomicUsize::new(0),
        listener: Mutex::new(Some(listener)),
    });

    // A relay that never answers must not hold the caller for ever.
    let ready = Arc::new(AtomicBool::new(false));
    let (waiting, late) = (ready.clone(), Arc::downgrade(&child));
    std::thread::Builder::new().name("relay wait".into()).spawn(move || {
        std::thread::sleep(READY);
        if !waiting.load(Ordering::Acquire)
            && let Some(child) = late.upgrade()
        {
            child.close();
        }
    })?;

    let failed = |why: String| {
        // The child is gone or going; hear it out.
        child.close();
        let said = child.said(LEAVING + HEARD_OUT, false);
        io::Error::other(if said.is_empty() { why } else { format!("{why}: {said}") })
    };
    loop {
        match line(&mut output) {
            Ok(Some(line)) if line == MARKER => break,
            Ok(Some(line)) if line.starts_with(MARKER_PREFIX) => {
                return Err(failed(format!(
                    "the relay speaks another version (\"{line}\", not \"{MARKER}\")"
                )));
            }
            Ok(Some(_)) => {}
            Ok(None) if child.aborted().is_some() => {
                return Err(failed(format!(
                    "the relay was not ready after {} seconds",
                    READY.as_secs()
                )));
            }
            Ok(None) => return Err(failed("the relay ended before it was ready".into())),
            Err(error) => return Err(failed(format!("the relay could not be read: {error}"))),
        }
    }
    ready.store(true, Ordering::Release);
    // What was said before this is from setting up: a banner, a warning.
    let before = child.said.lock().unwrap().len();
    child.said_before.store(before, Ordering::Release);
    *child.output.lock().unwrap() = Some(output);
    Ok(crate::pipe::Connection::through(child))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A relay that runs `script`: `cmd` on Windows, `sh` elsewhere.
    fn relay(test: &str, script: &str) -> Command {
        let folder = std::env::temp_dir().join(format!("nterm-relay-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        if cfg!(windows) {
            let path = folder.join("relay.cmd");
            std::fs::write(&path, script).unwrap();
            let mut command = Command::new("cmd.exe");
            command.arg("/d").arg("/c").arg(path);
            command
        } else {
            let path = folder.join("relay.sh");
            std::fs::write(&path, script).unwrap();
            let mut command = Command::new("sh");
            command.arg(path);
            command
        }
    }

    /// One that says something as it starts and something else as it
    /// ends, as `ssh` does, with time between for each to be heard.
    const SAYS_BOTH: &str = if cfg!(windows) {
        "@>&2 echo Warning: setting up\r\n\
         @ping -n 2 127.0.0.1 >nul\r\n\
         @echo nterm-relay 1\r\n\
         @ping -n 2 127.0.0.1 >nul\r\n\
         @>&2 echo Connection reset by peer\r\n"
    } else {
        "echo Warning: setting up >&2\n\
         sleep 1\n\
         echo nterm-relay 1\n\
         sleep 1\n\
         echo Connection reset by peer >&2\n"
    };

    const REFUSED: &str = if cfg!(windows) {
        "@>&2 echo Permission denied (publickey).\r\n"
    } else {
        "echo 'Permission denied (publickey).' >&2\n"
    };

    #[test]
    fn what_a_relay_says_once_ready_is_kept_for_when_it_ends() {
        let mut command = relay("said", SAYS_BOTH);
        let connection = connect(&mut command).expect("ready");
        let mut reader = connection.reader().unwrap();
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        // Only what came after it was ready: why it ended, not how it began.
        let said = connection.said(Duration::from_secs(5)).unwrap();
        assert_eq!(said, "Connection reset by peer");
    }

    #[test]
    fn a_relay_that_ends_before_it_is_ready_says_why_in_its_error() {
        let mut command = relay("early", REFUSED);
        let Err(error) = connect(&mut command) else { panic!("connected") };
        let error = error.to_string();
        assert!(error.starts_with("the relay ended before it was ready"), "{error}");
        assert!(error.ends_with("Permission denied (publickey)."), "{error}");
    }
}

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
    os::windows::process::CommandExt,
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// What a relay writes, on a line of its own, when the bytes that follow are
/// the daemon's. Anything before it (a login banner, say) is not.
pub const MARKER: &str = "nterm-relay 1";
const MARKER_PREFIX: &str = "nterm-relay ";

/// How long a relay gets to say it is ready.
const READY: Duration = Duration::from_secs(30);
/// No line before the marker is worth more than this.
const LONGEST_LINE: usize = 4096;

/// From `windows-sys`'s `CREATE_NO_WINDOW`: no console window for the child.
const NO_WINDOW: u32 = 0x0800_0000;

pub(crate) struct Child {
    process: Mutex<std::process::Child>,
    pid: u32,
    closed: AtomicBool,
    input: Mutex<Option<ChildStdin>>,
    output: Mutex<Option<ChildStdout>>,
}

impl Child {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// End the child, which ends reads and writes.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
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
        let input = self.input.lock().unwrap().take();
        let input = input.ok_or_else(|| io::Error::other("a relay has one writer"))?;
        Ok(ChildWriter { child: self.clone(), input })
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let process = self.process.get_mut().unwrap();
        let _ = process.kill();
        let _ = process.wait();
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
    input: ChildStdin,
}

impl Write for ChildWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Some(aborted) = self.child.aborted() {
            return Err(aborted);
        }
        self.input.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.input.flush()
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
    let mut process = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(NO_WINDOW)
        .spawn()?;
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
            if said.len() < 1 << 16 {
                said.extend_from_slice(&chunk[..read]);
            }
        }
    })?;
    let child = Arc::new(Child {
        pid: process.id(),
        process: Mutex::new(process),
        closed: AtomicBool::new(false),
        input: Mutex::new(input),
        output: Mutex::new(None),
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
        let _ = listener.join();
        let said = String::from_utf8_lossy(&said.lock().unwrap()).trim().to_owned();
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
    *child.output.lock().unwrap() = Some(output);
    Ok(crate::pipe::Connection::through(child))
}

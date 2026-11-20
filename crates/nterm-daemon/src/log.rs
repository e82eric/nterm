//! The daemon has no console, so what it has to say goes to a file: see
//! `nterm_session::log`, which the UI's and the relay's logs share.

use nterm_session::log::Kept;
use std::{fmt::Arguments, io::Write, sync::Mutex};

pub use nterm_session::log::{bytes, directory, duration};

static LOG: Mutex<Option<Kept>> = Mutex::new(None);

/// How long a log file gets: a session's logs take twice this at most,
/// 200 MB, whatever is traced and for however long.
const LIMIT: u64 = 100 << 20;

/// Start logging to the session's file, after what it already says.
pub fn open(session: &str) {
    let directory = directory();
    let _ = std::fs::create_dir_all(&directory);
    *LOG.lock().unwrap() = Kept::at(directory.join(format!("daemon-{session}.log")), LIMIT);
}

/// With `NTERM_DAEMON_TRACE` set, every byte to and from every shell is
/// logged. For finding out who said what to whom.
pub fn trace(pane: u32, what: &str, bytes: &[u8]) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("NTERM_DAEMON_TRACE").is_some()) {
        log(format_args!("pane {pane} {what} {:?}", String::from_utf8_lossy(bytes)));
    }
}

pub fn log(message: Arguments<'_>) {
    let line = nterm_session::log::line(message);
    if let Some(log) = LOG.lock().unwrap().as_mut() {
        log.write(line.as_bytes());
    }
    // Present when run by hand or by a test.
    let _ = std::io::stderr().write_all(line.as_bytes());
}

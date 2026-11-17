//! The daemon has no console, so what it has to say goes to a file.

use std::{
    fmt::Arguments,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// The file being written, and how long it is.
struct Log {
    file: File,
    path: PathBuf,
    length: u64,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// How long a log file gets. One that is full is kept as the one before,
/// in place of the one before it, so a session's logs take twice this at
/// most: 200 MB, whatever is traced and for however long.
const LIMIT: u64 = 100 << 20;

/// Where logs go: `%LOCALAPPDATA%\nterm`, or the temp directory.
pub fn directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("nterm")
}

/// Start logging to the session's file, after what it already says.
pub fn open(session: &str) {
    let directory = directory();
    let _ = std::fs::create_dir_all(&directory);
    *LOG.lock().unwrap() = Log::at(directory.join(format!("daemon-{session}.log")), LIMIT);
}

impl Log {
    /// The file at `path`, to add to; a new one if it is full.
    fn at(path: PathBuf, limit: u64) -> Option<Self> {
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= limit) {
            let _ = std::fs::rename(&path, before(&path));
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let length = file.metadata().map_or(0, |meta| meta.len());
        Some(Self { file, path, length })
    }

    /// Add a line. The line that fills the file is its last: the next
    /// starts a new file, and this one is the one before.
    fn write(&mut self, line: &[u8], limit: u64) {
        if self.length >= limit
            && let Some(next) = Self::at(self.path.clone(), limit)
        {
            *self = next;
        }
        if self.file.write_all(line).is_ok() {
            self.length += line.len() as u64;
        }
    }
}

/// Where a log file is kept once it is full: `daemon-x.log` as
/// `daemon-x.old.log`.
fn before(path: &Path) -> PathBuf {
    path.with_extension("old.log")
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
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let line = format!("{}.{:03} {message}\n", now.as_secs(), now.subsec_millis());
    if let Some(log) = LOG.lock().unwrap().as_mut() {
        log.write(line.as_bytes(), LIMIT);
    }
    // Present when run by hand or by a test.
    let _ = std::io::stderr().write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_log_is_kept_as_the_one_before_and_no_more_than_two_are() {
        let directory = std::env::temp_dir().join(format!("nterm-log-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("daemon-x.log");
        let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
        let mut log = Log::at(path.clone(), 8).unwrap();
        log.write(b"one\n", 8);
        log.write(b"two\n", 8);
        // Full, so the next line starts another.
        log.write(b"three\n", 8);
        assert_eq!(read(&before(&path)), "one\ntwo\n");
        assert_eq!(read(&path), "three\n");
        log.write(b"four\n", 8);
        log.write(b"five\n", 8);
        assert_eq!(read(&before(&path)), "three\nfour\n");
        assert_eq!(read(&path), "five\n");
        // A daemon that starts adds to what is there, unless it is full.
        drop(log);
        Log::at(path.clone(), 8).unwrap().write(b"six\n", 8);
        assert_eq!(read(&path), "five\nsix\n");
        Log::at(path.clone(), 8).unwrap().write(b"seven\n", 8);
        assert_eq!(read(&before(&path)), "five\nsix\n");
        assert_eq!(read(&path), "seven\n");
        std::fs::remove_dir_all(directory).unwrap();
    }
}

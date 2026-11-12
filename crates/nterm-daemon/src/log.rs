//! The daemon has no console, so what it has to say goes to a file.

use std::{
    fmt::Arguments,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

static FILE: Mutex<Option<File>> = Mutex::new(None);

/// Where logs go: `%LOCALAPPDATA%\nterm`, or the temp directory.
pub fn directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("nterm")
}

/// Start logging to the session's file. A file that has grown past a
/// megabyte starts over.
pub fn open(session: &str) {
    let directory = directory();
    let _ = std::fs::create_dir_all(&directory);
    let path = directory.join(format!("daemon-{session}.log"));
    let large = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 1 << 20);
    let file = OpenOptions::new().create(true).append(!large).write(true).truncate(large).open(path);
    *FILE.lock().unwrap() = file.ok();
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
    if let Some(file) = FILE.lock().unwrap().as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
    // Present when run by hand or by a test.
    let _ = std::io::stderr().write_all(line.as_bytes());
}

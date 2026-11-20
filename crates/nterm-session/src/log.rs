//! Logs: lines in files under `directory()`, each file kept to a
//! size. A full file is kept as the one before (`x.log` as `x.old.log`), in
//! place of the one before it, so a log takes twice its limit at most.
//!
//! Each line is `SECONDS.MILLIS PID MESSAGE`: when, in Unix time, and which
//! process, as several can write to one file.

use std::{
    fmt::Arguments,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Where logs go: `%LOCALAPPDATA%\nterm` on Windows; elsewhere
/// `$XDG_STATE_HOME/nterm`, which is `~/.local/state/nterm` unless set. The
/// temp directory when there is neither.
pub fn directory() -> PathBuf {
    let state = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|folder| !folder.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
    };
    state.unwrap_or_else(std::env::temp_dir).join("nterm")
}

/// A line as it is written, with its end.
pub fn line(message: Arguments<'_>) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let pid = std::process::id();
    format!("{}.{:03} {pid} {message}\n", now.as_secs(), now.subsec_millis())
}

/// Add a line to the log `name` in `directory()`, for a process that writes
/// a few: the file is opened for each, so that any number of processes can
/// write to it, and whichever finds it full starts the next. Nothing is
/// said if it cannot be written.
pub fn append(name: &str, limit: u64, message: Arguments<'_>) {
    let directory = directory();
    let _ = std::fs::create_dir_all(&directory);
    append_to(&directory.join(name), limit, &line(message));
}

fn append_to(path: &Path, limit: u64, line: &str) {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() >= limit) {
        let _ = std::fs::rename(path, before(path));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        // One write, so that lines of several processes do not mix.
        let _ = file.write_all(line.as_bytes());
    }
}

/// A log one process writes many lines to, which it keeps open.
pub struct Kept {
    file: File,
    path: PathBuf,
    length: u64,
    limit: u64,
}

impl Kept {
    /// The file at `path`, to add to; a new one if it is full.
    pub fn at(path: PathBuf, limit: u64) -> Option<Self> {
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= limit) {
            let _ = std::fs::rename(&path, before(&path));
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let length = file.metadata().map_or(0, |meta| meta.len());
        Some(Self { file, path, length, limit })
    }

    /// Add a line. The line that fills the file is its last: the next
    /// starts a new file, and this one is the one before.
    pub fn write(&mut self, line: &[u8]) {
        if self.length >= self.limit
            && let Some(next) = Self::at(self.path.clone(), self.limit)
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
pub fn before(path: &Path) -> PathBuf {
    path.with_extension("old.log")
}

/// A length of time as a log says it: `0.4s`, `12.0s`, `3m05s`, `5h49m`.
pub fn duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{:.1}s", duration.as_secs_f64()),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds / 60 % 60),
    }
}

/// A number of bytes as a log says it: `512 B`, `2.1 KB`, `48.0 MB`.
pub fn bytes(count: u64) -> String {
    match count {
        0..1024 => format!("{count} B"),
        1024..0x10_0000 => format!("{:.1} KB", count as f64 / 1024.0),
        0x10_0000..0x4000_0000 => format!("{:.1} MB", count as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", count as f64 / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(test: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("nterm-log-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn a_line_says_when_and_which_process() {
        let line = line(format_args!("hello {}", 7));
        let (when, rest) = line.split_once(' ').unwrap();
        let (seconds, millis) = when.split_once('.').unwrap();
        assert!(seconds.parse::<u64>().unwrap() > 1_700_000_000, "{line}");
        assert_eq!(millis.len(), 3);
        assert_eq!(rest, format!("{} hello 7\n", std::process::id()));
    }

    #[test]
    fn a_kept_log_that_is_full_is_kept_as_the_one_before_and_no_more_than_two_are() {
        let directory = folder("kept");
        let path = directory.join("daemon-x.log");
        let mut log = Kept::at(path.clone(), 8).unwrap();
        log.write(b"one\n");
        log.write(b"two\n");
        // Full, so the next line starts another.
        log.write(b"three\n");
        assert_eq!(read(&before(&path)), "one\ntwo\n");
        assert_eq!(read(&path), "three\n");
        log.write(b"four\n");
        log.write(b"five\n");
        assert_eq!(read(&before(&path)), "three\nfour\n");
        assert_eq!(read(&path), "five\n");
        // One opened again adds to what is there, unless it is full.
        drop(log);
        Kept::at(path.clone(), 8).unwrap().write(b"six\n");
        assert_eq!(read(&path), "five\nsix\n");
        Kept::at(path.clone(), 8).unwrap().write(b"seven\n");
        assert_eq!(read(&before(&path)), "five\nsix\n");
        assert_eq!(read(&path), "seven\n");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn lines_appended_go_on_in_a_new_file_once_one_is_full() {
        let directory = folder("append");
        let path = directory.join("relay-x.log");
        append_to(&path, 8, "one\n");
        append_to(&path, 8, "two\n");
        append_to(&path, 8, "three\n");
        assert_eq!(read(&before(&path)), "one\ntwo\n");
        assert_eq!(read(&path), "three\n");
        // Another process's file open on it is no hindrance.
        let held = OpenOptions::new().append(true).open(&path).unwrap();
        append_to(&path, 8, "four\n");
        append_to(&path, 8, "five\n");
        drop(held);
        assert_eq!(read(&before(&path)), "three\nfour\n");
        assert_eq!(read(&path), "five\n");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn lines_of_several_writers_at_once_are_whole() {
        let directory = folder("several");
        let path = directory.join("relay-x.log");
        let writers: Vec<_> = (0..4)
            .map(|writer| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for line in 0..200 {
                        append_to(&path, 1 << 20, &format!("writer {writer} line {line:03}\n"));
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let log = read(&path);
        assert_eq!(log.lines().count(), 800);
        assert!(log.lines().all(|line| line.starts_with("writer ") && line.len() == 17), "{log}");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn times_and_sizes_read_as_a_person_would_say_them() {
        assert_eq!(duration(Duration::from_millis(400)), "0.4s");
        assert_eq!(duration(Duration::from_secs(185)), "3m05s");
        assert_eq!(duration(Duration::from_secs(5 * 3600 + 49 * 60 + 12)), "5h49m");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2150), "2.1 KB");
        assert_eq!(bytes(48 << 20), "48.0 MB");
    }
}

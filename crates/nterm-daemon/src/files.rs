//! Files a UI sends to be kept on this machine: a picture pasted in a window
//! on another machine, say, for a program in a shell here to open.
//!
//! The daemon chooses where each goes and what it is called; a UI chooses
//! only how its name ends. Nothing kept is run or opened here. Those older
//! than `KEPT_FOR` are removed when another is kept.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The largest file kept.
pub const LARGEST: usize = 32 << 20;

/// How long a file is kept before the next one kept removes it.
const KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What every name kept starts with; nothing else in the folder is touched.
const PREFIX: &str = "pasted-";

/// Where files are kept: `pasted` beside the logs.
pub fn directory() -> PathBuf {
    crate::log::directory().join("pasted")
}

/// Whether `extension` can end a name: 1 to 10 ASCII letters and digits.
pub fn valid_extension(extension: &str) -> bool {
    (1..=10).contains(&extension.len()) && extension.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Keep `bytes` in `directory` as a new file ending `.extension`, empty for
/// `bin`, and say where. Old files are removed first.
pub fn keep(directory: &Path, extension: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let extension = if extension.is_empty() { "bin".to_owned() } else { extension.to_ascii_lowercase() };
    if !valid_extension(&extension) {
        return Err(format!("{extension:?} cannot end a file's name: up to 10 letters and digits"));
    }
    if bytes.len() > LARGEST {
        return Err(format!(
            "the file is {} MiB; no more than {} MiB is kept",
            bytes.len().div_ceil(1 << 20),
            LARGEST >> 20
        ));
    }
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("{} could not be made: {error}", directory.display()))?;
    if let Err(error) = clean(directory, SystemTime::now()) {
        crate::log::log(format_args!("files: {} not cleaned: {error}", directory.display()));
    }

    // Several daemons, one per session, keep files here, and each counts
    // from one: the pid keeps their names apart.
    static COUNT: AtomicU64 = AtomicU64::new(1);
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    let pid = std::process::id();
    let mut last = None;
    for _ in 0..16 {
        let count = COUNT.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("{PREFIX}{seconds}-{pid}-{count}.{extension}"));
        // Never in place of a file there is.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                let written = file.write_all(bytes).and_then(|()| file.flush());
                drop(file);
                return match written {
                    Ok(()) => Ok(path),
                    Err(error) => {
                        let _ = std::fs::remove_file(&path);
                        Err(format!("{} could not be written: {error}", path.display()))
                    }
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(format!("{} could not be made: {error}", path.display())),
        }
    }
    Err(format!("no free name in {}: {}", directory.display(), last.map_or_else(String::new, |e| e.to_string())))
}

/// Whether `name` is one `keep` gives: `pasted-`, three numbers joined by
/// `-`, a `.` and an extension.
fn kept_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PREFIX) else { return false };
    let Some((numbers, extension)) = rest.rsplit_once('.') else { return false };
    let numbers: Vec<&str> = numbers.split('-').collect();
    numbers.len() == 3
        && numbers.iter().all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && valid_extension(extension)
}

/// Remove the files `keep` made in `directory` that were last changed more
/// than `KEPT_FOR` before `now`. Only those directly in it, and only plain
/// files. One another daemon removed first is no matter.
fn clean(directory: &Path, now: SystemTime) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if !name.to_str().is_some_and(kept_name) {
            continue;
        }
        // Not followed: a link is not a plain file.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|changed| now.duration_since(changed).ok())
            .is_some_and(|age| age > KEPT_FOR);
        if old {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => crate::log::log(format_args!(
                    "files: {} not removed: {error}",
                    entry.path().display()
                )),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folder(PathBuf);
    impl Folder {
        fn new(test: &str) -> Self {
            let path = std::env::temp_dir().join(format!("nterm-files-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        /// A file there, last changed `age` ago.
        fn file(&self, name: &str, age: Duration) -> PathBuf {
            let path = self.0.join(name);
            let file = std::fs::File::create(&path).unwrap();
            file.set_modified(SystemTime::now() - age).unwrap();
            path
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_is_kept_as_it_was_sent_under_a_name_of_the_daemons() {
        let folder = Folder::new("kept");
        let bytes: Vec<u8> = (0..=255).cycle().take(100_000).collect();
        let path = keep(&folder.0, "PNG", &bytes).unwrap();
        assert_eq!(path.parent(), Some(folder.0.as_path()));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(kept_name(name) && name.ends_with(".png"), "{name}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // Another is another file; one with no extension is a .bin.
        let other = keep(&folder.0, "", b"x").unwrap();
        assert_ne!(other, path);
        assert!(other.to_str().unwrap().ends_with(".bin"));
    }

    #[test]
    fn what_cannot_be_kept_is_refused() {
        let folder = Folder::new("refused");
        for extension in ["p.ng", "..\\x", "png ", "abcdefghijk", "pñg"] {
            assert!(keep(&folder.0, extension, b"x").is_err(), "{extension}");
        }
        assert!(keep(&folder.0, "bin", &vec![0; LARGEST + 1]).is_err());
        assert!(keep(&folder.0, "bin", &vec![0; LARGEST]).is_ok());
    }

    #[test]
    fn only_old_files_of_the_daemons_are_removed() {
        let folder = Folder::new("clean");
        let week = KEPT_FOR + Duration::from_secs(60);
        let old = folder.file("pasted-1-2-3.png", week);
        let new = folder.file("pasted-4-5-6.png", Duration::from_secs(60));
        let mine = folder.file("notes.txt", week);
        let like = folder.file("pasted-screenshot.png", week);
        std::fs::create_dir(folder.0.join("pasted-7-8-9.png")).unwrap();
        std::fs::create_dir(folder.0.join("under")).unwrap();
        let under = folder.file("under/pasted-1-2-4.png", week);

        keep(&folder.0, "png", b"new").unwrap();
        assert!(!old.exists());
        for kept in [&new, &mine, &like, &under] {
            assert!(kept.exists(), "{}", kept.display());
        }
        assert!(folder.0.join("pasted-7-8-9.png").is_dir());
    }

    #[test]
    fn a_name_already_taken_is_not_written_over() {
        let folder = Folder::new("taken");
        // Every name the next few would have, taken.
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let next = {
            // What the counter is at: one more than the last name given.
            let given = keep(&folder.0, "bin", b"probe").unwrap();
            let name = given.file_stem().unwrap().to_str().unwrap().to_owned();
            name.rsplit('-').next().unwrap().parse::<u64>().unwrap() + 1
        };
        let pid = std::process::id();
        let mut taken = Vec::new();
        for count in next..next + 3 {
            for at in [seconds, seconds + 1] {
                let path = folder.0.join(format!("pasted-{at}-{pid}-{count}.bin"));
                std::fs::write(&path, b"theirs").unwrap();
                taken.push(path);
            }
        }
        let path = keep(&folder.0, "bin", b"mine").unwrap();
        assert!(!taken.contains(&path));
        assert_eq!(std::fs::read(&path).unwrap(), b"mine");
        for path in taken {
            assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
        }
    }
}

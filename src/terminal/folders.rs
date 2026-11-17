//! The folders a new workspace can be started in: those directly inside
//! `workspaces.new_roots`, which the picker of workspaces has a row for
//! each of.
//!
//! Looking for them can take as long as a disk likes, so no window does:
//! the rows are made of the folders last found, which are looked for again
//! off the UI thread whenever a picker of workspaces opens, and a window
//! gives its picker new rows when `version` has moved.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use nfm_file_system::walker::{FileWalkerScan, PublishedSnapshot, ScanEventSink, ScanStatus};

/// The folders last found, and the roots they were found in.
struct Known {
    roots: Vec<PathBuf>,
    /// How many levels inside them were looked in.
    depth: i32,
    /// As paths, in the order of their rows.
    folders: Vec<String>,
}

static KNOWN: Mutex<Known> = Mutex::new(Known { roots: Vec::new(), depth: 0, folders: Vec::new() });
static VERSION: AtomicU64 = AtomicU64::new(0);
/// How many scans have been started: the last is the one that counts.
static SCANS: AtomicU64 = AtomicU64::new(0);

/// Moves whenever the folders known may have changed.
pub(super) fn version() -> u64 {
    VERSION.load(Ordering::Acquire)
}

/// The folders of a scan as it finds them.
struct Folders {
    /// Which scan this is; one begun since has taken its place.
    scan: u64,
    /// Whether the folders are shown as they are found. They are when none
    /// were known; ones that were stay until all of the new are.
    as_found: bool,
    found: Mutex<Option<Arc<PublishedSnapshot>>>,
    ctx: egui::Context,
}

impl Folders {
    fn publish(&self) {
        let Some(found) = self.found.lock().unwrap().clone() else { return };
        let folders = folders_found(&found);
        if SCANS.load(Ordering::Acquire) != self.scan {
            return;
        }
        let mut known = KNOWN.lock().unwrap();
        if known.folders != folders {
            known.folders = folders;
            VERSION.fetch_add(1, Ordering::AcqRel);
            self.ctx.request_repaint();
        }
    }
}

impl ScanEventSink for Folders {
    fn snapshot(&self, snapshot: Arc<PublishedSnapshot>) {
        *self.found.lock().unwrap() = Some(snapshot);
        if self.as_found {
            self.publish();
        }
    }
    fn complete(&self, status: ScanStatus) {
        if status == ScanStatus::Completed {
            self.publish();
        }
    }
}

/// The folders a scan of roots found, as paths: in the order of the roots,
/// and within one by path, so that a folder's own folders follow it. A root
/// is an entry too, with no directory it is in, and is not one of them.
fn folders_found(found: &PublishedSnapshot) -> Vec<String> {
    use nfm_search_core::store::ItemsSource;
    // A path's parts, for telling which root it is in and what it follows.
    let parts = |path: &str| -> Vec<String> {
        let parts = path.split(['\\', '/']).filter(|part| !part.is_empty());
        parts.map(str::to_lowercase).collect()
    };
    let mut roots = Vec::new();
    let mut folders = Vec::new();
    for index in 0..found.len() {
        let Some((name, directory)) = found.name_and_directory(index) else { continue };
        if directory.is_empty() {
            roots.push(parts(&name));
        } else if found.is_dir(index)
            && let Some(path) = found.item(index)
        {
            folders.push((parts(&path), path));
        }
    }
    // The root a folder is in is the longest that its path starts with.
    let place = |folder: &[String]| {
        let inside = roots.iter().enumerate().filter(|(_, root)| folder.starts_with(root));
        inside.max_by_key(|(_, root)| root.len()).map_or(roots.len(), |(at, _)| at)
    };
    let mut folders: Vec<_> =
        folders.into_iter().map(|(parts, path)| (place(&parts), parts, path)).collect();
    folders.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    folders.into_iter().map(|(_, _, path)| path).collect()
}

/// Look for the folders of `roots` again, off this thread: the same scan a
/// picker of a new workspace's directory makes.
pub(super) fn scan(roots: &[PathBuf], depth: i32, ctx: &egui::Context) {
    use nfm_file_system::walker::{ScanOptions, start_scan};
    static SCAN: Mutex<Option<FileWalkerScan>> = Mutex::new(None);
    let scan = SCANS.fetch_add(1, Ordering::AcqRel) + 1;
    let as_found = {
        let mut known = KNOWN.lock().unwrap();
        if known.roots != roots || known.depth != depth {
            known.roots = roots.to_vec();
            known.depth = depth;
            known.folders.clear();
            VERSION.fetch_add(1, Ordering::AcqRel);
        }
        known.folders.is_empty()
    };
    let started = (!roots.is_empty()).then(|| {
        let folders = Folders { scan, as_found, found: Mutex::default(), ctx: ctx.clone() };
        let options = ScanOptions {
            roots: roots.to_vec(),
            // The folders of a root, and theirs, as many levels as asked.
            max_depth: depth,
            directories_only: true,
            files_only: false,
            relative: false,
        };
        start_scan(options, Arc::new(folders))
    });
    if let Some(earlier) = std::mem::replace(&mut *SCAN.lock().unwrap(), started) {
        earlier.cancel();
    }
}

/// The folders last found for `roots` to `depth`; none if it was others.
pub(super) fn known(roots: &[PathBuf], depth: i32) -> Vec<String> {
    let known = KNOWN.lock().unwrap();
    if known.roots == roots && known.depth == depth { known.folders.clone() } else { Vec::new() }
}
/// Whether any are known for `roots` to `depth`, without listing them.
pub(super) fn any_known(roots: &[PathBuf], depth: i32) -> bool {
    let known = KNOWN.lock().unwrap();
    known.roots == roots && known.depth == depth && !known.folders.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The folders a scan of `roots` to `depth` finds.
    fn scanned(roots: Vec<PathBuf>, depth: i32) -> Vec<String> {
        use nfm_file_system::walker::ScanOptions;
        use std::sync::mpsc;
        struct Sink(Mutex<Option<Arc<PublishedSnapshot>>>, Mutex<mpsc::Sender<ScanStatus>>);
        impl ScanEventSink for Sink {
            fn snapshot(&self, snapshot: Arc<PublishedSnapshot>) {
                *self.0.lock().unwrap() = Some(snapshot);
            }
            fn complete(&self, status: ScanStatus) {
                let _ = self.1.lock().unwrap().send(status);
            }
        }
        let (done, wait) = mpsc::channel();
        let sink = Arc::new(Sink(Mutex::new(None), Mutex::new(done)));
        nfm_file_system::walker::start_scan(
            ScanOptions {
                roots,
                max_depth: depth,
                directories_only: true,
                files_only: false,
                relative: false,
            },
            sink.clone(),
        );
        let status = wait.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        assert_eq!(status, ScanStatus::Completed);
        folders_found(&sink.0.lock().unwrap().clone().unwrap())
    }

    #[test]
    fn the_folders_of_each_root_are_found_by_name_and_files_and_the_roots_are_not() {
        let root = std::env::temp_dir().join(format!("nterm-folders-{}", std::process::id()));
        // `beta-x` is before `beta\inner` letter by letter, and after it as
        // a folder beside `beta`.
        for folder in ["one/zeta", "one/Alpha", "one/beta/inner/deep", "one/beta-x", "two/gamma"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("one/file.txt"), "").unwrap();
        let roots = vec![root.join("one"), root.join("missing"), root.join("two")];
        let inside = |found: &[String]| -> Vec<String> {
            let inside = |path: &String| {
                let path = std::path::Path::new(path).strip_prefix(&root).unwrap();
                path.to_str().unwrap().replace('\\', "/")
            };
            found.iter().map(inside).collect()
        };
        let found = scanned(roots.clone(), 1);
        assert_eq!(inside(&found), ["one/Alpha", "one/beta", "one/beta-x", "one/zeta", "two/gamma"]);
        // To two levels a folder's own folders follow it, and no deeper.
        let found = scanned(roots, 2);
        assert_eq!(
            inside(&found),
            ["one/Alpha", "one/beta", "one/beta/inner", "one/beta-x", "one/zeta", "two/gamma"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

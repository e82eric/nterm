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
    /// As paths, in the order of their rows.
    folders: Vec<String>,
}

static KNOWN: Mutex<Known> = Mutex::new(Known { roots: Vec::new(), folders: Vec::new() });
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
/// and by name within one. A root is an entry too, with no directory it is
/// in, and is not one of them.
fn folders_found(found: &PublishedSnapshot) -> Vec<String> {
    use nfm_search_core::store::ItemsSource;
    let bare = |path: &str| path.trim_end_matches(['\\', '/']).to_lowercase();
    let mut roots = Vec::new();
    let mut folders = Vec::new();
    for index in 0..found.len() {
        let Some((name, directory)) = found.name_and_directory(index) else { continue };
        if directory.is_empty() {
            roots.push(bare(&name));
        } else if found.is_dir(index)
            && let Some(path) = found.item(index)
        {
            folders.push((bare(&directory), name.to_lowercase(), path));
        }
    }
    let place = |root: &String| roots.iter().position(|known| known == root).unwrap_or(roots.len());
    folders.sort_by(|a, b| (place(&a.0), &a.0, &a.1).cmp(&(place(&b.0), &b.0, &b.1)));
    folders.into_iter().map(|(_, _, path)| path).collect()
}

/// Look for the folders of `roots` again, off this thread: the same scan a
/// picker of a new workspace's directory makes.
pub(super) fn scan(roots: &[PathBuf], ctx: &egui::Context) {
    use nfm_file_system::walker::{ScanOptions, start_scan};
    static SCAN: Mutex<Option<FileWalkerScan>> = Mutex::new(None);
    let scan = SCANS.fetch_add(1, Ordering::AcqRel) + 1;
    let as_found = {
        let mut known = KNOWN.lock().unwrap();
        if known.roots != roots {
            known.roots = roots.to_vec();
            known.folders.clear();
            VERSION.fetch_add(1, Ordering::AcqRel);
        }
        known.folders.is_empty()
    };
    let started = (!roots.is_empty()).then(|| {
        let folders = Folders { scan, as_found, found: Mutex::default(), ctx: ctx.clone() };
        let options = ScanOptions {
            roots: roots.to_vec(),
            // The folders of a root, and not what is in them.
            max_depth: 1,
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

/// The folders last found for `roots`; none if it was other roots.
pub(super) fn known(roots: &[PathBuf]) -> Vec<String> {
    let known = KNOWN.lock().unwrap();
    if known.roots == roots { known.folders.clone() } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folders_of_each_root_are_found_by_name_and_files_and_the_roots_are_not() {
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
        let root = std::env::temp_dir().join(format!("nterm-folders-{}", std::process::id()));
        for folder in ["one/zeta", "one/Alpha", "one/beta/inner", "two/gamma"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("one/file.txt"), "").unwrap();
        let (done, wait) = mpsc::channel();
        let sink = Arc::new(Sink(Mutex::new(None), Mutex::new(done)));
        nfm_file_system::walker::start_scan(
            ScanOptions {
                roots: vec![root.join("one"), root.join("missing"), root.join("two")],
                max_depth: 1,
                directories_only: true,
                files_only: false,
                relative: false,
            },
            sink.clone(),
        );
        let status = wait.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        assert_eq!(status, ScanStatus::Completed);
        let found = folders_found(&sink.0.lock().unwrap().clone().unwrap());
        let names: Vec<_> = found
            .iter()
            .map(|path| std::path::Path::new(path).file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["Alpha", "beta", "zeta", "gamma"]);
        assert_eq!(std::path::Path::new(&found[3]), root.join("two/gamma"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

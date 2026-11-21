//! Records the ghostty revision this build parses and encodes with. The two
//! ends of a session tell it to each other and log it. What decides whether
//! they can work together is the snapshot format version each one's library
//! reports, not this.
use std::{path::PathBuf, process::Command};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let revision = Command::new("git")
        .arg("-C")
        .arg(root.join("ghostty"))
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NTERM_GHOSTTY_REVISION={revision}");
    // The submodule's HEAD moves when its commit changes.
    let head = root.join(".git/modules/ghostty/HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    println!("cargo:rerun-if-changed={}", root.join(".git/modules/ghostty/logs/HEAD").display());
}

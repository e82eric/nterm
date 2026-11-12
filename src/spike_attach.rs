//! Phase 0 spike, throwaway: with `NTERM_SPIKE_ATTACH` set, the first pane
//! attaches to `daemon-spike serve` instead of starting a shell.
//!
//! Frames are `[kind u8][len u32 le][payload]`; see spike/daemon-spike.

use crate::ghostty_ffi;
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::time::Instant;

struct Client {
    to_daemon: Mutex<File>,
}

/// The attached host; zero once it is destroyed. The reader holds the lock
/// while it feeds output so the surface cannot be freed underneath it.
static HOST: Mutex<usize> = Mutex::new(0);

pub(crate) struct Pending {
    from_daemon: File,
    started: Instant,
}

fn log(line: &str) {
    let path = std::env::temp_dir().join("nterm-spike.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn frame(file: &Mutex<File>, kind: u8, payload: &[u8]) {
    let mut bytes = Vec::with_capacity(payload.len() + 5);
    bytes.push(kind);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    let _ = file.lock().unwrap().write_all(&bytes);
}

extern "C" fn write(userdata: *mut c_void, bytes: *const u8, len: usize) {
    let client = unsafe { &*userdata.cast::<Client>() };
    frame(&client.to_daemon, 1, unsafe { std::slice::from_raw_parts(bytes, len) });
}

extern "C" fn resize(userdata: *mut c_void, cols: u16, rows: u16, width: u32, height: u32) {
    let client = unsafe { &*userdata.cast::<Client>() };
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&cols.to_le_bytes());
    payload.extend_from_slice(&rows.to_le_bytes());
    payload.extend_from_slice(&width.to_le_bytes());
    payload.extend_from_slice(&height.to_le_bytes());
    frame(&client.to_daemon, 2, &payload);
    log(&format!("resize {cols}x{rows}"));
}

fn read_frame(file: &mut File) -> Option<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    file.read_exact(&mut header).ok()?;
    let len = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
    let mut payload = vec![0u8; len];
    file.read_exact(&mut payload).ok()?;
    Some((header[0], payload))
}

/// Call right before creating the surface. Makes that surface host-driven
/// and starting from the daemon's snapshot.
pub(crate) fn prepare() -> Option<Pending> {
    std::env::var_os("NTERM_SPIKE_ATTACH")?;
    let started = Instant::now();
    let mut from_daemon = File::open(r"\\.\pipe\nterm-spike-out")
        .inspect_err(|e| log(&format!("no daemon: {e}")))
        .ok()?;
    let to_daemon = std::fs::OpenOptions::new()
        .write(true)
        .open(r"\\.\pipe\nterm-spike-in")
        .inspect_err(|e| log(&format!("no daemon input pipe: {e}")))
        .ok()?;
    let (kind, snapshot) = read_frame(&mut from_daemon)?;
    if kind != 1 {
        return None;
    }
    log(&format!("snapshot {} bytes received after {:?}", snapshot.len(), started.elapsed()));
    let client = Box::leak(Box::new(Client { to_daemon: Mutex::new(to_daemon) }));
    let ok = unsafe {
        ghostty_ffi::nterm_host_backend_next(
            (client as *mut Client).cast(),
            write,
            resize,
            snapshot.as_ptr(),
            snapshot.len(),
        )
    };
    ok.then_some(Pending { from_daemon, started })
}

/// Call once the surface exists: start feeding it the daemon's output.
pub(crate) fn attach(pending: Pending, host: *mut ghostty_ffi::Host) {
    let Pending { mut from_daemon, started } = pending;
    log(&format!("surface created after {:?}", started.elapsed()));
    *HOST.lock().unwrap() = host as usize;
    std::thread::spawn(move || {
        while let Some((kind, payload)) = read_frame(&mut from_daemon) {
            let host = HOST.lock().unwrap();
            if *host == 0 {
                break;
            }
            let (target, ptr, len) = (*host as *mut ghostty_ffi::Host, payload.as_ptr(), payload.len());
            match kind {
                // The daemon's screen, sent again after it resized.
                1 => {
                    let start = Instant::now();
                    unsafe { ghostty_ffi::nterm_host_resync(target, ptr, len) };
                    log(&format!("resync {len} bytes applied in {:?}", start.elapsed()));
                }
                2 => unsafe { ghostty_ffi::nterm_host_output(target, ptr, len) },
                _ => {}
            }
        }
        log("reader ended");
    });
}

/// Call before a host is destroyed.
pub(crate) fn detach(host: *mut ghostty_ffi::Host) {
    let mut current = HOST.lock().unwrap();
    if *current == host as usize {
        *current = 0;
    }
}

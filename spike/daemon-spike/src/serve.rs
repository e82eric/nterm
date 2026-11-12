//! `daemon-spike serve <command...>`: a stand-in for the session daemon.
//!
//! One shell runs under ConPTY and feeds a headless terminal. A UI attaches
//! over two named pipes, gets a snapshot, then the live output.
//!
//! Frames are `[kind u8][len u32 le][payload]`.
//!   to the UI:   1 snapshot, 2 output, 3 shell exited
//!   from the UI: 1 input, 2 resize (cols u16, rows u16, width u32, height u32)

use super::{Headless, ghostty_terminal_resize};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FlushFileBuffers, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, CreatePipe, DisconnectNamedPipe, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, EXTENDED_STARTUPINFO_PRESENT, INFINITE, InitializeProcThreadAttributeList,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> i32;

/// The ConPTY nterm ships, when `NTERM_SPIKE_CONPTY` names its conpty.dll;
/// the system one otherwise.
fn conpty_api() -> (Create, Resize) {
    use windows_sys::Win32::System::Console::{CreatePseudoConsole, ResizePseudoConsole};
    let Ok(path) = std::env::var("NTERM_SPIKE_CONPTY") else {
        eprintln!("conpty: system");
        return (CreatePseudoConsole, ResizePseudoConsole);
    };
    unsafe {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let module = LoadLibraryW(wide.as_ptr());
        assert!(!module.is_null(), "cannot load {path}");
        let create = GetProcAddress(module, c"CreatePseudoConsole".as_ptr().cast()).unwrap();
        let resize = GetProcAddress(module, c"ResizePseudoConsole".as_ptr().cast()).unwrap();
        eprintln!("conpty: {path}");
        (std::mem::transmute(create), std::mem::transmute(resize))
    }
}

struct State {
    headless: Headless,
    console: HPCON,
    resize: Resize,
    pty_in: usize,
    /// The attached UI's output pipe.
    client: Option<usize>,
    fed: usize,
}

unsafe impl Send for State {}

fn write_all(handle: usize, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let mut n = 0u32;
        let len = bytes.len().min(1 << 20) as u32;
        let ok = unsafe { WriteFile(handle as HANDLE, bytes.as_ptr(), len, &mut n, null_mut()) };
        if ok == 0 {
            return false;
        }
        bytes = &bytes[n as usize..];
    }
    true
}

fn read_exact(handle: usize, mut buf: &mut [u8]) -> bool {
    while !buf.is_empty() {
        let mut n = 0u32;
        let ok = unsafe {
            ReadFile(handle as HANDLE, buf.as_mut_ptr(), buf.len() as u32, &mut n, null_mut())
        };
        if ok == 0 || n == 0 {
            return false;
        }
        buf = &mut buf[n as usize..];
    }
    true
}

fn send(handle: usize, kind: u8, payload: &[u8]) -> bool {
    let mut header = [kind, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    write_all(handle, &header) && write_all(handle, payload)
}

fn pipe(name: &str, access: u32) -> usize {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            access,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            1,
            1 << 20,
            1 << 20,
            0,
            null(),
        )
    };
    assert!(handle != INVALID_HANDLE_VALUE, "cannot create {name}");
    handle as usize
}

pub fn run(command: &str, cols: u16, rows: u16) {
    let (create, resize) = conpty_api();
    let (pty_in, pty_out, process) = unsafe {
        let (mut in_read, mut in_write): (HANDLE, HANDLE) = (null_mut(), null_mut());
        let (mut out_read, mut out_write): (HANDLE, HANDLE) = (null_mut(), null_mut());
        assert!(CreatePipe(&mut in_read, &mut in_write, null(), 0) != 0);
        assert!(CreatePipe(&mut out_read, &mut out_write, null(), 0) != 0);
        let mut console: HPCON = 0;
        let size = COORD { X: cols as i16, Y: rows as i16 };
        assert!(create(size, in_read, out_write, 0, &mut console) >= 0);
        CloseHandle(in_read);
        CloseHandle(out_write);

        let mut bytes = 0usize;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes);
        let list = Box::leak(vec![0u8; bytes].into_boxed_slice());
        let attributes = list.as_mut_ptr().cast();
        assert!(InitializeProcThreadAttributeList(attributes, 1, 0, &mut bytes) != 0);
        assert!(
            UpdateProcThreadAttribute(
                attributes,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                console as *const _,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            ) != 0
        );
        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.lpAttributeList = attributes;
        let mut process: PROCESS_INFORMATION = std::mem::zeroed();
        let mut line: Vec<u16> = command.encode_utf16().chain([0]).collect();
        assert!(
            CreateProcessW(
                null(),
                line.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup.StartupInfo,
                &mut process,
            ) != 0,
            "CreateProcessW failed for {command:?}"
        );
        eprintln!("shell pid {}", process.dwProcessId);
        let state = (in_write as usize, out_read as usize, process.hProcess as usize);
        CONSOLE.store(console as usize, std::sync::atomic::Ordering::Relaxed);
        state
    };
    let console = CONSOLE.load(std::sync::atomic::Ordering::Relaxed) as HPCON;

    let state = Arc::new(Mutex::new(State {
        headless: Headless::new(cols, rows),
        console,
        resize,
        pty_in,
        client: None,
        fed: 0,
    }));

    // Pty output: parse it, answer queries while nobody is attached, forward.
    {
        let state = state.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            loop {
                let mut n = 0u32;
                let ok = unsafe {
                    ReadFile(pty_out as HANDLE, buf.as_mut_ptr(), buf.len() as u32, &mut n, null_mut())
                };
                if ok == 0 || n == 0 {
                    break;
                }
                let bytes = &buf[..n as usize];
                let mut state = state.lock().unwrap();
                trace(&format!("pty> {:?}", String::from_utf8_lossy(bytes)));
                state.headless.feed(bytes);
                state.fed += bytes.len();
                let replies = std::mem::take(&mut state.headless.replies.bytes);
                // This terminal is the one the program talks to, attached or
                // not; the UI's copy stays silent.
                if !replies.is_empty() {
                    trace(&format!("reply {:?}", String::from_utf8_lossy(&replies)));
                    write_all(state.pty_in, &replies);
                }
                if let Some(client) = state.client {
                    if !send(client, 2, bytes) {
                        state.client = None;
                    }
                }
            }
        });
    }

    // The shell ending ends the session.
    {
        let state = state.clone();
        std::thread::spawn(move || {
            unsafe { WaitForSingleObject(process as HANDLE, INFINITE) };
            if let Some(client) = state.lock().unwrap().client {
                send(client, 3, &[]);
                unsafe { FlushFileBuffers(client as HANDLE) };
            }
            eprintln!("shell exited");
            std::process::exit(0);
        });
    }

    loop {
        let to_ui = pipe(r"\\.\pipe\nterm-spike-out", PIPE_ACCESS_OUTBOUND);
        let from_ui = pipe(r"\\.\pipe\nterm-spike-in", PIPE_ACCESS_INBOUND);
        unsafe {
            ConnectNamedPipe(to_ui as HANDLE, null_mut());
            ConnectNamedPipe(from_ui as HANDLE, null_mut());
        }

        {
            // Under the lock, so the output that follows starts exactly where
            // the snapshot ends.
            let mut state = state.lock().unwrap();
            let start = Instant::now();
            let snapshot = state.headless.snapshot();
            let encode = start.elapsed();
            let start = Instant::now();
            let sent = send(to_ui, 1, &snapshot);
            eprintln!(
                "attach: {} bytes fed so far, snapshot {} bytes, encode {encode:?}, send {:?}",
                state.fed,
                snapshot.len(),
                start.elapsed()
            );
            trace(&format!(
                "attach, screen is:\n{}",
                String::from_utf8_lossy(&super::format_vt(state.headless.terminal))
                    .replace('\x1b', "<ESC>")
            ));
            if sent {
                state.client = Some(to_ui);
            }
        }

        loop {
            let mut header = [0u8; 5];
            if !read_exact(from_ui, &mut header) {
                break;
            }
            let len = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
            let mut payload = vec![0u8; len];
            if !read_exact(from_ui, &mut payload) {
                break;
            }
            let mut state = state.lock().unwrap();
            match header[0] {
                1 => {
                    trace(&format!("ui>  {:?}", String::from_utf8_lossy(&payload)));
                    write_all(state.pty_in, &payload);
                }
                2 if len == 12 => {
                    let cols = u16::from_le_bytes([payload[0], payload[1]]);
                    let rows = u16::from_le_bytes([payload[2], payload[3]]);
                    let width = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                    let height = u32::from_le_bytes(payload[8..12].try_into().unwrap());
                    if cols == 0 || rows == 0 {
                        continue;
                    }
                    // Reflow first, then tell ConPTY, as the UI's termio does.
                    unsafe {
                        ghostty_terminal_resize(
                            state.headless.terminal,
                            cols,
                            rows,
                            width / cols as u32,
                            height / rows as u32,
                        );
                        (state.resize)(state.console, COORD { X: cols as i16, Y: rows as i16 });
                    }
                    // The UI reflowed its own copy, possibly from another
                    // size; send ours so both hold the same screen.
                    let start = Instant::now();
                    let snapshot = state.headless.snapshot();
                    if let Some(client) = state.client {
                        send(client, 1, &snapshot);
                    }
                    eprintln!("resize {cols}x{rows}, resync {} bytes in {:?}", snapshot.len(), start.elapsed());
                    trace(&format!("resize {cols}x{rows}"));
                }
                _ => {}
            }
        }

        state.lock().unwrap().client = None;
        eprintln!("detached");
        unsafe {
            DisconnectNamedPipe(to_ui as HANDLE);
            DisconnectNamedPipe(from_ui as HANDLE);
            CloseHandle(to_ui as HANDLE);
            CloseHandle(from_ui as HANDLE);
        }
    }
}

/// Everything that crosses the daemon, in order, when `NTERM_SPIKE_TRACE`
/// names a file.
fn trace(line: &str) {
    use std::io::Write;
    let Ok(path) = std::env::var("NTERM_SPIKE_TRACE") else { return };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

static CONSOLE:std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

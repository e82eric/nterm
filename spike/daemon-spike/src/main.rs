//! Phase 0 spike: a headless ghostty terminal in a plain Rust process.
//!
//!   daemon-spike synth  <out.bin>              feed generated output
//!   daemon-spike conpty <out.bin> <command...> run a command under ConPTY
//!
//! Either way the bytes go into a libghostty-vt terminal and the terminal is
//! encoded as a snapshot. `<out.bin>.raw` holds the bytes that were fed.

use std::ffi::c_void;
use std::time::Instant;

#[cfg(windows)]
mod serve;

#[repr(C)]
struct TerminalOpaque(c_void);
type Terminal = *mut TerminalOpaque;

#[repr(C)]
#[derive(Clone, Copy)]
struct Writer {
    write: extern "C" fn(*mut c_void, *const u8, usize) -> bool,
    userdata: *mut c_void,
}

const OPT_USERDATA: i32 = 1;
const OPT_WRITE_PTY: i32 = 2;
const OPT_SCROLLBACK_MAX_BYTES: i32 = 28;
const OPT_SCROLLBACK_MAX_LINES: i32 = 29;
const OPT_CONTINUATION_MAX_BYTES: i32 = 32;
const OPT_RESIZE_PULL_SCROLLBACK: i32 = 41;

unsafe extern "C" {
    fn ghostty_terminal_new(
        allocator: *const c_void,
        terminal: *mut Terminal,
        cols: u16,
        rows: u16,
    ) -> i32;
    fn ghostty_terminal_free(terminal: Terminal);
    fn ghostty_terminal_set(terminal: Terminal, option: i32, value: *const c_void) -> i32;
    fn ghostty_terminal_vt_write(terminal: Terminal, data: *const u8, len: usize);
    fn ghostty_terminal_resize(
        terminal: Terminal,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> i32;
    fn ghostty_snapshot_encode(terminal: Terminal, writer: Writer) -> i32;
    fn ghostty_snapshot_decoder_new_buf(
        allocator: *const c_void,
        decoder: *mut *mut c_void,
        ptr: *const u8,
        len: usize,
    ) -> i32;
    fn ghostty_snapshot_decoder_decode(decoder: *mut c_void, terminal: *mut Terminal) -> i32;
    fn ghostty_snapshot_decoder_free(decoder: *mut c_void);
    fn ghostty_snapshot_decoder_set(decoder: *mut c_void, option: i32, value: *const c_void)
    -> i32;
    fn ghostty_formatter_terminal_new(
        allocator: *const c_void,
        formatter: *mut *mut c_void,
        terminal: Terminal,
    ) -> i32;
    fn ghostty_formatter_set(formatter: *mut c_void, option: i32, value: *const c_void) -> i32;
    fn ghostty_formatter_format(formatter: *mut c_void, writer: Writer) -> i32;
    fn ghostty_formatter_free(formatter: *mut c_void);
    fn ghostty_terminal_get(terminal: Terminal, data: i32, out: *mut c_void) -> i32;
    fn ghostty_terminal_grid_ref(terminal: Terminal, point: Point, out: *mut GridRef) -> i32;
    fn ghostty_grid_ref_row(grid_ref: *const GridRef, out: *mut u64) -> i32;
    fn ghostty_row_get(row: u64, data: i32, out: *mut c_void) -> i32;
}

const DECODER_OPT_RETAIN_CONTINUATION: i32 = 2;
const FORMATTER_OPT_EMIT: i32 = 1;
const FORMATTER_OPT_EXTRAS: i32 = 5;
const FORMAT_VT: i32 = 1;
const EXTRAS_ALL: u32 = 0xFFF;
const DATA_TOTAL_ROWS: i32 = 14;
const POINT_TAG_SCREEN: i32 = 2;
const ROW_DATA_SEMANTIC_PROMPT: i32 = 6;

#[repr(C)]
#[derive(Clone, Copy)]
struct Coordinate {
    x: u16,
    y: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
union PointValue {
    coordinate: Coordinate,
    padding: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    tag: i32,
    value: PointValue,
}

#[repr(C)]
struct GridRef {
    node: *mut c_void,
    x: u16,
    y: u16,
}

fn encode(terminal: Terminal) -> Vec<u8> {
    let mut out = Vec::new();
    let writer = Writer {
        write: collect,
        userdata: (&raw mut out).cast(),
    };
    assert_eq!(unsafe { ghostty_snapshot_encode(terminal, writer) }, 0);
    out
}

fn format(terminal: Terminal, vt: bool) -> Vec<u8> {
    let mut out = Vec::new();
    unsafe {
        let mut formatter = std::ptr::null_mut();
        assert_eq!(
            ghostty_formatter_terminal_new(std::ptr::null(), &mut formatter, terminal),
            0
        );
        if vt {
            let (emit, extras) = (FORMAT_VT, EXTRAS_ALL);
            assert_eq!(
                ghostty_formatter_set(formatter, FORMATTER_OPT_EMIT, (&raw const emit).cast()),
                0
            );
            assert_eq!(
                ghostty_formatter_set(formatter, FORMATTER_OPT_EXTRAS, (&raw const extras).cast()),
                0
            );
        }
        let writer = Writer {
            write: collect,
            userdata: (&raw mut out).cast(),
        };
        assert_eq!(ghostty_formatter_format(formatter, writer), 0);
        ghostty_formatter_free(formatter);
    }
    out
}

fn format_vt(terminal: Terminal) -> Vec<u8> {
    format(terminal, true)
}

fn format_plain(terminal: Terminal) -> Vec<u8> {
    format(terminal, false)
}

fn total_rows(terminal: Terminal) -> usize {
    let mut rows = 0usize;
    assert_eq!(
        unsafe { ghostty_terminal_get(terminal, DATA_TOTAL_ROWS, (&raw mut rows).cast()) },
        0
    );
    rows
}

/// The OSC 133 state of every row, top of history first.
fn prompt_rows(terminal: Terminal) -> Vec<i32> {
    (0..total_rows(terminal))
        .map(|y| unsafe {
            let point = Point {
                tag: POINT_TAG_SCREEN,
                value: PointValue {
                    coordinate: Coordinate { x: 0, y: y as u32 },
                },
            };
            let mut grid_ref = GridRef {
                node: std::ptr::null_mut(),
                x: 0,
                y: 0,
            };
            assert_eq!(ghostty_terminal_grid_ref(terminal, point, &mut grid_ref), 0);
            let mut row = 0u64;
            assert_eq!(ghostty_grid_ref_row(&grid_ref, &mut row), 0);
            let mut prompt = 0i32;
            assert_eq!(
                ghostty_row_get(row, ROW_DATA_SEMANTIC_PROMPT, (&raw mut prompt).cast()),
                0
            );
            prompt
        })
        .collect()
}

/// Decode a snapshot file and compare it with a terminal rebuilt from the raw
/// bytes that produced it.
fn verify(path: &str) {
    let snapshot = std::fs::read(path).expect("read snapshot");
    let raw = std::fs::read(format!("{path}.raw")).expect("read raw");

    let mut original = Headless::new(120, 30);
    for chunk in raw.chunks(4096) {
        original.feed(chunk);
    }

    let start = Instant::now();
    let mut decoder = std::ptr::null_mut();
    let mut decoded: Terminal = std::ptr::null_mut();
    unsafe {
        assert_eq!(
            ghostty_snapshot_decoder_new_buf(
                std::ptr::null(),
                &mut decoder,
                snapshot.as_ptr(),
                snapshot.len()
            ),
            0
        );
        // Keep unfinished parser state so the decoded terminal can resume
        // mid-sequence and be encoded again.
        let retain = true;
        assert_eq!(
            ghostty_snapshot_decoder_set(
                decoder,
                DECODER_OPT_RETAIN_CONTINUATION,
                (&raw const retain).cast()
            ),
            0
        );
        let result = ghostty_snapshot_decoder_decode(decoder, &mut decoded);
        assert_eq!(result, 0, "decode failed");
        ghostty_snapshot_decoder_free(decoder);
    }
    println!("decoded        {} bytes in {:?}", snapshot.len(), start.elapsed());

    let (a, b) = (total_rows(original.terminal), total_rows(decoded));
    println!("total rows     original {a}, decoded {b}");

    let reencoded = encode(decoded);
    println!(
        "re-encode      {} bytes, identical to the snapshot: {}",
        reencoded.len(),
        reencoded == snapshot
    );

    if a <= 50_000 {
        let (pa, pb) = (prompt_rows(original.terminal), prompt_rows(decoded));
        let marked = |rows: &[i32]| rows.iter().filter(|p| **p != 0).count();
        println!(
            "prompt rows    original {}, decoded {}, per-row match: {}",
            marked(&pa),
            marked(&pb),
            pa == pb
        );
    }

    // Resume both mid-sequence: the snapshot ended inside `ESC [ 3`.
    let rest = b"1mresumed\x1b[0m\r\n";
    original.feed(rest);
    unsafe { ghostty_terminal_vt_write(decoded, rest.as_ptr(), rest.len()) };
    // Snapshot bytes also reflect how rows are packed into pages, which may
    // differ after a decode, so compare content: text, styles and modes as
    // the VT formatter writes them, over the whole history.
    let (va, vb) = (format_vt(original.terminal), format_vt(decoded));
    println!(
        "after resuming: {} bytes of styled text, identical: {}, snapshots identical: {}",
        va.len(),
        va == vb,
        encode(original.terminal) == encode(decoded)
    );
    println!(
        "  last line: {:?}",
        String::from_utf8_lossy(&format_plain(decoded))
            .lines()
            .last()
            .unwrap_or_default()
    );
    unsafe { ghostty_terminal_free(decoded) };
}

extern "C" fn collect(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
    let out = unsafe { &mut *userdata.cast::<Vec<u8>>() };
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    true
}

/// Replies the terminal wants written to the pty (query answers).
#[derive(Default)]
struct Replies {
    count: usize,
    bytes: Vec<u8>,
}

extern "C" fn write_pty(_: Terminal, userdata: *mut c_void, data: *const u8, len: usize) {
    let replies = unsafe { &mut *userdata.cast::<Replies>() };
    replies.count += 1;
    replies
        .bytes
        .extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
}

struct Headless {
    terminal: Terminal,
    replies: Box<Replies>,
}

impl Headless {
    fn new(cols: u16, rows: u16) -> Self {
        let mut terminal: Terminal = std::ptr::null_mut();
        let result = unsafe { ghostty_terminal_new(std::ptr::null(), &mut terminal, cols, rows) };
        assert_eq!(result, 0, "ghostty_terminal_new failed");
        let mut replies = Box::<Replies>::default();
        let continuation: usize = 1 << 20;
        let pull = false;
        // The library's default history limit is small; a daemon sets its own.
        let scrollback_bytes: usize = 1 << 30;
        let scrollback_lines: usize = 10_000_000;
        unsafe {
            let userdata: *mut Replies = &mut *replies;
            assert_eq!(ghostty_terminal_set(terminal, OPT_USERDATA, userdata.cast()), 0);
            assert_eq!(
                ghostty_terminal_set(terminal, OPT_WRITE_PTY, write_pty as *const c_void),
                0
            );
            // Lets a snapshot taken mid-sequence carry the unfinished bytes.
            assert_eq!(
                ghostty_terminal_set(
                    terminal,
                    OPT_CONTINUATION_MAX_BYTES,
                    (&raw const continuation).cast()
                ),
                0
            );
            assert_eq!(
                ghostty_terminal_set(
                    terminal,
                    OPT_SCROLLBACK_MAX_BYTES,
                    (&raw const scrollback_bytes).cast()
                ),
                0
            );
            assert_eq!(
                ghostty_terminal_set(
                    terminal,
                    OPT_SCROLLBACK_MAX_LINES,
                    (&raw const scrollback_lines).cast()
                ),
                0
            );
            // What the fork's Exec.initTerminal does on Windows.
            assert_eq!(
                ghostty_terminal_set(terminal, OPT_RESIZE_PULL_SCROLLBACK, (&raw const pull).cast()),
                0
            );
        }
        Self { terminal, replies }
    }

    fn feed(&mut self, bytes: &[u8]) {
        unsafe { ghostty_terminal_vt_write(self.terminal, bytes.as_ptr(), bytes.len()) };
    }

    fn snapshot(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let writer = Writer {
            write: collect,
            userdata: (&raw mut out).cast(),
        };
        let result = unsafe { ghostty_snapshot_encode(self.terminal, writer) };
        assert_eq!(result, 0, "ghostty_snapshot_encode failed");
        out
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        unsafe { ghostty_terminal_free(self.terminal) };
    }
}

/// Output shaped like a shell session: OSC 133 prompt marks, colors, wide
/// characters, a long history, and a device attributes query.
fn synthetic(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]0;spike title\x07\x1b]7;file://host/C:/src\x07");
    for i in 0..lines {
        out.extend_from_slice(b"\x1b]133;A\x07PS> \x1b]133;B\x07");
        out.extend_from_slice(format!("echo line {i}\r\n\x1b]133;C\x07").as_bytes());
        out.extend_from_slice(
            format!("\x1b[3{}mline {i}\x1b[0m \u{754c} e\u{301} tail\r\n", i % 8).as_bytes(),
        );
        out.extend_from_slice(b"\x1b]133;D;0\x07");
    }
    out.extend_from_slice(b"\x1b[c\x1b[6n");
    out.extend_from_slice(b"\x1b]133;A\x07PS> \x1b]133;B\x07unfinished \x1b[3");
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: daemon-spike synth <out.bin> [lines] | conpty <out.bin> <command...>";
    let (mode, out_path) = match (args.get(1), args.get(2)) {
        (Some(mode), Some(path)) => (mode.as_str(), path.clone()),
        _ => panic!("{usage}"),
    };
    if mode == "verify" {
        return verify(&out_path);
    }
    #[cfg(windows)]
    if mode == "serve" {
        return serve::run(&args[2..].join(" "), 120, 30);
    }
    let (cols, rows) = (120u16, 30u16);
    let mut headless = Headless::new(cols, rows);

    let raw = match mode {
        "synth" => {
            let lines = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(5000);
            synthetic(lines)
        }
        #[cfg(windows)]
        "conpty" => conpty::run(&args[3..].join(" "), cols, rows),
        _ => panic!("{usage}"),
    };

    let start = Instant::now();
    // Feed in pty-sized chunks, as a reader thread would.
    for chunk in raw.chunks(4096) {
        headless.feed(chunk);
    }
    let parse = start.elapsed();

    let start = Instant::now();
    let snapshot = headless.snapshot();
    let encode = start.elapsed();

    std::fs::write(&out_path, &snapshot).expect("write snapshot");
    std::fs::write(format!("{out_path}.raw"), &raw).expect("write raw");
    println!("fed            {} bytes in {parse:?}", raw.len());
    println!("snapshot       {} bytes in {encode:?}", snapshot.len());
    println!(
        "pty replies    {} ({} bytes): {:?}",
        headless.replies.count,
        headless.replies.bytes.len(),
        String::from_utf8_lossy(&headless.replies.bytes)
    );
}

#[cfg(windows)]
mod conpty {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::Console::{
        COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
        InitializeProcThreadAttributeList, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;

    /// Run `command` under a system ConPTY and return everything it printed.
    pub fn run(command: &str, cols: u16, rows: u16) -> Vec<u8> {
        unsafe {
            let (mut in_read, mut in_write): (HANDLE, HANDLE) = (null_mut(), null_mut());
            let (mut out_read, mut out_write): (HANDLE, HANDLE) = (null_mut(), null_mut());
            assert!(CreatePipe(&mut in_read, &mut in_write, null(), 0) != 0);
            assert!(CreatePipe(&mut out_read, &mut out_write, null(), 0) != 0);

            let mut console: HPCON = 0;
            let size = COORD {
                X: cols as i16,
                Y: rows as i16,
            };
            assert!(CreatePseudoConsole(size, in_read, out_write, 0, &mut console) >= 0);
            CloseHandle(in_read);
            CloseHandle(out_write);

            let mut bytes = 0usize;
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes);
            let mut list = vec![0u8; bytes];
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
            // With our own stdio redirected, the child would otherwise use
            // those handles instead of the pseudoconsole.
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

            // Drain on a thread so the child never blocks on a full pipe.
            let read_handle = out_read as usize;
            let reader = std::thread::spawn(move || {
                let mut all = Vec::new();
                let mut buf = [0u8; 65536];
                loop {
                    let mut n = 0u32;
                    let ok = ReadFile(
                        read_handle as HANDLE,
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        &mut n,
                        null_mut(),
                    );
                    if ok == 0 || n == 0 {
                        break;
                    }
                    all.extend_from_slice(&buf[..n as usize]);
                }
                all
            });

            WaitForSingleObject(process.hProcess, INFINITE);
            // Closing the console flushes its output and ends the read.
            ClosePseudoConsole(console);
            let all = reader.join().unwrap();

            DeleteProcThreadAttributeList(attributes);
            CloseHandle(process.hProcess);
            CloseHandle(process.hThread);
            CloseHandle(in_write);
            CloseHandle(out_read);
            all
        }
    }
}

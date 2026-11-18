//! A headless ghostty terminal: libghostty-vt behind a safe face.
//!
//! This is the daemon's copy of a pane. It parses everything the shell
//! writes, holds the history, and is the terminal the program talks to: every
//! reply to a query comes from here, whether or not a UI is attached.

use nterm_session::protocol::{CursorStyle, Identity, Size, TerminalSettings};
use std::ffi::c_void;

#[repr(C)]
struct Opaque(c_void);
type Raw = *mut Opaque;

#[repr(C)]
#[derive(Clone, Copy)]
struct Writer {
    write: extern "C" fn(*mut c_void, *const u8, usize) -> bool,
    userdata: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SizeReport {
    rows: u16,
    columns: u16,
    cell_width: u32,
    cell_height: u32,
}

#[repr(C)]
struct ModeConfig {
    mode: u16,
    value: bool,
}

/// Borrowed bytes the library reads before the callback's caller returns.
#[repr(C)]
struct Text {
    ptr: *const u8,
    len: usize,
}

#[repr(C)]
struct DeviceAttributes {
    conformance: u16,
    features: [u16; 64],
    features_len: usize,
    secondary: [u16; 3],
    unit_id: u32,
}

const OPT_USERDATA: i32 = 1;
const OPT_WRITE_PTY: i32 = 2;
const OPT_ENQUIRY: i32 = 4;
const OPT_XTVERSION: i32 = 5;
const OPT_SIZE: i32 = 7;
const OPT_COLOR_SCHEME: i32 = 8;
const OPT_DEVICE_ATTRIBUTES: i32 = 9;
const OPT_COLOR_FOREGROUND: i32 = 12;
const OPT_COLOR_BACKGROUND: i32 = 13;
const OPT_COLOR_CURSOR: i32 = 14;
const OPT_COLOR_PALETTE: i32 = 15;
const OPT_KITTY_IMAGE_STORAGE_LIMIT: i32 = 16;
const OPT_DEFAULT_CURSOR_STYLE: i32 = 23;
const OPT_DEFAULT_CURSOR_BLINK: i32 = 24;
const OPT_SCROLLBACK_MAX_BYTES: i32 = 28;
const OPT_SCROLLBACK_MAX_LINES: i32 = 29;
const OPT_CONTINUATION_MAX_BYTES: i32 = 32;
const OPT_MODE_DEFAULT: i32 = 34;
const OPT_RESIZE_PULL_SCROLLBACK: i32 = 41;

const DATA_COLS: i32 = 1;
const DATA_ROWS: i32 = 2;
const DATA_CURSOR_X: i32 = 3;
const DATA_CURSOR_Y: i32 = 4;
const DATA_TITLE: i32 = 12;
const DATA_PWD: i32 = 13;
const DATA_TOTAL_ROWS: i32 = 14;

const DECODER_OPT_MAX_CONTINUATION_BYTES: i32 = 1;
const DECODER_OPT_RETAIN_CONTINUATION: i32 = 2;

/// DEC private mode 2027; DEC modes carry no flag bit.
const MODE_GRAPHEME_CLUSTER: u16 = 2027;

/// Unfinished escape sequences up to this size travel in a snapshot.
const CONTINUATION_MAX: usize = 1 << 20;

const ENVELOPE_LEN: usize = 10;
const RECORD_HEADER_LEN: usize = 10;
const RECORD_READY: u16 = 5;
const RECORD_FINISH: u16 = 6;
const RECORD_KITTY_IMAGE_PAYLOAD: u16 = 9;

unsafe extern "C" {
    fn ghostty_terminal_new(
        allocator: *const c_void,
        terminal: *mut Raw,
        cols: u16,
        rows: u16,
    ) -> i32;
    fn ghostty_terminal_free(terminal: Raw);
    fn ghostty_terminal_set(terminal: Raw, option: i32, value: *const c_void) -> i32;
    fn ghostty_terminal_get(terminal: Raw, data: i32, out: *mut c_void) -> i32;
    fn ghostty_terminal_vt_write(terminal: Raw, data: *const u8, len: usize);
    fn ghostty_terminal_resize(
        terminal: Raw,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> i32;
    fn ghostty_snapshot_encode(terminal: Raw, writer: Writer) -> i32;
    fn ghostty_snapshot_decoder_new_buf(
        allocator: *const c_void,
        decoder: *mut *mut c_void,
        ptr: *const u8,
        len: usize,
    ) -> i32;
    fn ghostty_snapshot_decoder_set(decoder: *mut c_void, option: i32, value: *const c_void)
    -> i32;
    fn ghostty_snapshot_decoder_ready(decoder: *mut c_void, terminal: *mut Raw) -> i32;
    fn ghostty_snapshot_decoder_decode(decoder: *mut c_void, terminal: *mut Raw) -> i32;
    fn ghostty_snapshot_decoder_free(decoder: *mut c_void);
    fn ghostty_formatter_terminal_new(
        allocator: *const c_void,
        formatter: *mut *mut c_void,
        terminal: Raw,
    ) -> i32;
    fn ghostty_formatter_format(formatter: *mut c_void, writer: Writer) -> i32;
    fn ghostty_formatter_free(formatter: *mut c_void);
    fn ghostty_sys_set(option: i32, value: *const c_void) -> i32;
    fn ghostty_alloc(allocator: *const c_void, len: usize) -> *mut u8;
    fn ghostty_free(allocator: *const c_void, ptr: *mut u8, len: usize);
}

const SYS_OPT_DECODE_PNG: i32 = 2;

/// GhosttySysImage: 8-bit RGBA, row by row from the top left.
#[repr(C)]
struct Image {
    width: u32,
    height: u32,
    data: *mut u8,
    data_len: usize,
}

/// Give the library what it leaves to its host, before it is first used:
/// it decodes no PNG itself, and refuses the pictures a program sends as
/// one. The UI's terminal takes them, so without this a picture would be on
/// the UI's screen and not on this one, and would go when the UI is sent
/// this screen, as it is after a resize.
fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // Whoever is first sets it while the others wait: the library is not
    // to be given it while a thread is using it.
    ONCE.call_once(|| unsafe {
        ghostty_sys_set(SYS_OPT_DECODE_PNG, decode_png as *const c_void);
    });
}

extern "C" fn decode_png(
    _: *mut c_void,
    allocator: *const c_void,
    data: *const u8,
    len: usize,
    out: *mut Image,
) -> bool {
    let png = unsafe { std::slice::from_raw_parts(data, len) };
    // A PNG is whatever a program wrote. One that makes the decoder panic
    // is refused; it does not end the daemon and every shell with it.
    match std::panic::catch_unwind(|| pixels(png, allocator)) {
        Ok(Some(image)) => {
            unsafe { *out = image };
            true
        }
        _ => false,
    }
}

/// `png` as the pixels the library keeps, in memory of its `allocator`.
fn pixels(png: &[u8], allocator: *const c_void) -> Option<Image> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    // Eight bits a channel, with a palette or a transparent colour made
    // into channels.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    let count = (width as usize).checked_mul(height as usize)?;
    let len = count.checked_mul(4)?;
    // Before anything is decoded: the allocator is what limits how much one
    // picture may take, and it refuses one that is too large.
    let data = unsafe { ghostty_alloc(allocator, len) };
    if data.is_null() {
        return None;
    }
    let filled = (|| {
        let size = reader.output_buffer_size()?;
        let mut decoded = Vec::new();
        decoded.try_reserve_exact(size).ok()?;
        decoded.resize(size, 0);
        let frame = reader.next_frame(&mut decoded).ok()?;
        let channels = match frame.color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return None,
        };
        let decoded = decoded.get(..count.checked_mul(channels)?)?;
        for (index, pixel) in decoded.chunks_exact(channels).enumerate() {
            let rgba = match *pixel {
                [grey] => [grey, grey, grey, 255],
                [grey, alpha] => [grey, grey, grey, alpha],
                [red, green, blue] => [red, green, blue, 255],
                [red, green, blue, alpha] => [red, green, blue, alpha],
                _ => return None,
            };
            // The memory is not yet anything, so it is written, not read.
            unsafe { std::ptr::copy_nonoverlapping(rgba.as_ptr(), data.add(index * 4), 4) };
        }
        Some(())
    })();
    if filled.is_none() {
        unsafe { ghostty_free(allocator, data, len) };
        return None;
    }
    Some(Image { width, height, data, data_len: len })
}

/// What the library's callbacks read and write. Boxed so its address holds
/// while the terminal moves.
#[derive(Default)]
struct Hooks {
    /// Bytes the terminal wants written to the shell.
    replies: Vec<u8>,
    size: SizeReport,
    dark: Option<bool>,
    /// Unset, the library answers as itself.
    identity: Option<Identity>,
}

extern "C" fn report_xtversion(_: Raw, userdata: *mut c_void) -> Text {
    let hooks = unsafe { &*userdata.cast::<Hooks>() };
    // An empty answer makes the library use its own name.
    borrow(hooks.identity.as_ref().map_or("", |identity| &identity.xtversion))
}

extern "C" fn report_enquiry(_: Raw, userdata: *mut c_void) -> Text {
    let hooks = unsafe { &*userdata.cast::<Hooks>() };
    borrow(hooks.identity.as_ref().map_or("", |identity| &identity.enquiry))
}

fn borrow(text: &str) -> Text {
    Text { ptr: text.as_ptr(), len: text.len() }
}

extern "C" fn report_device_attributes(
    _: Raw,
    userdata: *mut c_void,
    out: *mut DeviceAttributes,
) -> bool {
    let hooks = unsafe { &*userdata.cast::<Hooks>() };
    let Some(identity) = &hooks.identity else {
        return false;
    };
    let out = unsafe { &mut *out };
    out.conformance = identity.conformance;
    let count = identity.features.len().min(out.features.len());
    out.features[..count].copy_from_slice(&identity.features[..count]);
    out.features_len = count;
    out.secondary = identity.secondary;
    out.unit_id = 0;
    true
}

extern "C" fn write_pty(_: Raw, userdata: *mut c_void, data: *const u8, len: usize) {
    let hooks = unsafe { &mut *userdata.cast::<Hooks>() };
    hooks.replies.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
}

extern "C" fn report_size(_: Raw, userdata: *mut c_void, out: *mut SizeReport) -> bool {
    let hooks = unsafe { &*userdata.cast::<Hooks>() };
    unsafe { *out = hooks.size };
    true
}

extern "C" fn report_color_scheme(_: Raw, userdata: *mut c_void, out: *mut i32) -> bool {
    let hooks = unsafe { &*userdata.cast::<Hooks>() };
    match hooks.dark {
        Some(dark) => {
            unsafe { *out = dark as i32 };
            true
        }
        None => false,
    }
}

extern "C" fn collect(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
    let out = unsafe { &mut *userdata.cast::<Vec<u8>>() };
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    true
}

/// Collects a snapshot up to the end of its READY record, and with
/// `pictures` the pixels of the pictures on screen, which come right after
/// it. Then it refuses more, which stops the encoder before it walks the
/// history.
struct ScreenOnly {
    out: Vec<u8>,
    /// Where the next record header starts.
    next: usize,
    pictures: bool,
    /// READY has been collected; what follows is pixels or the end.
    ready: bool,
    done: bool,
}

extern "C" fn collect_screen(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
    let state = unsafe { &mut *userdata.cast::<ScreenOnly>() };
    if state.done {
        return false;
    }
    state.out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    while state.out.len() >= state.next + RECORD_HEADER_LEN {
        let header = &state.out[state.next..state.next + RECORD_HEADER_LEN];
        let tag = u16::from_le_bytes([header[0], header[1]]);
        let payload = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;
        let end = state.next + RECORD_HEADER_LEN + payload;
        if state.ready && tag != RECORD_KITTY_IMAGE_PAYLOAD {
            // The history starts here.
            state.out.truncate(state.next);
            state.done = true;
            return false;
        }
        if tag != RECORD_READY {
            state.next = end;
        } else if state.out.len() < end {
            break;
        } else if state.pictures {
            state.ready = true;
            state.next = end;
        } else {
            state.out.truncate(end);
            state.done = true;
            return false;
        }
    }
    true
}

pub struct Terminal {
    raw: Raw,
    hooks: Box<Hooks>,
}

// The library keeps no thread affinity; the owner serializes access.
unsafe impl Send for Terminal {}

impl Terminal {
    /// A terminal with `settings` applied, ready for a shell's output.
    pub fn new(size: Size, settings: &TerminalSettings) -> Result<Self, String> {
        init();
        let mut raw: Raw = std::ptr::null_mut();
        let result =
            unsafe { ghostty_terminal_new(std::ptr::null(), &mut raw, size.cols, size.rows) };
        if result != 0 || raw.is_null() {
            return Err(format!("ghostty_terminal_new failed ({result})"));
        }
        let mut terminal = Self::adopt(raw);
        terminal.set(OPT_CONTINUATION_MAX_BYTES, &CONTINUATION_MAX);
        // What the UI's terminal does on Windows: ConPTY does not expect
        // history to come back when the screen grows.
        if cfg!(windows) {
            terminal.set(OPT_RESIZE_PULL_SCROLLBACK, &false);
        }
        terminal.hooks.size = report(size);
        // Cell size is not part of creation.
        if size.cell_width != 0 || size.cell_height != 0 {
            unsafe {
                ghostty_terminal_resize(
                    raw,
                    size.cols,
                    size.rows,
                    size.cell_width,
                    size.cell_height,
                );
            }
        }
        let refused = terminal.apply(settings);
        if !refused.is_empty() {
            return Err(format!("settings refused: {}", refused.join(", ")));
        }
        Ok(terminal)
    }

    /// Take ownership of a library terminal and point its callbacks here.
    fn adopt(raw: Raw) -> Self {
        let mut hooks = Box::<Hooks>::default();
        let userdata: *mut Hooks = &mut *hooks;
        unsafe {
            ghostty_terminal_set(raw, OPT_USERDATA, userdata.cast());
            ghostty_terminal_set(raw, OPT_WRITE_PTY, write_pty as *const c_void);
            ghostty_terminal_set(raw, OPT_SIZE, report_size as *const c_void);
            ghostty_terminal_set(raw, OPT_COLOR_SCHEME, report_color_scheme as *const c_void);
            ghostty_terminal_set(raw, OPT_XTVERSION, report_xtversion as *const c_void);
            ghostty_terminal_set(raw, OPT_ENQUIRY, report_enquiry as *const c_void);
            ghostty_terminal_set(
                raw,
                OPT_DEVICE_ATTRIBUTES,
                report_device_attributes as *const c_void,
            );
        }
        let mut terminal = Self { raw, hooks };
        let (cols, rows) = terminal.grid();
        terminal.hooks.size.columns = cols;
        terminal.hooks.size.rows = rows;
        terminal
    }

    fn set<T>(&mut self, option: i32, value: &T) -> bool {
        unsafe { ghostty_terminal_set(self.raw, option, std::ptr::from_ref(value).cast()) == 0 }
    }

    /// Apply the UI's settings. Returns the names of any the library refused.
    /// A setting the UI left out is not touched.
    pub fn apply(&mut self, settings: &TerminalSettings) -> Vec<&'static str> {
        let mut refused = Vec::new();
        let mut check = |ok: bool, name: &'static str| {
            if !ok {
                refused.push(name);
            }
        };
        if let Some(bytes) = settings.scrollback_bytes {
            let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
            check(self.set(OPT_SCROLLBACK_MAX_BYTES, &bytes), "scrollback_bytes");
        }
        if let Some(lines) = settings.scrollback_lines {
            let lines = usize::try_from(lines).unwrap_or(usize::MAX);
            check(self.set(OPT_SCROLLBACK_MAX_LINES, &lines), "scrollback_lines");
        }
        if let Some(color) = settings.foreground {
            check(self.set(OPT_COLOR_FOREGROUND, &color), "foreground");
        }
        if let Some(color) = settings.background {
            check(self.set(OPT_COLOR_BACKGROUND, &color), "background");
        }
        if let Some(color) = settings.cursor {
            check(self.set(OPT_COLOR_CURSOR, &color), "cursor");
        }
        if !settings.palette.is_empty() {
            match <&[[u8; 3]; 256]>::try_from(settings.palette.as_slice()) {
                Ok(palette) => check(self.set(OPT_COLOR_PALETTE, palette), "palette"),
                Err(_) => check(false, "palette"),
            }
        }
        if let Some(style) = settings.cursor_style {
            let style: i32 = match style {
                CursorStyle::Bar => 0,
                CursorStyle::Block => 1,
                CursorStyle::Underline => 2,
                CursorStyle::BlockHollow => 3,
            };
            check(self.set(OPT_DEFAULT_CURSOR_STYLE, &style), "cursor_style");
        }
        if let Some(blink) = settings.cursor_blink {
            check(self.set(OPT_DEFAULT_CURSOR_BLINK, &blink), "cursor_blink");
        }
        if let Some(value) = settings.grapheme_cluster {
            let mode = ModeConfig { mode: MODE_GRAPHEME_CLUSTER, value };
            check(self.set(OPT_MODE_DEFAULT, &mode), "grapheme_cluster");
        }
        if let Some(limit) = settings.image_storage_limit {
            check(self.set(OPT_KITTY_IMAGE_STORAGE_LIMIT, &limit), "image_storage_limit");
        }
        if settings.dark.is_some() {
            self.hooks.dark = settings.dark;
        }
        if settings.identity.is_some() {
            self.hooks.identity.clone_from(&settings.identity);
        }
        refused
    }

    /// Parse output from the shell. Returns what the terminal answers.
    #[must_use]
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        unsafe { ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) };
        std::mem::take(&mut self.hooks.replies)
    }

    /// Reflow to a new size. Returns any report the terminal sends for it.
    #[must_use]
    pub fn resize(&mut self, size: Size) -> Vec<u8> {
        self.hooks.size = report(size);
        unsafe {
            ghostty_terminal_resize(
                self.raw,
                size.cols,
                size.rows,
                size.cell_width,
                size.cell_height,
            );
        }
        std::mem::take(&mut self.hooks.replies)
    }

    /// The whole terminal, history included.
    pub fn snapshot(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let writer = Writer { write: collect, userdata: (&raw mut out).cast() };
        let result = unsafe { ghostty_snapshot_encode(self.raw, writer) };
        assert_eq!(result, 0, "ghostty_snapshot_encode failed");
        out
    }

    /// The visible screen, modes and any unfinished escape sequence: a
    /// snapshot cut before its history. It carries the page the screen
    /// sits on, so some recent history comes along, but the cost does not
    /// grow with the history. With `pictures` it has the pixels of the
    /// pictures on screen too; without, only where each one is.
    pub fn screen(&mut self, pictures: bool) -> Vec<u8> {
        let mut state = ScreenOnly {
            out: Vec::new(),
            next: ENVELOPE_LEN,
            pictures,
            ready: false,
            done: false,
        };
        let writer = Writer { write: collect_screen, userdata: (&raw mut state).cast() };
        // Refusing bytes after READY makes the encoder report a failed write.
        let result = unsafe { ghostty_snapshot_encode(self.raw, writer) };
        assert!(state.done, "snapshot had no READY record (encode result {result})");
        state.out
    }

    /// Rebuild a terminal from `snapshot` or `screen` bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        init();
        let mut decoder = std::ptr::null_mut();
        let mut raw: Raw = std::ptr::null_mut();
        let result = unsafe {
            let result = ghostty_snapshot_decoder_new_buf(
                std::ptr::null(),
                &mut decoder,
                bytes.as_ptr(),
                bytes.len(),
            );
            if result != 0 {
                return Err(format!("snapshot decoder failed to start ({result})"));
            }
            let (retain, continuation) = (true, CONTINUATION_MAX);
            ghostty_snapshot_decoder_set(
                decoder,
                DECODER_OPT_MAX_CONTINUATION_BYTES,
                (&raw const continuation).cast(),
            );
            ghostty_snapshot_decoder_set(
                decoder,
                DECODER_OPT_RETAIN_CONTINUATION,
                (&raw const retain).cast(),
            );
            // A screen stops at READY; a whole snapshot goes on to FINISH.
            let result = if is_screen_only(bytes) {
                ghostty_snapshot_decoder_ready(decoder, &mut raw)
            } else {
                ghostty_snapshot_decoder_decode(decoder, &mut raw)
            };
            ghostty_snapshot_decoder_free(decoder);
            result
        };
        if result != 0 || raw.is_null() {
            return Err(format!("snapshot decode failed ({result})"));
        }
        let mut terminal = Self::adopt(raw);
        // A snapshot does not carry this; see `new`.
        if cfg!(windows) {
            terminal.set(OPT_RESIZE_PULL_SCROLLBACK, &false);
        }
        Ok(terminal)
    }

    /// The text of the screen and history, one line per row.
    pub fn text(&mut self) -> String {
        let mut out = Vec::new();
        unsafe {
            let mut formatter = std::ptr::null_mut();
            let result =
                ghostty_formatter_terminal_new(std::ptr::null(), &mut formatter, self.raw);
            assert_eq!(result, 0, "ghostty_formatter_terminal_new failed");
            let writer = Writer { write: collect, userdata: (&raw mut out).cast() };
            ghostty_formatter_format(formatter, writer);
            ghostty_formatter_free(formatter);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Columns and rows.
    pub fn grid(&mut self) -> (u16, u16) {
        let (mut cols, mut rows) = (0u16, 0u16);
        unsafe {
            ghostty_terminal_get(self.raw, DATA_COLS, (&raw mut cols).cast());
            ghostty_terminal_get(self.raw, DATA_ROWS, (&raw mut rows).cast());
        }
        (cols, rows)
    }

    /// The title the shell last set, if it has set one.
    pub fn title(&mut self) -> Option<String> {
        self.string(DATA_TITLE)
    }

    /// The directory the shell last reported, as it reported it.
    pub fn pwd(&mut self) -> Option<String> {
        self.string(DATA_PWD)
    }

    fn string(&mut self, data: i32) -> Option<String> {
        let mut text = Text { ptr: std::ptr::null(), len: 0 };
        let ok = unsafe { ghostty_terminal_get(self.raw, data, (&raw mut text).cast()) } == 0;
        if !ok || text.ptr.is_null() || text.len == 0 {
            return None;
        }
        // Borrowed until the terminal next changes: copied before it can.
        let bytes = unsafe { std::slice::from_raw_parts(text.ptr, text.len) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Rows held, history included.
    pub fn total_rows(&mut self) -> usize {
        let mut rows = 0usize;
        unsafe { ghostty_terminal_get(self.raw, DATA_TOTAL_ROWS, (&raw mut rows).cast()) };
        rows
    }

    /// Where the shell's cursor is: its line, the part of the line before
    /// it, its column, and the last lines printed that are not blank, the
    /// newest last. It is what a picker that is a function is told of the
    /// pane it opens over. A row is a line here: one that wrapped is given
    /// as the row the cursor is on, and a wide character counts as one
    /// column.
    pub fn at_cursor(&mut self) -> AtCursor {
        const RECENT: usize = 10;
        const SEEN: usize = 256;
        let (mut column, mut row) = (0u16, 0u16);
        unsafe {
            ghostty_terminal_get(self.raw, DATA_CURSOR_X, (&raw mut column).cast());
            ghostty_terminal_get(self.raw, DATA_CURSOR_Y, (&raw mut row).cast());
        }
        let (_, rows) = self.grid();
        // The cursor's row, counted from the first row of the history.
        let at = self.total_rows().saturating_sub(rows as usize) + row as usize;
        let text = self.text();
        let lines: Vec<&str> = text.lines().collect();
        let line = lines.get(at).copied().unwrap_or_default().trim_end().to_owned();
        let before: String = line.chars().take(column as usize).collect();
        let seen = &lines[lines.len().saturating_sub(SEEN)..];
        let mut recent: Vec<String> = seen
            .iter()
            .rev()
            .map(|line| line.trim_end())
            .filter(|line| !line.trim().is_empty())
            .take(RECENT)
            .map(str::to_owned)
            .collect();
        recent.reverse();
        AtCursor { line, before, column, recent }
    }
}

/// What is at a shell's cursor: see `Terminal::at_cursor`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AtCursor {
    pub line: String,
    pub before: String,
    pub column: u16,
    pub recent: Vec<String>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { ghostty_terminal_free(self.raw) };
    }
}

fn report(size: Size) -> SizeReport {
    SizeReport {
        rows: size.rows,
        columns: size.cols,
        cell_width: size.cell_width,
        cell_height: size.cell_height,
    }
}

/// Whether a snapshot was cut before its history: it has no FINISH record.
fn is_screen_only(bytes: &[u8]) -> bool {
    let mut next = ENVELOPE_LEN;
    let mut last = 0u16;
    while bytes.len() >= next + RECORD_HEADER_LEN {
        last = u16::from_le_bytes([bytes[next], bytes[next + 1]]);
        let payload =
            u32::from_le_bytes([bytes[next + 2], bytes[next + 3], bytes[next + 4], bytes[next + 5]]);
        next += RECORD_HEADER_LEN + payload as usize;
    }
    last != RECORD_FINISH
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> Size {
        Size { cols, rows, cell_width: 9, cell_height: 18 }
    }

    fn terminal() -> Terminal {
        Terminal::new(size(80, 24), &TerminalSettings::default()).unwrap()
    }

    #[test]
    fn what_is_at_the_cursor_is_its_line_and_the_last_lines_printed() {
        let mut terminal = terminal();
        // Enough to push some lines into the history.
        for number in 0..40 {
            terminal.feed(format!("line {number}\r\n\r\n").as_bytes());
        }
        terminal.feed(b"PS C:/src> git switch main");
        // The cursor goes back over `main`.
        terminal.feed(b"\x1b[4D");
        let at = terminal.at_cursor();
        assert_eq!(at.line, "PS C:/src> git switch main");
        assert_eq!(at.before, "PS C:/src> git switch ");
        assert_eq!(at.column, 22);
        assert_eq!(at.recent.len(), 10);
        assert_eq!(at.recent[9], "PS C:/src> git switch main");
        assert_eq!(at.recent[8], "line 39");
        assert_eq!(at.recent[0], "line 31");
        // A terminal with nothing printed.
        assert_eq!(super::Terminal::new(size(80, 24), &TerminalSettings::default()).unwrap().at_cursor(), AtCursor::default());
    }

    /// A PNG of noise, which nothing packs small, and its pixel count.
    fn noise_png(width: u32, height: u32) -> Vec<u8> {
        let mut seed = 12345u32;
        let pixels: Vec<u8> = (0..width * height * 3)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels).unwrap();
        writer.finish().unwrap();
        out
    }

    fn base64(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let bits = chunk.iter().fold(0u32, |bits, byte| bits << 8 | *byte as u32)
                << (8 * (3 - chunk.len()));
            for digit in 0..4 {
                out.push(if digit <= chunk.len() {
                    DIGITS[(bits >> (18 - 6 * digit) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    }

    /// The kitty graphics command that shows a PNG as image `id`.
    fn show(id: u32, png: &[u8]) -> Vec<u8> {
        format!("\x1b_Ga=T,f=100,i={id};{}\x1b\\", base64(png)).into_bytes()
    }

    #[test]
    fn a_png_a_program_shows_is_kept_and_what_is_not_one_is_refused() {
        let mut terminal = terminal();
        let plain = terminal.screen(false).len();
        let reply = terminal.feed(&show(7, &noise_png(64, 64)));
        assert_eq!(String::from_utf8_lossy(&reply), "\x1b_Gi=7;OK\x1b\\");
        // The picture takes rows: what follows it is under it, as it is on
        // the UI's screen.
        let cursor = terminal.feed(b"\x1b[6n");
        assert_ne!(cursor, b"\x1b[1;1R", "{:?}", String::from_utf8_lossy(&cursor));
        // A screen without pictures says there is one and where. With
        // them it has the pixels too, as the whole snapshot does.
        let pixels = 64 * 64 * 3;
        let screen = terminal.screen(false);
        assert!(screen.len() > plain && screen.len() < plain + pixels);
        let pictured = terminal.screen(true);
        assert!(pictured.len() >= screen.len() + pixels);
        assert!(pictured.starts_with(&screen));
        assert!(terminal.snapshot().len() >= plain + pixels);
        // Both decode to the same screen.
        assert_eq!(Terminal::decode(&pictured).unwrap().text(), terminal.text());
        // Once the picture has scrolled into the history its pixels are
        // not part of the screen.
        let _ = terminal.feed("
".repeat(200).as_bytes());
        assert!(terminal.screen(true).len() < plain + pixels);

        // What is not a PNG is refused, and the terminal goes on.
        let mut terminal = self::terminal();
        let reply = terminal.feed(&show(8, b"not a png at all"));
        let reply = String::from_utf8_lossy(&reply);
        assert!(reply.starts_with("\x1b_Gi=8;E"), "{reply:?}");
        assert_eq!(terminal.feed(b"\x1b[6n"), b"\x1b[1;1R");
    }

    #[test]
    fn queries_are_answered_with_nothing_attached() {
        let mut terminal = terminal();
        // Cursor position, then device attributes.
        assert_eq!(terminal.feed(b"\x1b[6n"), b"\x1b[1;1R");
        assert_eq!(terminal.feed(b"ab\r\ncd\x1b[6n"), b"\x1b[2;3R");
        let attributes = terminal.feed(b"\x1b[c");
        assert!(attributes.starts_with(b"\x1b[?"), "{attributes:?}");
    }

    fn identity() -> Identity {
        Identity {
            xtversion: "ghostty 1.2.3".into(),
            enquiry: "here".into(),
            conformance: 62,
            features: vec![22, 52],
            secondary: [1, 10, 0],
        }
    }

    #[test]
    fn the_terminal_names_itself_as_the_ui_does() {
        let mut terminal = terminal();
        // Left alone, the library answers as itself.
        assert_eq!(terminal.feed(b"\x1b[c"), b"\x1b[?62;22c");
        assert_eq!(terminal.feed(b"\x1b[>q"), b"\x1bP>|libghostty\x1b\\");
        assert_eq!(terminal.feed(b"\x05"), b"");

        let settings = TerminalSettings { identity: Some(identity()), ..Default::default() };
        assert!(terminal.apply(&settings).is_empty());
        assert_eq!(terminal.feed(b"\x1b[c"), b"\x1b[?62;22;52c");
        assert_eq!(terminal.feed(b"\x1b[>c"), b"\x1b[>1;10;0c");
        assert_eq!(terminal.feed(b"\x1b[>q"), b"\x1bP>|ghostty 1.2.3\x1b\\");
        assert_eq!(terminal.feed(b"\x05"), b"here");

        // A snapshot's mirror is a separate terminal and keeps none of it.
        let mut mirror = Terminal::decode(&terminal.screen(false)).unwrap();
        assert_eq!(mirror.feed(b"\x1b[>q"), b"\x1bP>|libghostty\x1b\\");
    }

    #[test]
    fn the_title_and_directory_the_shell_set_are_remembered() {
        let mut terminal = terminal();
        assert_eq!(terminal.title(), None);
        assert_eq!(terminal.pwd(), None);
        let _ = terminal.feed(b"\x1b]2;build log\x07\x1b]7;file://host/C:/src\x07");
        assert_eq!(terminal.title().as_deref(), Some("build log"));
        assert_eq!(terminal.pwd().as_deref(), Some("file://host/C:/src"));
    }

    #[test]
    fn size_queries_use_the_size_the_ui_gave() {
        let mut terminal = terminal();
        assert_eq!(terminal.feed(b"\x1b[18t"), b"\x1b[8;24;80t");
        assert_eq!(terminal.feed(b"\x1b[16t"), b"\x1b[6;18;9t");
        let _ = terminal.resize(size(100, 30));
        assert_eq!(terminal.feed(b"\x1b[18t"), b"\x1b[8;30;100t");
    }

    #[test]
    fn the_color_scheme_is_answered_once_known() {
        let mut terminal = terminal();
        assert_eq!(terminal.feed(b"\x1b[?996n"), b"");
        let settings = TerminalSettings { dark: Some(true), ..Default::default() };
        assert!(terminal.apply(&settings).is_empty());
        assert_eq!(terminal.feed(b"\x1b[?996n"), b"\x1b[?997;1n");
    }

    #[test]
    fn colors_from_settings_answer_color_queries() {
        let settings = TerminalSettings {
            foreground: Some([0x11, 0x22, 0x33]),
            background: Some([0xaa, 0xbb, 0xcc]),
            ..Default::default()
        };
        let mut terminal = Terminal::new(size(80, 24), &settings).unwrap();
        let reply = String::from_utf8(terminal.feed(b"\x1b]10;?\x07\x1b]11;?\x07")).unwrap();
        assert!(reply.contains("rgb:1111/2222/3333"), "{reply:?}");
        assert!(reply.contains("rgb:aaaa/bbbb/cccc"), "{reply:?}");
    }

    #[test]
    fn every_setting_is_accepted_at_creation_and_afterwards() {
        let settings = TerminalSettings {
            scrollback_bytes: Some(1 << 24),
            scrollback_lines: Some(50_000),
            foreground: Some([1, 2, 3]),
            background: Some([4, 5, 6]),
            cursor: Some([7, 8, 9]),
            palette: vec![[10, 20, 30]; 256],
            cursor_style: Some(CursorStyle::Bar),
            cursor_blink: Some(true),
            grapheme_cluster: Some(true),
            image_storage_limit: Some(1 << 20),
            dark: Some(false),
            identity: Some(identity()),
        };
        let mut terminal = Terminal::new(size(80, 24), &settings).unwrap();
        let _ = terminal.feed(b"some output\r\n");
        assert_eq!(terminal.apply(&settings), Vec::<&str>::new());
        let short = TerminalSettings { palette: vec![[0, 0, 0]; 16], ..Default::default() };
        assert_eq!(terminal.apply(&short), vec!["palette"]);
    }

    /// A window on another machine is given these bytes by a daemon it has
    /// only ssh's word for. Whatever they are, decoding them must end: with a
    /// terminal or with an error, and never by bringing the process down.
    /// The window's terminal decodes with the same library.
    #[test]
    fn damaged_screen_bytes_are_refused_or_read_and_never_crash() {
        let mut terminal = Terminal::new(size(80, 24), &TerminalSettings::default()).unwrap();
        for line in 0..300 {
            let _ = terminal.feed(format!("\x1b[3{}mline {line} \u{e9}\x1b[0m\r\n", line % 8).as_bytes());
        }
        let _ = terminal.feed(b"\x1b]2;a title\x07\x1b]7;file://host/C:/src\x07prompt> \x1b[3");
        let mut next = 0x9e37_79b9_7f4a_7c15_u64;
        let mut random = move || {
            next ^= next << 13;
            next ^= next >> 7;
            next ^= next << 17;
            next
        };
        let mut refused = 0;
        let mut attempt = |bytes: &[u8]| {
            if let Ok(mut decoded) = Terminal::decode(bytes) {
                // One that was read is one that can be used.
                let _ = decoded.text();
                let _ = decoded.feed(b"more\r\n");
            } else {
                refused += 1;
            }
        };
        for good in [terminal.screen(false), terminal.snapshot()] {
            // Cut short, anywhere.
            for _ in 0..200 {
                attempt(&good[..random() as usize % good.len()]);
            }
            // One byte wrong, then a run of them.
            for round in 0..600 {
                let mut bad = good.clone();
                let at = random() as usize % bad.len();
                let run = if round % 3 == 0 { 1 + random() as usize % 64 } else { 1 };
                for byte in bad.iter_mut().skip(at).take(run) {
                    *byte = random() as u8;
                }
                attempt(&bad);
            }
            // Lengths and counts that claim far more than is there.
            for _ in 0..200 {
                let mut bad = good.clone();
                let at = random() as usize % bad.len().saturating_sub(8);
                bad[at..at + 8].copy_from_slice(&[0xff; 8]);
                attempt(&bad);
            }
        }
        // Nothing of a snapshot at all.
        for length in [0, 1, 7, 64, 4096] {
            let noise: Vec<u8> = (0..length).map(|_| random() as u8).collect();
            attempt(&noise);
        }
        assert!(refused > 0, "nothing was refused: the damage did no damage");
    }

    #[test]
    fn a_screen_is_small_and_decodes_to_the_same_screen() {
        let settings =
            TerminalSettings { scrollback_bytes: Some(1 << 26), ..Default::default() };
        let mut terminal = Terminal::new(size(80, 24), &settings).unwrap();
        for line in 0..5000 {
            let _ = terminal.feed(format!("\x1b[3{}mline {line}\x1b[0m\r\n", line % 8).as_bytes());
        }
        let _ = terminal.feed(b"prompt> typed \x1b[3");
        assert!(terminal.total_rows() > 4000);

        let whole = terminal.snapshot();
        let screen = terminal.screen(false);
        assert!(screen.len() * 10 < whole.len(), "{} vs {}", screen.len(), whole.len());
        assert_eq!(&whole[..screen.len()], &screen[..]);

        let mut mirror = Terminal::decode(&screen).unwrap();
        assert_eq!(mirror.grid(), (80, 24));
        // The screen comes with the rest of the page it sits on, so a few
        // hundred rows of recent history ride along, and no more.
        assert!((24..1000).contains(&mirror.total_rows()), "{}", mirror.total_rows());
        let text = mirror.text();
        assert!(text.contains("line 4999"), "{text}");
        assert!(text.trim_end().ends_with("prompt> typed"), "{text}");
        assert!(!text.contains("line 100\n"));

        // Both continue from the unfinished sequence the same way.
        let rest = b"1mbold\x1b[0m\x1b[6n";
        assert_eq!(terminal.feed(rest), mirror.feed(rest));
        assert!(mirror.text().trim_end().ends_with("prompt> typed bold"));

        let mut full = Terminal::decode(&whole).unwrap();
        assert_eq!(full.total_rows(), terminal.total_rows() );
    }

    #[test]
    fn a_resize_reflows_and_the_screen_follows() {
        let mut terminal = terminal();
        let _ = terminal.feed("x".repeat(100).as_bytes());
        let _ = terminal.resize(size(120, 24));
        let mut mirror = Terminal::decode(&terminal.screen(false)).unwrap();
        assert_eq!(mirror.grid(), (120, 24));
        assert_eq!(mirror.text().trim_end(), "x".repeat(100));
    }
}

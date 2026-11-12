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

/// Collects a snapshot up to the end of its READY record and then refuses
/// more, which stops the encoder before it walks the history.
struct ScreenOnly {
    out: Vec<u8>,
    /// Where the next record header starts.
    next: usize,
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
        if tag != RECORD_READY {
            state.next = end;
        } else if state.out.len() >= end {
            state.out.truncate(end);
            state.done = true;
            return false;
        } else {
            break;
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
    /// snapshot that ends at its READY record. It carries the page the screen
    /// sits on, so some recent history comes along, but the cost does not
    /// grow with the history.
    pub fn screen(&mut self) -> Vec<u8> {
        let mut state = ScreenOnly { out: Vec::new(), next: ENVELOPE_LEN, done: false };
        let writer = Writer { write: collect_screen, userdata: (&raw mut state).cast() };
        // Refusing bytes after READY makes the encoder report a failed write.
        let result = unsafe { ghostty_snapshot_encode(self.raw, writer) };
        assert!(state.done, "snapshot had no READY record (encode result {result})");
        state.out
    }

    /// Rebuild a terminal from `snapshot` or `screen` bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
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
        Ok(Self::adopt(raw))
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

    /// Rows held, history included.
    pub fn total_rows(&mut self) -> usize {
        let mut rows = 0usize;
        unsafe { ghostty_terminal_get(self.raw, DATA_TOTAL_ROWS, (&raw mut rows).cast()) };
        rows
    }
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

/// Whether the last record of a snapshot is READY rather than FINISH.
fn is_screen_only(bytes: &[u8]) -> bool {
    let mut next = ENVELOPE_LEN;
    let mut last = 0u16;
    while bytes.len() >= next + RECORD_HEADER_LEN {
        last = u16::from_le_bytes([bytes[next], bytes[next + 1]]);
        let payload =
            u32::from_le_bytes([bytes[next + 2], bytes[next + 3], bytes[next + 4], bytes[next + 5]]);
        next += RECORD_HEADER_LEN + payload as usize;
    }
    last == RECORD_READY
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
        let mut mirror = Terminal::decode(&terminal.screen()).unwrap();
        assert_eq!(mirror.feed(b"\x1b[>q"), b"\x1bP>|libghostty\x1b\\");
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
        let screen = terminal.screen();
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
        let mut mirror = Terminal::decode(&terminal.screen()).unwrap();
        assert_eq!(mirror.grid(), (120, 24));
        assert_eq!(mirror.text().trim_end(), "x".repeat(100));
    }
}

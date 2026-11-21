//! Messages between the UI and the daemon. Nothing here is specific to one
//! platform.
//!
//! A frame is a little-endian `u32` length, one kind byte, and a body. The
//! length counts the kind byte and the body. Control frames carry JSON, so a
//! daemon that outlives several UI versions can ignore fields and messages it
//! does not know. Terminal bytes travel in their own frame kinds, unencoded.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Bumped when a frame kind or a required field changes meaning.
pub const PROTOCOL_VERSION: u32 = 16;

/// A snapshot with a very long history is the largest frame.
pub const MAX_FRAME: usize = 1 << 30;

pub type PaneId = u32;

/// Settings of the UI that change how a terminal parses or answers. The
/// daemon's terminals must match the UI's or the two screens drift apart.
/// A field left out keeps the library's default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    pub scrollback_bytes: Option<u64>,
    pub scrollback_lines: Option<u64>,
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub cursor: Option<[u8; 3]>,
    /// All 256 entries, or empty for the built-in palette.
    pub palette: Vec<[u8; 3]>,
    pub cursor_style: Option<CursorStyle>,
    pub cursor_blink: Option<bool>,
    /// Width of multi-codepoint characters: mode 2027 on by default.
    pub grapheme_cluster: Option<bool>,
    pub image_storage_limit: Option<u64>,
    /// Answer to the light or dark query. Unanswered when unknown.
    pub dark: Option<bool>,
    /// How the terminal names itself to a program that asks. Left out, the
    /// library answers as itself.
    pub identity: Option<Identity>,
}

/// What a terminal answers to "who are you".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Identity {
    /// The XTVERSION text.
    pub xtversion: String,
    /// The answer to ENQ; empty for none.
    pub enquiry: String,
    /// Primary device attributes: conformance level, then feature codes.
    pub conformance: u16,
    pub features: Vec<u16>,
    /// Secondary device attributes: type, firmware version, cartridge.
    pub secondary: [u16; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

/// Grid size and the pixel size of one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub cell_width: u32,
    #[serde(default)]
    pub cell_height: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: PaneId,
    pub pid: u32,
    /// When the shell started, as its machine counts it: a FILETIME on
    /// Windows, clock ticks since the machine started on Linux. With the pid
    /// it identifies the shell for as long as the machine is up. Only ever
    /// compared with another of the same machine's.
    pub creation_time: u64,
    pub command: String,
    pub size: Size,
    /// The exit code once the shell has ended.
    pub exited: Option<u32>,
    /// Whether a UI shows it. A pane is shown by one UI at a time.
    #[serde(default)]
    pub attached: bool,
    /// The title the shell last set, for a UI that was not there to hear it.
    #[serde(default)]
    pub title: Option<String>,
    /// The directory the shell last reported, as it reported it.
    #[serde(default)]
    pub pwd: Option<String>,
}

/// A session and the shells of it that are running. One with no running
/// shell is forgotten. The shells in no session are given as one more, with
/// an empty `id` and no layout, last.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub panes: Vec<PaneInfo>,
    pub layout: String,
}

/// A key a picker's own machine does something for: as it is written in a
/// Lua file (`key = "d", mods = "CTRL"`), and what it is for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PickerBinding {
    pub key: String,
    #[serde(default)]
    pub mods: String,
    #[serde(default)]
    pub description: String,
}

/// One thing a UI does to a pane for a picker: see `Control::PickerActed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum PickerStep {
    /// Text typed at the shell, a line end in it being Enter.
    SendText { text: String },
    /// Text pasted at the shell.
    Paste { text: String },
    /// Text in place of what is at the shell's cursor: so many characters
    /// before it and after it are taken away first.
    ReplacePrompt { text: String, before: u32, after: u32 },
    /// A note shown for a while.
    Toast {
        #[serde(default)]
        title: Option<String>,
        message: String,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    /// Another picker of the daemon's, by its name, opened for the pane
    /// once this one is over.
    Picker {
        name: String,
        #[serde(default)]
        params: std::collections::BTreeMap<String, String>,
    },
    /// A step from a newer version.
    #[serde(other)]
    Unknown,
}

/// One row a picker shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PickerRow {
    /// The row's number in the picker's source, which it is known by.
    pub index: u64,
    /// What the row shows, and what its matches are found in.
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    // UI to daemon.
    /// First message on a connection.
    /// `snapshot` is the version of ghostty's snapshot format the UI reads,
    /// and `ghostty` the revision it was built from, which is only logged.
    Hello {
        protocol: u32,
        ghostty: String,
        #[serde(default)]
        snapshot: u16,
    },
    /// Applies to panes made afterwards and to every pane that exists.
    Settings { settings: TerminalSettings },
    Spawn {
        request: u32,
        command: String,
        #[serde(default)]
        cwd: Option<String>,
        /// A pane of the daemon's. The shell starts in the folder that pane's
        /// shell last reported, if there is one there; otherwise in `cwd`.
        /// A daemon from before it takes no notice of it.
        #[serde(default)]
        beside: Option<PaneId>,
        /// The complete environment of the new shell.
        env: Vec<(String, String)>,
        size: Size,
    },
    /// Resize the pane to `size`, send its screen, then its output.
    /// Answered with `Attached` before the screen. Answered with `Error`,
    /// and the pane left as it was, when there is no such pane, or when
    /// another UI shows it: a pane is shown by one UI at a time.
    Attach {
        request: u32,
        pane: PaneId,
        size: Size,
    },
    Detach { pane: PaneId },
    /// Send the pane's screen once, as it is. The pane is not resized, its
    /// output does not follow, and the asker does not count as showing it.
    Peek { pane: PaneId },
    /// The daemon resizes and sends the pane's screen again.
    Resize { pane: PaneId, size: Size },
    /// Ask for the whole terminal, history included.
    Snapshot { request: u32, pane: PaneId },
    /// End the shell if it is running and forget the pane.
    Close { pane: PaneId },
    List { request: u32 },
    /// One workspace of a UI, so that it can be shown again when nothing
    /// shows it: the panes it holds, and how the UI lays them out, which
    /// only UIs read. Sent whenever either changes. A pane is in one session,
    /// the one that named it last, and a session is forgotten when none of
    /// its panes is running or another has taken them all: there is nothing
    /// to say to end one.
    SessionSet { id: String, panes: Vec<PaneId>, layout: String },
    /// The first message of a program running in pane `pane`'s shell, in
    /// place of `Hello`. What it sends from then on, in `Frame::Shell`, is
    /// passed to the UI attached to the pane, and what that UI answers is
    /// passed back. Refused with `Error` when there is no such pane or no UI is
    /// attached to it. Any program of the user may name any pane.
    Shell { pane: PaneId },
    /// To a UI: a program in `pane`'s shell has connected, and `channel`
    /// is its conversation.
    ShellOpen { channel: u32, pane: PaneId },
    /// The conversation is over. From a UI: it has finished with the
    /// program. To a UI: the program has gone. To the program, with
    /// channel 0: the UI has finished, or has gone.
    ShellClosed { channel: u32 },
    /// The first message of a program running in pane `pane`'s shell that
    /// wants the daemon to run a picker for it, in place of `Hello`. What
    /// it sends from then on, in `Frame::Shell`, is for the daemon: which
    /// picker, and with what. The daemon finds the rows, and the UI attached
    /// to the pane shows them; see `PickerOpen`. Refused with `Error` as
    /// `Shell` is.
    Pick { pane: PaneId },
    /// From a UI showing picker `picker`: find rows for `query`, and answer
    /// with no more than `rows` of them. `revision` counts the UI's queries
    /// for this picker from none, and what is found says which it is for.
    PickerQuery { picker: u32, revision: u64, query: String, rows: u32 },
    /// From a UI: it has taken the `PickerRows` with this serial. More
    /// found for a query already answered is sent only after this.
    PickerSeen { picker: u32, serial: u64 },
    /// From a UI: row `row` was chosen, of the rows sent with `serial`. The
    /// picker is over.
    PickerAccept { picker: u32, serial: u64, row: u64 },
    /// From a UI: the picker is over with nothing chosen. With a reason
    /// when the UI would not show it at all.
    PickerClose {
        picker: u32,
        #[serde(default)]
        refused: Option<String>,
    },
    /// From a UI: run the picker named `picker` over `pane`, which the UI
    /// shows, for the UI itself and not for a program: as when its user
    /// asks for one with a key. `ask` is the UI's own number for this, said
    /// back in the `PickerOpen` that follows, or in `PickerRefused`. The UI
    /// is told what a row it accepts is with `PickerChosen`.
    PickerAsk {
        ask: u64,
        pane: PaneId,
        picker: String,
        #[serde(default)]
        params: std::collections::BTreeMap<String, String>,
    },
    /// From a UI: a search of the folders under `roots`, to `depth` levels
    /// of them (nothing or less is all), over no pane: for what the UI
    /// shows of its own, as its list of where a workspace can be started.
    /// It is answered and goes on as a picker asked for with `PickerAsk`
    /// does, with no pane in its `PickerOpen`, and its rows are the
    /// folders' whole paths. The UI lists them; the daemon does nothing
    /// with the one chosen.
    FoldersAsk {
        ask: u64,
        roots: Vec<String>,
        #[serde(default)]
        depth: i32,
    },
    /// From a UI: what would show for row `row` of the rows sent with
    /// `serial`. Answered `PickerPreviewed` with the same `request`; a UI
    /// that has asked again wants only the last.
    PickerPreview { picker: u32, request: u64, serial: u64, row: u64 },
    /// End every shell and the daemon.
    Shutdown,

    // Daemon to UI.
    Welcome {
        protocol: u32,
        /// The ghostty revision the daemon was built from. Nothing is
        /// checked with it.
        ghostty: String,
        /// The version of ghostty's snapshot format the daemon writes. A UI
        /// uses only a daemon whose version is its own. A daemon from
        /// before it leaves this out: 0, which is no version.
        #[serde(default)]
        snapshot: u16,
        pid: u32,
        session: String,
        /// Whether the daemon keeps files a UI sends it: see `Frame::File`.
        /// A daemon from before it could leaves this out.
        #[serde(default)]
        files: bool,
        /// The machine the daemon runs on, and the user it runs as, for a
        /// window to say where its shells are. Nothing is checked with
        /// them. A daemon from before them leaves them out: empty.
        #[serde(default)]
        machine: String,
        #[serde(default)]
        user: String,
    },
    Spawned { request: u32, info: PaneInfo },
    /// The attach asked for by `request` was done. `info.attached` is true:
    /// the asker shows it.
    Attached { request: u32, info: PaneInfo },
    Exited { pane: PaneId, code: u32 },
    Panes { request: u32, panes: Vec<PaneInfo> },
    /// Every session, to every UI: once after `Welcome`, and again whenever
    /// one changes.
    Sessions { sessions: Vec<SessionInfo> },
    Error {
        #[serde(default)]
        request: Option<u32>,
        message: String,
    },
    /// The file sent with `Frame::File` and this `request` was kept, at
    /// `path` on the daemon's machine. Refused with `Error` instead.
    FileKept { request: u32, path: String },

    /// To the UI attached to `pane`: show a picker over it, whose rows the
    /// daemon finds. The UI asks for them with `PickerQuery`, first for
    /// `query`, and ends the picker with `PickerAccept` or `PickerClose`.
    PickerOpen {
        picker: u32,
        /// The pane it is over. None for a search of folders the UI asked
        /// for with `FoldersAsk`, which is over none.
        #[serde(default)]
        pane: Option<PaneId>,
        /// What the picker is of, to show with it.
        label: String,
        /// The query it starts with.
        query: String,
        /// Whether its rows have previews, to be asked for with
        /// `PickerPreview`, and if so whether it opens showing them.
        #[serde(default)]
        preview: Option<bool>,
        /// Whether it is shown at the pane's cursor, as a picker of what to
        /// put there is, and not in the middle of the pane.
        #[serde(default)]
        at_cursor: bool,
        /// The keys the picker's own machine does something for, each
        /// known by its place here: see `PickerKey`.
        #[serde(default)]
        keys: Vec<PickerBinding>,
        /// The UI's number for it, when the UI asked with `PickerAsk`.
        #[serde(default)]
        ask: Option<u64>,
    },
    /// To a UI: the picker it asked for with `ask` is not run, and why.
    PickerRefused { ask: u64, message: String },
    /// To a UI that asked for the picker and accepted a row of it: what the
    /// row shows, what it is, and the query it was found with. The picker
    /// is over.
    PickerChosen { picker: u32, text: String, value: String, query: String },
    /// To a UI: what the picker's own machine says is done with a row, for
    /// the UI to do to the pane the picker is over, in order. It is all a
    /// UI does for a picker beyond drawing it: text for the pane's shell,
    /// which is the daemon's own, and a note to show.
    PickerActed { picker: u32, steps: Vec<PickerStep> },
    /// From a UI: a key of the picker's was pressed on a row, of those
    /// sent with `serial`. `key` is its place in the keys the picker
    /// opened with. What is to be done comes back as `PickerActed`; the
    /// picker stays open unless that is followed by `PickerClosed`.
    PickerKey { picker: u32, serial: u64, row: u64, key: u32 },
    /// What was found for the query with this revision. There can be
    /// several for one revision while the picker's source is still being
    /// added to, each in place of the one before.
    PickerRows {
        picker: u32,
        /// Counts what this picker has been sent, from one.
        serial: u64,
        revision: u64,
        query: String,
        /// What of the query the rows' text was matched against.
        matched_against: String,
        /// A line to show over the rows.
        #[serde(default)]
        header: Option<String>,
        /// The rows to show, the best first.
        rows: Vec<PickerRow>,
        matched: u64,
        total: u64,
        /// Source records left out, such as overlong lines.
        skipped: u64,
        /// Whether the source has all it will have.
        done: bool,
        /// The source's version: rows sent with the same one are the same
        /// rows under the same numbers.
        version: u64,
        /// Whether a row keeps its number and what it says while the source
        /// is added to, whatever the version.
        #[serde(default)]
        append_only: bool,
    },
    /// To a UI: the picker is over, and not by the UI's doing. With what
    /// went wrong, if something did.
    PickerClosed {
        picker: u32,
        #[serde(default)]
        message: Option<String>,
    },
    /// To a UI: what shows for the row it asked of with `request`. The UI
    /// lays it out; it is content, and nothing for the UI to open or run.
    PickerPreviewed {
        picker: u32,
        request: u64,
        /// What to show, as text.
        text: String,
        /// The name of the file the text is of, to colour it by. Without
        /// one the text is shown plain: a listing, or a note.
        #[serde(default)]
        name: Option<String>,
        /// Whether there is more of it than was sent.
        #[serde(default)]
        truncated: bool,
        /// The line of `text`, from one, that the row is about: shown in
        /// the middle, and marked.
        #[serde(default)]
        line: Option<u64>,
        /// How `text` is coloured, in place of by `name`: `PlainText` for
        /// not at all, or the name of a language.
        #[serde(default)]
        syntax: Option<String>,
        /// How the name of a file of what `text` is ends, to colour it by
        /// where it has no `name`.
        #[serde(default)]
        extension: Option<String>,
        /// Why there is nothing to show, in place of `text`.
        #[serde(default)]
        error: Option<String>,
    },

    /// A message from a newer version.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Control(Control),
    /// Bytes the shell wrote.
    Output { pane: PaneId, bytes: Vec<u8> },
    /// Bytes for the shell: encoded keys, mouse, paste and focus.
    Input { pane: PaneId, bytes: Vec<u8> },
    /// The visible screen and modes, as a ghostty snapshot that ends at its
    /// READY record. The output that follows continues from it.
    Screen { pane: PaneId, bytes: Vec<u8> },
    /// One message between a program in a shell and the UI showing it,
    /// which the daemon passes on unread. A program's frames carry channel
    /// 0; the daemon knows a program by its connection.
    Shell { channel: u32, bytes: Vec<u8> },
    /// The pane's terminal was resized here, between the output before and
    /// the output after. A UI resizes its copy at the same point, which
    /// keeps the two the same.
    Resized { pane: PaneId, cols: u16, rows: u16 },
    /// A whole ghostty snapshot, answering `Control::Snapshot`.
    Snapshot { request: u32, bytes: Vec<u8> },
    /// To a UI: what shows for the row it asked of with `request`, where
    /// that is not text: see `Control::PickerPreviewed`. It is content,
    /// and nothing for the UI to open or run.
    /// `line` is the line of what a program printed, from one, to show in
    /// the middle.
    Previewed { picker: u32, request: u64, kind: PreviewKind, line: Option<u32>, bytes: Vec<u8> },
    /// From a UI: keep these bytes as a file on the daemon's machine, whose
    /// name ends `.extension`. The daemon chooses the folder and the name,
    /// and answers `Control::FileKept` with the same `request`, or `Error`.
    /// It only writes the file: nothing is run or opened.
    File { request: u32, extension: String, bytes: Vec<u8> },
}

/// What the bytes of a preview are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    /// What a program printed, with the escape sequences that colour it.
    Ansi,
    /// A picture, as the file of one: a PNG, a JPEG.
    Image,
}
impl PreviewKind {
    fn byte(self) -> u8 {
        match self {
            Self::Ansi => 1,
            Self::Image => 2,
        }
    }
    fn of(byte: u8) -> Option<Self> {
        [Self::Ansi, Self::Image].into_iter().find(|kind| kind.byte() == byte)
    }
}

const KIND_CONTROL: u8 = 0;
const KIND_OUTPUT: u8 = 1;
const KIND_INPUT: u8 = 2;
const KIND_SCREEN: u8 = 3;
const KIND_SNAPSHOT: u8 = 4;
const KIND_RESIZED: u8 = 6;
const KIND_SHELL: u8 = 5;
const KIND_PREVIEWED: u8 = 7;
const KIND_FILE: u8 = 8;

impl Frame {
    /// The frame as it travels, length first.
    pub fn encode(&self) -> Vec<u8> {
        let (kind, id, bytes): (u8, Option<u32>, std::borrow::Cow<'_, [u8]>) = match self {
            Self::Control(control) => (
                KIND_CONTROL,
                None,
                serde_json::to_vec(control).expect("control messages serialize").into(),
            ),
            Self::Output { pane, bytes } => (KIND_OUTPUT, Some(*pane), bytes.as_slice().into()),
            Self::Input { pane, bytes } => (KIND_INPUT, Some(*pane), bytes.as_slice().into()),
            Self::Screen { pane, bytes } => (KIND_SCREEN, Some(*pane), bytes.as_slice().into()),
            Self::Resized { pane, cols, rows } => (
                KIND_RESIZED,
                Some(*pane),
                [cols.to_le_bytes(), rows.to_le_bytes()].concat().into(),
            ),
            Self::Shell { channel, bytes } => (KIND_SHELL, Some(*channel), bytes.as_slice().into()),
            Self::Previewed { picker, request, kind, line, bytes } => {
                let mut body = Vec::with_capacity(13 + bytes.len());
                body.extend_from_slice(&request.to_le_bytes());
                body.push(kind.byte());
                // No line is line nothing: they are counted from one.
                body.extend_from_slice(&line.unwrap_or(0).to_le_bytes());
                body.extend_from_slice(bytes);
                (KIND_PREVIEWED, Some(*picker), body.into())
            }
            Self::Snapshot { request, bytes } => {
                (KIND_SNAPSHOT, Some(*request), bytes.as_slice().into())
            }
            Self::File { request, extension, bytes } => {
                // The extension's length, the extension, the bytes. One
                // longer than a byte can say is cut: the daemon refuses it.
                let extension = &extension.as_bytes()[..extension.len().min(255)];
                let mut body = Vec::with_capacity(1 + extension.len() + bytes.len());
                body.push(extension.len() as u8);
                body.extend_from_slice(extension);
                body.extend_from_slice(bytes);
                (KIND_FILE, Some(*request), body.into())
            }
        };
        let len = 1 + id.map_or(0, |_| 4) + bytes.len();
        let mut out = Vec::with_capacity(4 + len);
        out.extend_from_slice(&(len as u32).to_le_bytes());
        out.push(kind);
        if let Some(id) = id {
            out.extend_from_slice(&id.to_le_bytes());
        }
        out.extend_from_slice(&bytes);
        out
    }

    /// An output frame without building a `Frame` first, for the hot path.
    pub fn encode_output(pane: PaneId, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(9 + bytes.len());
        out.extend_from_slice(&((5 + bytes.len()) as u32).to_le_bytes());
        out.push(KIND_OUTPUT);
        out.extend_from_slice(&pane.to_le_bytes());
        out.extend_from_slice(bytes);
        out
    }

    /// Decode the part of a frame after its length.
    pub fn decode(body: &[u8]) -> io::Result<Self> {
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        let (&kind, rest) = body.split_first().ok_or_else(|| invalid("empty frame"))?;
        if kind == KIND_CONTROL {
            return serde_json::from_slice(rest)
                .map(Self::Control)
                .map_err(|e| invalid(&format!("control message: {e}")));
        }
        let (id, bytes) = rest
            .split_first_chunk::<4>()
            .ok_or_else(|| invalid("short frame"))?;
        let (id, bytes) = (u32::from_le_bytes(*id), bytes.to_vec());
        match kind {
            KIND_OUTPUT => Ok(Self::Output { pane: id, bytes }),
            KIND_INPUT => Ok(Self::Input { pane: id, bytes }),
            KIND_SCREEN => Ok(Self::Screen { pane: id, bytes }),
            KIND_SNAPSHOT => Ok(Self::Snapshot { request: id, bytes }),
            KIND_RESIZED => match bytes.as_slice() {
                [c0, c1, r0, r1] => Ok(Self::Resized {
                    pane: id,
                    cols: u16::from_le_bytes([*c0, *c1]),
                    rows: u16::from_le_bytes([*r0, *r1]),
                }),
                _ => Err(invalid("resized frame")),
            },
            KIND_SHELL => Ok(Self::Shell { channel: id, bytes }),
            KIND_PREVIEWED => {
                let (request, rest) = bytes.split_first_chunk::<8>().ok_or_else(|| invalid("previewed frame"))?;
                let (kind, bytes) = rest.split_first().ok_or_else(|| invalid("previewed frame"))?;
                let (line, bytes) = bytes.split_first_chunk::<4>().ok_or_else(|| invalid("previewed frame"))?;
                let line = Some(u32::from_le_bytes(*line)).filter(|line| *line > 0);
                // A kind from a newer version.
                let Some(kind) = PreviewKind::of(*kind) else { return Ok(Self::Control(Control::Unknown)) };
                Ok(Self::Previewed { picker: id, request: u64::from_le_bytes(*request), kind, line, bytes: bytes.to_vec() })
            }
            KIND_FILE => {
                let (&len, rest) = bytes.split_first().ok_or_else(|| invalid("file frame"))?;
                if rest.len() < len as usize {
                    return Err(invalid("file frame"));
                }
                let (extension, bytes) = rest.split_at(len as usize);
                // Not checked here: a bad one is the daemon's to refuse, and
                // a frame that does not decode ends the connection.
                let extension = String::from_utf8_lossy(extension).into_owned();
                Ok(Self::File { request: id, extension, bytes: bytes.to_vec() })
            }
            // A frame kind from a newer version.
            _ => Ok(Self::Control(Control::Unknown)),
        }
    }

    pub fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(&self.encode())
    }

    /// Read one frame. An end of stream before the first byte is
    /// `UnexpectedEof`.
    pub fn read(reader: &mut impl Read) -> io::Result<Self> {
        let mut len = [0u8; 4];
        reader.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        if len == 0 || len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame length"));
        }
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body)?;
        Self::decode(&body)
    }
}

impl From<Control> for Frame {
    fn from(control: Control) -> Self {
        Self::Control(control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: Frame) {
        let bytes = frame.encode();
        assert_eq!(Frame::read(&mut bytes.as_slice()).unwrap(), frame);
    }

    #[test]
    fn frames_round_trip() {
        round_trip(Frame::Output { pane: 7, bytes: b"\x1b[31mred".to_vec() });
        round_trip(Frame::Input { pane: 7, bytes: vec![] });
        round_trip(Frame::Screen { pane: 1, bytes: vec![0, 1, 2] });
        round_trip(Frame::Snapshot { request: 9, bytes: vec![255; 300] });
        round_trip(Frame::Previewed { picker: 4, request: u64::MAX - 1, kind: PreviewKind::Image, line: None, bytes: vec![0, 137, 80] });
        round_trip(Frame::Previewed { picker: 4, request: 2, kind: PreviewKind::Ansi, line: Some(70_000), bytes: vec![] });
        round_trip(Frame::File { request: 3, extension: "png".into(), bytes: vec![137, 80, 0, 255] });
        round_trip(Frame::File { request: 4, extension: String::new(), bytes: vec![] });
        round_trip(Control::FileKept { request: 3, path: r"C:\a\b.png".into() }.into());
        round_trip(
            Control::Hello { protocol: PROTOCOL_VERSION, ghostty: "abc".into(), snapshot: 1 }.into(),
        );
        round_trip(
            Control::Spawn {
                request: 1,
                command: "pwsh.exe".into(),
                cwd: Some(r"C:\src".into()),
                beside: Some(4),
                env: vec![("PATH".into(), r"C:\Windows".into())],
                size: Size { cols: 120, rows: 30, cell_width: 9, cell_height: 18 },
            }
            .into(),
        );
    }

    #[test]
    fn a_shell_asked_for_by_a_ui_from_before_beside_starts_in_its_folder() {
        let asked = r#"{"type":"spawn","request":2,"command":"cmd.exe","cwd":"C:\\src","env":[],
            "size":{"cols":80,"rows":24,"cell_width":9,"cell_height":18}}"#;
        let Control::Spawn { cwd, beside, .. } = serde_json::from_str(asked).unwrap() else {
            panic!("not a spawn")
        };
        assert_eq!((cwd.as_deref(), beside), (Some(r"C:\src"), None));
    }

    #[test]
    fn the_hot_path_encoding_matches() {
        let frame = Frame::Output { pane: 3, bytes: b"abc".to_vec() };
        assert_eq!(Frame::encode_output(3, b"abc"), frame.encode());
    }

    #[test]
    fn messages_from_a_newer_version_are_tolerated() {
        let unknown = br#"{"type":"something_new","value":1}"#;
        let mut body = vec![KIND_CONTROL];
        body.extend_from_slice(unknown);
        assert_eq!(Frame::decode(&body).unwrap(), Frame::Control(Control::Unknown));

        // An unknown field on a known message, and an unknown frame kind.
        let extra = br#"{"type":"detach","pane":4,"reason":"new"}"#;
        let mut body = vec![KIND_CONTROL];
        body.extend_from_slice(extra);
        assert_eq!(Frame::decode(&body).unwrap(), Control::Detach { pane: 4 }.into());
        assert_eq!(
            Frame::decode(&[200, 0, 0, 0, 0, 1]).unwrap(),
            Frame::Control(Control::Unknown)
        );
    }

    #[test]
    fn a_daemon_from_before_files_keeps_none() {
        let welcome = br#"{"type":"welcome","protocol":16,"ghostty":"g","pid":1,"session":"s"}"#;
        let mut body = vec![KIND_CONTROL];
        body.extend_from_slice(welcome);
        let Frame::Control(Control::Welcome { files, machine, user, snapshot, .. }) =
            Frame::decode(&body).unwrap()
        else {
            panic!("not a welcome");
        };
        // Nor does it say where it is.
        assert!(!files && machine.is_empty() && user.is_empty());
        // Nor what snapshots it writes, and no library's version is 0.
        assert_eq!(snapshot, 0);
    }

    #[test]
    fn a_file_frame_says_how_long_its_extension_is() {
        // Cut short: a bad frame.
        assert!(Frame::decode(&[KIND_FILE, 1, 0, 0, 0, 5, b'p']).is_err());
        // Longer than a byte can say: cut, for the daemon to refuse.
        let long = Frame::File { request: 1, extension: "x".repeat(300), bytes: vec![1] }.encode();
        let Frame::File { extension, bytes, .. } = Frame::read(&mut long.as_slice()).unwrap() else {
            panic!("not a file frame")
        };
        assert_eq!((extension.len(), bytes), (255, vec![1]));
    }

    #[test]
    fn settings_default_to_nothing_set() {
        let settings: TerminalSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, TerminalSettings::default());
    }

    #[test]
    fn a_bad_length_is_refused() {
        assert!(Frame::read(&mut [0u8, 0, 0, 0].as_slice()).is_err());
        assert!(Frame::read(&mut [255u8, 255, 255, 255].as_slice()).is_err());
    }
}

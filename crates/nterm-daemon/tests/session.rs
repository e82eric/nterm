//! Drives the real daemon binary over its pipe, as a UI would.
#![cfg(windows)]

use nterm_daemon::vt::Terminal;
use nterm_session::{
    GHOSTTY_REVISION,
    pipe::{self, Connection, Writer},
    relay,
    protocol::{
        Control, Frame, PROTOCOL_VERSION, PaneId, PaneInfo, SessionInfo, Size, TerminalSettings,
    },
};
use std::{
    io::Write,
    path::Path,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

const DAEMON: &str = env!("CARGO_BIN_EXE_nterm-daemon");
const WAIT: Duration = Duration::from_secs(20);

fn size(cols: u16, rows: u16) -> Size {
    Size { cols, rows, cell_width: 9, cell_height: 18 }
}

/// With `NTERM_TEST_RELAY=1`, every UI in every test here reaches its daemon
/// through a relay, so the whole suite says whether a relay carries all that
/// the pipe does. Without it, one test does (`a_ui_through_a_relay`).
///
/// With `NTERM_TEST_RELAY_VIA` as well, the relay is run by that command, as
/// a UI on another machine would run it: `ssh -T -o BatchMode=yes HOST`,
/// where HOST is this machine and this account, reached with no password.
fn relayed() -> bool {
    std::env::var_os("NTERM_TEST_RELAY").is_some_and(|value| value == "1")
}

/// The relay to a session's daemon: this build's, run here or by the
/// command in `NTERM_TEST_RELAY_VIA`.
fn relay_command(session: &str) -> std::process::Command {
    let via = std::env::var("NTERM_TEST_RELAY_VIA").unwrap_or_default();
    let mut via = via.split_whitespace();
    let Some(program) = via.next() else {
        let mut relay = std::process::Command::new(DAEMON);
        relay.args(["--relay", "--session", session]);
        return relay;
    };
    // The other sign-in has neither this one's directory nor its PATH, on
    // which the daemon finds the terminal library.
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/bin");
    let libraries = libraries.canonicalize().expect("out/bin");
    let libraries = libraries.to_str().unwrap().trim_start_matches(r"\\?\");
    let mut relay = std::process::Command::new(program);
    relay.args(via).arg(format!(
        "set \"PATH={libraries};%PATH%\" && \"{DAEMON}\" --relay --session {session}"
    ));
    relay
}

/// A session name no other test or run uses.
fn session(test: &str) -> String {
    format!("test-{test}-{}", std::process::id())
}

/// The pseudoconsole nterm ships, when this checkout has built it. It hands
/// a program's queries to the terminal; the system's answers some itself.
fn sidecar() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/bin/conpty.dll");
    path.exists().then(|| path.to_str().unwrap().to_owned())
}

/// Start a daemon for the session with the chosen pseudoconsole and wait
/// until it serves.
fn start(session: &str, conpty: Option<&str>) -> std::process::Child {
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", session]);
    match conpty {
        Some(path) => command.env("NTERM_CONPTY_DLL", path),
        None => command.env_remove("NTERM_CONPTY_DLL"),
    };
    let child = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    child
}

/// Both pseudoconsoles, the shipped one only where it exists.
fn each_conpty(test: &str, run: impl Fn(&str)) {
    // Each run ends by telling its daemon to shut down.
    let system = session(&format!("{test}-system"));
    let mut daemon = start(&system, None);
    run(&system);
    assert!(daemon.wait().unwrap().success());
    match sidecar() {
        Some(path) => {
            let shipped = session(&format!("{test}-shipped"));
            let mut daemon = start(&shipped, Some(&path));
            run(&shipped);
            assert!(daemon.wait().unwrap().success());
        }
        None => eprintln!("{test}: out/bin/conpty.dll is not built; ran with the system's only"),
    }
}

/// A UI: one connection, with what the daemon sends arriving on a channel.
struct Ui {
    connection: Connection,
    writer: Writer,
    frames: Receiver<Frame>,
    welcome: Control,
    /// What this UI's copy of a pane has been sent, in order.
    mirror: Option<Terminal>,
    /// The sessions as the daemon last gave them.
    sessions: Option<Vec<SessionInfo>>,
}

impl Ui {
    /// Connect as nterm does: the daemon is started if the session has none.
    /// Through a relay when every test is asked to (see `relayed`).
    fn connect(session: &str) -> Self {
        if relayed() {
            return Self::through_relay(session);
        }
        Self::over(pipe::connect_or_start(session, Path::new(DAEMON)).expect("connect"))
    }

    /// Connect as a UI on another machine does: through `nterm-daemon
    /// --relay`, whose input and output stand for the pipe.
    fn through_relay(session: &str) -> Self {
        Self::over(relay::connect(&mut relay_command(session)).expect("connect through a relay"))
    }

    fn over(connection: Connection) -> Self {
        let mut reader = connection.reader().unwrap();
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(frame) = Frame::read(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        let mut ui = Self {
            writer: connection.writer().unwrap(),
            connection,
            frames,
            welcome: Control::Unknown,
            mirror: None,
            sessions: None,
        };
        ui.send(Control::Hello { protocol: PROTOCOL_VERSION, ghostty: GHOSTTY_REVISION.into() });
        ui.welcome = ui.control(|control| matches!(control, Control::Welcome { .. }));
        ui
    }

    fn send(&mut self, control: Control) {
        Frame::Control(control).write(&mut self.writer).expect("send");
    }

    fn input(&mut self, pane: PaneId, text: &str) {
        Frame::Input { pane, bytes: text.as_bytes().to_vec() }.write(&mut self.writer).unwrap();
    }

    /// Keep the mirror terminal up to date with a frame, as a UI would.
    fn absorb(&mut self, frame: &Frame) {
        match frame {
            Frame::Screen { bytes, .. } => {
                self.mirror = Some(Terminal::decode(bytes).expect("screen decodes"));
            }
            Frame::Output { bytes, .. } => {
                // A UI's terminal is silent: its replies are dropped.
                let _ = self.mirror.as_mut().expect("screen before output").feed(bytes);
            }
            Frame::Resized { cols, rows, .. } => {
                let mirror = self.mirror.as_mut().expect("screen before a resize");
                let _ = mirror.resize(size(*cols, *rows));
            }
            Frame::Control(Control::Sessions { sessions }) => {
                self.sessions = Some(sessions.clone());
            }
            _ => {}
        }
    }

    /// The next frame that `wanted` accepts. Everything before it is applied
    /// to the mirror and otherwise dropped.
    fn frame(&mut self, mut wanted: impl FnMut(&Frame) -> bool) -> Frame {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let frame = self.frames.recv_timeout(left).expect("the daemon went quiet");
            self.absorb(&frame);
            if wanted(&frame) {
                return frame;
            }
        }
    }

    /// Ask for the daemon's terminal and compare it with the mirror at the
    /// point in the output where it was made. The mirror is kept, not
    /// replaced. Returns what differs, if anything does.
    fn differs_from_daemon(&mut self, pane: PaneId) -> Option<String> {
        let mut theirs = self.snapshot(pane);
        let ours = self.mirror.as_mut().unwrap();
        let (their_grid, our_grid) = (theirs.grid(), ours.grid());
        let cursor = |terminal: &mut Terminal| {
            String::from_utf8_lossy(&terminal.feed(b"[6n")).replace('', "")
        };
        let (their_cursor, our_cursor) = (cursor(&mut theirs), cursor(&mut *ours));
        // The rows from the top of the screen down to the cursor.
        let rows: usize = their_cursor
            .trim_start_matches('[')
            .split(';')
            .next()
            .and_then(|row| row.parse().ok())
            .unwrap_or(1);
        let tail = |terminal: &mut Terminal| -> Vec<String> {
            let text = terminal.text();
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(rows)..].iter().map(|l| l.to_string()).collect()
        };
        let (their_rows, our_rows) = (tail(&mut theirs), tail(&mut *ours));
        if their_grid == our_grid && their_cursor == our_cursor && their_rows == our_rows {
            return None;
        }
        let rows_off: Vec<usize> = (0..their_rows.len().max(our_rows.len()))
            .filter(|&row| their_rows.get(row) != our_rows.get(row))
            .collect();
        Some(format!(
            "grid daemon {their_grid:?} copy {our_grid:?}, cursor daemon {their_cursor} copy 
             {our_cursor}, rows that differ {rows_off:?} of {}",
            their_rows.len()
        ))
    }

    fn control(&mut self, mut wanted: impl FnMut(&Control) -> bool) -> Control {
        match self.frame(|frame| matches!(frame, Frame::Control(control) if wanted(control))) {
            Frame::Control(control) => control,
            _ => unreachable!(),
        }
    }

    /// Read until the mirror's text satisfies `done`.
    fn until_text(&mut self, what: &str, mut done: impl FnMut(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.mirror.as_mut().map(|mirror| mirror.text()).unwrap_or_default();
            if done(&text) {
                return text;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.frames.recv_timeout(left) {
                Ok(frame) => self.absorb(&frame),
                Err(_) => panic!("never saw {what}; the screen is:\n{text}"),
            }
        }
    }

    /// The sessions, once the daemon has given them as `wanted` has them.
    fn sessions(&mut self, what: &str, wanted: impl Fn(&[SessionInfo]) -> bool) -> Vec<SessionInfo> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(sessions) = self.sessions.as_ref().filter(|sessions| wanted(sessions)) {
                return sessions.clone();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.frames.recv_timeout(left) {
                Ok(frame) => self.absorb(&frame),
                Err(_) => panic!("never told of {what}; the sessions are {:#?}", self.sessions),
            }
        }
    }

    fn spawn(&mut self, command: &str, size: Size) -> PaneInfo {
        self.send(Control::Spawn {
            request: 1,
            command: command.into(),
            cwd: None,
            beside: None,
            env: vec![],
            size,
        });
        match self.control(|c| matches!(c, Control::Spawned { .. } | Control::Error { .. })) {
            Control::Spawned { info, .. } => info,
            other => panic!("spawn failed: {other:?}"),
        }
    }

    fn panes(&mut self) -> Vec<PaneInfo> {
        self.send(Control::List { request: 7 });
        match self.control(|c| matches!(c, Control::Panes { .. })) {
            Control::Panes { panes, .. } => panes,
            _ => unreachable!(),
        }
    }

    fn snapshot(&mut self, pane: PaneId) -> Terminal {
        self.send(Control::Snapshot { request: 42, pane });
        match self.frame(|frame| matches!(frame, Frame::Snapshot { request: 42, .. })) {
            Frame::Snapshot { bytes, .. } => Terminal::decode(&bytes).expect("snapshot decodes"),
            _ => unreachable!(),
        }
    }

    fn shutdown(mut self) {
        self.send(Control::Shutdown);
    }
}

fn daemon_pid(ui: &Ui) -> u32 {
    match &ui.welcome {
        Control::Welcome { pid, .. } => *pid,
        other => panic!("not a welcome: {other:?}"),
    }
}

#[test]
fn spawn_type_resize_disconnect_reconnect_snapshot() {
    each_conpty("lifecycle", lifecycle);
}

fn lifecycle(session: &str) {
    let session = session.to_owned();
    let mut ui = Ui::connect(&session);
    let Control::Welcome { protocol, ghostty, pid, session: named, files } = ui.welcome.clone() else {
        panic!("no welcome");
    };
    assert!(files, "the daemon says it keeps files");
    assert_eq!((protocol, ghostty.as_str(), named.as_str()), (PROTOCOL_VERSION, GHOSTTY_REVISION, session.as_str()));
    // Through a relay the peer is the relay, which is not the daemon.
    assert_eq!(pid == ui.connection.peer_pid(), !ui.connection.relayed());

    ui.send(Control::Settings {
        settings: TerminalSettings { scrollback_bytes: Some(1 << 26), ..Default::default() },
    });
    let pane = ui.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    assert_ne!(pane.pid, 0);
    assert_ne!(pane.creation_time, 0);
    assert_eq!(pane.exited, None);

    // Attach, then type.
    ui.send(Control::Attach { request: 0, pane: pane.pane, size: size(80, 24) });
    ui.frame(|frame| matches!(frame, Frame::Screen { .. }));
    ui.input(pane.pane, "echo first-marker\r");
    ui.until_text("the echoed line", |text| text.contains("\nfirst-marker"));

    // Resize: the daemon says where in the output its terminal changed
    // size, and the shell sees the new width.
    ui.send(Control::Resize { pane: pane.pane, size: size(100, 30) });
    ui.frame(|frame| matches!(frame, Frame::Resized { .. }));
    assert_eq!(ui.mirror.as_mut().unwrap().grid(), (100, 30));
    ui.input(pane.pane, "mode con\r");
    ui.until_text("the new width", |text| {
        text.lines().any(|line| line.contains("Columns:") && line.trim_end().ends_with("100"))
    });
    assert_eq!(ui.panes()[0].size, size(100, 30));

    // Leave. The shell keeps running and keeps being parsed.
    ui.input(pane.pane, "for /l %i in (1,1,300) do @echo while-away-%i\r");
    ui.connection.close();
    drop(ui);

    let mut ui = Ui::connect(&session);
    assert_eq!(daemon_pid(&ui), pid, "the same daemon");
    let panes = ui.panes();
    assert_eq!(panes.len(), 1);
    assert_eq!((panes[0].pid, panes[0].creation_time), (pane.pid, pane.creation_time));

    // The whole history is there even though nobody was attached for it.
    let deadline = Instant::now() + WAIT;
    let text = loop {
        let text = ui.snapshot(pane.pane).text();
        if text.contains("\nwhile-away-300") {
            break text;
        }
        assert!(Instant::now() < deadline, "the loop never finished:\n{text}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(text.contains("\nfirst-marker"), "{text}");
    assert!(text.contains("\nwhile-away-1\n"), "{text}");

    // Reattach at another size: screen first, then live output.
    ui.send(Control::Attach { request: 0, pane: pane.pane, size: size(90, 20) });
    ui.frame(|frame| matches!(frame, Frame::Screen { .. }));
    assert_eq!(ui.mirror.as_mut().unwrap().grid(), (90, 20));
    ui.until_text("the end of the loop", |text| text.contains("while-away-300"));
    ui.input(pane.pane, "echo after-reattach\r");
    ui.until_text("output after reattach", |text| text.contains("\nafter-reattach"));

    // The shell ending is reported with its exit code, and the pane stays
    // until it is closed.
    ui.input(pane.pane, "exit 7\r");
    let exited = ui.control(|c| matches!(c, Control::Exited { .. }));
    assert_eq!(exited, Control::Exited { pane: pane.pane, code: 7 });
    assert_eq!(ui.panes()[0].exited, Some(7));
    ui.send(Control::Close { pane: pane.pane });
    assert!(ui.panes().is_empty());
    ui.shutdown();
}

#[test]
fn a_ui_through_a_relay() {
    let session = session("ui-relay");
    // The relay starts the daemon, as a UI at the pipe would.
    let mut ui = Ui::through_relay(&session);
    assert!(ui.connection.relayed());
    let pid = daemon_pid(&ui);
    assert_ne!(pid, ui.connection.peer_pid(), "the peer is the relay");

    let pane = ui.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    ui.send(Control::Attach { request: 0, pane: pane.pane, size: size(80, 24) });
    ui.frame(|frame| matches!(frame, Frame::Screen { .. }));
    ui.input(pane.pane, "echo through-the-relay\r");
    ui.until_text("the echoed line", |text| text.contains("\nthrough-the-relay"));
    // The whole terminal, as one large frame.
    assert!(ui.snapshot(pane.pane).text().contains("through-the-relay"));

    // The UI goes: the relay goes with it, and the daemon and shell stay.
    let relay = ui.connection.peer_pid();
    ui.connection.close();
    drop(ui);
    let deadline = Instant::now() + WAIT;
    while running(relay) {
        assert!(Instant::now() < deadline, "the relay outlived its UI");
        std::thread::sleep(Duration::from_millis(20));
    }

    // At the pipe, the same daemon still has the pane.
    let mut ui = Ui::over(pipe::connect(&session).expect("the daemon is still there"));
    assert_eq!(daemon_pid(&ui), pid, "the same daemon");
    let panes = ui.panes();
    assert_eq!(panes.len(), 1);
    assert_eq!((panes[0].pid, panes[0].exited), (pane.pid, None));
    ui.shutdown();
}

#[test]
fn a_file_sent_is_kept_and_its_path_said() {
    let session = session("files");
    let mut ui = Ui::connect(&session);
    // Large enough to take many reads, through a relay as well.
    let bytes: Vec<u8> = (0..3_000_000u32).map(|n| (n * 7 % 251) as u8).collect();
    Frame::File { request: 5, extension: "png".into(), bytes: bytes.clone() }.write(&mut ui.writer).unwrap();
    let Control::FileKept { request, path } = ui.control(|c| matches!(c, Control::FileKept { .. } | Control::Error { .. })) else {
        panic!("not kept");
    };
    assert_eq!(request, 5);
    let kept = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(path.ends_with(".png") && path.contains("pasted"), "{path}");
    assert!(kept == bytes, "the file is what was sent");

    // Refused, with the request it was sent with, and the daemon carries on.
    Frame::File { request: 6, extension: "../x".into(), bytes: vec![1] }.write(&mut ui.writer).unwrap();
    let error = ui.control(|c| matches!(c, Control::FileKept { .. } | Control::Error { .. }));
    assert!(matches!(error, Control::Error { request: Some(6), .. }), "{error:?}");
    assert!(ui.panes().is_empty());
    ui.shutdown();
}

/// Whether a process is still running.
fn running(pid: u32) -> bool {
    let list = std::process::Command::new("tasklist")
        .args(["/fi", &format!("PID eq {pid}"), "/fo", "csv", "/nh"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&list.stdout).contains(&format!("\"{pid}\""))
}

/// A relay to a daemon that cannot be started says why and ends.
#[test]
fn a_relay_that_cannot_reach_a_daemon_says_so() {
    let mut relay = std::process::Command::new(DAEMON);
    relay.args(["--relay", "--session", "not a name"]);
    let error = relay::connect(&mut relay).err().expect("no connection").to_string();
    assert!(error.contains("ended before it was ready"), "{error}");
    assert!(error.contains("a session name is letters"), "{error}");
}

#[test]
fn a_query_with_nothing_attached_does_not_hang() {
    each_conpty("query", query);
}

fn query(session: &str) {
    let mut ui = Ui::connect(session);
    // PowerShell asks the terminal where the cursor is and waits for the
    // answer. No UI is attached to the pane, so with the shipped
    // pseudoconsole only the daemon's terminal can give it. The console
    // hands the program that answer, `ESC [ 1 ; 1 R`, either as its
    // characters or as the F3 key it also spells.
    let script = "$e=[char]27; [Console]::Write(\"$e[6n\"); \
                  do { $k=[Console]::ReadKey($true) } until ($k.KeyChar -eq 'R' -or $k.Key -eq 'F3'); \
                  [Console]::WriteLine('answered'); exit 3";
    let pane = ui.spawn(
        &format!("powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"{}\"", script.replace('"', "\\\"")),
        size(80, 24),
    );
    let exited = ui.control(|c| matches!(c, Control::Exited { .. }));
    assert_eq!(exited, Control::Exited { pane: pane.pane, code: 3 });
    let text = ui.snapshot(pane.pane).text();
    assert!(text.contains("answered"), "{text}");
    ui.shutdown();
}

#[test]
fn a_pane_goes_from_one_ui_to_another() {
    let session = session("shared");
    let mut first = Ui::connect(&session);
    let mut second = Ui::connect(&session);
    assert_eq!(daemon_pid(&first), daemon_pid(&second));

    let pane = first.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    first.send(Control::Attach { request: 0, pane: pane.pane, size: size(80, 24) });
    first.frame(|frame| matches!(frame, Frame::Screen { .. }));
    first.input(pane.pane, "echo seen-by-first\r");
    first.until_text("the first UI's copy", |text| text.contains("\nseen-by-first"));

    // A UI that has left a pane gets no more of its output; the one that
    // takes the pane starts from its screen and gets what follows.
    first.send(Control::Detach { pane: pane.pane });
    first.panes();
    second.send(Control::Attach { request: 0, pane: pane.pane, size: size(80, 24) });
    second.frame(|frame| matches!(frame, Frame::Screen { .. }));
    second.until_text("the screen it starts from", |text| text.contains("\nseen-by-first"));
    second.input(pane.pane, "echo only-second\r");
    second.until_text("output after the attach", |text| text.contains("\nonly-second"));
    first.panes();
    assert!(!first.mirror.as_mut().unwrap().text().contains("only-second"));

    // Closing a running pane ends its shell.
    first.send(Control::Close { pane: pane.pane });
    first.control(|c| matches!(c, Control::Exited { .. }));
    assert!(first.panes().is_empty());
    first.shutdown();
}

#[test]
fn errors_are_reported_and_the_daemon_carries_on() {
    let session = session("errors");
    let mut ui = Ui::connect(&session);
    ui.send(Control::Spawn {
        request: 9,
        command: "no-such-program-nterm.exe".into(),
        cwd: None,
        beside: None,
        env: vec![],
        size: size(80, 24),
    });
    let error = ui.control(|c| matches!(c, Control::Error { .. }));
    assert!(matches!(error, Control::Error { request: Some(9), .. }), "{error:?}");
    ui.send(Control::Snapshot { request: 10, pane: 999 });
    let error = ui.control(|c| matches!(c, Control::Error { .. }));
    assert!(matches!(error, Control::Error { request: Some(10), .. }), "{error:?}");

    // The environment and directory the UI names are the shell's.
    ui.send(Control::Spawn {
        request: 11,
        command: "cmd.exe /d /q /c echo %NTERM_TEST_VALUE% in %CD%".into(),
        cwd: Some(r"C:\Windows".into()),
        beside: None,
        env: vec![
            ("NTERM_TEST_VALUE".into(), "from-the-ui".into()),
            ("SystemRoot".into(), std::env::var("SystemRoot").unwrap()),
            ("PATH".into(), std::env::var("PATH").unwrap()),
        ],
        size: size(80, 24),
    });
    let Control::Spawned { info, .. } = ui.control(|c| matches!(c, Control::Spawned { .. })) else {
        unreachable!()
    };
    ui.control(|c| matches!(c, Control::Exited { .. }));
    let text = ui.snapshot(info.pane).text();
    assert!(text.contains(r"from-the-ui in C:\Windows"), "{text}");
    ui.shutdown();
}

#[test]
fn a_shell_started_beside_a_pane_starts_in_the_folder_that_pane_reported() {
    let session = session("beside");
    let mut ui = Ui::connect(&session);
    let env = || {
        ["SystemRoot", "PATH"].map(|name| (name.to_owned(), std::env::var(name).unwrap())).to_vec()
    };
    // A shell whose prompt reports a folder, as a profile's would.
    let reporting = |ui: &mut Ui, request, folder: &str| {
        ui.send(Control::Spawn {
            request,
            command: format!("cmd.exe /d /q /k prompt $E]7;file://localhost/{folder}$E\\$G"),
            cwd: None,
            beside: None,
            env: env(),
            size: size(80, 24),
        });
        let Control::Spawned { info, .. } = ui.control(|c| matches!(c, Control::Spawned { .. })) else {
            unreachable!()
        };
        info.pane
    };
    let there = reporting(&mut ui, 1, "C:/Windows/System32");
    let gone = reporting(&mut ui, 2, "C:/no-such-folder-nterm");
    let deadline = Instant::now() + WAIT;
    while !ui.panes().iter().filter(|p| p.pane == there || p.pane == gone).all(|p| p.pwd.is_some()) {
        assert!(Instant::now() < deadline, "the shells never reported their folders");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Beside a pane, in its folder; beside one whose folder is not here, or
    // one there is not, in the folder named.
    for (request, beside, wanted) in [
        (3, there, r"in C:\Windows\System32"),
        (4, gone, r"in C:\Windows"),
        (5, 999, r"in C:\Windows"),
    ] {
        ui.send(Control::Spawn {
            request,
            command: "cmd.exe /d /q /c echo in %CD%".into(),
            cwd: Some(r"C:\Windows".into()),
            beside: Some(beside),
            env: env(),
            size: size(80, 24),
        });
        let Control::Spawned { info, .. } =
            ui.control(|c| matches!(c, Control::Spawned { request: r, .. } if *r == request))
        else {
            unreachable!()
        };
        ui.control(|c| matches!(c, Control::Exited { pane, .. } if *pane == info.pane));
        let text = ui.snapshot(info.pane).text();
        let line = text.lines().find(|line| line.starts_with("in ")).unwrap_or_default().trim_end();
        assert!(line.eq_ignore_ascii_case(wanted), "beside {beside}: {text}");
    }
    ui.shutdown();
}

#[test]
fn an_idle_daemon_exits_and_a_second_one_defers() {
    let session = session("idle");
    let mut daemon = std::process::Command::new(DAEMON)
        .args(["--session", &session, "--idle-seconds", "1"])
        .spawn()
        .unwrap();
    // Wait for it to serve, then start a rival: it must stand down at once.
    // Connecting as a UI does would start a daemon if this one were not
    // serving yet, and that one would have the session.
    let deadline = Instant::now() + WAIT;
    while pipe::connect(&session).is_err() {
        assert!(Instant::now() < deadline, "the daemon never served");
        std::thread::sleep(Duration::from_millis(20));
    }
    let ui = Ui::connect(&session);
    assert_eq!(daemon_pid(&ui), daemon.id());
    let rival = std::process::Command::new(DAEMON)
        .args(["--session", &session])
        .status()
        .unwrap();
    assert!(rival.success());

    // With a UI connected it stays; without, it goes.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(daemon.try_wait().unwrap().is_none(), "left while a UI was connected");
    ui.connection.close();
    drop(ui);
    let deadline = Instant::now() + WAIT;
    while daemon.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the idle daemon never left");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::io::stdout().flush();
}

#[test]
fn an_attach_is_answered_and_refused_when_the_pane_is_shown() {
    let session = session("exclusive");
    let mut first = Ui::connect(&session);
    let mut second = Ui::connect(&session);
    let pane = first.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    let answer = |ui: &mut Ui| {
        ui.control(|c| matches!(c, Control::Attached { .. } | Control::Error { .. }))
    };
    let attach = |ui: &mut Ui, request: u32, pane: PaneId, size: Size| {
        ui.send(Control::Attach { request, pane, size });
    };

    // No such pane: the refusal names the request.
    attach(&mut first, 11, pane.pane + 100, size(80, 24));
    assert!(matches!(answer(&mut first), Control::Error { request: Some(11), .. }));

    // The answer comes before the screen, and says the pane is shown.
    attach(&mut first, 12, pane.pane, size(80, 24));
    let Control::Attached { request: 12, info } = answer(&mut first) else {
        panic!("the attach was not answered");
    };
    assert_eq!((info.pane, info.attached), (pane.pane, true));
    first.frame(|frame| matches!(frame, Frame::Screen { .. }));

    // A second UI is refused, and the pane is not resized for it.
    attach(&mut second, 13, pane.pane, size(40, 10));
    assert!(matches!(answer(&mut second), Control::Error { request: Some(13), .. }));
    let listed = second.panes();
    assert_eq!((listed[0].size, listed[0].attached), (size(80, 24), true));

    // Once the first has let go, the second can have it.
    first.send(Control::Detach { pane: pane.pane });
    first.panes();
    attach(&mut second, 14, pane.pane, size(40, 10));
    let Control::Attached { request: 14, info } = answer(&mut second) else {
        panic!("the attach was not answered");
    };
    assert_eq!((info.size, info.attached), (size(40, 10), true));
    second.frame(|frame| matches!(frame, Frame::Screen { .. }));
    second.shutdown();
}

#[test]
fn a_look_is_sent_the_screen_and_takes_nothing() {
    let session = session("look");
    let mut first = Ui::connect(&session);
    let mut second = Ui::connect(&session);
    let pane = first.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));

    // The screen comes, and again when asked again.
    for _ in 0..2 {
        second.send(Control::Peek { pane: pane.pane });
        second.frame(|frame| matches!(frame, Frame::Screen { .. }));
    }
    // The pane is as it was: its size, and nothing showing it.
    let listed = second.panes();
    assert_eq!((listed[0].size, listed[0].attached), (size(80, 24), false));

    // So another UI can have it.
    first.send(Control::Attach { request: 21, pane: pane.pane, size: size(80, 24) });
    let answer = first.control(|c| matches!(c, Control::Attached { .. } | Control::Error { .. }));
    assert!(matches!(answer, Control::Attached { request: 21, .. }));

    // A pane that is not there is nothing to one only looking.
    second.send(Control::Peek { pane: pane.pane + 100 });
    second.panes();
    second.shutdown();
}

/// Set in a pane's shell to make this test program stand in for one that
/// talks to the UI, such as npick. See `a_program_in_a_shell`.
const PROGRAM: &str = "NTERM_TEST_PROGRAM";

/// Not a test of its own: run from a pane's shell by the tests below. It
/// sends `ask`, then prints what comes back, and leaves when told `quit`.
#[test]
fn a_program_in_a_shell() {
    use nterm_session::shell::{Event, Program};
    if std::env::var_os(PROGRAM).is_none() {
        return;
    }
    let mut program = Program::from_environment().expect("in a pane").expect("connect");
    program.send(b"ask").unwrap();
    loop {
        match program.receive(WAIT).expect("nothing came") {
            Event::Message(bytes) if bytes == b"quit" => std::process::exit(0),
            Event::Message(bytes) => println!("got:{}", String::from_utf8_lossy(&bytes)),
            Event::Closed => break println!("was:closed"),
            Event::Refused(why) => break println!("was:refused:{why}"),
            Event::Lost => break println!("was:lost"),
        }
    }
    std::process::exit(0);
}

impl Ui {
    /// A shell in a new pane, shown by this UI.
    fn shell(&mut self) -> PaneId {
        let pane = self.spawn("cmd.exe /d /q /k prompt $G$S", size(200, 24)).pane;
        self.show(pane);
        pane
    }

    fn show(&mut self, pane: PaneId) {
        self.send(Control::Attach { request: 0, pane, size: size(200, 24) });
        self.frame(|frame| matches!(frame, Frame::Screen { .. }));
    }

    /// Start the program in the pane's shell.
    fn run_program(&mut self, pane: PaneId) {
        let this = std::env::current_exe().unwrap();
        let line = format!(
            "set {PROGRAM}=1&\"{}\" --exact a_program_in_a_shell --nocapture\r",
            this.display()
        );
        self.input(pane, &line);
    }

    /// Start the program and take its conversation as far as its first
    /// message.
    fn program(&mut self, pane: PaneId) -> u32 {
        self.run_program(pane);
        let Control::ShellOpen { channel, pane: opened } =
            self.control(|c| matches!(c, Control::ShellOpen { .. }))
        else {
            unreachable!()
        };
        assert_eq!(opened, pane);
        assert_eq!(self.said(), (channel, "ask".to_owned()));
        channel
    }

    /// The program's next message.
    fn said(&mut self) -> (u32, String) {
        match self.frame(|frame| matches!(frame, Frame::Shell { .. })) {
            Frame::Shell { channel, bytes } => (channel, String::from_utf8(bytes).unwrap()),
            _ => unreachable!(),
        }
    }

    fn say(&mut self, channel: u32, text: &str) {
        Frame::Shell { channel, bytes: text.as_bytes().to_vec() }.write(&mut self.writer).unwrap();
    }

    fn sees(&mut self, text: &str) {
        self.until_text(text, |screen| screen.contains(text));
    }

    fn closed(&mut self, channel: u32) {
        self.control(|c| matches!(c, Control::ShellClosed { channel: n } if *n == channel));
    }
}

#[test]
fn a_program_and_the_ui_showing_its_pane_talk_until_either_is_done() {
    let session = session("relay");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();

    // The shell is told where it is.
    ui.input(pane, "echo at:%NTERM_DAEMON%:%NTERM_PANE%:%NTERM_DAEMON_PIPE%\r");
    ui.sees(&format!("at:{session}:{pane}:{}", pipe::name(&session).unwrap()));

    // Both ways, and the UI is done first: the program is told.
    let channel = ui.program(pane);
    ui.say(channel, "answer");
    ui.sees("got:answer");
    ui.send(Control::ShellClosed { channel });
    ui.sees("was:closed");

    // The program is done first: the UI is told.
    let next = ui.program(pane);
    assert_ne!(next, channel);
    ui.say(next, "quit");
    ui.closed(next);

    // Nothing is passed to a conversation that is over.
    ui.say(next, "late");
    ui.panes();
    ui.shutdown();
}

#[test]
fn a_program_is_told_when_the_ui_leaves_its_pane() {
    let session = session("relay-leave");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();

    // The UI lets go of the pane.
    ui.program(pane);
    ui.send(Control::Detach { pane });
    ui.panes();
    ui.show(pane);
    ui.sees("was:closed");

    // The UI goes altogether.
    ui.input(pane, "cls\r");
    ui.until_text("a cleared screen", |screen| !screen.contains("was:closed"));
    ui.program(pane);
    // The pipe closes with the last handle to it.
    ui.connection.close();
    drop(ui);
    let mut ui = Ui::connect(&session);
    ui.show(pane);
    ui.sees("was:closed");

    // The shell is ended under the program: the UI is told that too.
    let mut other = Ui::connect(&session);
    let second = other.shell();
    let channel = other.program(second);
    other.send(Control::Close { pane: second });
    other.closed(channel);
    ui.shutdown();
}

#[test]
fn a_program_is_refused_for_no_pane_or_with_no_ui() {
    use nterm_session::shell::{Event, Program};
    let session = session("relay-refuse");
    let mut ui = Ui::connect(&session);
    let pane = ui.spawn("cmd.exe /d /q /k prompt $G$S", size(200, 24)).pane;

    let nowhere = Program::connect(&session, pane + 100).unwrap();
    assert!(matches!(nowhere.receive(WAIT), Some(Event::Refused(_))));

    // Nothing shows the pane. Its screen is read without showing it.
    ui.run_program(pane);
    let wanted = "was:refused:no window is showing this pane";
    let deadline = Instant::now() + WAIT;
    while !ui.snapshot(pane).text().contains(wanted) {
        assert!(Instant::now() < deadline, "the program was not refused");
        std::thread::sleep(Duration::from_millis(50));
    }
    ui.shutdown();
}

#[test]
fn uis_are_told_of_the_sessions_as_they_change() {
    let session = session("sessions");
    let mut ui = Ui::connect(&session);
    assert!(ui.sessions("none at first", |_| true).is_empty());

    // Shells in no session are given as one with no name.
    let shell = "cmd.exe /d /q /k prompt $G$S";
    let (a, b) = (ui.spawn(shell, size(80, 24)).pane, ui.spawn(shell, size(80, 24)).pane);
    let told = ui.sessions("two strays", |told| told.len() == 1 && told[0].panes.len() == 2);
    assert_eq!((told[0].id.as_str(), told[0].layout.as_str()), ("", ""));

    let set = |id: &str, panes: &[PaneId], layout: &str| Control::SessionSet {
        id: id.into(),
        panes: panes.to_vec(),
        layout: layout.into(),
    };
    let panes = |told: &SessionInfo| told.panes.iter().map(|pane| pane.pane).collect::<Vec<_>>();
    ui.send(set("one", &[a], "first layout"));
    let told = ui.sessions("a session and a stray", |told| told.len() == 2);
    assert_eq!((told[0].id.as_str(), told[0].layout.as_str()), ("one", "first layout"));
    assert_eq!((panes(&told[0]), panes(&told[1])), (vec![a], vec![b]));
    assert_eq!(told[1].id, "");

    // A pane is in the session that named it last, and one left with no
    // pane is forgotten. The panes come in the order the session gave.
    ui.send(set("two", &[b, a], "second layout"));
    let told = ui.sessions("one session", |told| told.len() == 1 && told[0].id == "two");
    assert_eq!((panes(&told[0]), told[0].layout.as_str()), (vec![b, a], "second layout"));

    // Another UI starts from what there is, and hears of what this one does.
    let mut other = Ui::connect(&session);
    let told = other.sessions("what there is", |told| told.len() == 1);
    assert!(told[0].panes.iter().all(|pane| !pane.attached));
    ui.send(Control::Attach { request: 0, pane: a, size: size(80, 24) });
    other.sessions("the attach", |told| {
        told.len() == 1 && told[0].panes.iter().any(|pane| pane.pane == a && pane.attached)
    });
    ui.send(Control::Detach { pane: a });
    other.sessions("the detach", |told| {
        told.len() == 1 && told[0].panes.iter().all(|pane| !pane.attached)
    });

    // A shell nothing shows changes its title: nothing was asked of the
    // daemon, and the UIs are told.
    ui.input(b, "title later\r");
    other.sessions("the title", |told| {
        let titled = |pane: &PaneInfo| pane.title.as_deref().is_some_and(|title| title.contains("later"));
        told[0].panes.iter().any(titled)
    });

    // A session is told without its shells that have ended, and with none
    // left it is forgotten.
    ui.send(Control::Close { pane: a });
    let told = other.sessions("one shell", |told| told.len() == 1 && told[0].panes.len() == 1);
    assert_eq!((told[0].id.as_str(), panes(&told[0])), ("two", vec![b]));
    ui.send(Control::Close { pane: b });
    other.sessions("none", |told| told.is_empty());
    ui.shutdown();
}
#[test]
fn a_copy_resized_in_order_stays_the_same_as_the_daemon() {
    each_conpty("ordered", ordered_resizes);
}

/// A UI's copy is resized where the daemon's terminal was, and never given
/// the screen again. After every step it must be what the daemon has.
fn ordered_resizes(name: &str) {
    let session = session(name);
    let mut ui = Ui::connect(&session);
    let pane = ui.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    ui.send(Control::Attach { request: 0, pane: pane.pane, size: size(80, 24) });
    ui.frame(|frame| matches!(frame, Frame::Screen { .. }));
    // More than a screen of lines, some of them longer than the screen is
    // wide, so there is history and there are wrapped lines.
    ui.input(
        pane.pane,
        "for /l %i in (1,1,60) do @echo line-%i-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
         bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-%i\r",
    );
    ui.until_text("the lines", |text| text.contains("b-60\n"));
    ui.input(pane.pane, "echo settled\r");
    ui.until_text("the prompt", |text| text.contains("\nsettled"));

    let mut failures = Vec::new();
    let steps: [(u16, u16); 12] = [
        (80, 40),
        (80, 12),
        (60, 12),
        (120, 30),
        (40, 10),
        (200, 50),
        (80, 24),
        (81, 24),
        (79, 24),
        (30, 60),
        (160, 8),
        (80, 24),
    ];
    for (at, (cols, rows)) in steps.into_iter().enumerate() {
        ui.send(Control::Resize { pane: pane.pane, size: size(cols, rows) });
        ui.frame(|frame| matches!(frame, Frame::Resized { .. }));
        if let Some(what) = ui.differs_from_daemon(pane.pane) {
            failures.push(format!("step {at} to {cols}x{rows}, at once: {what}"));
        }
        let marker = format!("step-{at}-done");
        ui.input(pane.pane, &format!("echo {marker}\r"));
        ui.until_text("the step's echo", |text| text.contains(&format!("\n{marker}")));
        if let Some(what) = ui.differs_from_daemon(pane.pane) {
            failures.push(format!("step {at} to {cols}x{rows}, after output: {what}"));
        }
    }
    // Several sizes with no time between them, as a dragged edge sends.
    for cols in (60..=120).step_by(3) {
        ui.send(Control::Resize { pane: pane.pane, size: size(cols, 30) });
    }
    ui.input(pane.pane, "echo dragged\r");
    ui.until_text("the echo after the drag", |text| text.contains("\ndragged"));
    std::thread::sleep(Duration::from_millis(500));
    if let Some(what) = ui.differs_from_daemon(pane.pane) {
        failures.push(format!("after a drag: {what}"));
    }
    ui.shutdown();
    assert!(failures.is_empty(), "{name}:\n{}", failures.join("\n"));
}

/// What the daemon's log says of the session, once it says what is wanted.
fn logged(session: &str, what: &str, wanted: impl Fn(&str) -> bool) -> String {
    let path = nterm_daemon::log::directory().join(format!("daemon-{session}.log"));
    let deadline = Instant::now() + WAIT;
    loop {
        let log = std::fs::read_to_string(&path).unwrap_or_default();
        if wanted(&log) {
            return log;
        }
        assert!(Instant::now() < deadline, "the log never said {what}:\n{log}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_pseudoconsole_that_goes_on_its_own_is_logged() {
    each_conpty("console-gone", |session| {
        let mut ui = Ui::connect(session);
        let pane = ui.shell();
        ui.input(pane, "echo ready\r");
        ui.until_text("the shell's echo", |text| text.contains("\nready"));
        // What runs a pseudoconsole is a process the daemon started.
        let ended = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "$hosts = @(Get-CimInstance Win32_Process -Filter \"ParentProcessId={} and \
                 (Name='OpenConsole.exe' or Name='conhost.exe')\"); \
                 $hosts | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}; $hosts.Count",
                daemon_pid(&ui)
            ))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&ended.stdout).trim(), "1", "{ended:?}");
        // Its output ends before the shell is known to have, and is said to.
        let ended = format!("pane {pane}: output ended (error ");
        let log = logged(session, "that the output ended", |log| log.contains(&ended));
        assert!(log.contains("with its shell still running"), "{log}");
        // Nothing more is taken for the shell, which is said as well. The
        // first bytes after may still be taken, so more than one are sent.
        let refused = format!("pane {pane}: input not taken (error ");
        let deadline = Instant::now() + WAIT;
        while !logged(session, "anything", |_| true).contains(&refused) {
            assert!(Instant::now() < deadline, "input was never refused");
            ui.input(pane, "echo more\r");
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("{session}:\n{}", logged(session, "anything", |_| true));
        ui.shutdown();
    });
}

/// What a picker the daemon runs said was found.
struct Rows {
    /// The names of its columns, of a picker that has them.
    header: Option<String>,
    serial: u64,
    query: String,
    matched_against: String,
    rows: Vec<(u64, String)>,
    matched: u64,
}
impl Rows {
    fn texts(&self) -> Vec<&str> {
        let mut texts: Vec<&str> = self.rows.iter().map(|(_, text)| text.as_str()).collect();
        texts.sort();
        texts
    }
}

/// A folder of a few files and folders, removed when dropped.
struct Tree(std::path::PathBuf);
impl Tree {
    fn new(test: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nterm-pick-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for file in ["a/alpha.txt", "a/beta.txt", "b/gamma.rs", "top.md"] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file).unwrap();
        }
        Self(root)
    }
    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A program that has asked the daemon for a picker.
struct Asker(nterm_session::shell::Program);
impl Asker {
    fn ask(session: &str, pane: PaneId, picker: &str, params: serde_json::Value) -> Self {
        let mut program = nterm_session::shell::Program::pick(session, pane).expect("connect");
        let open = serde_json::json!({
            "type": "open", "request_id": "r1", "picker": picker, "params": params,
        });
        // Refused already for a pane there is not, and nothing more is read.
        let _ = program.send(open.to_string().as_bytes());
        Self(program)
    }
    /// What the daemon says next.
    fn told(&self) -> serde_json::Value {
        use nterm_session::shell::Event;
        match self.0.receive(WAIT).expect("the daemon said nothing") {
            Event::Message(bytes) => serde_json::from_slice(&bytes).unwrap(),
            Event::Closed => serde_json::json!({"type": "closed"}),
            Event::Refused(why) => serde_json::json!({"type": "refused", "message": why}),
            Event::Lost => serde_json::json!({"type": "lost"}),
        }
    }
    /// How the picker ended: its status, and the rest of the result.
    fn result(&self) -> (String, serde_json::Value) {
        let told = self.told();
        assert_eq!((&told["type"], &told["request_id"]), (&"result".into(), &"r1".into()), "{told}");
        (told["status"].as_str().unwrap().to_owned(), told)
    }
}

impl Ui {
    /// A program asks for a picker over `pane`, and this UI is asked to
    /// show it.
    fn picker(
        &mut self,
        session: &str,
        pane: PaneId,
        name: &str,
        params: serde_json::Value,
    ) -> (Asker, u32) {
        let asker = Asker::ask(session, pane, name, params);
        let Control::PickerOpen { picker, pane: Some(over), .. } =
            self.control(|c| matches!(c, Control::PickerOpen { .. }))
        else {
            unreachable!()
        };
        assert_eq!(over, pane);
        let opened = asker.told();
        assert_eq!((&opened["type"], &opened["request_id"]), (&"opened".into(), &"r1".into()));
        (asker, picker)
    }

    /// Ask a picker for `query`, and take what it finds until its source
    /// has all it will have.
    fn query(&mut self, picker: u32, revision: u64, query: &str) -> Rows {
        self.send(Control::PickerQuery { picker, revision, query: query.into(), rows: 200 });
        loop {
            let found = self
                .control(|c| matches!(c, Control::PickerRows { picker: of, .. } if *of == picker));
            let Control::PickerRows {
                serial, revision: of, query, matched_against, rows, matched, done, header, ..
            } = found
            else {
                unreachable!()
            };
            self.send(Control::PickerSeen { picker, serial });
            if of == revision && done {
                let rows = rows.into_iter().map(|row| (row.index, row.text)).collect();
                return Rows { serial, query, matched_against, rows, matched, header };
            }
        }
    }
}

#[test]
fn a_program_is_run_a_picker_of_files_and_given_the_one_chosen() {
    let session = session("pick-files");
    let tree = Tree::new("files");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let params = serde_json::json!({"cwd": tree.path()});
    let (asker, picker) = ui.picker(&session, pane, "files", params);

    // Everything under the folder, said from there, and not the folders.
    let all = ui.query(picker, 0, "");
    assert_eq!(all.texts(), ["a\\alpha.txt", "a\\beta.txt", "b\\gamma.rs", "top.md"]);
    assert_eq!(all.matched, 4);

    let some = ui.query(picker, 1, "gam");
    assert_eq!((some.query.as_str(), some.matched_against.as_str()), ("gam", "gam"));
    assert_eq!(some.texts(), ["b\\gamma.rs"]);

    // The UI says only which row; the program is given what it is.
    let (row, _) = some.rows[0];
    ui.send(Control::PickerAccept { picker, serial: some.serial, row });
    let (status, result) = asker.result();
    assert_eq!(status, "accepted");
    assert_eq!(result["query"], "gam");
    assert_eq!(result["selected"][0]["text"], "b\\gamma.rs");
    let whole = tree.0.join("b").join("gamma.rs");
    assert_eq!(result["selected"][0]["value"], whole.to_string_lossy().as_ref());
    // The conversation is over, and the daemon closed it.
    assert!(matches!(asker.told()["type"].as_str(), Some("closed" | "lost")));
    // Nothing is said of a picker that is over.
    ui.send(Control::PickerQuery { picker, revision: 2, query: "a".into(), rows: 200 });
    ui.panes();
    ui.shutdown();
}

#[test]
fn a_picker_of_folders_and_the_ways_a_ui_ends_one() {
    let session = session("pick-folders");
    let tree = Tree::new("folders");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let params = || {
        let roots = serde_json::to_string(&[tree.path()]).unwrap();
        serde_json::json!({"roots": roots, "query": "b"})
    };

    // It starts with the query it was asked for with.
    let asker = Asker::ask(&session, pane, "directories", params());
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. }));
    let Control::PickerOpen { picker, label, query, .. } = opened else { unreachable!() };
    assert_eq!((label.as_str(), query.as_str()), ("Directories", "b"));
    assert_eq!(asker.told()["type"], "opened");
    assert_eq!(ui.query(picker, 0, "b").texts(), ["b"]);
    assert_eq!(ui.query(picker, 1, "").texts(), ["a", "b"]);

    // Closed with nothing chosen.
    ui.send(Control::PickerClose { picker, refused: None });
    assert_eq!(asker.result().0, "cancelled");

    // Not shown at all.
    let (asker, picker) = ui.picker(&session, pane, "directories", params());
    ui.send(Control::PickerClose { picker, refused: Some("busy".into()) });
    let (status, result) = asker.result();
    assert_eq!(status, "error");
    assert_eq!((&result["code"], &result["message"]), (&"refused".into(), &"busy".into()));

    // A row it was not sent.
    let (asker, picker) = ui.picker(&session, pane, "directories", params());
    let found = ui.query(picker, 0, "");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: 9999 });
    let (status, result) = asker.result();
    assert_eq!((status.as_str(), &result["code"]), ("error", &"no_such_row".into()));
    ui.shutdown();
}

#[test]
fn a_picker_ends_when_its_program_or_its_ui_goes() {
    let session = session("pick-ends");
    let tree = Tree::new("ends");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let params = || serde_json::json!({"cwd": tree.path()});
    let closed = |ui: &mut Ui, picker: u32| {
        ui.control(|c| matches!(c, Control::PickerClosed { picker: of, .. } if *of == picker));
    };

    // The program says it is done with it.
    let (mut asker, picker) = ui.picker(&session, pane, "files", params());
    asker.0.send(b"{\"type\":\"cancel\"}").unwrap();
    closed(&mut ui, picker);
    assert_eq!(asker.result().0, "cancelled");

    // The program goes.
    let (asker, picker) = ui.picker(&session, pane, "files", params());
    drop(asker);
    closed(&mut ui, picker);

    // The UI lets go of the pane.
    let (asker, picker) = ui.picker(&session, pane, "files", params());
    ui.send(Control::Detach { pane });
    closed(&mut ui, picker);
    assert_eq!(asker.result().0, "cancelled");
    ui.show(pane);

    // The shell of the pane ends.
    let (asker, picker) = ui.picker(&session, pane, "files", params());
    let mut other = Ui::connect(&session);
    let second = other.shell();
    let (theirs, _) = other.picker(&session, second, "files", params());
    ui.send(Control::Close { pane });
    closed(&mut ui, picker);
    assert_eq!(asker.result().0, "cancelled");

    // The UI goes altogether.
    other.connection.close();
    drop(other);
    assert_eq!(theirs.result().0, "cancelled");
    ui.shutdown();
}

#[test]
fn a_picker_is_refused_for_what_cannot_be_shown_or_is_not_one() {
    let session = session("pick-refuse");
    let tree = Tree::new("refuse");
    let mut ui = Ui::connect(&session);
    let pane = ui.spawn("cmd.exe /d /q /k prompt $G$S", size(200, 24)).pane;
    let params = serde_json::json!({"cwd": tree.path()});

    let nowhere = Asker::ask(&session, pane + 100, "files", params.clone());
    assert_eq!(nowhere.told()["type"], "refused");

    // Nothing shows the pane.
    let (status, result) = Asker::ask(&session, pane, "files", params.clone()).result();
    assert_eq!((status.as_str(), &result["code"]), ("error", &"no_window".into()));

    ui.show(pane);
    let missing = tree.0.join("none").to_string_lossy().into_owned();
    for (name, params) in [
        ("themes", params),
        ("files", serde_json::json!({})),
        ("files", serde_json::json!({"cwd": missing})),
    ] {
        let (status, result) = Asker::ask(&session, pane, name, params).result();
        assert_eq!((status.as_str(), &result["code"]), ("error", &"bad_request".into()), "{result}");
    }
    ui.shutdown();
}

#[test]
fn a_client_that_comes_and_goes_before_it_is_waited_for_does_not_stop_the_next() {
    // The daemon's own listener, held here so that nothing waits on it
    // while the first client comes and goes.
    let session = session("came-and-went");
    let mut listener = pipe::Listener::bind(&session).unwrap();
    drop(pipe::connect(&session).unwrap());
    let refused = listener.accept().err().expect("the client that went was not one");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    let client = std::thread::spawn({
        let session = session.clone();
        move || pipe::connect(&session).map(drop)
    });
    listener.accept().expect("the next client is taken");
    client.join().unwrap().unwrap();
}

impl Ui {
    /// Ask what shows for a row of an answer, and take the answer to that.
    fn preview(&mut self, picker: u32, request: u64, serial: u64, row: u64) -> Control {
        self.send(Control::PickerPreview { picker, request, serial, row });
        self.control(
            |c| matches!(c, Control::PickerPreviewed { request: of, .. } if *of == request),
        )
    }
}

#[test]
fn a_ui_is_sent_what_shows_for_a_row_of_a_picker() {
    let session = session("pick-preview");
    let tree = Tree::new("preview");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();

    // Files: hidden unless asked for shown, and the file as it is.
    let asker = Asker::ask(&session, pane, "files", serde_json::json!({"cwd": tree.path()}));
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. }));
    let Control::PickerOpen { picker, preview, .. } = opened else { unreachable!() };
    assert_eq!(preview, Some(false));
    assert_eq!(asker.told()["type"], "opened");
    let found = ui.query(picker, 0, "gam");
    let (row, _) = found.rows[0];
    let shown = ui.preview(picker, 1, found.serial, row);
    let Control::PickerPreviewed { text, name, truncated, error, .. } = shown else { unreachable!() };
    // The tree's files hold their own names.
    assert_eq!((text.as_str(), name.as_deref()), ("b/gamma.rs", Some("gamma.rs")));
    assert_eq!((truncated, error), (false, None));

    // A row that is not one of an answer's is said to be none, and the
    // picker goes on.
    let shown = ui.preview(picker, 2, found.serial, 9999);
    let Control::PickerPreviewed { error, .. } = shown else { unreachable!() };
    assert!(error.is_some());
    assert_eq!(ui.query(picker, 1, "alp").texts(), ["a\\alpha.txt"]);

    // A picture is sent as the file it is, and not as text. It is made
    // after the picker opened: what is listed is found again by asking.
    let picture = [137u8, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 255];
    std::fs::write(std::path::Path::new(&tree.path()).join("top.md"), picture).unwrap();
    std::fs::rename(
        std::path::Path::new(&tree.path()).join("top.md"),
        std::path::Path::new(&tree.path()).join("Top.PNG"),
    )
    .unwrap();
    ui.send(Control::PickerClose { picker, refused: None });
    assert_eq!(asker.result().0, "cancelled");
    let (asker, picker) = ui.picker(&session, pane, "files", serde_json::json!({"cwd": tree.path()}));
    let found = ui.query(picker, 0, "top");
    assert_eq!(found.texts(), ["Top.PNG"]);
    ui.send(Control::PickerPreview { picker, request: 7, serial: found.serial, row: found.rows[0].0 });
    let sent = ui.frame(|frame| matches!(frame, Frame::Previewed { .. }));
    let Frame::Previewed { picker: of, request, kind, bytes, .. } = sent else { unreachable!() };
    assert_eq!((of, request, kind), (picker, 7, nterm_session::protocol::PreviewKind::Image));
    assert_eq!(bytes, picture);
    ui.send(Control::PickerClose { picker, refused: None });
    assert_eq!(asker.result().0, "cancelled");

    // Folders: what is in the one selected.
    let params = serde_json::json!({"cwd": tree.path(), "preview": "true"});
    let asker = Asker::ask(&session, pane, "directories", params);
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. }));
    let Control::PickerOpen { picker, preview, .. } = opened else { unreachable!() };
    assert_eq!(preview, Some(true));
    assert_eq!(asker.told()["type"], "opened");
    let found = ui.query(picker, 0, "a");
    let (row, _) = found.rows.iter().find(|(_, text)| text == "a").unwrap().clone();
    let shown = ui.preview(picker, 1, found.serial, row);
    let Control::PickerPreviewed { text, name, .. } = shown else { unreachable!() };
    assert_eq!((text.as_str(), name), ("alpha.txt\nbeta.txt", None));
    ui.send(Control::PickerClose { picker, refused: None });
    assert_eq!(asker.result().0, "cancelled");
    ui.shutdown();
}

impl Asker {
    /// Say something more to the daemon, and take what it answers.
    fn say(&mut self, what: serde_json::Value) -> serde_json::Value {
        self.0.send(what.to_string().as_bytes()).expect("send");
        self.told()
    }
}

#[test]
fn a_picker_of_what_was_typed_lists_the_last_first_and_gives_a_command_whole() {
    let session = session("pick-history");
    let tree = Tree::new("history");
    // The second command is of two lines, and the first was typed twice.
    let typed = tree.0.join("history.txt");
    std::fs::write(&typed, "git status\r\nls`\r\n  -Force\r\ngit status\r\ncargo build\r\n").unwrap();
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();

    let params = serde_json::json!({"path": typed.to_string_lossy(), "query": "g", "position": "cursor"});
    let asker = Asker::ask(&session, pane, "history", params);
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. }));
    let Control::PickerOpen { picker, label, query, preview, at_cursor, .. } = opened else { unreachable!() };
    // No previews, and where the cursor is.
    assert_eq!((label.as_str(), query.as_str(), preview, at_cursor), ("History", "g", None, true));
    assert_eq!(asker.told()["type"], "opened");

    // The last typed first, each once, and a command of two lines as one.
    let all = ui.query(picker, 0, "");
    let texts: Vec<&str> = all.rows.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["cargo build", "git status", "ls`   -Force"]);
    // What matches stays in that order, whichever matches best.
    let some = ui.query(picker, 1, "s");
    let texts: Vec<&str> = some.rows.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["git status", "ls`   -Force"]);
    // There is nothing to show for a row.
    let (row, _) = some.rows[1];
    let Control::PickerPreviewed { error, .. } = ui.preview(picker, 1, some.serial, row) else { unreachable!() };
    assert!(error.is_some());

    // The program is given the command as it was typed, on its two lines.
    ui.send(Control::PickerAccept { picker, serial: some.serial, row });
    let (status, result) = asker.result();
    assert_eq!((status.as_str(), &result["query"]), ("accepted", &"s".into()));
    assert_eq!(result["selected"][0]["text"], "ls`   -Force");
    assert_eq!(result["selected"][0]["value"], "ls\n  -Force");

    // A file there is not is refused, with no picker shown.
    let asker = Asker::ask(&session, pane, "history", serde_json::json!({"path": tree.0.join("none").to_string_lossy()}));
    let (status, result) = asker.result();
    assert_eq!((status.as_str(), &result["code"]), ("error", &"bad_request".into()));
    ui.shutdown();
}

#[test]
fn a_picker_of_rows_its_program_gives_and_what_the_program_is_told_of_one() {
    let session = session("pick-items");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();

    let mut asker = Asker::ask(&session, pane, "items", serde_json::json!({"label": "Completions"}));
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. }));
    let Control::PickerOpen { picker, label, preview, at_cursor, .. } = opened else { unreachable!() };
    assert_eq!((label.as_str(), preview, at_cursor), ("Completions", None, false));
    assert_eq!(asker.told()["type"], "opened");

    // Each lot of rows is answered by its number.
    let ack = asker.say(serde_json::json!({"type": "items", "request_id": "r1", "sequence": 0, "items": [
        {"id": "a", "text": "Get-Item", "value": "Get-Item"},
        {"id": "b", "text": "Get-ChildItem", "value": {"x": 1}},
    ]}));
    assert_eq!((&ack["type"], &ack["request_id"], &ack["sequence"]), (&"ack".into(), &"r1".into(), &0.into()));
    // They are found before the program has given them all.
    ui.send(Control::PickerQuery { picker, revision: 0, query: String::new(), rows: 200 });
    loop {
        let found = ui.control(|c| matches!(c, Control::PickerRows { .. }));
        let Control::PickerRows { serial, rows, done, .. } = found else { unreachable!() };
        ui.send(Control::PickerSeen { picker, serial });
        if rows.len() == 2 {
            assert!(!done);
            break;
        }
    }
    // A row with no name or value of its own: its place, and what it shows.
    let ack = asker.say(serde_json::json!({"type": "items", "request_id": "r1", "sequence": 1, "items": [
        {"text": "two\nlines"},
    ]}));
    assert_eq!((&ack["type"], &ack["sequence"]), (&"ack".into(), &1.into()));
    let ended = asker.say(serde_json::json!({"type": "end_items", "request_id": "r1", "last_sequence": 1}));
    assert_eq!((&ended["type"], &ended["request_id"]), (&"input_complete".into(), &"r1".into()));
    // Rows given after the last are not taken, and are answered all the same.
    let ack = asker.say(serde_json::json!({"type": "items", "request_id": "r1", "sequence": 2, "items": [
        {"text": "late"},
    ]}));
    assert_eq!(ack["type"], "ack");

    let all = ui.query(picker, 1, "");
    assert_eq!(all.texts(), ["Get-ChildItem", "Get-Item", "two lines"]);
    // The program is told its own name for the row chosen, and its value.
    let some = ui.query(picker, 2, "child");
    assert_eq!(some.texts(), ["Get-ChildItem"]);
    ui.send(Control::PickerAccept { picker, serial: some.serial, row: some.rows[0].0 });
    let (status, result) = asker.result();
    assert_eq!(status, "accepted");
    let chosen = &result["selected"][0];
    assert_eq!((&chosen["id"], &chosen["text"]), (&"b".into(), &"Get-ChildItem".into()));
    assert_eq!(chosen["value"], serde_json::json!({"x": 1}));

    let (mut asker, picker) = ui.picker(&session, pane, "items", serde_json::json!({}));
    asker.say(serde_json::json!({"type": "items", "request_id": "r1", "sequence": 0, "items": [
        {"text": "one"}, {"text": "two\nlines"},
    ]}));
    asker.say(serde_json::json!({"type": "end_items", "request_id": "r1"}));
    let some = ui.query(picker, 0, "lines");
    ui.send(Control::PickerAccept { picker, serial: some.serial, row: some.rows[0].0 });
    let chosen = asker.result().1["selected"][0].clone();
    assert_eq!((&chosen["id"], &chosen["text"], &chosen["value"]), (&"1".into(), &"two lines".into(), &"two\nlines".into()));

    // Rows given a picker that lists something else are answered, not listed.
    let tree = Tree::new("items");
    let (mut asker, picker) = ui.picker(&session, pane, "files", serde_json::json!({"cwd": tree.path()}));
    let ack = asker.say(serde_json::json!({"type": "items", "request_id": "r1", "sequence": 0, "items": [{"text": "x"}]}));
    assert_eq!(ack["type"], "ack");
    assert_eq!(ui.query(picker, 0, "").matched, 4);
    ui.send(Control::PickerClose { picker, refused: None });
    assert_eq!(asker.result().0, "cancelled");
    ui.shutdown();
}

/// A daemon serving `session` whose file of pickers, in `folder`, is
/// `text`, with the modules that come with nterm beside it.
fn daemon_with_file(session: &str, folder: &std::path::Path, text: &str) -> std::process::Child {
    let modules = folder.join("nterm");
    std::fs::create_dir_all(&modules).unwrap();
    let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lua/nterm");
    for module in std::fs::read_dir(shipped).unwrap() {
        let module = module.unwrap().path();
        std::fs::copy(&module, modules.join(module.file_name().unwrap())).unwrap();
    }
    let file = folder.join("nterm-daemon.lua");
    std::fs::write(&file, text).unwrap();
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", session, "--lua-config"]).arg(&file).env_remove("NTERM_CONPTY_DLL");
    // It holds nothing of the test's, so that a test that fails ends.
    command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let daemon = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    daemon
}

#[test]
fn a_picker_of_the_lines_a_search_finds_and_one_whose_search_fails() {
    let session = session("pick-ripgrep");
    let tree = Tree::new("ripgrep");
    // The file is not in the tree searched, so that it is not found in it.
    let config = Tree::new("ripgrep-file");
    let text = r#"local ripgrep = require("nterm.ripgrep")
        return { pickers = { Ripgrep = ripgrep.search { preview_visible = false } } }"#;
    let mut daemon = daemon_with_file(&session, &config.0, text);
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();

    // A search that cannot be run, or that ends saying it failed with
    // nothing found, ends the picker, and both are told why.
    let asker = Asker::ask(&session, pane, "Ripgrep", serde_json::json!({"cwd": tree.path(), "pattern": "("}));
    let Control::PickerOpen { picker, label, preview, .. } = ui.control(|c| matches!(c, Control::PickerOpen { .. }))
    else {
        unreachable!()
    };
    assert_eq!((label.as_str(), preview), ("Ripgrep", Some(false)));
    let closed = ui.control(|c| matches!(c, Control::PickerClosed { picker: of, .. } if *of == picker));
    let Control::PickerClosed { message, .. } = closed else { unreachable!() };
    assert!(message.is_some());
    // It may have ended before the program was told it had opened.
    let mut told = asker.told();
    if told["type"] == "opened" {
        told = asker.told();
    }
    assert_eq!((&told["type"], &told["status"], &told["code"]), (&"result".into(), &"error".into(), &"source_failed".into()));

    // The rest is of what `rg` finds, where there is one to run.
    if std::process::Command::new("rg").arg("--version").output().is_err() {
        eprintln!("rg is not installed: what a search finds was not tested");
        ui.shutdown();
        assert!(daemon.wait().unwrap().success());
        return;
    }
    // The tree's files hold their own names. Whatever the case of a letter.
    let params = serde_json::json!({"cwd": tree.path(), "pattern": "ALPHA|gamma"});
    let (asker, picker) = ui.picker(&session, pane, "Ripgrep", params);
    let all = ui.query(picker, 0, "");
    assert_eq!(all.texts(), ["a\\alpha.txt:1:3:a/alpha.txt", "b\\gamma.rs:1:3:b/gamma.rs"]);
    // What is typed narrows what was found: the search is not run again.
    let some = ui.query(picker, 1, "rs");
    assert_eq!(some.texts(), ["b\\gamma.rs:1:3:b/gamma.rs"]);
    ui.send(Control::PickerAccept { picker, serial: some.serial, row: some.rows[0].0 });
    let (status, result) = asker.result();
    assert_eq!(status, "accepted");
    assert_eq!(result["selected"][0]["value"], "b\\gamma.rs:1:3:b/gamma.rs");

    // A row shows its file, at the line it was found on. One more line is
    // put before what was found, so that it is not the first.
    std::fs::write(std::path::Path::new(&tree.path()).join("b").join("gamma.rs"), "// first\nb/gamma.rs\n").unwrap();
    let params = serde_json::json!({"cwd": tree.path(), "pattern": "gamma", "preview": "true"});
    let (asker, picker) = ui.picker(&session, pane, "Ripgrep", params);
    let found = ui.query(picker, 0, "");
    assert_eq!(found.texts(), [r"b\gamma.rs:2:3:b/gamma.rs"]);
    let shown = ui.preview(picker, 1, found.serial, found.rows[0].0);
    let Control::PickerPreviewed { text, name, line, error, .. } = shown else { unreachable!() };
    assert_eq!((text.as_str(), name.as_deref(), line, error), ("// first\nb/gamma.rs\n", Some("gamma.rs"), Some(2), None));
    ui.send(Control::PickerClose { picker, refused: None });
    assert_eq!(asker.result().0, "cancelled");

    // Nothing found is a picker of nothing, not one that failed.
    let params = serde_json::json!({"cwd": tree.path(), "pattern": "nowhere in any of them"});
    let (asker, picker) = ui.picker(&session, pane, "Ripgrep", params);
    assert_eq!(ui.query(picker, 0, "").matched, 0);
    ui.send(Control::PickerClose { picker, refused: None });
    assert_eq!(asker.result().0, "cancelled");
    ui.shutdown();
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn a_ui_asks_for_a_picker_for_itself_and_is_told_what_the_row_it_chose_is() {
    let session = session("pick-asked");
    let tree = Tree::new("asked");
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let params = |pairs: &[(&str, &str)]| -> std::collections::BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    };
    let cwd = tree.path();

    // No program is in it: the UI asks, and is the one told.
    ui.send(Control::PickerAsk { ask: 41, pane, picker: "files".into(), params: params(&[("cwd", &cwd), ("query", "gam")]) });
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. }));
    let Control::PickerOpen { picker, pane: Some(over), label, query, ask, .. } = opened else { unreachable!() };
    assert_eq!((over, label.as_str(), query.as_str(), ask), (pane, "Files", "gam", Some(41)));
    let found = ui.query(picker, 0, "gam");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let chosen = ui.control(|c| matches!(c, Control::PickerChosen { .. }));
    let Control::PickerChosen { picker: of, text, value, query } = chosen else { unreachable!() };
    let whole = tree.0.join("b").join("gamma.rs").to_string_lossy().into_owned();
    assert_eq!((of, text.as_str(), value, query.as_str()), (picker, "b/gamma.rs".replace('/', "\u{5c}").as_str(), whole, "gam"));

    // One it closes is over with nothing more said, and the next is another.
    ui.send(Control::PickerAsk { ask: 42, pane, picker: "directories".into(), params: params(&[("cwd", &cwd)]) });
    let Control::PickerOpen { picker: second, ask, .. } = ui.control(|c| matches!(c, Control::PickerOpen { .. })) else {
        unreachable!()
    };
    assert_eq!(ask, Some(42));
    assert_ne!(second, picker);
    ui.send(Control::PickerClose { picker: second, refused: None });

    // Refused, by its own number: a picker there is not, one that is for a
    // program, one that cannot be shown, and one over a pane it does not show.
    for (ask, pane, name, given) in [
        (43, pane, "themes", params(&[("cwd", &cwd)])),
        (44, pane, "items", params(&[])),
        (45, pane, "files", params(&[])),
        (46, pane + 100, "files", params(&[("cwd", &cwd)])),
    ] {
        ui.send(Control::PickerAsk { ask, pane, picker: name.into(), params: given });
        let refused = ui.control(|c| matches!(c, Control::PickerRefused { .. }));
        let Control::PickerRefused { ask: of, message } = refused else { unreachable!() };
        assert_eq!(of, ask, "{message}");
        assert!(!message.is_empty());
    }
    ui.shutdown();
}

#[test]
fn the_pickers_of_the_machines_file_are_opened_by_name_as_the_daemons_own_are() {
    let session = session("pick-file");
    let tree = Tree::new("file");
    let cwd = tree.path();
    let file = tree.0.join("nterm-daemon.lua");
    let write = |text: &str| {
        // A file's time changes by no less than some of a second.
        let before = std::fs::metadata(&file).and_then(|file| file.modified()).ok();
        loop {
            std::fs::write(&file, text).unwrap();
            if std::fs::metadata(&file).and_then(|file| file.modified()).ok() != before {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    write(
        r#"return { pickers = {
          Said = { type = "Command", args = { "cmd.exe", "/c", "echo", "{word}" }, sort = "SourceOrder" },
          Typed = { type = "File", path = "{history}", reverse = true, unique = true },
          Fixed = { type = "Items", position = "Cursor", initial_query = "t",
                    items = { { text = "One", value = "1" }, { text = "Two", value = "2" } } },
          Here = { type = "FileSystem", files_only = true, relative = true, preview_visible = false },
          files = { type = "Items" },
        } }"#,
    );
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", &session, "--lua-config"]).arg(&file).env_remove("NTERM_CONPTY_DLL");
    let mut daemon = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(&session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let params = |pairs: &[(&str, &str)]| -> std::collections::BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    };
    let mut asks = 0;
    let mut ask = |ui: &mut Ui, name: &str, given: std::collections::BTreeMap<String, String>| {
        asks += 1;
        ui.send(Control::PickerAsk { ask: asks, pane, picker: name.into(), params: given });
        let answered = |c: &Control| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. });
        ui.control(answered)
    };

    // A program the file names, run with what the picker is opened with as
    // one argument, whatever is in it.
    let opened = ask(&mut ui, "Said", params(&[("cwd", &cwd), ("word", "one & echo two")]));
    let Control::PickerOpen { picker, label, preview, at_cursor, .. } = opened else { panic!("{opened:?}") };
    assert_eq!((label.as_str(), preview, at_cursor), ("Said", None, false));
    let said = ui.query(picker, 0, "");
    assert_eq!(said.texts().len(), 1, "{:?}", said.texts());
    assert!(said.texts()[0].contains("one & echo two"), "{:?}", said.texts());
    ui.send(Control::PickerAccept { picker, serial: said.serial, row: said.rows[0].0 });
    let Control::PickerChosen { value, .. } = ui.control(|c| matches!(c, Control::PickerChosen { .. })) else {
        unreachable!()
    };
    assert!(value.contains("one & echo two"));

    // The lines of a file, the last first.
    std::fs::write(tree.0.join("typed.txt"), "first\nsecond\nfirst\n").unwrap();
    let opened = ask(&mut ui, "Typed", params(&[("cwd", &cwd), ("history", "typed.txt")]));
    let Control::PickerOpen { picker, .. } = opened else { panic!("{opened:?}") };
    assert_eq!(ui.query(picker, 0, "").texts(), ["first", "second"]);
    ui.send(Control::PickerClose { picker, refused: None });

    // Rows written in the file, shown where the file says and with what it
    // says is typed already. What the row chosen is, is its value.
    let opened = ask(&mut ui, "Fixed", params(&[]));
    let Control::PickerOpen { picker, query, at_cursor, .. } = opened else { panic!("{opened:?}") };
    assert_eq!((query.as_str(), at_cursor), ("t", true));
    let found = ui.query(picker, 0, "t");
    assert_eq!(found.texts(), ["Two"]);
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let Control::PickerChosen { text, value, .. } = ui.control(|c| matches!(c, Control::PickerChosen { .. })) else {
        unreachable!()
    };
    assert_eq!((text.as_str(), value.as_str()), ("Two", "2"));

    // Files under where it is opened, with the previews files have.
    let opened = ask(&mut ui, "Here", params(&[("cwd", &cwd)]));
    let Control::PickerOpen { picker, preview, .. } = opened else { panic!("{opened:?}") };
    assert_eq!(preview, Some(false));
    let found = ui.query(picker, 0, "gam");
    assert_eq!(found.texts().len(), 1);
    let shown = ui.preview(picker, 1, found.serial, found.rows[0].0);
    let Control::PickerPreviewed { text, .. } = shown else { unreachable!() };
    assert_eq!(text, "b/gamma.rs");
    ui.send(Control::PickerClose { picker, refused: None });

    // A program in the shell opens them as it does the daemon's own.
    let (asker, picker) = ui.picker(&session, pane, "Fixed", serde_json::json!({}));
    let found = ui.query(picker, 0, "one");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let (status, result) = asker.result();
    assert_eq!((status.as_str(), &result["selected"][0]["value"]), ("accepted", &serde_json::json!("1")));

    // Refused, saying why: what it was not opened with, one there is not,
    // and the daemon's own are not the file's to say.
    for (name, given, why) in [
        ("Said", params(&[("cwd", &cwd)]), "not opened with word"),
        ("Typed", params(&[("history", "typed.txt")]), "not a whole path"),
        ("Here", params(&[]), "no cwd"),
        ("Other", params(&[]), "no picker named Other"),
    ] {
        let refused = ask(&mut ui, name, given);
        let Control::PickerRefused { message, .. } = refused else { panic!("{name}: {refused:?}") };
        assert!(message.contains(why), "{name}: {message}");
    }
    let Control::PickerOpen { picker, label, .. } = ask(&mut ui, "files", params(&[("cwd", &cwd)])) else {
        panic!("the daemon's own files picker did not open")
    };
    assert_eq!(label, "Files");
    ui.send(Control::PickerClose { picker, refused: None });

    // The file is read again once it has changed: no restart.
    write(r#"return { pickers = { Fixed = { type = "Items", items = { { text = "Three", value = "3" } } } } }"#);
    let Control::PickerOpen { picker, .. } = ask(&mut ui, "Fixed", params(&[])) else { panic!("did not open") };
    assert_eq!(ui.query(picker, 0, "").texts(), ["Three"]);
    ui.send(Control::PickerClose { picker, refused: None });
    let Control::PickerRefused { message, .. } = ask(&mut ui, "Said", params(&[("word", "x")])) else {
        panic!("a picker the file no longer has opened")
    };
    assert!(message.contains("no picker named Said"), "{message}");
    // One that cannot be read says so, and the daemon goes on.
    write("return {");
    let Control::PickerRefused { message, .. } = ask(&mut ui, "Fixed", params(&[])) else { panic!("opened") };
    assert!(message.contains("could not be read"), "{message}");
    ui.shutdown();
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn a_function_of_the_machines_file_says_what_shows_for_a_row() {
    let session = session("pick-resolve");
    let tree = Tree::new("resolve");
    let cwd = tree.path();
    let file = tree.0.join("nterm-daemon.lua");
    std::fs::write(
        &file,
        r#"local shows = { type = "Dynamic", resolve = function(item, params)
          if item.value == "text" then
            local file = assert(io.open(params.cwd .. "/b/gamma.rs", "rb"))
            local text = file:read("a")
            file:close()
            return { type = "Text", text = text .. "\nsecond", name = "gamma.rs", extension = "rs", center_line = 2 }
          end
          if item.value == "plain" then return { type = "Text", text = item.text, syntax = "PlainText" } end
          if item.value == "ran" then
            local printed = preview.run { args = { "cmd.exe", "/c", "echo", params.word }, cwd = params.cwd }
            return { type = "Ansi", bytes = printed.stdout, center_line = 1 }
          end
          if item.value == "picture" then return { type = "Image", bytes = "\137PNG\0\255" } end
          if item.value == "slow" then
            preview.run { args = { "cmd.exe", "/c", "ping -n 30 127.0.0.1 > nul" }, timeout_ms = 60000 }
          end
          if item.value == "wrong" then error("no such thing", 0) end
          return nil
        end }
        local rows = {}
        for _, value in ipairs({ "text", "plain", "ran", "picture", "slow", "wrong", "none" }) do
          rows[#rows + 1] = { text = "row " .. value, value = value }
        end
        return { pickers = {
          Rows = { type = "Items", items = rows, preview = shows, sort = "SourceOrder" },
          Hidden = { type = "Items", items = rows, preview = shows, preview_visible = false },
          Here = { type = "FileSystem", files_only = true, relative = true,
                   preview = { type = "Dynamic", resolve = function(item) return { type = "Text", text = "is " .. item.path } end } },
        } }"#,
    )
    .unwrap();
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", &session, "--lua-config"]).arg(&file).env_remove("NTERM_CONPTY_DLL");
    let mut daemon = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(&session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let params = |pairs: &[(&str, &str)]| -> std::collections::BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    };
    let mut open = |ui: &mut Ui, ask: u64, name: &str| -> (u32, Option<bool>) {
        let given = params(&[("cwd", &cwd), ("word", "said")]);
        ui.send(Control::PickerAsk { ask, pane, picker: name.into(), params: given });
        let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
        let Control::PickerOpen { picker, preview, .. } = opened else { panic!("{name}: {opened:?}") };
        (picker, preview)
    };

    // Every row of it has a preview, shown or hidden at first as it says.
    let (hidden, preview) = open(&mut ui, 1, "Hidden");
    assert_eq!(preview, Some(false));
    ui.send(Control::PickerClose { picker: hidden, refused: None });
    let (picker, preview) = open(&mut ui, 2, "Rows");
    assert_eq!(preview, Some(true));
    let found = ui.query(picker, 0, "");
    let row = |value: &str| found.rows.iter().find(|(_, text)| text == &format!("row {value}")).unwrap().0;
    let mut request = 0;
    let mut ask = |ui: &mut Ui, value: &str| -> u64 {
        request += 1;
        ui.send(Control::PickerPreview { picker, request, serial: found.serial, row: row(value) });
        request
    };
    let answered = |ui: &mut Ui, request: u64| {
        ui.frame(|frame| match frame {
            Frame::Previewed { request: of, .. } => *of == request,
            Frame::Control(Control::PickerPreviewed { request: of, .. }) => *of == request,
            _ => false,
        })
    };

    // Text of a file of this machine, at a line, and how to colour it.
    let asked = ask(&mut ui, "text");
    let Frame::Control(Control::PickerPreviewed { text, name, line, extension, syntax, error, .. }) = answered(&mut ui, asked)
    else {
        panic!("not text")
    };
    assert_eq!((text.as_str(), name.as_deref(), line), ("b/gamma.rs\nsecond", Some("gamma.rs"), Some(2)));
    assert_eq!((extension.as_deref(), syntax, error), (Some("rs"), None, None));
    let asked = ask(&mut ui, "plain");
    let Frame::Control(Control::PickerPreviewed { text, syntax, .. }) = answered(&mut ui, asked) else { panic!() };
    assert_eq!((text.as_str(), syntax.as_deref()), ("row plain", Some("PlainText")));
    // What a program printed, and a picture, as the bytes they are.
    let asked = ask(&mut ui, "ran");
    let Frame::Previewed { kind, line, bytes, .. } = answered(&mut ui, asked) else { panic!("not bytes") };
    assert_eq!((kind, line), (nterm_session::protocol::PreviewKind::Ansi, Some(1)));
    assert!(String::from_utf8_lossy(&bytes).contains("said"));
    let asked = ask(&mut ui, "picture");
    let Frame::Previewed { kind, bytes, .. } = answered(&mut ui, asked) else { panic!("not bytes") };
    assert_eq!((kind, bytes.as_slice()), (nterm_session::protocol::PreviewKind::Image, &b"\x89PNG\0\xff"[..]));
    // Nothing to show is no text, and what goes wrong is said.
    let asked = ask(&mut ui, "none");
    let Frame::Control(Control::PickerPreviewed { text, error, .. }) = answered(&mut ui, asked) else { panic!() };
    assert_eq!((text.as_str(), error), ("", None));
    let asked = ask(&mut ui, "wrong");
    let Frame::Control(Control::PickerPreviewed { error, .. }) = answered(&mut ui, asked) else { panic!() };
    assert!(error.is_some_and(|error| error.contains("no such thing")));
    // A row rested on while a program runs for another: the program is
    // stopped, and the row is answered long before it would have ended.
    let began = Instant::now();
    ask(&mut ui, "slow");
    std::thread::sleep(Duration::from_millis(300));
    let asked = ask(&mut ui, "plain");
    let Frame::Control(Control::PickerPreviewed { text, .. }) = answered(&mut ui, asked) else { panic!() };
    assert_eq!(text, "row plain");
    assert!(began.elapsed() < Duration::from_secs(10), "{:?}", began.elapsed());
    // A picker that ends while a program runs for a row does not wait.
    ask(&mut ui, "slow");
    std::thread::sleep(Duration::from_millis(300));
    ui.send(Control::PickerClose { picker, refused: None });

    // Rows that are files are given as `item.path`, and the file's
    // function is what says what shows: not the daemon's own.
    let (picker, preview) = open(&mut ui, 3, "Here");
    assert_eq!(preview, Some(true));
    let found = ui.query(picker, 0, "gam");
    let shown = ui.preview(picker, 1, found.serial, found.rows[0].0);
    let Control::PickerPreviewed { text, .. } = shown else { unreachable!() };
    assert!(text.starts_with("is ") && text.ends_with("gamma.rs"), "{text}");
    ui.send(Control::PickerClose { picker, refused: None });
    ui.shutdown();
    assert!(daemon.wait().unwrap().success());
    assert!(began.elapsed() < Duration::from_secs(20), "the daemon waited for a stopped program");
}

#[test]
fn the_machines_file_says_what_is_done_with_the_row_a_ui_chose() {
    let session = session("pick-action");
    let tree = Tree::new("action");
    let cwd = tree.path();
    let file = tree.0.join("nterm-daemon.lua");
    std::fs::write(
        &file,
        r#"local rows = { { text = "Types", value = "types" }, { text = "Fails", value = "fails" } }
        return { pickers = {
          Acts = { type = "Items", items = rows, action = function(app, item, params)
            if item.value == "fails" then error("no such branch", 0) end
            app:send_text("echo " .. item.value .. " " .. params.word)
            app:show_toast("Done", item.text)
          end },
          Shows = { type = "Items", items = rows },
        } }"#,
    )
    .unwrap();
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", &session, "--lua-config"]).arg(&file).env_remove("NTERM_CONPTY_DLL");
    let mut daemon = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(&session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let mut open = |ui: &mut Ui, ask: u64, name: &str| -> u32 {
        let given = [("cwd".to_owned(), cwd.clone()), ("word".to_owned(), "given".to_owned())].into_iter().collect();
        ui.send(Control::PickerAsk { ask, pane, picker: name.into(), params: given });
        let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
        let Control::PickerOpen { picker, .. } = opened else { panic!("{name}: {opened:?}") };
        picker
    };
    use nterm_session::protocol::PickerStep;

    // The row a UI chose is acted on where the file is: the UI is told what
    // to do to the pane, and that the picker is over. It is not told what
    // the row is, to act on itself.
    let picker = open(&mut ui, 1, "Acts");
    let found = ui.query(picker, 0, "types");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let told = ui.control(|c| {
        matches!(c, Control::PickerActed { .. } | Control::PickerChosen { .. } | Control::PickerClosed { .. })
    });
    let Control::PickerActed { picker: of, steps } = told else { panic!("{told:?}") };
    assert_eq!(of, picker);
    assert_eq!(
        steps,
        [
            PickerStep::SendText { text: "echo types given".into() },
            PickerStep::Toast { title: Some("Done".into()), message: "Types".into(), duration_ms: None },
        ]
    );
    let closed = ui.control(|c| matches!(c, Control::PickerChosen { .. } | Control::PickerClosed { .. }));
    assert_eq!(closed, Control::PickerClosed { picker, message: None });

    // An action that goes wrong ends the picker saying why, with nothing
    // done to the pane.
    let picker = open(&mut ui, 2, "Acts");
    let found = ui.query(picker, 0, "fails");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let told = ui.control(|c| matches!(c, Control::PickerActed { .. } | Control::PickerClosed { .. }));
    let Control::PickerClosed { message, .. } = told else { panic!("{told:?}") };
    assert!(message.is_some_and(|message| message.contains("no such branch")));

    // With no action in the file the UI is told what the row is, as it is
    // of the daemon's own pickers.
    let picker = open(&mut ui, 3, "Shows");
    let found = ui.query(picker, 0, "types");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let told = ui.control(|c| matches!(c, Control::PickerActed { .. } | Control::PickerChosen { .. }));
    assert!(matches!(told, Control::PickerChosen { value, .. } if value == "types"));

    // A program that asked is given the row: what is done with it is the
    // program's to say.
    let (asker, picker) = ui.picker(&session, pane, "Acts", serde_json::json!({"word": "w"}));
    let found = ui.query(picker, 0, "types");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let (status, result) = asker.result();
    assert_eq!((status.as_str(), &result["selected"][0]["value"]), ("accepted", &serde_json::json!("types")));
    ui.shutdown();
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn a_key_of_the_machines_file_acts_on_a_row_and_leaves_the_picker_open() {
    let session = session("pick-keys");
    let tree = Tree::new("keys");
    let cwd = tree.path();
    let file = tree.0.join("nterm-daemon.lua");
    std::fs::write(
        &file,
        r#"return { pickers = {
          Keyed = { type = "Items", items = { { text = "Alpha", value = "a" }, { text = "Beta", value = "b" } },
            keys = {
              { key = "d", mods = "CTRL", description = "Note it", action = function(app, item, params)
                  app:show_toast("Noted", item.value .. " in " .. params.word)
                end },
              { key = "y", mods = "CTRL", action = function(app, item)
                  app:paste(item.value)
                  app:close_picker()
                end },
              { key = "x", mods = "CTRL|SHIFT", action = function() error("not this one", 0) end },
            } },
          Bad = { type = "Items", items = {}, keys = { { key = "d", action = "typed" } } },
        } }"#,
    )
    .unwrap();
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", &session, "--lua-config"]).arg(&file).env_remove("NTERM_CONPTY_DLL");
    let mut daemon = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(&session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    use nterm_session::protocol::{PickerBinding, PickerStep};
    let given: std::collections::BTreeMap<String, String> =
        [("cwd".to_owned(), cwd.clone()), ("word".to_owned(), "given".to_owned())].into_iter().collect();

    // The UI is told the picker's keys when it opens, as the file has them.
    ui.send(Control::PickerAsk { ask: 1, pane, picker: "Keyed".into(), params: given.clone() });
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
    let Control::PickerOpen { picker, keys, .. } = opened else { panic!("{opened:?}") };
    assert_eq!(
        keys,
        [
            PickerBinding { key: "d".into(), mods: "CTRL".into(), description: "Note it".into() },
            PickerBinding { key: "y".into(), mods: "CTRL".into(), description: String::new() },
            PickerBinding { key: "x".into(), mods: "CTRL|SHIFT".into(), description: String::new() },
        ]
    );
    let acted = |ui: &mut Ui| {
        let told = ui.control(|c| matches!(c, Control::PickerActed { .. } | Control::PickerClosed { .. }));
        let Control::PickerActed { steps, .. } = told else { panic!("{told:?}") };
        steps
    };

    // A key runs its function for the row, and the picker goes on: the
    // same key again, on another row.
    let found = ui.query(picker, 0, "");
    let (alpha, beta) = (found.rows[0].0, found.rows[1].0);
    ui.send(Control::PickerKey { picker, serial: found.serial, row: alpha, key: 0 });
    let noted = |value: &str| {
        [PickerStep::Toast { title: Some("Noted".into()), message: format!("{value} in given"), duration_ms: None }]
    };
    assert_eq!(acted(&mut ui), noted("a"));
    ui.send(Control::PickerKey { picker, serial: found.serial, row: beta, key: 0 });
    assert_eq!(acted(&mut ui), noted("b"));
    // One that fails says so, and the picker goes on. A key there is not
    // is nothing.
    ui.send(Control::PickerKey { picker, serial: found.serial, row: alpha, key: 9 });
    ui.send(Control::PickerKey { picker, serial: found.serial, row: alpha, key: 2 });
    let failed = acted(&mut ui);
    assert!(matches!(&failed[..], [PickerStep::Toast { message, .. }] if message.contains("not this one")));
    assert_eq!(ui.query(picker, 1, "bet").texts(), ["Beta"]);
    // One that closes the picker: what it asked for, then that it is over.
    let found = ui.query(picker, 2, "bet");
    ui.send(Control::PickerKey { picker, serial: found.serial, row: found.rows[0].0, key: 1 });
    assert_eq!(acted(&mut ui), [PickerStep::Paste { text: "b".into() }]);
    let closed = ui.control(|c| matches!(c, Control::PickerActed { .. } | Control::PickerClosed { .. }));
    assert_eq!(closed, Control::PickerClosed { picker, message: None });

    // Keys that are not written as keys are: the picker is refused.
    ui.send(Control::PickerAsk { ask: 2, pane, picker: "Bad".into(), params: given });
    let refused = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
    let Control::PickerRefused { message, .. } = refused else { panic!("{refused:?}") };
    assert!(message.contains("keys[1].action is not a function"), "{message}");
    ui.shutdown();
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn a_picker_of_the_machines_file_that_is_a_function_is_told_what_is_at_the_cursor() {
    let session = session("pick-function");
    let tree = Tree::new("function");
    let cwd = tree.path();
    let file = tree.0.join("nterm-daemon.lua");
    std::fs::write(
        &file,
        r#"return { pickers = {
          Told = function(context)
            -- What the function keeps is what its action sees.
            local kept = context.line_before_cursor
            return { type = "Items",
              items = {
                { text = "line", value = context.line },
                { text = "before", value = context.line_before_cursor },
                { text = "column", value = tostring(context.cursor_column) },
                { text = "recent", value = context.recent_lines[#context.recent_lines] or "" },
                { text = "folder", value = context.cwd or "" },
                { text = "copied", value = context.copy_line .. "|" .. context.copy_line_before_cursor },
              },
              action = function(app, item) app:send_text(kept .. "+" .. item.text) end,
            }
          end,
          Wrong = function() return "not a table" end,
          Fails = function() error("no such thing", 0) end,
        } }"#,
    )
    .unwrap();
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", &session, "--lua-config"]).arg(&file).env_remove("NTERM_CONPTY_DLL");
    let mut daemon = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(&session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    // Something at the shell's prompt, with the cursor after it.
    ui.input(pane, "rem marker-typed");
    ui.until_text("what was typed", |text| text.contains("rem marker-typed"));
    let given: std::collections::BTreeMap<String, String> = [
        ("cwd".to_owned(), cwd.clone()),
        ("copy_line".to_owned(), "a copied line".to_owned()),
        ("copy_line_before_cursor".to_owned(), "a copied".to_owned()),
    ]
    .into_iter()
    .collect();
    ui.send(Control::PickerAsk { ask: 1, pane, picker: "Told".into(), params: given.clone() });
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
    let Control::PickerOpen { picker, .. } = opened else { panic!("{opened:?}") };
    // The rows say what the function was told: read through an action,
    // which sees what the function kept.
    let found = ui.query(picker, 0, "line");
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let told = ui.control(|c| matches!(c, Control::PickerActed { .. } | Control::PickerClosed { .. }));
    let Control::PickerActed { steps, .. } = told else { panic!("{told:?}") };
    let [nterm_session::protocol::PickerStep::SendText { text }] = &steps[..] else { panic!("{steps:?}") };
    assert!(text.ends_with("rem marker-typed+line"), "{text}");

    // One that gives no table, and one that fails, are refused saying why.
    for (ask, name, why) in [(2, "Wrong", "does not return a picker's table"), (3, "Fails", "no such thing")] {
        ui.send(Control::PickerAsk { ask, pane, picker: name.into(), params: given.clone() });
        let refused = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
        let Control::PickerRefused { message, .. } = refused else { panic!("{refused:?}") };
        assert!(message.contains(why), "{message}");
    }
    ui.shutdown();
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn a_picker_of_the_machines_file_with_columns_shows_them_lined_up_under_their_names() {
    let session = session("pick-columns");
    let tree = Tree::new("columns");
    let cwd = tree.path();
    std::fs::write(tree.0.join("rows.txt"), "abc123\t2026-01-02\tFirst subject\r\nde\t2026-11-18\tSecond\twith a tab\r\n").unwrap();
    let file = tree.0.join("nterm-daemon.lua");
    std::fs::write(
        &file,
        r#"return { pickers = {
          Table = { type = "Command", sort = "Source", args = { "cmd.exe", "/c", "type", "rows.txt" },
                    columns = { "Hash", "Date", "Subject" } },
          Both = { type = "Command", args = { "cmd.exe", "/c", "ver" }, columns = { "A" }, unique = true },
        } }"#,
    )
    .unwrap();
    let mut command = std::process::Command::new(DAEMON);
    command.args(["--session", &session, "--lua-config"]).arg(&file).env_remove("NTERM_CONPTY_DLL");
    let mut daemon = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while let Err(error) = pipe::connect(&session) {
        assert!(Instant::now() < deadline, "the daemon never served: {error}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut ui = Ui::connect(&session);
    let pane = ui.shell();
    let given: std::collections::BTreeMap<String, String> = [("cwd".to_owned(), cwd.clone())].into_iter().collect();
    ui.send(Control::PickerAsk { ask: 1, pane, picker: "Table".into(), params: given.clone() });
    let opened = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
    let Control::PickerOpen { picker, .. } = opened else { panic!("{opened:?}") };
    // The rows are padded to their columns, and the header to the same.
    let found = ui.query(picker, 0, "");
    assert_eq!(found.header.as_deref(), Some("Hash    Date        Subject"));
    assert_eq!(found.texts(), ["abc123  2026-01-02  First subject", "de      2026-11-18  Second\twith a tab"]);
    // What is typed is looked for in the rows.
    let found = ui.query(picker, 1, "second");
    assert_eq!(found.texts().len(), 1);
    // The row chosen is the line as it was printed.
    ui.send(Control::PickerAccept { picker, serial: found.serial, row: found.rows[0].0 });
    let Control::PickerChosen { value, .. } = ui.control(|c| matches!(c, Control::PickerChosen { .. })) else {
        unreachable!()
    };
    assert_eq!(value, "de\t2026-11-18\tSecond\twith a tab");

    ui.send(Control::PickerAsk { ask: 2, pane, picker: "Both".into(), params: given });
    let refused = ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }));
    let Control::PickerRefused { message, .. } = refused else { panic!("{refused:?}") };
    assert!(message.contains("columns do not go with"), "{message}");
    ui.shutdown();
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn a_ui_is_given_a_search_of_folders_over_no_pane() {
    let session = session("folders");
    let tree = Tree::new("folders");
    for folder in ["one/inner/deepest", "two", "three-of-them"] {
        std::fs::create_dir_all(tree.0.join("roots").join(folder)).unwrap();
    }
    std::fs::write(tree.0.join("roots/one/a-file.txt"), "text").unwrap();
    let root = tree.0.join("roots").to_string_lossy().into_owned();
    let mut ui = Ui::connect(&session);
    // No pane at all: a window with no shell yet asks.
    let mut ask = |ui: &mut Ui, ask: u64, roots: Vec<String>, depth: i32| {
        ui.send(Control::FoldersAsk { ask, roots, depth });
        ui.control(|c| matches!(c, Control::PickerOpen { .. } | Control::PickerRefused { .. }))
    };
    let opened = ask(&mut ui, 7, vec![root.clone()], 1);
    let Control::PickerOpen { picker, pane, ask: said, .. } = opened else { panic!("{opened:?}") };
    assert_eq!((pane, said), (None, Some(7)));
    // The folders one level down, each as its whole path, and no files.
    let found = ui.query(picker, 0, "");
    let mut folders = found.texts();
    folders.sort();
    let whole = |name: &str| std::path::Path::new(&root).join(name).to_string_lossy().into_owned();
    assert_eq!(folders, [whole("one"), whole("three-of-them"), whole("two")]);
    // What is typed finds among them, and what a folder holds shows for it.
    let found = ui.query(picker, 1, "thr");
    assert_eq!(found.texts(), [whole("three-of-them")]);
    let found = ui.query(picker, 2, "one");
    let shown = ui.preview(picker, 1, found.serial, found.rows[0].0);
    let Control::PickerPreviewed { text, .. } = shown else { unreachable!() };
    assert!(text.contains("inner") && text.contains("a-file.txt"), "{text}");
    ui.send(Control::PickerClose { picker, refused: None });

    // Two levels of them.
    let opened = ask(&mut ui, 8, vec![root.clone()], 2);
    let Control::PickerOpen { picker, .. } = opened else { panic!("{opened:?}") };
    let found = ui.query(picker, 0, "inner");
    assert_eq!(found.texts(), [whole("one").to_owned() + "\\inner"]);
    // A third level is not listed, whatever else of the path matches.
    assert!(ui.query(picker, 1, "deepest").texts().iter().all(|folder| !folder.ends_with("deepest")));
    ui.send(Control::PickerClose { picker, refused: None });

    // With nowhere to look, or somewhere that is no folder, it is refused.
    for roots in [Vec::new(), vec![whole("none-such")]] {
        let refused = ask(&mut ui, 9, roots, 1);
        assert!(matches!(refused, Control::PickerRefused { ask: 9, .. }), "{refused:?}");
    }
    ui.shutdown();
}

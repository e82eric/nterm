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
    let Control::Welcome { protocol, ghostty, pid, session: named } = ui.welcome.clone() else {
        panic!("no welcome");
    };
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
        let Control::PickerOpen { picker, pane: over, .. } =
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
                serial, revision: of, query, matched_against, rows, matched, done, ..
            } = found
            else {
                unreachable!()
            };
            self.send(Control::PickerSeen { picker, serial });
            if of == revision && done {
                let rows = rows.into_iter().map(|row| (row.index, row.text)).collect();
                return Rows { serial, query, matched_against, rows, matched };
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

//! Drives the real daemon binary over its pipe, as a UI would.
#![cfg(windows)]

use nterm_daemon::vt::Terminal;
use nterm_session::{
    GHOSTTY_REVISION,
    pipe::{self, Connection, Writer},
    protocol::{Control, Frame, PROTOCOL_VERSION, PaneId, PaneInfo, Size, TerminalSettings},
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
}

impl Ui {
    /// Connect as nterm does: the daemon is started if the session has none.
    fn connect(session: &str) -> Self {
        let connection = pipe::connect_or_start(session, Path::new(DAEMON)).expect("connect");
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
    assert_eq!(pid, ui.connection.peer_pid());

    ui.send(Control::Settings {
        settings: TerminalSettings { scrollback_bytes: Some(1 << 26), ..Default::default() },
    });
    let pane = ui.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    assert_ne!(pane.pid, 0);
    assert_ne!(pane.creation_time, 0);
    assert_eq!(pane.exited, None);

    // Attach, then type.
    ui.send(Control::Attach { pane: pane.pane, size: size(80, 24) });
    ui.frame(|frame| matches!(frame, Frame::Screen { .. }));
    ui.input(pane.pane, "echo first-marker\r");
    ui.until_text("the echoed line", |text| text.contains("\nfirst-marker"));

    // Resize: the daemon sends its own reflowed screen, and the shell sees
    // the new width.
    ui.send(Control::Resize { pane: pane.pane, size: size(100, 30) });
    ui.frame(|frame| matches!(frame, Frame::Screen { .. }));
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
    ui.send(Control::Attach { pane: pane.pane, size: size(90, 20) });
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
fn two_uis_share_a_pane_and_blobs() {
    let session = session("shared");
    let mut first = Ui::connect(&session);
    let mut second = Ui::connect(&session);
    assert_eq!(daemon_pid(&first), daemon_pid(&second));

    let pane = first.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    for ui in [&mut first, &mut second] {
        ui.send(Control::Attach { pane: pane.pane, size: size(80, 24) });
        ui.frame(|frame| matches!(frame, Frame::Screen { .. }));
    }
    first.input(pane.pane, "echo seen-by-both\r");
    first.until_text("the first UI's copy", |text| text.contains("\nseen-by-both"));
    second.until_text("the second UI's copy", |text| text.contains("\nseen-by-both"));

    // A detached UI gets no more output; the other still does.
    second.send(Control::Detach { pane: pane.pane });
    second.panes();
    first.input(pane.pane, "echo only-first\r");
    first.until_text("output after the detach", |text| text.contains("\nonly-first"));
    second.panes();
    assert!(!second.mirror.as_mut().unwrap().text().contains("only-first"));

    first.send(Control::BlobSet { key: "layout".into(), value: "{\"tabs\":2}".into() });
    // The two connections are not ordered against each other; a round trip
    // on the first says its message has been handled.
    first.panes();
    second.send(Control::BlobGet { request: 5, key: "layout".into() });
    assert_eq!(
        second.control(|c| matches!(c, Control::Blob { .. })),
        Control::Blob { request: 5, key: "layout".into(), value: Some("{\"tabs\":2}".into()) }
    );

    // Blobs are listed by prefix, and gone once removed.
    first.send(Control::BlobSet { key: "session/b".into(), value: "2".into() });
    first.send(Control::BlobSet { key: "session/a".into(), value: "1".into() });
    first.send(Control::BlobList { request: 6, prefix: "session/".into() });
    assert_eq!(
        first.control(|c| matches!(c, Control::Blobs { .. })),
        Control::Blobs {
            request: 6,
            blobs: vec![("session/a".into(), "1".into()), ("session/b".into(), "2".into())],
        }
    );
    first.send(Control::BlobRemove { key: "session/a".into() });
    first.send(Control::BlobList { request: 7, prefix: "session/".into() });
    assert_eq!(
        first.control(|c| matches!(c, Control::Blobs { .. })),
        Control::Blobs { request: 7, blobs: vec![("session/b".into(), "2".into())] }
    );

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

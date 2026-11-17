//! Drives the real daemon binary over its pipe, as a UI would.
#![cfg(windows)]

use nterm_daemon::vt::Terminal;
use nterm_session::{
    GHOSTTY_REVISION,
    pipe::{self, Connection, Writer},
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
    assert_eq!(pid, ui.connection.peer_pid());

    ui.send(Control::Settings {
        settings: TerminalSettings { scrollback_bytes: Some(1 << 26), ..Default::default() },
    });
    let pane = ui.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    assert_ne!(pane.pid, 0);
    assert_ne!(pane.creation_time, 0);
    assert_eq!(pane.exited, None);

    // Attach, then type.
    ui.send(Control::Attach { request: 0, pane: pane.pane, size: size(80, 24), exclusive: false });
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
    ui.send(Control::Attach { request: 0, pane: pane.pane, size: size(90, 20), exclusive: false });
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
        ui.send(Control::Attach { request: 0, pane: pane.pane, size: size(80, 24), exclusive: false });
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

#[test]
fn an_attach_is_answered_and_an_exclusive_one_is_refused_when_taken() {
    let session = session("exclusive");
    let mut first = Ui::connect(&session);
    let mut second = Ui::connect(&session);
    let pane = first.spawn("cmd.exe /d /q /k prompt $G$S", size(80, 24));
    let answer = |ui: &mut Ui| {
        ui.control(|c| matches!(c, Control::Attached { .. } | Control::Error { .. }))
    };
    let attach = |ui: &mut Ui, request: u32, pane: PaneId, size: Size| {
        ui.send(Control::Attach { request, pane, size, exclusive: true });
    };

    // No such pane: the refusal names the request.
    attach(&mut first, 11, pane.pane + 100, size(80, 24));
    assert!(matches!(answer(&mut first), Control::Error { request: Some(11), .. }));

    // The answer comes before the screen, and counts the asker.
    attach(&mut first, 12, pane.pane, size(80, 24));
    let Control::Attached { request: 12, info } = answer(&mut first) else {
        panic!("the attach was not answered");
    };
    assert_eq!((info.pane, info.attached), (pane.pane, 1));
    first.frame(|frame| matches!(frame, Frame::Screen { .. }));

    // A second UI is refused, and the pane is not resized for it.
    attach(&mut second, 13, pane.pane, size(40, 10));
    assert!(matches!(answer(&mut second), Control::Error { request: Some(13), .. }));
    let listed = second.panes();
    assert_eq!((listed[0].size, listed[0].attached), (size(80, 24), 1));

    // Once the first has let go, the second can have it.
    first.send(Control::Detach { pane: pane.pane });
    first.panes();
    attach(&mut second, 14, pane.pane, size(40, 10));
    let Control::Attached { request: 14, info } = answer(&mut second) else {
        panic!("the attach was not answered");
    };
    assert_eq!((info.size, info.attached), (size(40, 10), 1));
    second.frame(|frame| matches!(frame, Frame::Screen { .. }));
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
        self.send(Control::Attach { request: 0, pane, size: size(200, 24), exclusive: false });
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
    assert!(told[0].panes.iter().all(|pane| pane.attached == 0));
    ui.send(Control::Attach { request: 0, pane: a, size: size(80, 24), exclusive: true });
    other.sessions("the attach", |told| {
        told.len() == 1 && told[0].panes.iter().any(|pane| pane.pane == a && pane.attached == 1)
    });
    ui.send(Control::Detach { pane: a });
    other.sessions("the detach", |told| {
        told.len() == 1 && told[0].panes.iter().all(|pane| pane.attached == 0)
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

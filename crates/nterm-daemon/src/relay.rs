//! `nterm-daemon --relay`: this process's input and output become a
//! connection to the session's daemon, which is started if there is none.
//!
//! For a UI that cannot open the daemon's pipe itself because it is on
//! another machine: it runs this through `ssh` and talks to the daemon over
//! the relay's input and output (see `nterm_session::relay`).
//!
//! The relay never looks at what it carries. It ends when either side does,
//! and says in `relay-SESSION.log` which it was: what is known on this
//! machine of a connection that was lost.

use nterm_session::{log, pipe, relay::MARKER};
use std::{
    fmt::Arguments,
    fs::File,
    io::{self, Read, Write},
    mem::ManuallyDrop,
    os::windows::io::{AsRawHandle, FromRawHandle},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long a relay's log gets: it says a few lines for each connection.
const LIMIT: u64 = 1 << 20;

/// Relay until the UI or the daemon goes. An error is one from before the
/// relaying began: after it, an end is an end.
pub fn run(session: &str) -> io::Result<()> {
    let say = |message: Arguments<'_>| {
        log::append(&format!("relay-{session}.log"), LIMIT, message);
    };
    let from = from();
    relay(session, &from, &say).inspect_err(|error| say(format_args!("{from}: not relaying: {error}")))
}

fn relay(session: &str, from: &str, say: &(impl Fn(Arguments<'_>) + Sync)) -> io::Result<()> {
    // The daemon to start is this same program.
    let (connection, started) =
        pipe::connect_or_start_saying(session, &std::env::current_exe()?)?;
    let mut from_daemon = connection.reader()?;
    let mut to_daemon = connection.writer()?;

    // The handles themselves, not `io::stdout()`, which holds back what
    // follows the last line end until it sees another. Never closed: they
    // are the process's.
    let mut output =
        ManuallyDrop::new(unsafe { File::from_raw_handle(io::stdout().as_raw_handle()) });
    let mut input =
        ManuallyDrop::new(unsafe { File::from_raw_handle(io::stdin().as_raw_handle()) });

    // What comes before this line is not the daemon's: a login banner, say.
    output.write_all(format!("{MARKER}\n").as_bytes())?;
    let daemon = if started { "started the session's daemon" } else { "the session's daemon was running" };
    say(format_args!("{from}: relaying to daemon pid {}; {daemon}", connection.peer_pid()));

    let traffic = Arc::new(Traffic::new());
    std::thread::scope(|scope| -> io::Result<()> {
        let carried = traffic.clone();
        scope.spawn(move || {
            let mut chunk = vec![0u8; 1 << 16];
            let why = loop {
                match input.read(&mut chunk) {
                    Ok(0) => break "its input ended: the SSH session closed it".to_owned(),
                    Ok(read) => {
                        carried.input(read);
                        if let Err(error) = to_daemon.write_all(&chunk[..read]) {
                            break format!("writing to the daemon failed: {error}");
                        }
                    }
                    Err(error) => break format!("reading its input failed: {error}"),
                }
            };
            carried.end(from, &why, say);
            // The UI has gone. The other copy may be waiting on the daemon
            // for ever, so this ends the process rather than only the thread.
            std::process::exit(0);
        });
        let mut chunk = vec![0u8; 1 << 16];
        let why = loop {
            match from_daemon.read(&mut chunk) {
                Ok(0) => break "the daemon ended the connection".to_owned(),
                Ok(read) => {
                    traffic.output(read);
                    if let Err(error) = output.write_all(&chunk[..read]) {
                        break format!("writing to ssh failed: {error}");
                    }
                }
                Err(error) => break format!("reading from the daemon failed: {error}"),
            }
        };
        traffic.end(from, &why, say);
        // As the other copy does: it may be waiting on input for ever.
        std::process::exit(0);
    })
}

/// Who the relay is for: the two ends of the SSH connection it runs in.
fn from() -> String {
    let connection = std::env::var("SSH_CONNECTION").unwrap_or_default();
    match connection.split_whitespace().collect::<Vec<_>>()[..] {
        [client, client_port, server, server_port] => {
            format!("from {client} port {client_port} to {server} port {server_port}")
        }
        _ => "not over ssh".to_owned(),
    }
}

/// What has gone each way, and when last.
struct Traffic {
    began: Instant,
    input: AtomicU64,
    output: AtomicU64,
    /// Milliseconds after `began`.
    last_input: AtomicU64,
    last_output: AtomicU64,
    ended: AtomicBool,
}

impl Traffic {
    fn new() -> Self {
        Self {
            began: Instant::now(),
            input: AtomicU64::new(0),
            output: AtomicU64::new(0),
            last_input: AtomicU64::new(0),
            last_output: AtomicU64::new(0),
            ended: AtomicBool::new(false),
        }
    }

    fn now(&self) -> u64 {
        self.began.elapsed().as_millis() as u64
    }

    fn input(&self, bytes: usize) {
        self.input.fetch_add(bytes as u64, Ordering::Relaxed);
        self.last_input.store(self.now(), Ordering::Relaxed);
    }

    fn output(&self, bytes: usize) {
        self.output.fetch_add(bytes as u64, Ordering::Relaxed);
        self.last_output.store(self.now(), Ordering::Relaxed);
    }

    /// Say how the relaying ended, once: whichever side ends first.
    fn end(&self, from: &str, why: &str, say: &impl Fn(Arguments<'_>)) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        let now = self.now();
        let ago = |last: &AtomicU64| log::duration(Duration::from_millis(now - last.load(Ordering::Relaxed).min(now)));
        say(format_args!(
            "{from}: ended after {}: {why}; {} in, {} out; last input {} ago, last output {} ago",
            log::duration(self.began.elapsed()),
            log::bytes(self.input.load(Ordering::Relaxed)),
            log::bytes(self.output.load(Ordering::Relaxed)),
            ago(&self.last_input),
            ago(&self.last_output),
        ));
    }
}

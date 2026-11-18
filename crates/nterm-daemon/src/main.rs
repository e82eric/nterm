//! `nterm-daemon [--session NAME] [--idle-seconds N]`
//!
//! Started by the UI when the session has no daemon. Exits when told to, or
//! when it has had no panes and no UI for the idle time.
//!
//! `nterm-daemon --relay [--session NAME]` is not a daemon but a way to one:
//! see `relay`.

#[cfg(windows)]
fn main() {
    use nterm_daemon::{log, server};
    use std::time::Duration;

    let mut session = nterm_session::DEFAULT_SESSION.to_owned();
    let mut idle = Duration::from_secs(10);
    let mut relay = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--relay" => relay = true,
            "--session" => match args.next() {
                Some(value) => session = value,
                None => usage(),
            },
            "--idle-seconds" => match args.next().and_then(|value| value.parse().ok()) {
                Some(seconds) => idle = Duration::from_secs(seconds),
                None => usage(),
            },
            _ => usage(),
        }
    }

    if relay {
        // Not the daemon: a go-between for a UI that cannot reach its pipe.
        if let Err(error) = nterm_daemon::relay::run(&session) {
            eprintln!("nterm-daemon --relay: {error}");
            std::process::exit(1);
        }
        return;
    }

    log::open(&session);
    // Not the directory of whichever UI happened to start the daemon, which
    // would be held open for as long as the daemon runs.
    let _ = std::env::set_current_dir(log::directory());

    match server::run(server::Options { session, idle }) {
        Ok(()) => {}
        // Another daemon serves the session; the UI connects to that one.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            log::log(format_args!("cannot serve: {error}"));
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
fn usage() -> ! {
    eprintln!(
        "usage: nterm-daemon [--session NAME] [--idle-seconds N]\n       \
         nterm-daemon --relay [--session NAME]"
    );
    std::process::exit(2);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("nterm-daemon: only Windows is supported so far");
    std::process::exit(1);
}

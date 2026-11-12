//! `nterm-daemon [--session NAME] [--idle-seconds N]`
//!
//! Started by the UI when the session has no daemon. Exits when told to, or
//! when it has had no panes and no UI for the idle time.

#[cfg(windows)]
fn main() {
    use nterm_daemon::{log, server};
    use std::time::Duration;

    let mut session = nterm_session::DEFAULT_SESSION.to_owned();
    let mut idle = Duration::from_secs(10);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--session", Some(value)) => session = value,
            ("--idle-seconds", Some(value)) if value.parse::<u64>().is_ok() => {
                idle = Duration::from_secs(value.parse().unwrap());
            }
            _ => {
                eprintln!("usage: nterm-daemon [--session NAME] [--idle-seconds N]");
                std::process::exit(2);
            }
        }
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

#[cfg(not(windows))]
fn main() {
    eprintln!("nterm-daemon: only Windows is supported so far");
    std::process::exit(1);
}

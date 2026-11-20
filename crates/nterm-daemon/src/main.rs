//! `nterm-daemon [--session NAME] [--idle-seconds N]`
//!
//! Started by the UI when the session has no daemon. Exits when told to, or
//! when it has had no panes and no UI for the idle time.
//!
//! `nterm-daemon --relay [--session NAME]` is not a daemon but a way to one:
//! see `relay`.
//!
//! `nterm-daemon clipboard paste [--lf]` is not a daemon either, but a
//! program for a pane's shell: it prints the text on the clipboard of the
//! window showing the pane. See `clipboard`.

fn main() {
    use nterm_daemon::{log, server};
    use std::time::Duration;

    if std::env::args().nth(1).as_deref() == Some("clipboard") {
        clipboard(std::env::args().skip(2).collect());
    }

    let mut session = nterm_session::DEFAULT_SESSION.to_owned();
    let mut idle = Duration::from_secs(10);
    let mut relay = false;
    let mut pickers = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--relay" => relay = true,
            "--lua-config" => match args.next() {
                Some(value) => pickers = Some(std::path::PathBuf::from(value)),
                None => usage(),
            },
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

    // Started from a terminal or an SSH session, the daemon outlives it:
    // its hangup is not the daemon's. Its shells set it back.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    log::open(&session);
    // Not the directory of whichever UI happened to start the daemon, which
    // would be held open for as long as the daemon runs.
    let _ = std::env::set_current_dir(log::directory());

    match server::run(server::Options { session, idle, pickers }) {
        Ok(()) => {}
        // Another daemon serves the session; the UI connects to that one.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            log::log(format_args!("cannot serve: {error}"));
            std::process::exit(1);
        }
    }
}

/// `clipboard paste [--lf]`: the text, as it is, on standard output; with
/// `--lf`, its line ends as `\n`. Exits 1, saying why, when there is none
/// to give.
fn clipboard(args: Vec<String>) -> ! {
    use std::io::Write;
    let lf = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["paste"] => false,
        ["paste", "--lf"] => true,
        _ => usage(),
    };
    match nterm_daemon::clipboard::paste() {
        Ok(text) => {
            let text = if lf { nterm_daemon::clipboard::lf(&text) } else { text };
            let mut out = std::io::stdout().lock();
            if out.write_all(text.as_bytes()).and_then(|()| out.flush()).is_err() {
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("nterm-daemon clipboard paste: {error}");
            std::process::exit(1);
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: nterm-daemon [--session NAME] [--idle-seconds N] [--lua-config FILE]\n       \
         nterm-daemon --relay [--session NAME]\n       \
         nterm-daemon clipboard paste [--lf]"
    );
    std::process::exit(2);
}

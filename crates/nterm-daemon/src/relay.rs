//! `nterm-daemon --relay`: this process's input and output become a
//! connection to the session's daemon, which is started if there is none.
//!
//! For a UI that cannot open the daemon's pipe itself because it is on
//! another machine: it runs this through `ssh` and talks to the daemon over
//! the relay's input and output (see `nterm_session::relay`).
//!
//! The relay never looks at what it carries. It ends when either side does.

use nterm_session::{pipe, relay::MARKER};
use std::{
    fs::File,
    io::{self, Read, Write},
    mem::ManuallyDrop,
    os::windows::io::{AsRawHandle, FromRawHandle},
};

/// Relay until the UI or the daemon goes. An error is one from before the
/// relaying began: after it, an end is an end.
pub fn run(session: &str) -> io::Result<()> {
    // The daemon to start is this same program.
    let connection = pipe::connect_or_start(session, &std::env::current_exe()?)?;
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

    std::thread::Builder::new().name("to daemon".into()).spawn(move || {
        let _ = io::copy(&mut *input, &mut to_daemon);
        // The UI has gone. The other copy may be waiting on the daemon for
        // ever, so this ends the process rather than only the thread.
        std::process::exit(0);
    })?;
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        match from_daemon.read(&mut chunk) {
            Ok(0) | Err(_) => return Ok(()),
            Ok(read) => {
                if output.write_all(&chunk[..read]).is_err() {
                    return Ok(());
                }
            }
        }
    }
}

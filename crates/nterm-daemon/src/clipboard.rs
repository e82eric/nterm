//! `nterm-daemon clipboard paste`: the text on the clipboard of the machine
//! of the window showing this shell's pane, for an editor in the shell to
//! paste. That may be another machine than this one, in a `--remote`
//! window. Not the daemon: a program in a pane's shell, which reaches the
//! window through the daemon that runs the shell (see
//! `nterm_session::shell`). The window's configuration says whether it may.

use nterm_session::shell::{Event, Program};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How long the window gets to answer. It reads its clipboard and answers
/// at once: this is only for one that is stuck.
const WAIT: Duration = Duration::from_secs(10);

/// The clipboard's text, from the window showing this shell's pane.
pub fn paste() -> Result<String, String> {
    let mut program = Program::from_environment()
        .ok_or("not in a pane of an nterm session daemon (NTERM_DAEMON is not set)")?
        .map_err(|error| format!("unable to reach the session daemon: {error}"))?;
    paste_from(&mut program, WAIT)
}

/// As `paste`, over a conversation already open, waiting up to `wait`.
pub fn paste_from(program: &mut Program, wait: Duration) -> Result<String, String> {
    let ask = json!({"type": "clipboard_read", "request_id": "clipboard"});
    program
        .send(ask.to_string().as_bytes())
        .map_err(|error| format!("the session daemon closed the connection: {error}"))?;
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the window did not answer in time".into());
        }
        match program.receive(left) {
            None => {}
            Some(Event::Message(bytes)) => {
                let reply: Value = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("the window's answer is not JSON: {error}"))?;
                if reply["type"] != "result" {
                    continue;
                }
                return match (reply["status"].as_str(), reply["text"].as_str()) {
                    (Some("ok"), Some(text)) => Ok(text.to_owned()),
                    _ => Err(reply["message"].as_str().unwrap_or("the window gave no text").to_owned()),
                };
            }
            // A window from before this ends it without a word.
            Some(Event::Closed) => {
                return Err("the window ended the request without answering: it may be older than this daemon".into());
            }
            Some(Event::Refused(why)) => return Err(why),
            Some(Event::Lost) => return Err("the session daemon closed the connection".into()),
        }
    }
}

/// `text` with Windows' line ends made `\n`, for an editor that wants them.
pub fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_windows_line_ends_change() {
        assert_eq!(lf("a\r\nb\rc\n"), "a\nb\rc\n");
    }
}

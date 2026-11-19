//! What a pane's Lua functions asked for that takes a while (the clipboard
//! read, a file sent to the daemon), and the functions to call when it is
//! there. Each is answered once, on a later frame, never inside the
//! function that asked.

use crate::lua_callbacks::{Command, Later};
use std::{
    collections::HashMap,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

/// How long a daemon gets to say where it kept a file.
const KEEP_WAIT: Duration = Duration::from_secs(30);

/// The largest file sent; the daemon keeps no larger.
const LARGEST: usize = 32 << 20;

pub(super) enum Answer {
    Clipboard(Result<crate::clipboard::Clip, String>),
    Kept(Result<String, String>),
}

struct Waiting {
    then: Later,
    /// When to give up on it, for what only another machine answers.
    deadline: Option<Instant>,
}

pub(super) struct Laters {
    answers: (Sender<(u64, Answer)>, Receiver<(u64, Answer)>),
    waiting: HashMap<u64, Waiting>,
    next: u64,
}

impl Default for Laters {
    fn default() -> Self {
        Self { answers: mpsc::channel(), waiting: HashMap::new(), next: 1 }
    }
}

impl Laters {
    /// Start what `command` asks, if it is one of these; otherwise give it
    /// back. `session` is the pane's tie to its daemon.
    pub(super) fn ask(
        &mut self,
        command: Command,
        session: Option<&std::sync::Arc<crate::session::Link>>,
        ctx: &egui::Context,
    ) -> Option<Command> {
        let id = self.next;
        let answer = {
            let (sender, ctx) = (self.answers.0.clone(), ctx.clone());
            move |answer: Answer| {
                let _ = sender.send((id, answer));
                ctx.request_repaint();
            }
        };
        match command {
            Command::Clipboard { kinds, then } => {
                self.next += 1;
                self.waiting.insert(id, Waiting { then, deadline: None });
                let read = move || answer(Answer::Clipboard(crate::clipboard::read(kinds)));
                if let Err(error) = std::thread::Builder::new().name("clipboard".into()).spawn(read) {
                    let why = format!("the clipboard could not be read: {error}");
                    let _ = self.answers.0.send((id, Answer::Clipboard(Err(why))));
                }
            }
            Command::SendFile { bytes, extension, then } => {
                self.next += 1;
                let deadline = Instant::now() + KEEP_WAIT;
                self.waiting.insert(id, Waiting { then, deadline: Some(deadline) });
                ctx.request_repaint_after(KEEP_WAIT);
                let refused = if bytes.len() > LARGEST {
                    Some(format!(
                        "the file is {} MiB; no more than {} MiB is sent",
                        bytes.len().div_ceil(1 << 20),
                        LARGEST >> 20
                    ))
                } else if session.is_none() {
                    Some("the pane's shell is not one a session daemon runs".to_owned())
                } else {
                    None
                };
                match (refused, session) {
                    (None, Some(link)) => {
                        link.send_file(extension, bytes, Box::new(move |kept| answer(Answer::Kept(kept))))
                    }
                    (why, _) => answer(Answer::Kept(Err(why.unwrap_or_default()))),
                }
            }
            other => return Some(other),
        }
        None
    }

    /// What has been answered since the last frame, and what has waited too
    /// long, each with the function to call.
    pub(super) fn answered(&mut self) -> Vec<(Later, Answer)> {
        let mut done = Vec::new();
        while let Ok((id, answer)) = self.answers.1.try_recv() {
            // Not waited for any more: it was given up on.
            if let Some(waiting) = self.waiting.remove(&id) {
                done.push((waiting.then, answer));
            }
        }
        let now = Instant::now();
        let late: Vec<u64> = self
            .waiting
            .iter()
            .filter(|(_, waiting)| waiting.deadline.is_some_and(|deadline| deadline <= now))
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            if let Some(waiting) = self.waiting.remove(&id) {
                done.push((waiting.then, Answer::Kept(Err("the session daemon did not answer".into()))));
            }
        }
        done
    }
}

impl Answer {
    /// Call `then` with this.
    pub(super) fn call(self, then: Later) -> Result<Vec<Command>, String> {
        match self {
            Self::Clipboard(clip) => then.clipboard(clip),
            Self::Kept(path) => then.kept(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A function that pastes what it is told.
    fn paste_why() -> Later {
        Later::of("return function(app, path, why) app:paste(path or why) end")
    }

    fn pasted(laters: &mut Laters) -> Vec<String> {
        laters
            .answered()
            .into_iter()
            .flat_map(|(then, answer)| answer.call(then).unwrap())
            .map(|command| match command {
                Command::Paste(text) => text,
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_file_that_cannot_be_sent_is_answered_on_the_next_frame() {
        let ctx = egui::Context::default();
        let mut laters = Laters::default();
        let send = |bytes: usize| Command::SendFile { bytes: vec![0; bytes], extension: "png".into(), then: paste_why() };
        assert!(laters.ask(send(1), None, &ctx).is_none());
        assert!(laters.ask(send(LARGEST + 1), None, &ctx).is_none());
        let said = pasted(&mut laters);
        assert_eq!(said.len(), 2);
        assert!(said.iter().any(|why| why.contains("not one a session daemon runs")), "{said:?}");
        assert!(said.iter().any(|why| why.contains("33 MiB")), "{said:?}");
        // Each is answered once.
        assert!(pasted(&mut laters).is_empty());
        // Anything else is given back to be done at once.
        assert!(matches!(laters.ask(Command::Paste("x".into()), None, &ctx), Some(Command::Paste(_))));
    }

    #[test]
    fn a_daemon_that_does_not_answer_is_given_up_on() {
        let mut laters = Laters::default();
        laters.waiting.insert(7, Waiting { then: paste_why(), deadline: Some(Instant::now()) });
        laters.waiting.insert(8, Waiting { then: paste_why(), deadline: None });
        assert_eq!(pasted(&mut laters), ["the session daemon did not answer"]);
        // The answer that comes after it was given up on is dropped.
        laters.answers.0.send((7, Answer::Kept(Ok("late".into())))).unwrap();
        laters.answers.0.send((8, Answer::Kept(Ok("kept".into())))).unwrap();
        assert_eq!(pasted(&mut laters), ["kept"]);
    }
}

//! A picker request that comes through the session daemon.
//!
//! A shell the daemon runs has no pipe of its own to the pane. A program
//! in it connects to the daemon, which admits only the daemon's own user
//! and passes the program's messages to the UI showing the pane. So there
//! is no handshake and nothing to prove here: one conversation is one
//! request, starting with `open`, and it ends with the result.
//!
//! This is the part of a pipe server that is not the pipe: what order the
//! messages may come in, and how much they may hold.

use super::{Event, MAX_FRAME, MAX_ITEM, MAX_ITEMS, MAX_STORAGE, Message, Session, parse};
use crossbeam_channel::Receiver;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};

pub(crate) struct Request {
    session: Arc<Session>,
    stream: bool,
    /// The batch expected next.
    sequence: u64,
    ended: bool,
    bytes: usize,
    items: usize,
    ids: HashSet<String>,
}

/// The program broke the protocol; the conversation is to be ended.
pub(crate) struct Broken;

impl Request {
    /// From a conversation's first message. With it come what the pane is
    /// to be given, and where the picker's replies arrive.
    pub(crate) fn open(bytes: &[u8]) -> Result<(Self, Event, Receiver<Value>), Broken> {
        let Message::Open { request_id, request } = read(bytes)? else {
            return Err(Broken);
        };
        if request_id.is_empty() || request_id.len() > 128 {
            return Err(Broken);
        }
        let (session, replies) = Session::new(request_id);
        let this = Self {
            session: session.clone(),
            stream: request.streams_items(),
            sequence: 0,
            ended: false,
            bytes: 0,
            items: 0,
            ids: HashSet::new(),
        };
        Ok((this, Event::Open { request, session }, replies))
    }

    /// A later message. `None` when it is nothing to the pane.
    pub(crate) fn accept(&mut self, bytes: &[u8]) -> Result<Option<Event>, Broken> {
        let message = read(bytes)?;
        let session = &self.session;
        // What was on its way when the result was sent.
        if session.done() {
            return Ok(None);
        }
        match message {
            Message::Cancel { request_id } if request_id == session.id => {
                session.cancelled.store(true, Ordering::Release);
                session.finish("cancelled", json!({"reason":"client"}));
                Ok(None)
            }
            // A pipe's client confirms the result; here the daemon keeps it.
            Message::Received { request_id } if request_id == session.id => Ok(None),
            Message::Items { request_id, sequence, items }
                if self.stream
                    && !self.ended
                    && request_id == session.id
                    && sequence == self.sequence =>
            {
                if items.is_empty() || items.len() > 1024 {
                    return Err(Broken);
                }
                for item in &items {
                    let value = serde_json::to_vec(&item.value).map_err(|_| Broken)?;
                    let size = item.text.len() + value.len() + item.id.len();
                    if item.id.is_empty()
                        || item.id.len() > 128
                        || size > MAX_ITEM
                        || !self.ids.insert(item.id.clone())
                    {
                        return Err(Broken);
                    }
                    self.bytes += size;
                    self.items += 1;
                    if self.bytes > MAX_STORAGE || self.items > MAX_ITEMS {
                        return Err(Broken);
                    }
                }
                self.sequence += 1;
                Ok(Some(Event::Items { sequence, items, session: session.clone() }))
            }
            Message::EndItems { request_id, last_sequence }
                if self.stream
                    && !self.ended
                    && request_id == session.id
                    && last_sequence == self.sequence.checked_sub(1) =>
            {
                self.ended = true;
                Ok(Some(Event::End { session: session.clone() }))
            }
            _ => Err(Broken),
        }
    }

    /// The program has gone, or the daemon ended the conversation: a
    /// picker still open for it closes.
    pub(crate) fn gone(&self) {
        self.session.cancelled.store(true, Ordering::Release);
    }
}

fn read(bytes: &[u8]) -> Result<Message, Broken> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Broken);
    }
    parse(bytes).map_err(|_| Broken)
}

/// Whether a reply is the last of its request.
pub(crate) fn last(reply: &Value) -> bool {
    matches!(reply["type"].as_str(), Some("result" | "error"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(value: Value) -> Vec<u8> {
        serde_json::to_vec(&value).unwrap()
    }

    fn stream() -> (Request, Receiver<Value>) {
        let open = json!({"type":"open","request_id":"r","request":{"kind":"stream","options":{}}});
        let Ok((request, Event::Open { .. }, replies)) = Request::open(&bytes(open)) else {
            panic!("the open was not taken");
        };
        (request, replies)
    }

    fn items(sequence: u64, id: &str) -> Vec<u8> {
        bytes(json!({"type":"items","request_id":"r","sequence":sequence,
            "items":[{"id":id,"text":"t","value":"v"}]}))
    }

    #[test]
    fn a_request_starts_with_an_open_and_takes_batches_in_order() {
        assert!(Request::open(&items(0, "a")).is_err());
        assert!(Request::open(b"not json").is_err());

        let (mut request, _replies) = stream();
        assert!(matches!(request.accept(&items(0, "a")), Ok(Some(Event::Items { sequence: 0, .. }))));
        // Out of order, and an item sent twice.
        assert!(request.accept(&items(2, "b")).is_err());
        assert!(request.accept(&items(1, "a")).is_err());
        assert!(matches!(request.accept(&items(1, "b")), Ok(Some(Event::Items { sequence: 1, .. }))));
        let end = bytes(json!({"type":"end_items","request_id":"r","last_sequence":1}));
        assert!(matches!(request.accept(&end), Ok(Some(Event::End { .. }))));
        // Nothing follows the end.
        assert!(request.accept(&items(2, "c")).is_err());
    }

    #[test]
    fn a_cancel_is_answered_and_what_follows_a_result_is_dropped() {
        let (mut request, replies) = stream();
        let cancel = bytes(json!({"type":"cancel","request_id":"r"}));
        assert!(matches!(request.accept(&cancel), Ok(None)));
        let reply = replies.try_recv().unwrap();
        assert_eq!((reply["status"].as_str(), last(&reply)), (Some("cancelled"), true));
        // Sent before the program saw the result.
        assert!(matches!(request.accept(&items(0, "a")), Ok(None)));
    }
}

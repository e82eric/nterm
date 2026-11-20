//! The client side of the picker protocol (docs/picker-pipe.md): reach the
//! session daemon that runs this shell, open one picker, stream items and
//! return its result.
use serde_json::{Value, json};
use std::{io::Write, time::Duration};

/// The most one message may hold.
const MAX_FRAME: usize = 1024 * 1024;

/// One streamed candidate: what the picker shows and what it returns.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub(crate) text: String,
    pub(crate) value: String,
}

/// How a request ended.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Accepted(Value),
    Cancelled,
}

const REQUEST_ID: &str = "npick";
/// Matches the server's per-item limit on id + text + encoded value.
const MAX_ITEM: usize = 64 * 1024;
const MAX_BATCH_ITEMS: usize = 1024;
/// Leaves room for JSON escaping and framing inside one 1 MiB frame.
const MAX_BATCH_BYTES: usize = MAX_FRAME / 4;
const POLL: Duration = Duration::from_millis(50);

/// The connection a picker is opened over: always the session daemon's.
pub(crate) use Daemon as Connection;

/// To the pane this shell runs in, through the session daemon that runs it.
pub(crate) fn connect() -> Result<Connection, String> {
    daemon().ok_or("not running in an nterm pane whose shell the session daemon runs")?
}

/// To the session daemon that runs this shell, for a picker it runs itself:
/// see `nterm_session::shell::Program::pick`.
pub(crate) fn connect_daemon() -> Result<Connection, String> {
    let program = nterm_session::shell::Program::pick_from_environment()
        .ok_or("not running in an nterm pane whose shell a session daemon runs (NTERM_DAEMON is not set)")?;
    program.map(Daemon).map_err(|error| format!("unable to reach the session daemon: {error}"))
}

/// The session daemon runs this shell and knows which pane shows it. It
/// has checked who is asking, so there is nothing to prove to the pane.
pub(crate) struct Daemon(nterm_session::shell::Program);

/// The daemon whose shell this is, if the environment names one.
fn daemon() -> Option<Result<Daemon, String>> {
    let program = nterm_session::shell::Program::from_environment()?;
    Some(program.map(Daemon).map_err(|error| format!("unable to reach the session daemon: {error}")))
}

impl Daemon {
    fn send(&mut self, value: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FRAME {
            return Err("request exceeds the 1 MiB frame limit".into());
        }
        self.0.send(&bytes).map_err(|e| format!("connection lost: {e}"))
    }
    fn poll(&mut self) -> Result<Option<Value>, String> {
        use nterm_session::shell::Event;
        match self.0.receive(POLL) {
            None => Ok(None),
            Some(Event::Message(bytes)) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| format!("invalid message from terminal: {e}")),
            Some(Event::Closed) => Err("the terminal ended the request".into()),
            Some(Event::Refused(why)) => Err(why),
            Some(Event::Lost) => Err("the session daemon closed the connection".into()),
        }
    }
    /// The daemon keeps the result until it is read; nothing to confirm.
    fn received(&mut self) {}
}

fn error_message(value: &Value) -> String {
    match (value["code"].as_str(), value["message"].as_str()) {
        (Some(code), Some(message)) => format!("{message} ({code})"),
        (_, Some(message)) => message.into(),
        _ => "the terminal reported an error".into(),
    }
}

/// Bytes `value` takes as a JSON string, which is how the server measures it:
/// quotes plus escapes, e.g. 2 bytes for `"` and 6 for a control character.
fn encoded_len(value: &str) -> usize {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).expect("writing to a counter cannot fail");
    counter.0
}

/// Collect the next batch from `items`, waiting at most `POLL` for the first.
/// Returns `None` once the producer has finished and nothing is left.
fn next_batch(items: &crossbeam_channel::Receiver<Item>, next_id: &mut u64) -> Option<Vec<Value>> {
    let mut batch = Vec::new();
    let mut bytes = 0;
    let first = match items.recv_timeout(POLL) {
        Ok(item) => item,
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => return Some(batch),
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return None,
    };
    let mut pending = Some(first);
    while let Some(item) = pending.take() {
        let id = next_id.to_string();
        *next_id += 1;
        let size = id.len() + item.text.len() + encoded_len(&item.value);
        if size > MAX_ITEM {
            eprintln!("npick: skipping an item larger than 64 KiB");
        } else {
            bytes += size;
            batch.push(json!({"id": id, "text": item.text, "value": item.value}));
        }
        if batch.len() >= MAX_BATCH_ITEMS || bytes >= MAX_BATCH_BYTES {
            break;
        }
        pending = items.try_recv().ok();
    }
    Some(batch)
}

/// Open a picker with `opening`, the fields of an `open` message beside its
/// type and number (`request` for the window, `picker` and `params` for the
/// daemon), and wait for its result. A picker of streamed rows is sent
/// `items` until the channel closes.
pub(crate) fn run(
    mut connection: Connection,
    opening: Value,
    items: Option<crossbeam_channel::Receiver<Item>>,
) -> Result<Outcome, String> {
    let mut open = json!({"type":"open","request_id":REQUEST_ID});
    if let (Value::Object(open), Value::Object(opening)) = (&mut open, opening) {
        open.extend(opening);
    }
    connection.send(&open)?;
    let mut opened = false;
    let mut in_flight = false;
    let mut ended = items.is_none();
    let mut sequence: u64 = 0;
    let mut next_id: u64 = 0;
    loop {
        // One batch in flight at a time; wait for its ack before the next.
        if opened && !in_flight && !ended {
            match next_batch(items.as_ref().unwrap(), &mut next_id) {
                Some(batch) if batch.is_empty() => {}
                Some(batch) => {
                    connection.send(&json!({"type":"items","request_id":REQUEST_ID,"sequence":sequence,"items":batch}))?;
                    sequence += 1;
                    in_flight = true;
                }
                None => {
                    let last = sequence.checked_sub(1);
                    connection.send(
                        &json!({"type":"end_items","request_id":REQUEST_ID,"last_sequence":last}),
                    )?;
                    ended = true;
                    in_flight = true;
                }
            }
        }
        let Some(message) = connection.poll()? else {
            continue;
        };
        match message["type"].as_str() {
            Some("opened") => opened = true,
            Some("ack" | "input_complete") => in_flight = false,
            Some("result") => {
                connection.received();
                return match message["status"].as_str() {
                    Some("accepted") => {
                        Ok(Outcome::Accepted(message["selected"][0]["value"].clone()))
                    }
                    Some("cancelled") => Ok(Outcome::Cancelled),
                    _ => Err(error_message(&message)),
                };
            }
            _ => return Err("unexpected message from terminal".into()),
        }
    }
}

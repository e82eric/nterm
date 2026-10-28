//! The client side of the picker IPC protocol (docs/picker-pipe.md): connect,
//! authenticate, open one picker, stream items and return its result.
//!
//! Included by the `npick` binary and by the Unix server's tests, so it only
//! uses `super::protocol`, serde_json, crossbeam-channel and getrandom.
use super::protocol::{MAX_FRAME, hex, proof};
use serde_json::{Value, json};
use std::{
    io::{ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

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

struct Connection {
    stream: UnixStream,
    buffer: Vec<u8>,
}
impl Connection {
    fn send(&mut self, value: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FRAME {
            return Err("request exceeds the 1 MiB frame limit".into());
        }
        let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&bytes);
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("connection lost: {e}"))
    }
    /// The next complete message, or `None` if none arrives within `POLL`.
    fn poll(&mut self) -> Result<Option<Value>, String> {
        loop {
            if self.buffer.len() >= 4 {
                let len = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
                if len == 0 || len > MAX_FRAME {
                    return Err("invalid frame from terminal".into());
                }
                if self.buffer.len() >= 4 + len {
                    let value = serde_json::from_slice(&self.buffer[4..4 + len])
                        .map_err(|e| format!("invalid message from terminal: {e}"))?;
                    self.buffer.drain(..4 + len);
                    return Ok(Some(value));
                }
            }
            let mut chunk = [0; 64 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("the terminal closed the connection".into()),
                Ok(count) => self.buffer.extend_from_slice(&chunk[..count]),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return Ok(None);
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("connection lost: {error}")),
            }
        }
    }
    /// Wait up to `timeout` for the next message.
    fn recv(&mut self, timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = self.poll()? {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for the terminal".into());
            }
        }
    }
}

fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("unable to generate a nonce: {e}"))?;
    Ok(hex(&bytes))
}

/// Connect and authenticate. `endpoint` is the socket path from
/// NTERM_PICKER_ENDPOINT, which is also part of the HMAC transcript.
fn connect(endpoint: &str, token: &str) -> Result<Connection, String> {
    let stream = UnixStream::connect(endpoint)
        .map_err(|e| format!("unable to connect to {endpoint}: {e}"))?;
    stream
        .set_read_timeout(Some(POLL))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(5))))
        .map_err(|e| e.to_string())?;
    let mut connection = Connection {
        stream,
        buffer: Vec::new(),
    };
    let challenge = connection.recv(Duration::from_secs(5))?;
    let server_nonce = match (&challenge["type"], &challenge["nonce"]) {
        (Value::String(kind), Value::String(nonce)) if kind == "challenge" => nonce.clone(),
        _ => return Err("unexpected handshake from terminal".into()),
    };
    let client_nonce = nonce()?;
    connection.send(&json!({
        "type": "authenticate",
        "version": 1,
        "client_nonce": client_nonce,
        "proof": hex(&proof(token, endpoint, &server_nonce, &client_nonce, "client")),
    }))?;
    let ready = connection.recv(Duration::from_secs(5))?;
    match ready["type"].as_str() {
        Some("ready") => {}
        Some("error") => {
            let _ = connection.send(&json!({"type":"received","request_id":ready["request_id"]}));
            return Err(error_message(&ready));
        }
        _ => return Err("authentication failed; check NTERM_PICKER_TOKEN".into()),
    }
    let expected = hex(&proof(
        token,
        endpoint,
        &server_nonce,
        &client_nonce,
        "server",
    ));
    if ready["proof"].as_str() != Some(expected.as_str()) {
        return Err("the terminal failed to prove it knows the token".into());
    }
    Ok(connection)
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

/// Open `request` (the `request` object of an `open` message) and wait for
/// its result. Stream requests send `items` until the channel closes.
pub(crate) fn run(
    endpoint: &str,
    token: &str,
    request: Value,
    items: Option<crossbeam_channel::Receiver<Item>>,
) -> Result<Outcome, String> {
    let mut connection = connect(endpoint, token)?;
    connection.send(&json!({"type":"open","request_id":REQUEST_ID,"request":request}))?;
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
                connection.send(&json!({"type":"received","request_id":REQUEST_ID}))?;
                // Let the server finish the request cleanly before closing.
                if let Ok(idle) = connection.recv(Duration::from_secs(5)) {
                    if idle["type"] == "idle" {
                        let _ =
                            connection.send(&json!({"type":"received","request_id":REQUEST_ID}));
                    }
                }
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

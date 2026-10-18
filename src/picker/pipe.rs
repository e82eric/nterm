//! Transport-independent picker IPC schema and request lifecycle.
//! An authenticated connection serves sequential requests; no request replay.
use crossbeam_channel::{Receiver, Sender};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

// Shared with the npick client binary, so it depends only on hmac and sha2.
mod protocol;
pub(crate) use protocol::{MAX_FRAME, hex, proof, verify_proof};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::Service;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::Service;

pub(crate) const MAX_ITEM: usize = 64 * 1024;
pub(crate) const MAX_ITEMS: usize = 1_000_000;
pub(crate) const MAX_STORAGE: usize = 256 * 1024 * 1024;

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Policy {
    pub enabled: bool,
    pub allow_stream: bool,
    pub named: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Open {
    Named {
        name: String,
        #[serde(default)]
        params: std::collections::BTreeMap<String, String>,
    },
    Stream {
        #[serde(default)]
        options: StreamOptions,
    },
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct StreamOptions {
    pub position: super::config::Position,
    pub initial_query: String,
    pub preview: Option<Value>,
    pub preview_visible: bool,
    pub result_rows: Option<usize>,
    pub preview_rows: Option<usize>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Item {
    pub id: String,
    pub text: String,
    pub value: Value,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Message {
    Authenticate {
        version: u32,
        client_nonce: String,
        proof: String,
    },
    Open {
        request_id: String,
        request: Open,
    },
    Items {
        request_id: String,
        sequence: u64,
        items: Vec<Item>,
    },
    EndItems {
        request_id: String,
        last_sequence: Option<u64>,
    },
    Cancel {
        request_id: String,
    },
    Received {
        request_id: String,
    },
}

pub(crate) enum Event {
    Open {
        request: Open,
        session: Arc<Session>,
    },
    Items {
        sequence: u64,
        items: Vec<Item>,
        session: Arc<Session>,
    },
    End {
        session: Arc<Session>,
    },
}

pub(crate) struct Session {
    pub id: String,
    pub cancelled: AtomicBool,
    done: AtomicBool,
    replies: Sender<Value>,
}
impl Session {
    pub fn new(id: String) -> (Arc<Self>, Receiver<Value>) {
        let (tx, rx) = crossbeam_channel::bounded(4);
        (
            Arc::new(Self {
                id,
                cancelled: AtomicBool::new(false),
                done: AtomicBool::new(false),
                replies: tx,
            }),
            rx,
        )
    }
    pub fn reply(&self, value: Value) {
        let _ = self.replies.try_send(value);
    }
    pub fn finish(&self, status: &str, extra: Value) {
        if self
            .done
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let mut value = json!({"type":"result", "request_id":self.id, "status":status});
        if let (Some(dst), Some(src)) = (value.as_object_mut(), extra.as_object()) {
            dst.extend(src.clone());
        }
        if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() > MAX_FRAME) {
            value = json!({"type":"result","request_id":self.id,"status":"error","code":"result_limit","message":"picker result exceeds 1 MiB"});
        }
        self.reply(value);
    }
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

impl Policy {
    pub fn authorize(&self, request: &Open) -> Result<(), String> {
        if !self.enabled {
            return Err("picker pipe is disabled in Lua configuration".into());
        }
        match request {
            Open::Named { name, params } => {
                if !self.named.contains(name) {
                    return Err("named picker is not exposed to IPC".into());
                }
                if name.is_empty()
                    || name.len() > 128
                    || params.len() > 32
                    || params
                        .iter()
                        .any(|(k, v)| k.is_empty() || k.len() > 128 || v.len() > MAX_ITEM)
                {
                    return Err("invalid named picker parameters".into());
                }
            }
            Open::Stream { .. } if !self.allow_stream => {
                return Err("streamed pickers are not permitted".into());
            }
            Open::Stream { options } => {
                if options.initial_query.len() > 4096
                    || options.result_rows.is_some_and(|n| !(1..=256).contains(&n))
                    || options
                        .preview_rows
                        .is_some_and(|n| !(1..=256).contains(&n))
                {
                    return Err("invalid stream presentation".into());
                }
                if let Some(preview) = &options.preview {
                    match preview.get("type").and_then(Value::as_str) {
                        Some("Text" | "Ansi") => {}
                        _ => return Err("stream previews must be Text or Ansi".into()),
                    }
                }
            }
        }
        Ok(())
    }
}

/// Reject duplicate fields even inside definition/value objects.
pub(crate) fn parse(bytes: &[u8]) -> Result<Message, String> {
    struct Strict(Value);
    impl<'de> Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Strict;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON")
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> Result<Strict, A::Error> {
                    let mut v = Vec::new();
                    while let Some(Strict(x)) = a.next_element()? {
                        v.push(x);
                    }
                    Ok(Strict(Value::Array(v)))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> Result<Strict, A::Error> {
                    let mut v = serde_json::Map::new();
                    while let Some(k) = a.next_key::<String>()? {
                        if v.contains_key(&k) {
                            return Err(serde::de::Error::custom("duplicate JSON field"));
                        }
                        let Strict(x) = a.next_value()?;
                        v.insert(k, x);
                    }
                    Ok(Strict(Value::Object(v)))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err("invalid frame size".into());
    }
    let Strict(value) = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentication_is_bound_to_endpoint_nonce_and_role() {
        let p = hex(&proof("token", "endpoint", "server", "client", "client"));
        assert!(verify_proof("token", "endpoint", "server", "client", &p));
        assert!(!verify_proof("token", "other", "server", "client", &p));
        assert!(!verify_proof("token", "endpoint", "new", "client", &p));
        assert_ne!(
            p,
            hex(&proof("token", "endpoint", "server", "client", "server"))
        );
    }
    #[test]
    fn schema_rejects_duplicate_unknown_and_oversized_fields() {
        for s in [
            r#"{"type":"cancel","request_id":"a","request_id":"b"}"#,
            r#"{"type":"cancel","request_id":"a","extra":1}"#,
            r#"{"type":"open","request_id":"a","request":{"kind":"definition","definition":{"type":"Items","type":"Command"}}}"#,
        ] {
            assert!(parse(s.as_bytes()).is_err());
        }
        assert!(parse(&vec![0; MAX_FRAME + 1]).is_err());
    }
    #[test]
    fn capabilities_are_separate_and_disabled_by_default() {
        let request = Open::Stream {
            options: Default::default(),
        };
        assert!(Policy::default().authorize(&request).is_err());
        let policy = Policy {
            enabled: true,
            allow_stream: true,
            ..Default::default()
        };
        assert!(policy.authorize(&request).is_ok());
        let command_preview = Open::Stream {
            options: StreamOptions {
                preview: Some(json!({"type":"Command","args":["pwsh.exe"]})),
                ..Default::default()
            },
        };
        assert!(policy.authorize(&command_preview).is_err());
        assert!(
            policy
                .authorize(&Open::Stream {
                    options: StreamOptions {
                        preview: Some(json!({"type":"File"})),
                        ..Default::default()
                    }
                })
                .is_err()
        );
        assert!(parse(br#"{"type":"open","request_id":"test","request":{"kind":"definition","definition":{"type":"Command","args":["pwsh.exe"]}}}"#).is_err());
        assert!(
            policy
                .authorize(&Open::Named {
                    name: "files".into(),
                    params: Default::default()
                })
                .is_err()
        );
    }
    #[test]
    fn only_one_terminal_result_wins() {
        let (s, rx) = Session::new("a".into());
        s.finish("accepted", json!({}));
        s.finish("cancelled", json!({}));
        assert_eq!(rx.try_recv().unwrap()["status"], "accepted");
        assert!(rx.try_recv().is_err());
    }
}

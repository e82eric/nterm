//! Bounded timing-only traces. Never retain item text, commands, or credentials.
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) const HISTORY_LIMIT: usize = 64;
const EVENT_LIMIT: usize = 64;

#[derive(Clone, Debug)]
struct Event {
    stage: &'static str,
    elapsed_ms: f64,
    preview_id: Option<u64>,
}
#[derive(Debug)]
pub(crate) struct Trace {
    request_id: String,
    start: Instant,
    started_unix_ms: u128,
    events: Mutex<(VecDeque<Event>, u64)>,
}
impl Trace {
    pub fn new(request_id: String, start: Instant) -> Arc<Self> {
        let trace = Arc::new(Self {
            request_id,
            start,
            started_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            events: Mutex::new((VecDeque::new(), 0)),
        });
        trace.mark_at("request_received", None, start);
        trace
    }
    pub fn mark(&self, stage: &'static str, preview_id: Option<u64>) {
        self.mark_at(stage, preview_id, Instant::now());
    }
    pub fn mark_at(&self, stage: &'static str, preview_id: Option<u64>, at: Instant) {
        let mut events = self.events.lock().unwrap();
        if events
            .0
            .iter()
            .any(|e| e.stage == stage && e.preview_id == preview_id)
        {
            return;
        }
        // Retain startup events and the most recent preview events.
        if events.0.len() == EVENT_LIMIT {
            let index = events
                .0
                .iter()
                .position(|e| e.preview_id.is_some())
                .unwrap_or(0);
            events.0.remove(index);
            events.1 += 1;
        }
        events.0.push_back(Event {
            stage,
            elapsed_ms: at
                .checked_duration_since(self.start)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0,
            preview_id,
        });
    }
    pub fn snapshot(&self) -> Value {
        let events = self.events.lock().unwrap();
        let mut spans = Vec::new();
        for (name, from, to) in [
            (
                "pipe_wake_delivery",
                "pipe_wake_queued",
                "pipe_wake_handled",
            ),
            ("pipe_wake_to_redraw", "pipe_wake_handled", "redraw_started"),
            ("redraw_to_ui", "redraw_started", "ui_frame_started"),
            (
                "search_worker_to_snapshot",
                "search_started",
                "search_snapshot_ready",
            ),
            (
                "snapshot_to_search_results",
                "search_snapshot_ready",
                "search_matches_ready",
            ),
            (
                "search_results_to_ui_pickup",
                "search_matches_ready",
                "search_matches_taken",
            ),
            (
                "search_wake_delivery",
                "search_wake_queued",
                "search_wake_handled",
            ),
            (
                "search_pickup_to_frame",
                "search_matches_taken",
                "first_results_frame_built",
            ),
            (
                "picker_creation",
                "definition_resolution_finished",
                "picker_created",
            ),
            ("security_checks", "request_received", "security_checked"),
            ("ui_dispatch", "authorized", "ui_dispatched"),
            (
                "definition_resolution",
                "definition_resolution_started",
                "definition_resolution_finished",
            ),
            (
                "first_picker_frame_build",
                "picker_frame_started",
                "picker_frame_built",
            ),
            (
                "item_publication",
                "first_items_received",
                "first_items_published",
            ),
            (
                "request_to_picker_frame",
                "request_received",
                "picker_frame_built",
            ),
            (
                "request_to_first_results_frame",
                "request_received",
                "first_results_frame_built",
            ),
            (
                "items_to_first_results_frame",
                "first_items_received",
                "first_results_frame_built",
            ),
            ("command_spawn", "command_spawn_started", "command_spawned"),
            (
                "command_to_first_stdout",
                "command_spawned",
                "command_first_stdout",
            ),
            (
                "command_to_first_items",
                "command_spawn_started",
                "command_first_items_published",
            ),
            ("preview_spawn", "preview_spawn_started", "preview_spawned"),
            (
                "preview_to_first_stdout",
                "preview_spawned",
                "preview_first_stdout",
            ),
            (
                "preview_command",
                "preview_spawn_started",
                "preview_process_finished",
            ),
            (
                "preview_prepare_to_document",
                "preview_requested",
                "preview_document_ready",
            ),
            (
                "preview_document_to_frame",
                "preview_document_ready",
                "preview_frame_built",
            ),
            (
                "preview_prepare_to_frame",
                "preview_requested",
                "preview_frame_built",
            ),
        ] {
            for first in events.0.iter().filter(|e| e.stage == from) {
                if let Some(last) = events
                    .0
                    .iter()
                    .find(|e| e.stage == to && e.preview_id == first.preview_id)
                {
                    if last.elapsed_ms >= first.elapsed_ms {
                        spans.push(json!({"name":name,"preview_id":first.preview_id,"duration_ms":last.elapsed_ms-first.elapsed_ms}));
                    }
                }
            }
        }
        json!({"request_id":self.request_id,"started_unix_ms":self.started_unix_ms,
            "events":events.0.iter().map(|e| json!({"stage":e.stage,"elapsed_ms":e.elapsed_ms,"preview_id":e.preview_id})).collect::<Vec<_>>(),
            "spans":spans,"dropped_events":events.1})
    }
}

pub(crate) fn record_search(trace: &Trace, timings: &nfm_egui::SearchTimings) {
    for (stage, at) in [
        ("search_started", timings.started),
        ("search_snapshot_ready", timings.first_snapshot_ready),
        ("search_update_ready", timings.first_update_ready),
        ("search_matches_ready", timings.first_matches_ready),
        ("search_matches_taken", timings.first_matches_taken),
    ] {
        if let Some(at) = at {
            trace.mark_at(stage, None, at);
        }
    }
}

thread_local! { static CURRENT: RefCell<Option<Arc<Trace>>> = const { RefCell::new(None) }; }
pub(crate) fn current() -> Option<Arc<Trace>> {
    CURRENT.with(|c| c.borrow().clone())
}
pub(crate) fn command_observer(
    trace: Arc<Trace>,
    preview_id: Option<u64>,
) -> nfm_picker_sources::command::CommandObserver {
    use nfm_picker_sources::command::CommandEvent;
    nfm_picker_sources::command::CommandObserver::new(move |event| {
        let stage = match (preview_id.is_some(), event) {
            (false, CommandEvent::SpawnStarted) => "command_spawn_started",
            (false, CommandEvent::Spawned) => "command_spawned",
            (false, CommandEvent::FirstStdout) => "command_first_stdout",
            (false, CommandEvent::Finished) => "command_process_finished",
            (true, CommandEvent::SpawnStarted) => "preview_spawn_started",
            (true, CommandEvent::Spawned) => "preview_spawned",
            (true, CommandEvent::FirstStdout) => "preview_first_stdout",
            (true, CommandEvent::Finished) => "preview_process_finished",
        };
        trace.mark(stage, preview_id);
    })
}
pub(crate) struct Scope(Option<Arc<Trace>>);
impl Scope {
    pub fn enter(trace: Arc<Trace>) -> Self {
        Self(CURRENT.with(|c| c.replace(Some(trace))))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|c| c.replace(self.0.take()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn security_timing_includes_authorization_since_receipt() {
        let start = Instant::now();
        let trace = Trace::new("security".into(), start);
        trace.mark_at(
            "security_checked",
            None,
            start + std::time::Duration::from_millis(5),
        );
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["events"][0]["elapsed_ms"], 0.0);
        assert_eq!(snapshot["spans"][0]["duration_ms"], 5.0);
    }
    #[test]
    fn traces_are_bounded_and_preview_spans_never_mix_cycles() {
        let trace = Trace::new("correlation".into(), Instant::now());
        trace.mark("preview_requested", Some(1));
        trace.mark("preview_frame_built", Some(2));
        assert!(trace.snapshot()["spans"].as_array().unwrap().is_empty());
        trace.mark("preview_frame_built", Some(1));
        assert_eq!(trace.snapshot()["spans"][0]["preview_id"], 1);
        for i in 3..100 {
            trace.mark("preview_requested", Some(i));
        }
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["events"].as_array().unwrap().len(), EVENT_LIMIT);
        assert!(snapshot["dropped_events"].as_u64().unwrap() > 0);
        assert_eq!(snapshot["events"][0]["stage"], "request_received");
    }
}

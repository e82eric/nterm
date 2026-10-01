use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const CAPACITY: usize = 200;
const DETAIL_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Severity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) id: u64,
    pub(crate) timestamp: u64,
    pub(crate) severity: Severity,
    pub(crate) source: String,
    pub(crate) summary: String,
    pub(crate) details: String,
    pub(crate) repeats: u64,
}

impl Record {
    pub(crate) fn label(&self) -> String {
        let time = self.timestamp % 86400;
        format!(
            "{:02}:{:02}:{:02} UTC · {:?} · {} · {}{}",
            time / 3600,
            time / 60 % 60,
            time % 60,
            self.severity,
            self.source,
            self.summary,
            if self.repeats > 1 {
                format!(" (×{})", self.repeats)
            } else {
                String::new()
            }
        )
    }
    pub(crate) fn text(&self) -> String {
        format!("{}\n\n{}", self.label(), self.details)
    }
}

#[derive(Default)]
struct History {
    records: VecDeque<Record>,
    next_id: u64,
}

fn bounded(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.into();
    }
    let mut end = limit - "\n[truncated]".len();
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &value[..end])
}

impl History {
    fn push(&mut self, severity: Severity, source: &str, summary: &str, details: &str) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let source = bounded(source, 128);
        let summary = bounded(summary, 256);
        let details = bounded(details, DETAIL_LIMIT);
        // Aggregate identical failures, moving the most recent occurrence to
        // the front. Compare bounded values to keep storage and matching bounded.
        if let Some(index) = self.records.iter().position(|record| {
            record.severity == severity
                && record.source == source
                && record.summary == summary
                && record.details == details
        }) {
            let mut record = self.records.remove(index).unwrap();
            record.repeats = record.repeats.saturating_add(1);
            record.timestamp = timestamp;
            self.records.push_front(record);
            return;
        }
        self.next_id = self.next_id.wrapping_add(1);
        self.records.push_front(Record {
            id: self.next_id,
            timestamp,
            severity,
            source,
            summary,
            details,
            repeats: 1,
        });
        self.records.truncate(CAPACITY);
    }
}

static HISTORY: LazyLock<Mutex<History>> = LazyLock::new(|| Mutex::new(History::default()));

pub(crate) fn report(severity: Severity, source: &str, summary: &str, details: &str) {
    eprintln!("{source}: {summary}: {details}");
    HISTORY
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(severity, source, summary, details);
}
pub(crate) fn error(source: &str, summary: &str, details: &str) {
    report(Severity::Error, source, summary, details);
}
pub(crate) fn warning(source: &str, summary: &str, details: &str) {
    report(Severity::Warning, source, summary, details);
}
pub(crate) fn snapshot() -> Vec<Record> {
    HISTORY
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .records
        .iter()
        .cloned()
        .collect()
}

pub(crate) fn picker_options() -> crate::picker::config::Options {
    options_from_records(snapshot())
}

fn options_from_records(records: Vec<Record>) -> crate::picker::config::Options {
    let mut options = crate::themes::picker_options(Vec::new());
    options.sort = crate::picker::config::Sort::SourceOrder;
    options
        .params
        .insert("__error_history".into(), "true".into());
    options.source = crate::picker::config::Source::CommandPalette(
        records
            .into_iter()
            .map(|record| crate::actions::Entry {
                id: format!("error.{}", record.id),
                label: record.label(),
                shortcut: None,
                action: crate::actions::AppAction::ShowErrorRecord(Box::new(record)),
            })
            .collect(),
    );
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picker_returns_full_record_even_after_history_eviction() {
        let mut history = History::default();
        history.push(
            Severity::Error,
            "test",
            "Unique failure",
            "full diagnostic details",
        );
        let options = options_from_records(history.records.iter().cloned().collect());
        for index in 0..CAPACITY {
            history.push(Severity::Error, "test", &index.to_string(), "later");
        }
        let ctx = egui::Context::default();
        let mut picker = crate::picker::Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            options,
        );
        assert_eq!(picker.label(), "ERROR HISTORY");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut selected = None;
        while selected.is_none() {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: Some(egui::Key::Enter),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    selected = picker.show(ui.ctx(), 24.0);
                },
            );
            output.textures_delta.clear();
            assert!(std::time::Instant::now() < deadline);
            if selected.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        assert!(
            matches!(selected, Some(crate::picker::Action::Execute(crate::actions::AppAction::ShowErrorRecord(record))) if record.details == "full diagnostic details")
        );
    }
    #[test]
    fn history_is_bounded_and_duplicates_move_to_front() {
        let mut history = History::default();
        for index in 0..250 {
            history.push(Severity::Error, "test", &index.to_string(), "details");
        }
        assert_eq!(history.records.len(), CAPACITY);
        assert_eq!(history.records.back().unwrap().summary, "50");
        history.push(Severity::Error, "test", "50", "details");
        assert_eq!(history.records.front().unwrap().repeats, 2);
        assert_eq!(history.records.len(), CAPACITY);
    }
    #[test]
    fn details_are_bounded_at_unicode_boundaries() {
        let text = bounded(&"🦀".repeat(10000), DETAIL_LIMIT);
        assert!(text.len() <= DETAIL_LIMIT);
        assert!(text.ends_with("[truncated]"));
    }
}

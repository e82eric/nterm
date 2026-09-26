use crate::copy_mode::TextRow;
use crate::ffi::{
    SnapshotCell, SnapshotCellRange, SnapshotCellText, SnapshotDecorationStyle, SnapshotView,
};
use regex::Regex;
use std::sync::OnceLock;

const ALPHABET: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";

pub(crate) struct Hint {
    pub(crate) range: SnapshotCellRange,
    pub(crate) label: String,
}

pub(crate) struct QuickSelect {
    pub(crate) generation: u64,
    pub(crate) from_copy: bool,
    pub(crate) hints: Vec<Hint>,
    pub(crate) prefix: String,
    pub(crate) view: SnapshotView,
}

impl QuickSelect {
    pub(crate) fn new(
        generation: u64,
        from_copy: bool,
        view: SnapshotView,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<Self> {
        static MATCHER: OnceLock<Regex> = OnceLock::new();
        let matcher = MATCHER.get_or_init(|| Regex::new(r"\S+").expect("valid big-word regex"));
        let bottom = view
            .viewport_top
            .saturating_add(u64::from(view.viewport_rows))
            .min(view.total_rows);
        let mut row = view.viewport_top;
        // Include the beginning of the first logical line. Matches whose
        // start is above the viewport are excluded rather than mislabeled.
        while row > 0 && read(row - 1)?.wraps_to_next {
            row -= 1;
        }
        let mut text = String::new();
        let mut points = Vec::new();
        let mut ranges = Vec::new();
        while row < view.total_rows {
            let line = read(row)?;
            if line.text.len() != line.columns.len() {
                return None;
            }
            text.push_str(&line.text);
            points.extend(line.columns.iter().map(|&x| SnapshotCell { x, y: row }));
            row += 1;
            if !line.wraps_to_next || row == view.total_rows {
                for found in matcher.find_iter(&text) {
                    let start = points[found.start()];
                    let end = points[found.end() - 1];
                    if start.y >= view.viewport_top && start.y < bottom {
                        ranges.push(SnapshotCellRange { start, end });
                    }
                }
                text.clear();
                points.clear();
                if row >= bottom {
                    break;
                }
            }
        }
        // Prefer the most recent output. Decorations expect newest first.
        ranges.reverse();
        let labels = labels(ranges.len());
        Some(Self {
            generation,
            from_copy,
            view,
            prefix: String::new(),
            hints: ranges
                .into_iter()
                .zip(labels)
                .map(|(range, label)| Hint { range, label })
                .collect(),
        })
    }

    pub(crate) fn type_text(&mut self, text: &str) -> Option<SnapshotCell> {
        for ch in text.chars() {
            if !ch.is_ascii_alphabetic() {
                continue;
            }
            let mut next = self.prefix.clone();
            next.push(ch.to_ascii_lowercase());
            if !self.hints.iter().any(|hint| hint.label.starts_with(&next)) {
                continue;
            }
            self.prefix = next;
            if let Some(hint) = self.hints.iter().find(|hint| hint.label == self.prefix) {
                return Some(hint.range.start);
            }
        }
        None
    }

    pub(crate) fn visible_hints(&self) -> impl Iterator<Item = &Hint> {
        self.hints
            .iter()
            .filter(|hint| hint.label.starts_with(&self.prefix))
    }

    pub(crate) fn cell_text(&self) -> Vec<SnapshotCellText> {
        let mut text = Vec::new();
        let visible: Vec<_> = self.visible_hints().collect();
        for (index, hint) in visible.iter().enumerate() {
            let start = hint.range.start;
            // Show the untyped suffix without covering another label.
            let end_x = index
                .checked_sub(1)
                .and_then(|previous| visible.get(previous))
                .filter(|other| other.range.start.y == start.y)
                .map_or(self.view.columns, |other| other.range.start.x);
            let label = &hint.label[self.prefix.len()..];
            for (offset, ch) in label.bytes().enumerate() {
                let x = start.x + offset as u32;
                if x >= end_x || x >= self.view.columns {
                    break;
                }
                text.push(SnapshotCellText {
                    cell: SnapshotCell { x, y: start.y },
                    codepoint: u32::from(ch.to_ascii_uppercase()),
                    style: SnapshotDecorationStyle {
                        foreground_rgb: 0xd79921,
                        background_rgb: 0,
                        flags: 1, // Override foreground only.
                    },
                    bold: true,
                });
            }
        }
        text
    }
}

fn labels(count: usize) -> Vec<String> {
    let mut width = 1;
    let mut capacity = ALPHABET.len();
    while capacity < count {
        width += 1;
        capacity = capacity.saturating_mul(ALPHABET.len());
    }
    (0..count)
        .map(|mut index| {
            let mut label = vec![ALPHABET[0]; width];
            for ch in label.iter_mut().rev() {
                *ch = ALPHABET[index % ALPHABET.len()];
                index /= ALPHABET.len();
            }
            String::from_utf8(label).expect("ASCII hint alphabet")
        })
        .collect()
}

pub(crate) fn consume_shortcut(events: &mut Vec<egui::Event>) -> (bool, bool) {
    let mut incoming = std::mem::take(events).into_iter().peekable();
    let mut open = false;
    let mut consumed = false;
    while let Some(event) = incoming.next() {
        if let egui::Event::Key {
            key: egui::Key::Space,
            pressed,
            repeat,
            modifiers,
            ..
        } = &event
            && modifiers.alt
            && !modifiers.ctrl
            && !modifiers.shift
            && !modifiers.command
            && !modifiers.mac_cmd
        {
            consumed = true;
            open |= *pressed && !*repeat;
            if *pressed && matches!(incoming.peek(), Some(egui::Event::Text(_))) {
                incoming.next();
            }
        } else {
            events.push(event);
        }
    }
    (open, consumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_space_is_consumed_once_with_its_paired_text() {
        let key = |pressed, repeat, modifiers| egui::Event::Key {
            key: egui::Key::Space,
            physical_key: Some(egui::Key::Space),
            pressed,
            repeat,
            modifiers,
        };
        let modifiers = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let mut events = vec![
            key(true, false, modifiers),
            egui::Event::Text(" ".into()),
            egui::Event::Text("a".into()),
        ];
        assert_eq!(consume_shortcut(&mut events), (true, true));
        assert!(matches!(events.as_slice(), [egui::Event::Text(text)] if text == "a"));
        for (pressed, repeat) in [(true, true), (false, false)] {
            let mut events = vec![key(pressed, repeat, modifiers)];
            assert_eq!(consume_shortcut(&mut events), (false, true));
            assert!(events.is_empty());
        }
        for modifiers in [
            egui::Modifiers::NONE,
            egui::Modifiers {
                ctrl: true,
                ..modifiers
            },
            egui::Modifiers {
                shift: true,
                ..modifiers
            },
        ] {
            let mut events = vec![key(true, false, modifiers)];
            assert_eq!(consume_shortcut(&mut events), (false, false));
            assert_eq!(events.len(), 1);
        }
    }

    #[test]
    fn word_cells_follow_exported_wide_and_combining_character_mapping() {
        let rows = [TextRow {
            text: "e\u{301} 界x".into(),
            columns: vec![0, 0, 0, 1, 2, 2, 2, 4],
            wraps_to_next: false,
        }];
        let view = SnapshotView {
            viewport_rows: 1,
            total_rows: 1,
            columns: 5,
            ..Default::default()
        };
        let state = QuickSelect::new(1, false, view, |y| rows.get(y as usize).cloned()).unwrap();
        assert_eq!(
            state.hints[0].range,
            SnapshotCellRange {
                start: SnapshotCell { x: 2, y: 0 },
                end: SnapshotCell { x: 4, y: 0 },
            }
        );
        assert_eq!(state.hints[1].range.start, state.hints[1].range.end);
        assert!(!state.from_copy);
        assert_eq!(state.generation, 1);
    }

    fn row(text: &str, wraps_to_next: bool) -> TextRow {
        let mut columns = Vec::new();
        for (x, ch) in text.chars().enumerate() {
            columns.extend(std::iter::repeat_n(x as u32, ch.len_utf8()));
        }
        TextRow {
            text: text.into(),
            columns,
            wraps_to_next,
        }
    }

    #[test]
    fn labels_are_unique_and_prefix_free_at_each_size() {
        for count in [0, 1, 26, 27, 676, 677, 2000] {
            let result = labels(count);
            let unique: std::collections::HashSet<_> = result.iter().collect();
            assert_eq!(unique.len(), count);
            if let Some(first) = result.first() {
                assert!(result.iter().all(|label| label.len() == first.len()));
            }
        }
    }

    #[test]
    fn big_words_join_soft_wraps_and_map_unicode_to_cells() {
        let rows = [
            row("older", false),
            row("ab", true),
            row("cd 界x", false),
            row("tail", false),
        ];
        let view = SnapshotView {
            viewport_top: 1,
            viewport_rows: 2,
            total_rows: 4,
            columns: 10,
            ..Default::default()
        };
        let mut state = QuickSelect::new(7, true, view, |y| rows.get(y as usize).cloned()).unwrap();
        assert_eq!(state.hints.len(), 2);
        assert_eq!(state.hints[0].range.start, SnapshotCell { x: 3, y: 2 });
        assert_eq!(state.hints[1].range.end, SnapshotCell { x: 1, y: 2 });
        assert_eq!(state.type_text("A"), Some(SnapshotCell { x: 3, y: 2 }));
        let view = SnapshotView {
            viewport_top: 2,
            viewport_rows: 1,
            ..view
        };
        let state = QuickSelect::new(7, true, view, |y| rows.get(y as usize).cloned()).unwrap();
        assert_eq!(state.hints.len(), 1); // exclude the word starting above the viewport
    }

    #[test]
    fn prefix_filters_and_invalid_input_does_not_destroy_it() {
        let rows = [row(&"x ".repeat(30), false)];
        let view = SnapshotView {
            viewport_rows: 1,
            total_rows: 1,
            columns: 60,
            ..Default::default()
        };
        let mut state =
            QuickSelect::new(1, false, view, |y| rows.get(y as usize).cloned()).unwrap();
        assert_eq!(state.type_text("a"), None);
        assert_eq!(state.visible_hints().count(), 26);
        assert_eq!(state.type_text("!"), None);
        assert_eq!(state.prefix, "a");
        assert!(state.type_text("a").is_some());
        state.prefix.pop();
        assert_eq!(state.prefix, "a");
        let cells = state.cell_text();
        let coords: std::collections::HashSet<_> = cells
            .iter()
            .map(|text| (text.cell.x, text.cell.y))
            .collect();
        assert_eq!(coords.len(), cells.len());
    }
}

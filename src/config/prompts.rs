use crate::ghostty_ffi::RowPrompt;
use nfm_egui::copy_mode::TextRow;

/// One physical row of a logical line: its byte-to-column map and what OSC 133
/// marks say about it.
pub(crate) struct LineRow<'a> {
    pub(crate) columns: &'a [u32],
    pub(crate) prompt: RowPrompt,
}

pub(crate) fn defaults() -> Vec<String> {
    [
        r"^PS [^>]*>\s*",
        r"^\d+:\d+>\s*",
        r"^›\s*",
        r"^────────────────────*",
    ]
    .map(str::to_owned)
    .to_vec()
}

#[derive(Clone)]
pub(crate) struct Patterns(Vec<regex::Regex>);
impl Default for Patterns {
    fn default() -> Self {
        Self::compile(&defaults()).expect("valid default prompt patterns")
    }
}
impl Patterns {
    pub(crate) fn compile(patterns: &[String]) -> Result<Self, String> {
        patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                regex::Regex::new(pattern)
                    .map_err(|error| format!("prompt_patterns[{}]: {error}", index + 1))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }
    pub(crate) fn prefix_end(&self, line: &str) -> Option<usize> {
        self.0
            .iter()
            .filter_map(|pattern| {
                pattern
                    .find(line)
                    .filter(|m| m.start() == 0)
                    .map(|m| m.end())
            })
            .max()
    }
    /// The byte offset in a logical line's text where the command typed at its
    /// prompt starts. OSC 133 input marks are exact, so a marked line uses
    /// them even when a pattern also matches; other lines use the patterns.
    pub(crate) fn command_start(&self, text: &str, rows: &[LineRow]) -> Option<usize> {
        if !rows.iter().any(|row| row.prompt.marked) {
            return self.prefix_end(text);
        }
        let mut offset = 0;
        for row in rows {
            if let Some(column) = row.prompt.input_column {
                let index = row.columns.iter().position(|&x| x >= column)?;
                return Some(offset + index);
            }
            offset += row.columns.len();
        }
        None
    }
    /// The first row of each logical line that starts a command: a shell
    /// marked it as a prompt with OSC 133, or one of the patterns matches it.
    pub(crate) fn boundaries(
        &self,
        rows: u64,
        mut read: impl FnMut(u64) -> Option<(TextRow, RowPrompt)>,
    ) -> Option<Vec<u64>> {
        let mut boundaries = Vec::new();
        let mut line = String::new();
        let mut primary = false;
        let mut start = 0;
        for y in 0..rows {
            let (row, prompt) = read(y)?;
            line.push_str(&row.text);
            primary |= prompt.primary;
            if !row.wraps_to_next || y + 1 == rows {
                if primary || self.prefix_end(&line).is_some() {
                    boundaries.push(start);
                }
                line.clear();
                primary = false;
                start = y + 1;
            }
        }
        Some(boundaries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_prompts_match_logical_lines_and_empty_list_disables_detection() {
        let patterns = Patterns::compile(&[r"^custom>\s*".into()]).unwrap();
        let rows = [
            TextRow {
                text: "custom".into(),
                columns: vec![],
                wraps_to_next: true,
            },
            TextRow {
                text: "> run".into(),
                columns: vec![],
                wraps_to_next: false,
            },
        ];
        assert_eq!(
            patterns.boundaries(2, |y| Some((rows[y as usize].clone(), RowPrompt::default()))),
            Some(vec![0])
        );
        assert_eq!(patterns.prefix_end("PS C:>"), None);
        assert_eq!(Patterns::compile(&[]).unwrap().prefix_end("custom>"), None);
        assert!(
            Patterns::compile(&["[".into()])
                .err()
                .unwrap()
                .contains("prompt_patterns[1]")
        );
    }

    fn row(text: &str, wraps_to_next: bool) -> TextRow {
        TextRow {
            text: text.into(),
            columns: (0..text.len() as u32).collect(),
            wraps_to_next,
        }
    }
    fn marked(input_column: Option<u32>) -> RowPrompt {
        RowPrompt {
            primary: true,
            marked: true,
            input_column,
        }
    }

    #[test]
    fn boundaries_combine_osc_marks_with_patterns() {
        let patterns = Patterns::default();
        let rows = [
            // A custom prompt only OSC 133 can find.
            (row("~/src λ ls", false), marked(Some(8))),
            (row("output", false), RowPrompt::default()),
            // A debugger prompt without marks, found by its pattern.
            (row("0:000> k", false), RowPrompt::default()),
            // A prompt both find is one boundary.
            (row("PS C:\\> dir", false), marked(Some(8))),
        ];
        assert_eq!(
            patterns.boundaries(rows.len() as u64, |y| Some(rows[y as usize].clone())),
            Some(vec![0, 2, 3])
        );
    }

    #[test]
    fn command_start_prefers_osc_input_marks() {
        let patterns = Patterns::default();
        let start = |text: &str, prompt| {
            let columns: Vec<u32> = (0..text.len() as u32).collect();
            patterns.command_start(text, &[LineRow { columns: &columns, prompt }])
        };
        // The test rows map each byte to its own column, so "ls" is at 9.
        assert_eq!(start("~/src λ ls", marked(Some(9))).map(|i| &"~/src λ ls"[i..]), Some("ls"));
        // The pattern would stop after "> "; the mark says the command starts later.
        assert_eq!(start("PS C:\\> x> y", marked(Some(11))), Some(11));
        // A marked prompt with nothing typed has no command.
        assert_eq!(start("PS C:\\> ", marked(None)), None);
        assert_eq!(start("0:000> k", RowPrompt::default()), Some(7));
        assert_eq!(start("plain output", RowPrompt::default()), None);
    }
}

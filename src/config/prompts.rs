use nfm_egui::copy_mode::TextRow;

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
    pub(crate) fn boundaries(
        &self,
        rows: u64,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<Vec<u64>> {
        let mut boundaries = Vec::new();
        let mut line = String::new();
        let mut start = 0;
        for y in 0..rows {
            let row = read(y)?;
            line.push_str(&row.text);
            if !row.wraps_to_next || y + 1 == rows {
                if self.prefix_end(&line).is_some() {
                    boundaries.push(start);
                }
                line.clear();
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
            patterns.boundaries(2, |y| Some(rows[y as usize].clone())),
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
}

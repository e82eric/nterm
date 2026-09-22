use crate::ffi::SnapshotRange;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(crate) struct SearchEvents {
    pub(crate) open: Option<String>,
    pub(crate) close: bool,
}

#[derive(Default)]
pub(crate) struct SearchUi {
    pub(crate) active: bool,
    pub(crate) query: String,
    pub(crate) generation: Option<u64>,
    pub(crate) columns: u32,
    pub(crate) rows: Vec<String>,
    pub(crate) matches: Vec<SnapshotRange>,
    pub(crate) selected: Option<usize>,
    pub(crate) focus_requested: bool,
}

impl SearchUi {
    pub(crate) fn rebuild_matches(&mut self) {
        self.matches = find_search_matches(&self.rows, &self.query, self.columns);
        self.selected = (!self.matches.is_empty()).then_some(0);
    }

    pub(crate) fn navigate(&mut self, previous: bool) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        self.selected = Some(match self.selected {
            Some(selected) if previous => (selected + count - 1) % count,
            Some(selected) => (selected + 1) % count,
            None if previous => count - 1,
            None => 0,
        });
    }

    pub(crate) fn selected_range(&self) -> Option<&SnapshotRange> {
        self.selected.and_then(|index| self.matches.get(index))
    }

    pub(crate) fn match_label(&self) -> String {
        match self.selected {
            Some(selected) => format!("{}/{}", selected + 1, self.matches.len()),
            None => format!("0/{}", self.matches.len()),
        }
    }
}

fn find_search_matches(rows: &[String], query: &str, columns: u32) -> Vec<SnapshotRange> {
    let mut result = Vec::new();
    if query.is_empty() || columns == 0 {
        return result;
    }

    // Ghostty search navigation is newest-to-oldest. Screen row zero is the
    // oldest scrollback row, so collect rows and matches in reverse order.
    for y in (0..rows.len()).rev() {
        let row = &rows[y];
        let row_matches = ascii_case_insensitive_matches(row, query);
        for byte in row_matches.into_iter().rev() {
            let matched = &row[byte..byte + query.len()];
            let start_x = UnicodeWidthStr::width(&row[..byte]);
            let width = UnicodeWidthStr::width(matched);
            if width == 0 || start_x >= columns as usize {
                continue;
            }
            result.push(SnapshotRange {
                start_x: start_x as u32,
                start_y: y as u32,
                end_x: (start_x + width - 1).min(columns as usize - 1) as u32,
                end_y: y as u32,
            });
        }
    }
    result
}

fn ascii_case_insensitive_matches(haystack: &str, needle: &str) -> Vec<usize> {
    let mut result = Vec::new();
    if needle.is_empty() || needle.len() > haystack.len() {
        return result;
    }

    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut offset = 0;
    while offset + needle_bytes.len() <= haystack_bytes.len() {
        let end = offset + needle_bytes.len();
        if haystack.is_char_boundary(offset)
            && haystack.is_char_boundary(end)
            && haystack_bytes[offset..end].eq_ignore_ascii_case(needle_bytes)
        {
            result.push(offset);
            offset = end;
        } else {
            offset += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{SnapshotRange, find_search_matches};

    #[test]
    fn matches_are_newest_to_oldest() {
        let rows = vec!["hit old".to_owned(), "hit hit new".to_owned()];
        assert_eq!(
            find_search_matches(&rows, "hit", 20),
            vec![
                SnapshotRange {
                    start_x: 4,
                    start_y: 1,
                    end_x: 6,
                    end_y: 1,
                },
                SnapshotRange {
                    start_x: 0,
                    start_y: 1,
                    end_x: 2,
                    end_y: 1,
                },
                SnapshotRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 2,
                    end_y: 0,
                },
            ]
        );
    }

    #[test]
    fn refined_query_selects_newest_remaining_match() {
        let rows = vec!["ab old".to_owned(), "a new".to_owned()];
        let matches = find_search_matches(&rows, "ab", 20);
        assert_eq!(matches[0].start_y, 0);
    }

    #[test]
    fn matching_is_ascii_case_insensitive_like_ghostty_search() {
        let rows = vec!["LastWriteTime".to_owned(), ".winapp .wt .WTD".to_owned()];
        let matches = find_search_matches(&rows, "W", 20);
        assert_eq!(matches.len(), 4);
        assert_eq!(matches[0].start_y, 1);
        assert_eq!(matches[0].start_x, 13);
        assert_eq!(matches[3].start_y, 0);
    }
}

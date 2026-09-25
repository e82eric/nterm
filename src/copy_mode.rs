use crate::ffi::{SnapshotCell, SnapshotCellRange, SnapshotView};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Motion {
    Left,
    Down,
    Up,
    Right,
    RowStart,
    ScrollbackTop,
    ScrollbackBottom,
    ViewportTop,
    ViewportMiddle,
    ViewportBottom,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    FirstNonblank,
    LastNonblank,
    ForwardWord,
    BackwardWord,
    ForwardWordEnd,
    ForwardBigWord,
    BackwardBigWord,
    ForwardBigWordEnd,
    PreviousWordEnd,
    PreviousBigWordEnd,
    NextLineNonblank,
    PreviousLineNonblank,
    LineNonblank,
    CharacterJump,
    MatchingBracket,
}

impl Motion {
    pub(crate) fn needs_view(self) -> bool {
        matches!(
            self,
            Self::ViewportTop
                | Self::ViewportMiddle
                | Self::ViewportBottom
                | Self::PageUp
                | Self::PageDown
                | Self::HalfPageUp
                | Self::HalfPageDown
        )
    }

    pub(crate) fn needs_text(self) -> bool {
        matches!(
            self,
            Self::FirstNonblank
                | Self::LastNonblank
                | Self::ForwardWord
                | Self::BackwardWord
                | Self::ForwardWordEnd
                | Self::ForwardBigWord
                | Self::BackwardBigWord
                | Self::ForwardBigWordEnd
                | Self::PreviousWordEnd
                | Self::PreviousBigWordEnd
                | Self::NextLineNonblank
                | Self::PreviousLineNonblank
                | Self::LineNonblank
                | Self::CharacterJump
                | Self::MatchingBracket
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TextRow {
    pub(crate) text: String,
    pub(crate) columns: Vec<u32>,
    pub(crate) wraps_to_next: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WordClass {
    Whitespace,
    Keyword,
    Punctuation,
}

#[derive(Clone, Copy, Debug)]
struct Glyph {
    x: u32,
    class: WordClass,
    nonblank: bool,
}

impl TextRow {
    fn grapheme_cells(&self) -> Vec<(u32, &str)> {
        self.text
            .grapheme_indices(true)
            .filter_map(|(byte, grapheme)| Some((*self.columns.get(byte)?, grapheme)))
            .collect()
    }

    fn glyphs(&self, big_word: bool) -> Vec<Glyph> {
        self.text
            .grapheme_indices(true)
            .filter_map(|(byte, grapheme)| {
                let x = *self.columns.get(byte)?;
                let class = if grapheme.chars().all(char::is_whitespace) {
                    WordClass::Whitespace
                } else if big_word
                    || grapheme
                        .chars()
                        .next()
                        .is_some_and(|ch| ch == '_' || ch.is_alphanumeric())
                {
                    WordClass::Keyword
                } else {
                    WordClass::Punctuation
                };
                Some(Glyph {
                    x,
                    class,
                    nonblank: grapheme != " ",
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Jump {
    pub(crate) target: String,
    pub(crate) forward: bool,
    pub(crate) till: bool,
}

#[derive(Clone, Default)]
pub(crate) struct CopyMode {
    pub(crate) generation: Option<u64>,
    pub(crate) cursor: SnapshotCell,
    preferred_x: u32,
    columns: u32,
    rows: u64,
    pub(crate) pending_g: bool,
    pub(crate) pending_count: Option<u32>,
    pub(crate) pending_jump: Option<(bool, bool)>,
    pub(crate) last_jump: Option<Jump>,
}

impl CopyMode {
    pub(crate) fn start(&mut self, generation: u64, state: SnapshotView) {
        self.generation = Some(generation);
        self.cursor = state.cursor;
        self.preferred_x = state.cursor.x;
        self.columns = state.columns;
        self.rows = state.total_rows;
        self.pending_g = false;
        self.pending_count = None;
        self.pending_jump = None;
        self.last_jump = None;
    }

    pub(crate) fn move_to(
        &self,
        motion: Motion,
        view: Option<SnapshotView>,
    ) -> Option<(SnapshotCell, bool)> {
        if self.columns == 0 || self.rows == 0 {
            return None;
        }
        let mut cell = self.cursor;
        let snap_right = matches!(motion, Motion::Right);
        let last_row = self.rows - 1;
        match motion {
            Motion::Left => cell.x = cell.x.saturating_sub(1),
            Motion::Right => cell.x = cell.x.saturating_add(1).min(self.columns - 1),
            Motion::RowStart => cell.x = 0,
            Motion::ScrollbackTop => cell.y = 0,
            Motion::ScrollbackBottom => cell.y = last_row,
            Motion::Up => {
                cell.y = cell.y.saturating_sub(1);
                cell.x = self.preferred_x.min(self.columns - 1);
            }
            Motion::Down => {
                cell.y = cell.y.saturating_add(1).min(last_row);
                cell.x = self.preferred_x.min(self.columns - 1);
            }
            Motion::ViewportTop | Motion::ViewportMiddle | Motion::ViewportBottom => {
                let view = view?;
                if view.viewport_rows == 0
                    || view.total_rows != self.rows
                    || view.columns != self.columns
                {
                    return None;
                }
                let top = view.viewport_top.min(last_row);
                let bottom = top
                    .saturating_add(u64::from(view.viewport_rows - 1))
                    .min(last_row);
                cell.y = match motion {
                    Motion::ViewportTop => top,
                    Motion::ViewportMiddle => top + (bottom - top + 1) / 2,
                    Motion::ViewportBottom => bottom,
                    _ => unreachable!(),
                };
                cell.x = self.preferred_x.min(self.columns - 1);
            }
            Motion::PageUp | Motion::PageDown | Motion::HalfPageUp | Motion::HalfPageDown => {
                let view = view?;
                if view.viewport_rows == 0
                    || view.total_rows != self.rows
                    || view.columns != self.columns
                {
                    return None;
                }
                let rows = u64::from(view.viewport_rows);
                let step = if matches!(motion, Motion::HalfPageUp | Motion::HalfPageDown) {
                    (rows / 2).max(1)
                } else {
                    rows
                };
                cell.y = if matches!(motion, Motion::PageUp | Motion::HalfPageUp) {
                    cell.y.saturating_sub(step)
                } else {
                    cell.y.saturating_add(step).min(last_row)
                };
                cell.x = self.preferred_x.min(self.columns - 1);
            }
            Motion::FirstNonblank
            | Motion::LastNonblank
            | Motion::ForwardWord
            | Motion::BackwardWord
            | Motion::ForwardWordEnd => return None,
            Motion::ForwardBigWord
            | Motion::BackwardBigWord
            | Motion::ForwardBigWordEnd
            | Motion::PreviousWordEnd
            | Motion::PreviousBigWordEnd
            | Motion::NextLineNonblank
            | Motion::PreviousLineNonblank
            | Motion::LineNonblank
            | Motion::CharacterJump
            | Motion::MatchingBracket => return None,
        }
        Some((cell, snap_right))
    }

    pub(crate) fn moved(&mut self, motion: Motion, cell: SnapshotCell) {
        self.cursor = cell;
        if matches!(motion, Motion::Left | Motion::Right | Motion::RowStart) || motion.needs_text()
        {
            self.preferred_x = cell.x;
        }
    }

    pub(crate) fn add_count_digit(&mut self, digit: u32) -> bool {
        if digit == 0 && self.pending_count.is_none() {
            return false;
        }
        self.pending_count = Some(
            self.pending_count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(digit),
        );
        true
    }

    pub(crate) fn take_count(&mut self) -> u32 {
        self.pending_count.take().unwrap_or(1).max(1)
    }

    pub(crate) fn numbered_row_target(&self, count: u32) -> Option<SnapshotCell> {
        let last = self.rows.checked_sub(1)?;
        Some(SnapshotCell {
            x: self.preferred_x.min(self.columns.checked_sub(1)?),
            y: u64::from(count - 1).min(last),
        })
    }

    pub(crate) fn percent_row_target(&self, percent: u32) -> Option<SnapshotCell> {
        if percent == 0 || percent > 100 {
            return None;
        }
        let last = self.rows.checked_sub(1)?;
        Some(SnapshotCell {
            x: self.preferred_x.min(self.columns.checked_sub(1)?),
            y: u64::from(percent - 1).saturating_mul(last) / 99,
        })
    }

    pub(crate) fn counted_simple_target(
        &self,
        motion: Motion,
        count: u32,
        view: Option<SnapshotView>,
    ) -> Option<(SnapshotCell, bool)> {
        let (mut cell, snap_right) = self.move_to(motion, view)?;
        let extra = u64::from(count - 1);
        let last = self.rows - 1;
        match motion {
            Motion::Up => cell.y = self.cursor.y.saturating_sub(u64::from(count)),
            Motion::Down => cell.y = self.cursor.y.saturating_add(u64::from(count)).min(last),
            Motion::PageUp | Motion::PageDown | Motion::HalfPageUp | Motion::HalfPageDown => {
                let rows = u64::from(view?.viewport_rows);
                let step = if matches!(motion, Motion::HalfPageUp | Motion::HalfPageDown) {
                    (rows / 2).max(1)
                } else {
                    rows
                };
                let delta = step.saturating_mul(u64::from(count));
                cell.y = if matches!(motion, Motion::PageUp | Motion::HalfPageUp) {
                    self.cursor.y.saturating_sub(delta)
                } else {
                    self.cursor.y.saturating_add(delta).min(last)
                };
            }
            Motion::ViewportTop => {
                let view = view?;
                let bottom = view
                    .viewport_top
                    .saturating_add(u64::from(view.viewport_rows - 1))
                    .min(last);
                cell.y = cell.y.saturating_add(extra).min(bottom);
            }
            Motion::ViewportBottom => {
                let view = view?;
                cell.y = cell.y.saturating_sub(extra).max(view.viewport_top);
            }
            // Horizontal counts must resolve each intermediate cell so wide
            // graphemes count as one motion rather than two terminal columns.
            _ => return None,
        }
        Some((cell, snap_right))
    }

    pub(crate) fn nonblank_line_target(
        &self,
        motion: Motion,
        count: u32,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCell> {
        let last = self.rows.checked_sub(1)?;
        let y = match motion {
            Motion::NextLineNonblank => self.cursor.y.saturating_add(u64::from(count)).min(last),
            Motion::PreviousLineNonblank => self.cursor.y.saturating_sub(u64::from(count)),
            Motion::LineNonblank => self.cursor.y.saturating_add(u64::from(count - 1)).min(last),
            _ => return None,
        };
        let row = read(y)?;
        let x = row
            .glyphs(false)
            .iter()
            .find(|glyph| glyph.nonblank)
            .map_or(0, |glyph| glyph.x);
        Some(SnapshotCell { x, y })
    }

    pub(crate) fn word_under_cursor(
        &self,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<String> {
        let row = read(self.cursor.y)?;
        let glyphs = row.glyphs(false);
        let cells = row.grapheme_cells();
        let index = glyphs
            .iter()
            .position(|glyph| glyph.class == WordClass::Keyword && glyph.x >= self.cursor.x)
            .or_else(|| {
                glyphs.iter().rposition(|glyph| {
                    glyph.x <= self.cursor.x && glyph.class == WordClass::Keyword
                })
            })?;
        let mut start = index;
        while start > 0 && glyphs[start - 1].class == WordClass::Keyword {
            start -= 1;
        }
        let mut end = index + 1;
        while end < glyphs.len() && glyphs[end].class == WordClass::Keyword {
            end += 1;
        }
        let mut word: String = cells[start..end].iter().map(|(_, text)| *text).collect();
        if start == 0 {
            let mut y = self.cursor.y;
            while let Some(previous) = y.checked_sub(1) {
                let prior = read(previous)?;
                if !prior.wraps_to_next {
                    break;
                }
                let prior_glyphs = prior.glyphs(false);
                let prior_cells = prior.grapheme_cells();
                let prior_start = prior_glyphs
                    .iter()
                    .rposition(|glyph| glyph.class != WordClass::Keyword)
                    .map_or(0, |index| index + 1);
                if prior_start == prior_glyphs.len() {
                    break;
                }
                let prefix: String = prior_cells[prior_start..]
                    .iter()
                    .map(|(_, text)| *text)
                    .collect();
                word.insert_str(0, &prefix);
                if prior_start != 0 {
                    break;
                }
                y = previous;
            }
        }
        if end == glyphs.len() && row.wraps_to_next {
            let mut y = self.cursor.y + 1;
            while y < self.rows {
                let next = read(y)?;
                let next_glyphs = next.glyphs(false);
                let next_cells = next.grapheme_cells();
                let next_end = next_glyphs
                    .iter()
                    .position(|glyph| glyph.class != WordClass::Keyword)
                    .unwrap_or(next_glyphs.len());
                if next_end == 0 {
                    break;
                }
                word.extend(next_cells[..next_end].iter().map(|(_, text)| *text));
                if next_end != next_glyphs.len() || !next.wraps_to_next {
                    break;
                }
                y += 1;
            }
        }
        Some(word)
    }

    pub(crate) fn text_target(
        &self,
        motion: Motion,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCell> {
        let current = self.cursor;
        match motion {
            Motion::FirstNonblank | Motion::LastNonblank => {
                let row = read(current.y)?;
                let glyphs = row.glyphs(false);
                let x = if motion == Motion::FirstNonblank {
                    glyphs.iter().find(|g| g.nonblank).map_or(0, |g| g.x)
                } else {
                    glyphs.iter().rev().find(|g| g.nonblank).map_or(0, |g| g.x)
                };
                Some(SnapshotCell { x, y: current.y })
            }
            Motion::ForwardWord => self.forward_word(&mut read, false),
            Motion::BackwardWord => self.backward_word(&mut read, false),
            Motion::ForwardWordEnd => self.forward_word_end(&mut read, false),
            Motion::ForwardBigWord => self.forward_word(&mut read, true),
            Motion::BackwardBigWord => self.backward_word(&mut read, true),
            Motion::ForwardBigWordEnd => self.forward_word_end(&mut read, true),
            Motion::PreviousWordEnd => self.previous_word_end(&mut read, false),
            Motion::PreviousBigWordEnd => self.previous_word_end(&mut read, true),
            Motion::NextLineNonblank | Motion::PreviousLineNonblank | Motion::LineNonblank => {
                self.nonblank_line_target(motion, 1, read)
            }
            Motion::MatchingBracket => self.matching_bracket(&mut read),
            _ => None,
        }
    }

    pub(crate) fn jump_target(
        &self,
        jump: &Jump,
        repeat: bool,
        read: impl FnOnce(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCell> {
        let row = read(self.cursor.y)?;
        let cells = row.grapheme_cells();
        let cursor = self.cursor.x;
        let cutoff = if repeat && jump.till {
            if jump.forward {
                cursor.saturating_add(1)
            } else {
                cursor.saturating_sub(1)
            }
        } else {
            cursor
        };
        let index = if jump.forward {
            cells
                .iter()
                .position(|(x, text)| *x > cutoff && *text == jump.target)
        } else {
            cells
                .iter()
                .rposition(|(x, text)| *x < cutoff && *text == jump.target)
        }?;
        let x = if jump.till {
            if jump.forward {
                cells.get(index.checked_sub(1)?)?.0
            } else {
                cells.get(index + 1)?.0
            }
        } else {
            cells[index].0
        };
        Some(SnapshotCell {
            x,
            y: self.cursor.y,
        })
    }

    fn matching_bracket(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCell> {
        let row = read(self.cursor.y)?;
        let (source_x, source, target, forward) = row
            .grapheme_cells()
            .into_iter()
            .filter(|(x, _)| *x >= self.cursor.x)
            .find_map(|(x, text)| match text {
                "(" => Some((x, "(", ")", true)),
                "[" => Some((x, "[", "]", true)),
                "{" => Some((x, "{", "}", true)),
                ")" => Some((x, ")", "(", false)),
                "]" => Some((x, "]", "[", false)),
                "}" => Some((x, "}", "{", false)),
                _ => None,
            })?;
        let mut depth = 0usize;
        let mut y = self.cursor.y;
        loop {
            let row = read(y)?;
            let mut cells = row.grapheme_cells();
            if !forward {
                cells.reverse();
            }
            for (x, text) in cells {
                if y == self.cursor.y && (if forward { x < source_x } else { x > source_x }) {
                    continue;
                }
                if text == source {
                    depth += 1;
                } else if text == target {
                    depth -= 1;
                    if depth == 0 {
                        return Some(SnapshotCell { x, y });
                    }
                }
            }
            if forward {
                y = y.checked_add(1)?;
                if y >= self.rows {
                    return None;
                }
            } else {
                y = y.checked_sub(1)?;
            }
        }
    }

    fn forward_word(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
        big: bool,
    ) -> Option<SnapshotCell> {
        let mut previous = WordClass::Whitespace;
        for y in self.cursor.y..self.rows {
            let row = read(y)?;
            let glyphs = row.glyphs(big);
            for glyph in glyphs {
                if y == self.cursor.y && glyph.x <= self.cursor.x {
                    previous = glyph.class;
                    continue;
                }
                if glyph.class != WordClass::Whitespace && glyph.class != previous {
                    return Some(SnapshotCell { x: glyph.x, y });
                }
                previous = glyph.class;
            }
            if !row.wraps_to_next {
                previous = WordClass::Whitespace;
            }
        }
        None
    }

    fn backward_word(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
        big: bool,
    ) -> Option<SnapshotCell> {
        for y in (0..=self.cursor.y).rev() {
            let row = read(y)?;
            let previous_class = if y > 0 {
                let prior = read(y - 1)?;
                if prior.wraps_to_next {
                    prior
                        .glyphs(big)
                        .last()
                        .map_or(WordClass::Whitespace, |glyph| glyph.class)
                } else {
                    WordClass::Whitespace
                }
            } else {
                WordClass::Whitespace
            };
            let mut previous = previous_class;
            let mut candidate = None;
            for glyph in row.glyphs(big) {
                if glyph.class != WordClass::Whitespace
                    && glyph.class != previous
                    && (y < self.cursor.y || glyph.x < self.cursor.x)
                {
                    candidate = Some(SnapshotCell { x: glyph.x, y });
                }
                previous = glyph.class;
            }
            if candidate.is_some() {
                return candidate;
            }
        }
        None
    }

    fn forward_word_end(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
        big: bool,
    ) -> Option<SnapshotCell> {
        let mut active_class = WordClass::Whitespace;
        let mut last: Option<SnapshotCell> = None;
        for y in self.cursor.y..self.rows {
            let row = read(y)?;
            for glyph in row.glyphs(big) {
                if y == self.cursor.y && glyph.x < self.cursor.x {
                    continue;
                }
                if glyph.class != active_class {
                    if let Some(end) = last
                        && (end.y, end.x) > (self.cursor.y, self.cursor.x)
                    {
                        return Some(end);
                    }
                    last = None;
                }
                active_class = glyph.class;
                if glyph.class != WordClass::Whitespace {
                    last = Some(SnapshotCell { x: glyph.x, y });
                }
            }
            if !row.wraps_to_next {
                if let Some(end) = last
                    && (end.y, end.x) > (self.cursor.y, self.cursor.x)
                {
                    return Some(end);
                }
                active_class = WordClass::Whitespace;
                last = None;
            }
        }
        last.filter(|end| (end.y, end.x) > (self.cursor.y, self.cursor.x))
    }

    fn previous_word_end(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
        big: bool,
    ) -> Option<SnapshotCell> {
        for y in (0..=self.cursor.y).rev() {
            let row = read(y)?;
            let mut next_class = if row.wraps_to_next && y + 1 < self.rows {
                read(y + 1)?
                    .glyphs(big)
                    .first()
                    .map_or(WordClass::Whitespace, |glyph| glyph.class)
            } else {
                WordClass::Whitespace
            };
            for glyph in row.glyphs(big).into_iter().rev() {
                if y == self.cursor.y && glyph.x >= self.cursor.x {
                    next_class = glyph.class;
                    continue;
                }
                if glyph.class != WordClass::Whitespace && glyph.class != next_class {
                    return Some(SnapshotCell { x: glyph.x, y });
                }
                next_class = glyph.class;
            }
        }
        None
    }
}

/// Matches are sorted by their start cell, oldest to newest.
pub(crate) fn next_match_cell(
    matches: &[SnapshotCellRange],
    cursor: SnapshotCell,
    forward: bool,
    count: u32,
) -> Option<SnapshotCell> {
    if matches.is_empty() {
        return None;
    }
    let at = (cursor.y, cursor.x);
    let index = if forward {
        matches.partition_point(|range| (range.start.y, range.start.x) <= at) % matches.len()
    } else {
        let before = matches.partition_point(|range| (range.start.y, range.start.x) < at);
        (before + matches.len() - 1) % matches.len()
    };
    let steps = ((count - 1) as usize) % matches.len();
    let index = if forward {
        (index + steps) % matches.len()
    } else {
        (index + matches.len() - steps) % matches.len()
    };
    Some(matches[index].start)
}

pub(crate) fn word_search_pattern(word: &str) -> String {
    format!(r"(?i:\b{}\b)", regex::escape(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, wraps_to_next: bool) -> TextRow {
        TextRow {
            text: text.into(),
            columns: (0..text.len() as u32).collect(),
            wraps_to_next,
        }
    }

    fn copy_at(x: u32, y: u64, rows: u64) -> CopyMode {
        let mut result = CopyMode::default();
        result.start(
            1,
            SnapshotView {
                cursor: SnapshotCell { x, y },
                columns: 20,
                total_rows: rows,
                ..Default::default()
            },
        );
        result
    }

    #[test]
    fn line_content_motions_use_first_and_last_nonblank_cell() {
        let mode = copy_at(5, 0, 1);
        let read = |_| Some(row("  foo  ", false));
        assert_eq!(mode.text_target(Motion::FirstNonblank, read).unwrap().x, 2);
        assert_eq!(mode.text_target(Motion::LastNonblank, read).unwrap().x, 4);
        let blank = |_| Some(row("    ", false));
        assert_eq!(mode.text_target(Motion::FirstNonblank, blank).unwrap().x, 0);
        assert_eq!(mode.text_target(Motion::LastNonblank, blank).unwrap().x, 0);
    }

    #[test]
    fn word_motions_distinguish_keywords_punctuation_and_whitespace() {
        let mode = copy_at(1, 0, 1);
        let read = |_| Some(row("foo.bar  baz", false));
        assert_eq!(mode.text_target(Motion::ForwardWord, read).unwrap().x, 3);
        assert_eq!(mode.text_target(Motion::ForwardWordEnd, read).unwrap().x, 2);
        let mode = copy_at(9, 0, 1);
        assert_eq!(mode.text_target(Motion::BackwardWord, read).unwrap().x, 4);
    }

    #[test]
    fn word_motions_cross_soft_wrap_without_splitting_word() {
        let rows = [row("foo", true), row("bar baz", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        let mode = copy_at(1, 0, 2);
        assert_eq!(
            mode.text_target(Motion::ForwardWord, read),
            Some(SnapshotCell { x: 4, y: 1 })
        );
        assert_eq!(
            mode.text_target(Motion::ForwardWordEnd, read),
            Some(SnapshotCell { x: 2, y: 1 })
        );
        let mode = copy_at(1, 1, 2);
        assert_eq!(
            mode.text_target(Motion::BackwardWord, read),
            Some(SnapshotCell { x: 0, y: 0 })
        );
    }

    #[test]
    fn multibyte_graphemes_map_to_ghostty_cell_columns() {
        let row = TextRow {
            text: "a.界 b".into(),
            columns: vec![0, 1, 2, 2, 2, 4, 5],
            wraps_to_next: false,
        };
        let mode = copy_at(0, 0, 1);
        assert_eq!(
            mode.text_target(Motion::ForwardWord, |_| Some(row.clone()))
                .unwrap()
                .x,
            1
        );
        let mode = copy_at(1, 0, 1);
        assert_eq!(
            mode.text_target(Motion::ForwardWord, |_| Some(row.clone()))
                .unwrap()
                .x,
            2
        );
    }

    #[test]
    fn combining_mark_stays_with_its_cell_and_word() {
        let row = TextRow {
            text: "a\u{0301}.b".into(),
            columns: vec![0, 0, 0, 1, 2],
            wraps_to_next: false,
        };
        let mode = copy_at(0, 0, 1);
        assert_eq!(
            mode.text_target(Motion::ForwardWord, |_| Some(row.clone()))
                .unwrap()
                .x,
            1
        );
        assert_eq!(
            mode.text_target(Motion::ForwardWordEnd, |_| Some(row.clone()))
                .unwrap()
                .x,
            1
        );
    }

    #[test]
    fn word_end_skips_current_single_cell_word() {
        let mode = copy_at(0, 0, 1);
        assert_eq!(
            mode.text_target(Motion::ForwardWordEnd, |_| Some(row("a b", false))),
            Some(SnapshotCell { x: 2, y: 0 })
        );
    }

    #[test]
    fn big_words_join_punctuation_and_words() {
        let read = |_| Some(row("foo.bar baz", false));
        let mode = copy_at(1, 0, 1);
        assert_eq!(mode.text_target(Motion::ForwardBigWord, read).unwrap().x, 8);
        assert_eq!(
            mode.text_target(Motion::ForwardBigWordEnd, read).unwrap().x,
            6
        );
        let mode = copy_at(8, 0, 1);
        assert_eq!(
            mode.text_target(Motion::BackwardBigWord, read).unwrap().x,
            0
        );
    }

    #[test]
    fn counts_keep_zero_as_a_motion_only_without_a_prefix() {
        let mut mode = copy_at(5, 3, 10);
        assert!(!mode.add_count_digit(0));
        assert!(mode.add_count_digit(2));
        assert!(mode.add_count_digit(0));
        assert_eq!(mode.take_count(), 20);
        assert_eq!(mode.take_count(), 1);
        assert_eq!(mode.numbered_row_target(3).unwrap().y, 2);
    }

    #[test]
    fn previous_word_end_distinguishes_punctuation_and_big_words() {
        let mode = copy_at(6, 0, 1);
        let read = |_| Some(row("foo.bar", false));
        assert_eq!(
            mode.text_target(Motion::PreviousWordEnd, read).unwrap().x,
            3
        );
        assert_eq!(mode.text_target(Motion::PreviousBigWordEnd, read), None);
        let rows = [row("foo", true), row("bar baz", false)];
        let mode = copy_at(5, 1, 2);
        assert_eq!(
            mode.text_target(Motion::PreviousWordEnd, |y| rows.get(y as usize).cloned()),
            Some(SnapshotCell { x: 2, y: 1 })
        );
    }

    #[test]
    fn nonblank_line_motions_use_count_and_clamp() {
        let rows = [
            row("  one", false),
            row("    ", false),
            row(" three", false),
        ];
        let mode = copy_at(3, 0, 3);
        let read = |y: u64| rows.get(y as usize).cloned();
        assert_eq!(
            mode.nonblank_line_target(Motion::NextLineNonblank, 2, read),
            Some(SnapshotCell { x: 1, y: 2 })
        );
        assert_eq!(
            mode.nonblank_line_target(Motion::LineNonblank, 1, read),
            Some(SnapshotCell { x: 2, y: 0 })
        );
        let mode = copy_at(3, 2, 3);
        assert_eq!(
            mode.nonblank_line_target(Motion::PreviousLineNonblank, 1, read),
            Some(SnapshotCell { x: 0, y: 1 })
        );
    }

    #[test]
    fn word_under_cursor_joins_soft_wrapped_keyword() {
        let rows = [row("foo", true), row("bar baz", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        assert_eq!(
            copy_at(1, 0, 2).word_under_cursor(read).as_deref(),
            Some("foobar")
        );
        assert_eq!(
            copy_at(1, 1, 2).word_under_cursor(read).as_deref(),
            Some("foobar")
        );
        let unicode = TextRow {
            text: "猫語 test".into(),
            columns: vec![0, 0, 0, 1, 1, 1, 2, 3, 4, 5, 6],
            wraps_to_next: false,
        };
        assert_eq!(
            copy_at(1, 0, 1)
                .word_under_cursor(|_| Some(unicode.clone()))
                .as_deref(),
            Some("猫語")
        );
    }

    #[test]
    fn word_search_matches_whole_unicode_words() {
        let matcher = regex::Regex::new(&word_search_pattern("猫")).unwrap();
        assert!(matcher.is_match("a 猫 b"));
        assert!(!matcher.is_match("a 猫語 b"));
        let matcher = regex::Regex::new(&word_search_pattern("foo")).unwrap();
        assert!(matcher.is_match("FOO"));
        assert!(!matcher.is_match("foobar"));
    }

    #[test]
    fn match_navigation_skips_current_and_wraps_in_both_directions() {
        let matches = [0, 4, 9].map(|x| SnapshotCellRange {
            start: SnapshotCell { x, y: 0 },
            end: SnapshotCell { x, y: 0 },
        });
        assert_eq!(
            next_match_cell(&matches, SnapshotCell { x: 4, y: 0 }, true, 1)
                .unwrap()
                .x,
            9
        );
        assert_eq!(
            next_match_cell(&matches, SnapshotCell { x: 4, y: 0 }, false, 1)
                .unwrap()
                .x,
            0
        );
        assert_eq!(
            next_match_cell(&matches, SnapshotCell { x: 9, y: 0 }, true, 2)
                .unwrap()
                .x,
            4
        );
        assert_eq!(
            next_match_cell(&matches, SnapshotCell { x: 0, y: 0 }, false, 2)
                .unwrap()
                .x,
            4
        );
    }

    #[test]
    fn row_and_scrollback_edges_are_clamped() {
        let mode = copy_at(5, 3, 10);
        assert_eq!(
            mode.move_to(Motion::RowStart, None).unwrap().0,
            SnapshotCell { x: 0, y: 3 }
        );
        assert_eq!(mode.move_to(Motion::ScrollbackTop, None).unwrap().0.y, 0);
        assert_eq!(mode.move_to(Motion::ScrollbackBottom, None).unwrap().0.y, 9);
    }

    #[test]
    fn character_jumps_find_graphemes_and_till_stops_before_target() {
        let row = TextRow {
            text: "a界b界c".into(),
            columns: vec![0, 1, 1, 1, 3, 4, 4, 4, 6],
            wraps_to_next: false,
        };
        let mode = copy_at(0, 0, 1);
        let jump = Jump {
            target: "界".into(),
            forward: true,
            till: false,
        };
        assert_eq!(
            mode.jump_target(&jump, false, |_| Some(row.clone()))
                .unwrap()
                .x,
            1
        );
        let till = Jump {
            till: true,
            ..jump.clone()
        };
        assert_eq!(
            mode.jump_target(&till, false, |_| Some(row.clone()))
                .unwrap()
                .x,
            0
        );
        let mode = copy_at(0, 0, 1);
        assert_eq!(
            mode.jump_target(&till, true, |_| Some(row.clone()))
                .unwrap()
                .x,
            3
        );
        let reverse = Jump {
            target: "界".into(),
            forward: false,
            till: false,
        };
        let mode = copy_at(6, 0, 1);
        assert_eq!(
            mode.jump_target(&reverse, true, |_| Some(row.clone()))
                .unwrap()
                .x,
            4
        );
    }

    #[test]
    fn matching_bracket_handles_nesting_across_rows() {
        let rows = [row("(a(", false), row("b)", false), row(")", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        let mode = copy_at(0, 0, 3);
        assert_eq!(
            mode.text_target(Motion::MatchingBracket, read),
            Some(SnapshotCell { x: 0, y: 2 })
        );
        let mode = copy_at(0, 2, 3);
        assert_eq!(
            mode.text_target(Motion::MatchingBracket, read),
            Some(SnapshotCell { x: 0, y: 0 })
        );
    }

    #[test]
    fn matching_bracket_starts_at_next_bracket_on_row() {
        let mode = copy_at(0, 0, 1);
        assert_eq!(
            mode.text_target(Motion::MatchingBracket, |_| Some(row("hi [x]", false))),
            Some(SnapshotCell { x: 5, y: 0 })
        );
        assert_eq!(
            mode.text_target(Motion::MatchingBracket, |_| Some(row("plain", false))),
            None
        );
    }

    #[test]
    fn movement_clamps_at_snapshot_edges() {
        let mut mode = CopyMode::default();
        mode.start(
            1,
            SnapshotView {
                cursor: SnapshotCell { x: 0, y: 0 },
                columns: 4,
                total_rows: 3,
                ..Default::default()
            },
        );
        assert_eq!(mode.move_to(Motion::Left, None).unwrap().0, mode.cursor);
        assert_eq!(mode.move_to(Motion::Up, None).unwrap().0, mode.cursor);
        mode.moved(Motion::Right, SnapshotCell { x: 3, y: 2 });
        assert_eq!(mode.move_to(Motion::Right, None).unwrap().0, mode.cursor);
        assert_eq!(mode.move_to(Motion::Down, None).unwrap().0, mode.cursor);
    }

    #[test]
    fn viewport_and_page_motions_use_current_view() {
        let mut mode = CopyMode::default();
        mode.start(
            7,
            SnapshotView {
                cursor: SnapshotCell { x: 4, y: 41 },
                columns: 10,
                total_rows: 100,
                ..Default::default()
            },
        );
        let view = SnapshotView {
            viewport_top: 30,
            viewport_rows: 20,
            total_rows: 100,
            columns: 10,
            ..Default::default()
        };
        let target = |motion| mode.move_to(motion, Some(view)).unwrap().0;
        assert_eq!(target(Motion::ViewportTop), SnapshotCell { x: 4, y: 30 });
        assert_eq!(target(Motion::ViewportMiddle), SnapshotCell { x: 4, y: 40 });
        assert_eq!(target(Motion::ViewportBottom), SnapshotCell { x: 4, y: 49 });
        assert_eq!(target(Motion::PageUp), SnapshotCell { x: 4, y: 21 });
        assert_eq!(target(Motion::PageDown), SnapshotCell { x: 4, y: 61 });
        assert_eq!(target(Motion::HalfPageUp), SnapshotCell { x: 4, y: 31 });
        assert_eq!(target(Motion::HalfPageDown), SnapshotCell { x: 4, y: 51 });
        assert!(mode.move_to(Motion::PageDown, None).is_none());
    }

    #[test]
    fn page_motions_clamp_at_scrollback_edges() {
        let mut mode = CopyMode::default();
        mode.start(
            1,
            SnapshotView {
                cursor: SnapshotCell { x: 2, y: 97 },
                columns: 5,
                total_rows: 100,
                ..Default::default()
            },
        );
        let view = SnapshotView {
            viewport_top: 80,
            viewport_rows: 20,
            total_rows: 100,
            columns: 5,
            ..Default::default()
        };
        assert_eq!(mode.move_to(Motion::PageDown, Some(view)).unwrap().0.y, 99);
        mode.cursor.y = 2;
        assert_eq!(mode.move_to(Motion::HalfPageUp, Some(view)).unwrap().0.y, 0);
    }
}

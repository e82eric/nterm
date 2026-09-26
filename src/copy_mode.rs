use crate::ffi::{SnapshotCell, SnapshotCellRange, SnapshotView};
use regex::RegexSet;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const YANK_HIGHLIGHT_DURATION: Duration = Duration::from_millis(250);
pub(crate) const SCROLL_PADDING_ROWS: u32 = 3;

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
    pub(crate) fn result(self, target: SnapshotCell) -> MotionResult {
        let kind = if matches!(
            self,
            Self::Up
                | Self::Down
                | Self::ScrollbackTop
                | Self::ScrollbackBottom
                | Self::ViewportTop
                | Self::ViewportMiddle
                | Self::ViewportBottom
                | Self::PageUp
                | Self::PageDown
                | Self::HalfPageUp
                | Self::HalfPageDown
                | Self::NextLineNonblank
                | Self::PreviousLineNonblank
                | Self::LineNonblank
        ) {
            RangeKind::Linewise
        } else {
            RangeKind::Characterwise
        };
        let endpoint = if matches!(
            self,
            Self::ForwardWord | Self::ForwardBigWord | Self::BackwardWord | Self::BackwardBigWord
        ) {
            Endpoint::Exclusive
        } else {
            Endpoint::Inclusive
        };
        MotionResult {
            target,
            kind,
            endpoint,
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RangeKind {
    Characterwise,
    Linewise,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Endpoint {
    Inclusive,
    Exclusive,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MotionResult {
    pub(crate) target: SnapshotCell,
    pub(crate) kind: RangeKind,
    pub(crate) endpoint: Endpoint,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MotionEvaluation {
    NoMovement,
    Complete(MotionResult),
    Partial(MotionResult),
    InvalidSnapshot,
}

#[derive(Clone, Debug)]
pub(crate) struct TextRow {
    pub(crate) text: String,
    pub(crate) columns: Vec<u32>,
    pub(crate) wraps_to_next: bool,
}

fn prompt_patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // Copied from the reference WezTerm config.prompt_patterns.
        // Keep these here until the sample has Lua configuration.
        RegexSet::new([
            r"^PS [^>]*>\s*",
            r"^\d+:\d+>\s*",
            r"^›\s*",
            r"^────────────────────*",
        ])
        .expect("valid hardcoded prompt patterns")
    })
}

/// Prompt regexes operate on logical lines, so a soft-wrapped prompt is
/// matched once at its first physical row. The snapshot never changes.
pub(crate) fn find_prompt_boundaries(
    rows: u64,
    mut read: impl FnMut(u64) -> Option<TextRow>,
) -> Option<Vec<u64>> {
    let mut boundaries = Vec::new();
    let mut line = String::new();
    let mut line_start = 0;
    for y in 0..rows {
        let row = read(y)?;
        if y == 0 {
            line_start = y;
        }
        line.push_str(&row.text);
        if !row.wraps_to_next || y + 1 == rows {
            if prompt_patterns().is_match(&line) {
                boundaries.push(line_start);
            }
            line.clear();
            line_start = y + 1;
        }
    }
    Some(boundaries)
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
    byte_start: usize,
    byte_end: usize,
}

#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Backward,
}

#[derive(Clone, Copy)]
struct ScannedGlyph {
    cell: SnapshotCell,
    class: WordClass,
    first: char,
    single: bool,
}

enum TextStep {
    Glyph(ScannedGlyph),
    LineBreak,
    EmptyRow,
}

/// Row-at-a-time traversal shared by motions and delimiter scans. Soft wraps
/// preserve word continuity; hard breaks are explicit events. Only the current
/// row's graphemes are retained, even when scanning a large scrollback.
struct TextScanner<'a, F> {
    read: &'a mut F,
    rows: u64,
    y: u64,
    direction: Direction,
    big: bool,
    glyphs: std::vec::IntoIter<ScannedGlyph>,
    wraps: bool,
    loaded: bool,
    finished: bool,
}

impl<'a, F: FnMut(u64) -> Option<TextRow>> TextScanner<'a, F> {
    fn new(read: &'a mut F, rows: u64, y: u64, direction: Direction, big: bool) -> Self {
        Self {
            read,
            rows,
            y,
            direction,
            big,
            glyphs: Vec::new().into_iter(),
            wraps: false,
            loaded: false,
            finished: false,
        }
    }

    fn load(&mut self) -> Option<()> {
        let row = (self.read)(self.y)?;
        self.wraps = row.wraps_to_next;
        let mut glyphs: Vec<_> = row
            .glyphs(self.big)
            .into_iter()
            .map(|glyph| {
                let mut chars = row.text[glyph.byte_start..glyph.byte_end].chars();
                ScannedGlyph {
                    cell: SnapshotCell {
                        x: glyph.x,
                        y: self.y,
                    },
                    class: glyph.class,
                    first: chars.next().unwrap(),
                    single: chars.next().is_none(),
                }
            })
            .collect();
        if matches!(self.direction, Direction::Backward) {
            glyphs.reverse();
        }
        self.glyphs = glyphs.into_iter();
        self.loaded = true;
        Some(())
    }
}

impl<F: FnMut(u64) -> Option<TextRow>> Iterator for TextScanner<'_, F> {
    type Item = TextStep;

    fn next(&mut self) -> Option<TextStep> {
        if self.finished {
            return None;
        }
        loop {
            if !self.loaded && (self.y >= self.rows || self.load().is_none()) {
                self.finished = true;
                return None;
            }
            if let Some(glyph) = self.glyphs.next() {
                return Some(TextStep::Glyph(glyph));
            }
            let previous_wraps = self.wraps;
            let next = match self.direction {
                Direction::Forward => self.y.checked_add(1).filter(|&y| y < self.rows),
                Direction::Backward => self.y.checked_sub(1),
            };
            let Some(y) = next else {
                self.finished = true;
                return None;
            };
            self.y = y;
            if self.load().is_none() {
                self.finished = true;
                return None;
            }
            let hard_break = match self.direction {
                Direction::Forward => !previous_wraps,
                Direction::Backward => !self.wraps,
            };
            if hard_break {
                return Some(TextStep::LineBreak);
            }
            if self.glyphs.len() == 0 {
                return Some(TextStep::EmptyRow);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WordObject {
    Inner,
    Around,
}

#[derive(Clone, Copy)]
pub(crate) struct YankPending {
    pub(crate) count: u32,
    pub(crate) object: Option<WordObject>,
    pub(crate) jump: Option<(bool, bool)>,
}

#[derive(Clone, Copy)]
struct LocatedGlyph {
    cell: SnapshotCell,
    class: WordClass,
    byte_start: usize,
    byte_end: usize,
}

struct LogicalWordLine {
    text: String,
    glyphs: Vec<LocatedGlyph>,
}

struct WordSpan {
    start: usize,
    end: usize,
}

fn word_span_at(
    glyphs: &[LocatedGlyph],
    cursor: SnapshotCell,
    keywords_only: bool,
) -> Option<WordSpan> {
    let cursor_at = (cursor.y, cursor.x);
    let index = if keywords_only {
        // Search chooses a keyword on the cursor's physical row, then expands
        // through soft wraps. It must not choose a word on another row.
        glyphs
            .iter()
            .position(|glyph| {
                glyph.cell.y == cursor.y
                    && glyph.cell.x >= cursor.x
                    && glyph.class == WordClass::Keyword
            })
            .or_else(|| {
                glyphs.iter().rposition(|glyph| {
                    glyph.cell.y == cursor.y
                        && glyph.cell.x <= cursor.x
                        && glyph.class == WordClass::Keyword
                })
            })?
    } else {
        let cursor_index = glyphs
            .iter()
            .position(|glyph| (glyph.cell.y, glyph.cell.x) >= cursor_at)
            .or_else(|| glyphs.len().checked_sub(1))?;
        if glyphs[cursor_index].class == WordClass::Whitespace {
            glyphs[cursor_index..]
                .iter()
                .position(|glyph| glyph.class != WordClass::Whitespace)
                .map(|offset| cursor_index + offset)
                .or_else(|| {
                    glyphs[..cursor_index]
                        .iter()
                        .rposition(|glyph| glyph.class != WordClass::Whitespace)
                })?
        } else {
            cursor_index
        }
    };
    let mut start = index;
    while start > 0 && glyphs[start - 1].class == glyphs[index].class {
        start -= 1;
    }
    let mut end = index + 1;
    while end < glyphs.len() && glyphs[end].class == glyphs[index].class {
        end += 1;
    }
    Some(WordSpan { start, end })
}

#[derive(Clone, Copy)]
struct YankHighlight {
    range: SnapshotCellRange,
    rectangle: bool,
    expires_at: Instant,
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
                    byte_start: byte,
                    byte_end: byte + grapheme.len(),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub(crate) enum VisualMode {
    #[default]
    Characterwise,
    Linewise,
    Blockwise,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewportPosition {
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Default)]
pub(crate) struct CopyMode {
    pub(crate) generation: Option<u64>,
    pub(crate) cursor: SnapshotCell,
    pub(crate) visual_anchor: Option<SnapshotCell>,
    pub(crate) visual_mode: VisualMode,
    pub(crate) visual_trim: bool,
    pub(crate) pending_visual_object: Option<(WordObject, u32)>,
    preferred_x: u32,
    columns: u32,
    rows: u64,
    pub(crate) pending_g: bool,
    pub(crate) pending_z: bool,
    pub(crate) pending_count: Option<u32>,
    pub(crate) pending_jump: Option<(bool, bool)>,
    pub(crate) pending_yank: Option<YankPending>,
    yank_highlight: Option<YankHighlight>,
    pub(crate) last_jump: Option<Jump>,
}

impl CopyMode {
    pub(crate) fn start(&mut self, generation: u64, state: SnapshotView) {
        self.generation = Some(generation);
        self.cursor = state.cursor;
        self.visual_anchor = None;
        self.visual_mode = VisualMode::Characterwise;
        self.visual_trim = true;
        self.pending_visual_object = None;
        self.preferred_x = state.cursor.x;
        self.columns = state.columns;
        self.rows = state.total_rows;
        self.pending_g = false;
        self.pending_z = false;
        self.pending_count = None;
        self.pending_jump = None;
        self.pending_yank = None;
        self.yank_highlight = None;
        self.last_jump = None;
    }

    pub(crate) fn move_to(
        &self,
        motion: Motion,
        view: Option<SnapshotView>,
    ) -> Option<(SnapshotCell, bool)> {
        self.simple_target(motion, 1, view)
    }
    fn simple_target(
        &self,
        motion: Motion,
        count: u32,
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
                cell.y = cell.y.saturating_sub(u64::from(count));
                cell.x = self.preferred_x.min(self.columns - 1);
            }
            Motion::Down => {
                cell.y = cell.y.saturating_add(u64::from(count)).min(last_row);
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
                    Motion::ViewportTop => top.saturating_add(u64::from(count - 1)).min(bottom),
                    Motion::ViewportMiddle => top + (bottom - top + 1) / 2,
                    Motion::ViewportBottom => bottom.saturating_sub(u64::from(count - 1)).max(top),
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
                    cell.y.saturating_sub(step.saturating_mul(u64::from(count)))
                } else {
                    cell.y
                        .saturating_add(step.saturating_mul(u64::from(count)))
                        .min(last_row)
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

    pub(crate) fn highlight_yank(&mut self, range: SnapshotCellRange, now: Instant) {
        self.yank_highlight = Some(YankHighlight {
            range,
            rectangle: false,
            expires_at: now + YANK_HIGHLIGHT_DURATION,
        });
    }

    pub(crate) fn yank_highlight_range(&self) -> Option<SnapshotCellRange> {
        self.yank_highlight.map(|highlight| highlight.range)
    }

    pub(crate) fn highlight_rectangle_yank(&mut self, range: SnapshotCellRange, now: Instant) {
        self.highlight_yank(range, now);
        self.yank_highlight.as_mut().unwrap().rectangle = true;
    }

    pub(crate) fn yank_highlight_is_rectangle(&self) -> bool {
        self.yank_highlight
            .is_some_and(|highlight| highlight.rectangle)
    }

    pub(crate) fn yank_highlight_remaining(&self, now: Instant) -> Option<Duration> {
        self.yank_highlight
            .map(|highlight| highlight.expires_at.saturating_duration_since(now))
    }

    pub(crate) fn clear_expired_yank_highlight(&mut self, now: Instant) -> bool {
        if self
            .yank_highlight
            .is_some_and(|highlight| now >= highlight.expires_at)
        {
            self.yank_highlight = None;
            return true;
        }
        false
    }

    pub(crate) fn toggle_visual(&mut self, mode: VisualMode) {
        self.pending_visual_object = None;
        self.visual_trim = true;
        if self.visual_anchor.is_some() && self.visual_mode == mode {
            self.visual_anchor = None;
        } else {
            if self.visual_anchor.is_none() {
                self.visual_anchor = Some(self.cursor);
            }
            self.visual_mode = mode;
        }
    }

    pub(crate) fn swap_visual_endpoint(&mut self) -> bool {
        let Some(anchor) = self.visual_anchor else {
            return false;
        };
        self.visual_anchor = Some(self.cursor);
        self.cursor = anchor;
        self.preferred_x = anchor.x;
        true
    }

    pub(crate) fn swap_visual_horizontal_endpoint(&mut self) -> bool {
        if self.visual_mode != VisualMode::Blockwise {
            return self.swap_visual_endpoint();
        }
        let Some(anchor) = self.visual_anchor.as_mut() else {
            return false;
        };
        std::mem::swap(&mut anchor.x, &mut self.cursor.x);
        self.preferred_x = self.cursor.x;
        true
    }

    pub(crate) fn visual_rectangle_to(&self, cursor: SnapshotCell) -> Option<SnapshotCellRange> {
        let anchor = self.visual_anchor?;
        Some(SnapshotCellRange {
            start: SnapshotCell {
                x: anchor.x.min(cursor.x),
                y: anchor.y.min(cursor.y),
            },
            end: SnapshotCell {
                x: anchor.x.max(cursor.x),
                y: anchor.y.max(cursor.y),
            },
        })
    }

    pub(crate) fn begin_visual_object(&mut self, object: WordObject) -> bool {
        if self.visual_anchor.is_none() {
            return false;
        }
        self.pending_visual_object = Some((object, self.take_count()));
        self.pending_g = false;
        self.pending_jump = None;
        true
    }

    /// Replace the selection with an object; repeated objects do not yet
    /// expand an existing selection as Vim does.
    pub(crate) fn select_visual_object(&mut self, range: SnapshotCellRange, trim: bool) {
        self.visual_anchor = Some(range.start);
        self.visual_mode = VisualMode::Characterwise;
        self.visual_trim = trim;
        self.moved(Motion::CharacterJump, range.end);
        self.pending_visual_object = None;
        self.pending_count = None;
    }

    pub(crate) fn visual_range_to(&self, cursor: SnapshotCell) -> Option<SnapshotCellRange> {
        let anchor = self.visual_anchor?;
        let (start, end) = if (anchor.y, anchor.x) <= (cursor.y, cursor.x) {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some(SnapshotCellRange { start, end })
    }

    pub(crate) fn linewise_visual_range_to(
        &self,
        cursor: SnapshotCell,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        self.logical_line_range_between(self.visual_anchor?.y, cursor.y, &mut read)
    }
    fn logical_line_bounds(
        &self,
        row: u64,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<(u64, u64)> {
        if row >= self.rows {
            return None;
        }
        let mut start = row;
        while start > 0 && read(start - 1)?.wraps_to_next {
            start -= 1;
        }
        let mut end = row;
        while end < self.rows - 1 && read(end)?.wraps_to_next {
            end += 1;
        }
        Some((start, end))
    }

    pub(crate) fn yank_lines_range(
        &self,
        count: u32,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        let (start, mut end) = self.logical_line_bounds(self.cursor.y, &mut read)?;
        for _ in 1..count {
            if end >= self.rows - 1 {
                break;
            }
            (_, end) = self.logical_line_bounds(end + 1, &mut read)?;
        }
        Some(SnapshotCellRange {
            start: SnapshotCell { x: 0, y: start },
            end: SnapshotCell {
                x: self.columns.checked_sub(1)?,
                y: end,
            },
        })
    }

    pub(crate) fn yank_to_row_end_range(
        &self,
        count: u32,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        let y = self
            .cursor
            .y
            .saturating_add(u64::from(count.checked_sub(1)?));
        let y = y.min(self.rows.checked_sub(1)?);
        let end_x = read(y)?
            .glyphs(false)
            .iter()
            .rev()
            .find(|glyph| glyph.nonblank)
            .map_or(0, |glyph| glyph.x);
        let end = SnapshotCell { x: end_x, y };
        ((end.y, end.x) >= (self.cursor.y, self.cursor.x)).then_some(SnapshotCellRange {
            start: self.cursor,
            end,
        })
    }

    pub(crate) fn yank_motion_range(
        &self,
        result: MotionResult,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<(SnapshotCellRange, bool)> {
        if result.kind == RangeKind::Linewise {
            return Some((
                self.logical_line_range_between(self.cursor.y, result.target.y, &mut read)?,
                true,
            ));
        }
        let a = self.cursor;
        let b = result.target;
        let (start, mut end) = if (a.y, a.x) <= (b.y, b.x) {
            (a, b)
        } else {
            (b, a)
        };
        if result.endpoint == Endpoint::Exclusive {
            end = self.previous_text_cell(end, &mut read)?;
        }
        ((start.y, start.x) <= (end.y, end.x)).then_some((SnapshotCellRange { start, end }, false))
    }

    fn logical_line_range_between(
        &self,
        a: u64,
        b: u64,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        let (a_start, a_end) = self.logical_line_bounds(a, read)?;
        let (b_start, b_end) = if a == b {
            (a_start, a_end)
        } else {
            self.logical_line_bounds(b, read)?
        };
        Some(SnapshotCellRange {
            start: SnapshotCell {
                x: 0,
                y: a_start.min(b_start),
            },
            end: SnapshotCell {
                x: self.columns.checked_sub(1)?,
                y: a_end.max(b_end),
            },
        })
    }
    fn previous_text_cell(
        &self,
        cell: SnapshotCell,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCell> {
        for y in (0..=cell.y).rev() {
            let row = read(y)?;
            if let Some((x, _)) = row
                .grapheme_cells()
                .into_iter()
                .rev()
                .find(|(x, _)| y < cell.y || *x < cell.x)
            {
                return Some(SnapshotCell { x, y });
            }
        }
        None
    }

    pub(crate) fn word_object_range(
        &self,
        count: u32,
        object: WordObject,
        big: bool,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        let line = self.logical_word_line(big, &mut read)?;
        let glyphs = &line.glyphs;
        let WordSpan { mut start, mut end } = word_span_at(glyphs, self.cursor, false)?;
        for _ in 1..count {
            let Some(next) = glyphs[end..]
                .iter()
                .position(|glyph| glyph.class != WordClass::Whitespace)
                .map(|offset| end + offset)
            else {
                break;
            };
            end = next + 1;
            while end < glyphs.len() && glyphs[end].class == glyphs[next].class {
                end += 1;
            }
        }
        if object == WordObject::Around {
            if end < glyphs.len() && glyphs[end].class == WordClass::Whitespace {
                while end < glyphs.len() && glyphs[end].class == WordClass::Whitespace {
                    end += 1;
                }
            } else {
                while start > 0 && glyphs[start - 1].class == WordClass::Whitespace {
                    start -= 1;
                }
            }
        }
        Some(SnapshotCellRange {
            start: glyphs[start].cell,
            end: glyphs[end - 1].cell,
        })
    }

    pub(crate) fn quote_object_range(
        &self,
        object: WordObject,
        quote: char,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        let (first, last) = self.logical_line_bounds(self.cursor.y, &mut read)?;
        let mut quotes = Vec::new();
        for y in first..=last {
            let row = read(y)?;
            let mut escaped = false;
            for (x, grapheme) in row.grapheme_cells() {
                if grapheme == "\\" {
                    escaped = !escaped;
                } else {
                    if grapheme.starts_with(quote) && !escaped {
                        quotes.push(SnapshotCell { x, y });
                    }
                    escaped = false;
                }
            }
        }
        let cursor = (self.cursor.y, self.cursor.x);
        for pair in quotes.chunks_exact(2) {
            if (pair[0].y, pair[0].x) <= cursor && cursor <= (pair[1].y, pair[1].x) {
                return self.bounded_object_range(pair[0], pair[1], object, &mut read);
            }
        }
        None
    }

    pub(crate) fn delimiter_object_range(
        &self,
        object: WordObject,
        open: char,
        close: char,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        let cursor = (self.cursor.y, self.cursor.x);
        let mut depth = 0usize;
        let mut opener = None;
        for step in TextScanner::new(
            &mut read,
            self.rows,
            self.cursor.y,
            Direction::Backward,
            false,
        ) {
            let TextStep::Glyph(glyph) = step else {
                continue;
            };
            if (glyph.cell.y, glyph.cell.x) > cursor {
                continue;
            }
            if glyph.first == close {
                depth += 1;
            }
            if glyph.first == open {
                if depth == 0 {
                    opener = Some(glyph.cell);
                    break;
                }
                depth -= 1;
            }
        }
        let opener = opener?;
        depth = 0;
        let mut closer = None;
        for step in TextScanner::new(&mut read, self.rows, opener.y, Direction::Forward, false) {
            let TextStep::Glyph(glyph) = step else {
                continue;
            };
            if (glyph.cell.y, glyph.cell.x) < (opener.y, opener.x) {
                continue;
            }
            if glyph.first == open {
                depth += 1;
            }
            if glyph.first == close {
                depth -= 1;
                if depth == 0 {
                    closer = Some(glyph.cell);
                    break;
                }
            }
        }
        self.bounded_object_range(opener, closer?, object, &mut read)
    }
    pub(crate) fn command_object_range(
        &self,
        object: WordObject,
        count: u32,
        boundaries: &[u64],
    ) -> Option<SnapshotCellRange> {
        let index = boundaries
            .partition_point(|&y| y <= self.cursor.y)
            .checked_sub(1)?;
        let prompt = boundaries[index];
        let mut end = self.rows.checked_sub(1)?;
        if let Some(&next) = boundaries.get(index.saturating_add(count as usize)) {
            end = next.checked_sub(1)?;
        }
        let start = if object == WordObject::Around {
            prompt
        } else {
            prompt + 1
        };
        if start > end {
            return None;
        }
        Some(SnapshotCellRange {
            start: SnapshotCell { x: 0, y: start },
            end: SnapshotCell {
                x: self.columns.checked_sub(1)?,
                y: end,
            },
        })
    }

    pub(crate) fn total_rows(&self) -> u64 {
        self.rows
    }

    pub(crate) fn positioned_viewport_top(
        &self,
        position: ViewportPosition,
        view: SnapshotView,
    ) -> Option<u64> {
        if view.viewport_rows == 0 || view.total_rows != self.rows || view.columns != self.columns {
            return None;
        }
        let height = u64::from(view.viewport_rows);
        let padding = u64::from(SCROLL_PADDING_ROWS).min((height - 1) / 2);
        let offset = match position {
            ViewportPosition::Top => padding,
            ViewportPosition::Center => height / 2,
            ViewportPosition::Bottom => height - 1 - padding,
        };
        Some(
            self.cursor
                .y
                .saturating_sub(offset)
                .min(self.rows.saturating_sub(height)),
        )
    }

    /// Scroll physical rows, moving the cursor only to keep it within the
    /// padded viewport. The caller resolves wide cells before publishing.
    pub(crate) fn scrolled_viewport_target(
        &self,
        down: bool,
        count: u32,
        view: SnapshotView,
    ) -> Option<(u64, SnapshotCell)> {
        if self.rows == 0
            || self.columns == 0
            || view.viewport_rows == 0
            || view.total_rows != self.rows
            || view.columns != self.columns
        {
            return None;
        }
        let height = u64::from(view.viewport_rows).min(self.rows);
        let max_top = self.rows - height;
        let current = view.viewport_top.min(max_top);
        let top = if down {
            current.saturating_add(u64::from(count)).min(max_top)
        } else {
            current.saturating_sub(u64::from(count))
        };
        // A clamped scroll at either boundary must not move the cursor.
        if top == current {
            return Some((top, self.cursor));
        }
        let padding = u64::from(SCROLL_PADDING_ROWS).min((height - 1) / 2);
        let y = self
            .cursor
            .y
            .clamp(top + padding, top + height - 1 - padding);
        let x = if y == self.cursor.y {
            self.cursor.x
        } else {
            self.preferred_x.min(self.columns - 1)
        };
        Some((top, SnapshotCell { x, y }))
    }

    /// Evaluate without changing the displayed cursor. Operators can reject a
    /// partial result, while ordinary movement can keep the progress made.
    pub(crate) fn evaluate_motion(
        &self,
        motion: Motion,
        count: u32,
        explicit_count: bool,
        view: Option<SnapshotView>,
        read: impl FnMut(u64) -> Option<TextRow>,
        mut resolve: impl FnMut(SnapshotCell, bool) -> Option<SnapshotCell>,
    ) -> MotionEvaluation {
        let count = count.max(1);
        // A counted motion often revisits the same rows. Keep a small cache
        // for this evaluation only; scanning never retains the whole history.
        let mut source = read;
        let mut rows: VecDeque<(u64, TextRow)> = VecDeque::new();
        let mut read = |y| {
            if let Some((_, row)) = rows.iter().find(|(cached, _)| *cached == y) {
                return Some(row.clone());
            }
            let row = source(y)?;
            if rows.len() == 4 {
                rows.pop_front();
            }
            rows.push_back((y, row.clone()));
            Some(row)
        };
        let direct = if explicit_count
            && matches!(motion, Motion::ScrollbackTop | Motion::ScrollbackBottom)
        {
            Some(self.numbered_row_target(count).map(|cell| (cell, false)))
        } else if explicit_count && motion == Motion::MatchingBracket {
            Some(self.percent_row_target(count).map(|cell| (cell, false)))
        } else if matches!(
            motion,
            Motion::NextLineNonblank | Motion::PreviousLineNonblank | Motion::LineNonblank
        ) {
            Some(
                self.nonblank_line_target(motion, count, &mut read)
                    .map(|cell| (cell, false)),
            )
        } else {
            self.counted_simple_target(motion, count, view).map(Some)
        };
        if let Some(target) = direct {
            return match target {
                None => MotionEvaluation::NoMovement,
                Some((cell, right)) => match resolve(cell, right) {
                    Some(actual) => MotionEvaluation::Complete(motion.result(actual)),
                    None => MotionEvaluation::InvalidSnapshot,
                },
            };
        }
        let mut working = self.clone();
        let mut result = None;
        for _ in 0..count {
            let target = if motion.needs_text() {
                working
                    .text_target(motion, &mut read)
                    .map(|cell| (cell, false))
            } else {
                working.move_to(motion, view)
            };
            let Some((cell, right)) = target else {
                return result.map_or(MotionEvaluation::NoMovement, MotionEvaluation::Partial);
            };
            let Some(actual) = resolve(cell, right) else {
                return MotionEvaluation::InvalidSnapshot;
            };
            if actual == working.cursor {
                break;
            }
            working.moved(motion, actual);
            result = Some(motion.result(actual));
        }
        result.map_or(MotionEvaluation::NoMovement, MotionEvaluation::Complete)
    }

    pub(crate) fn command_target(
        &self,
        forward: bool,
        count: u32,
        boundaries: &[u64],
    ) -> Option<SnapshotCell> {
        let steps = usize::try_from(count.max(1)).ok()?;
        let index = if forward {
            boundaries
                .partition_point(|&row| row <= self.cursor.y)
                .checked_add(steps - 1)?
        } else {
            boundaries
                .partition_point(|&row| row < self.cursor.y)
                .checked_sub(steps)?
        };
        Some(SnapshotCell {
            x: 0,
            y: *boundaries.get(index)?,
        })
    }

    fn bounded_object_range(
        &self,
        start: SnapshotCell,
        end: SnapshotCell,
        object: WordObject,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<SnapshotCellRange> {
        if object == WordObject::Around {
            return Some(SnapshotCellRange { start, end });
        }
        let first = (start.y..=end.y).find_map(|y| {
            read(y)?
                .grapheme_cells()
                .into_iter()
                .find(|(x, _)| (y, *x) > (start.y, start.x))
                .map(|(x, _)| SnapshotCell { x, y })
        })?;
        let last = (start.y..=end.y).rev().find_map(|y| {
            read(y)?
                .grapheme_cells()
                .into_iter()
                .rev()
                .find(|(x, _)| (y, *x) < (end.y, end.x))
                .map(|(x, _)| SnapshotCell { x, y })
        })?;
        ((first.y, first.x) <= (last.y, last.x)).then_some(SnapshotCellRange {
            start: first,
            end: last,
        })
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
        // Horizontal counts resolve intermediate cells so a wide grapheme
        // counts as one motion rather than two terminal columns.
        if matches!(
            motion,
            Motion::Up
                | Motion::Down
                | Motion::PageUp
                | Motion::PageDown
                | Motion::HalfPageUp
                | Motion::HalfPageDown
                | Motion::ViewportTop
                | Motion::ViewportBottom
        ) {
            self.simple_target(motion, count.max(1), view)
        } else {
            None
        }
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

    fn logical_word_line(
        &self,
        big: bool,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<LogicalWordLine> {
        let (start, end) = self.logical_line_bounds(self.cursor.y, read)?;
        let mut line = LogicalWordLine {
            text: String::new(),
            glyphs: Vec::new(),
        };
        for y in start..=end {
            let row = read(y)?;
            let byte_base = line.text.len();
            for glyph in row.glyphs(big) {
                line.glyphs.push(LocatedGlyph {
                    cell: SnapshotCell { x: glyph.x, y },
                    class: glyph.class,
                    byte_start: byte_base + glyph.byte_start,
                    byte_end: byte_base + glyph.byte_end,
                });
            }
            line.text.push_str(&row.text);
        }
        Some(line)
    }

    pub(crate) fn word_under_cursor(
        &self,
        mut read: impl FnMut(u64) -> Option<TextRow>,
    ) -> Option<String> {
        let line = self.logical_word_line(false, &mut read)?;
        let span = word_span_at(&line.glyphs, self.cursor, true)?;
        Some(
            line.text[line.glyphs[span.start].byte_start..line.glyphs[span.end - 1].byte_end]
                .to_owned(),
        )
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
                "(" => Some((x, '(', ')', true)),
                "[" => Some((x, '[', ']', true)),
                "{" => Some((x, '{', '}', true)),
                ")" => Some((x, ')', '(', false)),
                "]" => Some((x, ']', '[', false)),
                "}" => Some((x, '}', '{', false)),
                _ => None,
            })?;
        let direction = if forward {
            Direction::Forward
        } else {
            Direction::Backward
        };
        let mut depth = 0usize;
        for step in TextScanner::new(read, self.rows, self.cursor.y, direction, false) {
            let TextStep::Glyph(glyph) = step else {
                continue;
            };
            if glyph.cell.y == self.cursor.y
                && (if forward {
                    glyph.cell.x < source_x
                } else {
                    glyph.cell.x > source_x
                })
            {
                continue;
            }
            if !glyph.single {
                continue;
            }
            if glyph.first == source {
                depth += 1;
            } else if glyph.first == target {
                depth -= 1;
                if depth == 0 {
                    return Some(glyph.cell);
                }
            }
        }
        None
    }

    fn forward_word(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
        big: bool,
    ) -> Option<SnapshotCell> {
        let mut previous = WordClass::Whitespace;
        for step in TextScanner::new(read, self.rows, self.cursor.y, Direction::Forward, big) {
            if matches!(step, TextStep::EmptyRow) {
                continue;
            }
            let TextStep::Glyph(glyph) = step else {
                previous = WordClass::Whitespace;
                continue;
            };
            if (glyph.cell.y, glyph.cell.x) <= (self.cursor.y, self.cursor.x) {
                previous = glyph.class;
                continue;
            }
            if glyph.class != WordClass::Whitespace && glyph.class != previous {
                return Some(glyph.cell);
            }
            previous = glyph.class;
        }
        None
    }

    fn backward_word(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
        big: bool,
    ) -> Option<SnapshotCell> {
        let mut scan =
            TextScanner::new(read, self.rows, self.cursor.y, Direction::Backward, big).peekable();
        while let Some(step) = scan.next() {
            let TextStep::Glyph(glyph) = step else {
                continue;
            };
            if (glyph.cell.y, glyph.cell.x) >= (self.cursor.y, self.cursor.x)
                || glyph.class == WordClass::Whitespace
            {
                continue;
            }
            let previous_class = match scan.peek() {
                Some(TextStep::Glyph(previous)) => previous.class,
                _ => WordClass::Whitespace,
            };
            if glyph.class != previous_class {
                return Some(glyph.cell);
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
        for step in TextScanner::new(read, self.rows, self.cursor.y, Direction::Forward, big) {
            let (class, cell) = match step {
                TextStep::Glyph(glyph) => {
                    if (glyph.cell.y, glyph.cell.x) < (self.cursor.y, self.cursor.x) {
                        continue;
                    }
                    (glyph.class, Some(glyph.cell))
                }
                TextStep::LineBreak => (WordClass::Whitespace, None),
                TextStep::EmptyRow => continue,
            };
            if class != active_class {
                if let Some(end) = last
                    && (end.y, end.x) > (self.cursor.y, self.cursor.x)
                {
                    return Some(end);
                }
                last = None;
            }
            active_class = class;
            if class != WordClass::Whitespace {
                last = cell;
            }
        }
        last.filter(|end| (end.y, end.x) > (self.cursor.y, self.cursor.x))
    }

    fn previous_word_end(
        &self,
        read: &mut impl FnMut(u64) -> Option<TextRow>,
        big: bool,
    ) -> Option<SnapshotCell> {
        // Include text to the right of the cursor to establish the class on
        // the other side of the first candidate, even across a soft wrap.
        let row = read(self.cursor.y)?;
        let mut next_class = if row.wraps_to_next && self.cursor.y + 1 < self.rows {
            read(self.cursor.y + 1)?
                .glyphs(big)
                .first()
                .map_or(WordClass::Whitespace, |glyph| glyph.class)
        } else {
            WordClass::Whitespace
        };
        for step in TextScanner::new(read, self.rows, self.cursor.y, Direction::Backward, big) {
            let TextStep::Glyph(glyph) = step else {
                next_class = WordClass::Whitespace;
                continue;
            };
            if (glyph.cell.y, glyph.cell.x) >= (self.cursor.y, self.cursor.x) {
                next_class = glyph.class;
                continue;
            }
            if glyph.class != WordClass::Whitespace && glyph.class != next_class {
                return Some(glyph.cell);
            }
            next_class = glyph.class;
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

/// Decoration ranges are ordered newest row first. Only materialize visible
/// physical rows, even when a block extends across the entire scrollback.
pub(crate) fn rectangle_row_ranges(
    range: SnapshotCellRange,
    top: u64,
    height: u64,
) -> Vec<SnapshotCellRange> {
    let start = range.start.y.max(top);
    let end = range
        .end
        .y
        .min(top.saturating_add(height).saturating_sub(1));
    if height == 0 || start > end {
        return Vec::new();
    }
    (start..=end)
        .rev()
        .map(|y| SnapshotCellRange {
            start: SnapshotCell {
                x: range.start.x,
                y,
            },
            end: SnapshotCell { x: range.end.x, y },
        })
        .collect()
}

pub(crate) fn word_search_pattern(word: &str) -> String {
    format!(r"(?i:\b{}\b)", regex::escape(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_selection_normalizes_corners_and_swaps_endpoints() {
        let mut mode = copy_at(7, 50, 100);
        mode.toggle_visual(VisualMode::Blockwise);
        mode.moved(Motion::CharacterJump, SnapshotCell { x: 3, y: 55 });
        let rectangle = SnapshotCellRange {
            start: SnapshotCell { x: 3, y: 50 },
            end: SnapshotCell { x: 7, y: 55 },
        };
        assert_eq!(mode.visual_rectangle_to(mode.cursor), Some(rectangle));
        assert!(mode.swap_visual_horizontal_endpoint());
        assert_eq!(mode.cursor, SnapshotCell { x: 7, y: 55 });
        assert_eq!(mode.visual_anchor, Some(SnapshotCell { x: 3, y: 50 }));
        assert_eq!(mode.visual_rectangle_to(mode.cursor), Some(rectangle));
        assert!(mode.swap_visual_endpoint());
        assert_eq!(mode.cursor, SnapshotCell { x: 3, y: 50 });
        assert_eq!(mode.visual_rectangle_to(mode.cursor), Some(rectangle));
        mode.toggle_visual(VisualMode::Linewise);
        assert_eq!(mode.visual_anchor, Some(SnapshotCell { x: 7, y: 55 }));
        mode.toggle_visual(VisualMode::Blockwise);
        mode.toggle_visual(VisualMode::Blockwise);
        assert!(mode.visual_rectangle_to(mode.cursor).is_none());
    }

    #[test]
    fn rectangle_decorations_are_clipped_and_ordered_newest_first() {
        let rectangle = SnapshotCellRange {
            start: SnapshotCell { x: 2, y: 0 },
            end: SnapshotCell { x: 5, y: 1_000_000 },
        };
        let ranges = rectangle_row_ranges(rectangle, 50, 3);
        assert_eq!(ranges.len(), 3);
        for (range, y) in ranges.iter().zip([52, 51, 50]) {
            assert_eq!(
                *range,
                SnapshotCellRange {
                    start: SnapshotCell { x: 2, y },
                    end: SnapshotCell { x: 5, y },
                }
            );
        }
        assert!(rectangle_row_ranges(rectangle, 50, 0).is_empty());
        assert!(rectangle_row_ranges(rectangle, 1_000_001, 3).is_empty());
    }

    #[test]
    fn rectangle_yank_flash_retains_shape_until_replaced_or_expired() {
        let mut mode = copy_at(2, 1, 10);
        let range = SnapshotCellRange {
            start: SnapshotCell { x: 1, y: 1 },
            end: SnapshotCell { x: 5, y: 3 },
        };
        let now = Instant::now();
        mode.highlight_rectangle_yank(range, now);
        assert!(mode.yank_highlight_is_rectangle());
        assert_eq!(mode.yank_highlight_range(), Some(range));
        mode.highlight_yank(range, now);
        assert!(!mode.yank_highlight_is_rectangle());
        mode.highlight_rectangle_yank(range, now);
        assert!(mode.clear_expired_yank_highlight(now + YANK_HIGHLIGHT_DURATION));
        assert!(!mode.yank_highlight_is_rectangle());
    }

    #[test]
    fn visual_endpoint_swap_preserves_range_and_updates_preferred_column() {
        for visual in [VisualMode::Characterwise, VisualMode::Linewise] {
            let mut mode = copy_at(3, 50, 100);
            assert!(!mode.swap_visual_endpoint());
            mode.toggle_visual(visual);
            mode.moved(Motion::Right, SnapshotCell { x: 7, y: 55 });
            let range = mode.visual_range_to(mode.cursor);
            assert!(mode.swap_visual_endpoint());
            assert_eq!(mode.cursor, SnapshotCell { x: 3, y: 50 });
            assert_eq!(mode.visual_anchor, Some(SnapshotCell { x: 7, y: 55 }));
            assert_eq!(mode.visual_range_to(mode.cursor), range);
            assert_eq!(mode.visual_mode, visual);
            assert_eq!(mode.move_to(Motion::Down, None).unwrap().0.x, 3);
            assert!(mode.swap_visual_endpoint());
            assert_eq!(mode.cursor, SnapshotCell { x: 7, y: 55 });
        }
    }

    #[test]
    fn viewport_scrolling_counts_padding_boundaries_and_selection() {
        let mut mode = copy_at(3, 50, 100);
        mode.toggle_visual(VisualMode::Characterwise);
        let view = SnapshotView {
            columns: mode.columns,
            total_rows: 100,
            viewport_top: 40,
            viewport_rows: 20,
            ..Default::default()
        };
        assert_eq!(
            mode.scrolled_viewport_target(true, 1, view),
            Some((41, mode.cursor))
        );
        assert_eq!(
            mode.scrolled_viewport_target(false, 1, view),
            Some((39, mode.cursor))
        );
        assert_eq!(
            mode.scrolled_viewport_target(true, 10, view),
            Some((50, SnapshotCell { x: 3, y: 53 }))
        );
        assert_eq!(
            mode.scrolled_viewport_target(false, 10, view),
            Some((30, SnapshotCell { x: 3, y: 46 }))
        );
        assert_eq!(
            mode.scrolled_viewport_target(true, u32::MAX, view),
            Some((80, SnapshotCell { x: 3, y: 83 }))
        );
        assert_eq!(
            mode.scrolled_viewport_target(false, u32::MAX, view),
            Some((0, SnapshotCell { x: 3, y: 16 }))
        );
        assert_eq!(mode.visual_anchor, Some(mode.cursor));
        assert_eq!(
            mode.scrolled_viewport_target(
                false,
                1,
                SnapshotView {
                    viewport_top: 0,
                    ..view
                }
            ),
            Some((0, mode.cursor))
        );
        assert_eq!(
            mode.scrolled_viewport_target(
                true,
                1,
                SnapshotView {
                    viewport_top: 80,
                    ..view
                }
            ),
            Some((80, mode.cursor))
        );
        assert_eq!(
            mode.scrolled_viewport_target(
                true,
                1,
                SnapshotView {
                    viewport_rows: 1,
                    viewport_top: 49,
                    ..view
                }
            ),
            Some((50, mode.cursor))
        );
        assert!(
            mode.scrolled_viewport_target(
                true,
                1,
                SnapshotView {
                    viewport_rows: 0,
                    ..view
                }
            )
            .is_none()
        );
    }

    #[test]
    fn viewport_positioning_preserves_cursor_and_selection_and_clamps() {
        let mut mode = copy_at(3, 50, 100);
        mode.toggle_visual(VisualMode::Characterwise);
        mode.moved(Motion::Down, SnapshotCell { x: 3, y: 52 });
        let cursor = mode.cursor;
        let anchor = mode.visual_anchor;
        let view = SnapshotView {
            columns: mode.columns,
            total_rows: 100,
            viewport_rows: 20,
            ..Default::default()
        };
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Top, view),
            Some(49)
        );
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Center, view),
            Some(42)
        );
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Bottom, view),
            Some(36)
        );
        assert_eq!(mode.cursor, cursor);
        assert_eq!(mode.visual_anchor, anchor);
        mode.cursor.y = 2;
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Center, view),
            Some(0)
        );
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Bottom, view),
            Some(0)
        );
        mode.cursor.y = 99;
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Top, view),
            Some(80)
        );
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Center, view),
            Some(80)
        );
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Bottom, view),
            Some(80)
        );
        let empty_view = SnapshotView {
            viewport_rows: 0,
            ..view
        };
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Top, empty_view),
            None
        );
        let mismatched = SnapshotView {
            total_rows: 99,
            ..view
        };
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Top, mismatched),
            None
        );
        let short_view = SnapshotView {
            total_rows: 5,
            viewport_rows: 20,
            ..view
        };
        let mode = copy_at(0, 2, 5);
        assert_eq!(
            mode.positioned_viewport_top(ViewportPosition::Top, short_view),
            Some(0)
        );
    }

    #[test]
    fn visual_word_objects_replace_selection_and_keep_motions_available() {
        for (object, end, trim) in [(WordObject::Inner, 2, true), (WordObject::Around, 3, false)] {
            let mut mode = copy_at(1, 0, 1);
            mode.toggle_visual(VisualMode::Linewise);
            assert!(mode.begin_visual_object(object));
            let (object, count) = mode.pending_visual_object.unwrap();
            let range = mode
                .word_object_range(count, object, false, |_| Some(row("foo bar", false)))
                .unwrap();
            mode.select_visual_object(range, trim);
            assert_eq!(mode.visual_mode, VisualMode::Characterwise);
            assert_eq!(mode.visual_anchor, Some(SnapshotCell { x: 0, y: 0 }));
            assert_eq!(mode.cursor, SnapshotCell { x: end, y: 0 });
            assert_eq!(mode.visual_range_to(mode.cursor), Some(range));
            assert_eq!(mode.visual_trim, trim);
            assert!(mode.pending_visual_object.is_none());
            // Subsequent motion extends the selection without losing its anchor.
            mode.moved(Motion::Right, SnapshotCell { x: end + 1, y: 0 });
            assert_eq!(mode.visual_range_to(mode.cursor).unwrap().end.x, end + 1);
        }
    }

    #[test]
    fn counted_visual_objects_cross_soft_wraps_and_reset_pending_state() {
        let mut mode = copy_at(1, 0, 2);
        assert!(!mode.begin_visual_object(WordObject::Inner));
        mode.toggle_visual(VisualMode::Characterwise);
        mode.add_count_digit(2);
        assert!(mode.begin_visual_object(WordObject::Inner));
        let (object, count) = mode.pending_visual_object.unwrap();
        assert_eq!(count, 2);
        assert!(mode.pending_count.is_none());
        let rows = [row("foo", true), row("bar baz qux", false)];
        let range = mode
            .word_object_range(count, object, false, |y| rows.get(y as usize).cloned())
            .unwrap();
        mode.select_visual_object(range, true);
        assert_eq!(range.start, SnapshotCell { x: 0, y: 0 });
        assert_eq!(range.end, SnapshotCell { x: 6, y: 1 });
        assert_eq!(mode.cursor, range.end);
        assert_eq!(mode.visual_range_to(mode.cursor), Some(range));
        mode.begin_visual_object(WordObject::Around);
        mode.toggle_visual(VisualMode::Linewise);
        assert!(mode.pending_visual_object.is_none());
    }

    #[test]
    fn evaluation_distinguishes_partial_motion_and_invalid_snapshot() {
        let mode = copy_at(0, 0, 1);
        let rows = [row("foo bar", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        let resolve = |cell, _| Some(cell);
        let MotionEvaluation::Partial(result) =
            mode.evaluate_motion(Motion::ForwardWord, 3, true, None, read, resolve)
        else {
            panic!("count exhaustion should preserve partial movement");
        };
        assert_eq!(result.target, SnapshotCell { x: 4, y: 0 });
        assert_eq!(result.endpoint, Endpoint::Exclusive);
        assert_eq!(mode.cursor, SnapshotCell { x: 0, y: 0 });
        assert!(matches!(
            mode.evaluate_motion(Motion::BackwardWord, 1, false, None, read, resolve,),
            MotionEvaluation::NoMovement
        ));
        assert!(matches!(
            mode.evaluate_motion(Motion::ForwardWord, 1, false, None, read, |_, _| None,),
            MotionEvaluation::InvalidSnapshot
        ));
    }

    #[test]
    fn counted_horizontal_motion_resolves_each_wide_cell() {
        let mut mode = copy_at(0, 0, 1);
        mode.columns = 6;
        let mut requested = Vec::new();
        let result = mode.evaluate_motion(
            Motion::Right,
            2,
            true,
            None,
            |_| None,
            |mut cell, right| {
                requested.push((cell.x, right));
                if cell.x == 1 {
                    cell.x = 2;
                }
                Some(cell)
            },
        );
        let MotionEvaluation::Complete(result) = result else {
            panic!("expected a complete motion");
        };
        assert_eq!(result.target, SnapshotCell { x: 3, y: 0 });
        assert_eq!(requested, vec![(1, true), (3, true)]);
        assert_eq!(mode.cursor.x, 0);
    }

    #[test]
    fn counted_word_motion_reuses_rows_and_produces_yank_metadata() {
        let mode = copy_at(0, 0, 1);
        let mut reads = 0;
        let result = mode.evaluate_motion(
            Motion::ForwardWordEnd,
            2,
            true,
            None,
            |_| {
                reads += 1;
                Some(row("foo bar baz", false))
            },
            |cell, _| Some(cell),
        );
        let MotionEvaluation::Complete(result) = result else {
            panic!("expected a complete motion");
        };
        assert_eq!(result.target.x, 6);
        assert_eq!(result.kind, RangeKind::Characterwise);
        assert_eq!(result.endpoint, Endpoint::Inclusive);
        assert_eq!(reads, 1);
        let (range, linewise) = mode
            .yank_motion_range(result, |_| Some(row("foo bar baz", false)))
            .unwrap();
        assert_eq!(range.end.x, 6);
        assert!(!linewise);
    }

    #[test]
    fn word_scanning_distinguishes_soft_wraps_and_hard_breaks() {
        let rows = [row("foo", true), row("bar", false), row("baz", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        assert_eq!(
            copy_at(0, 0, 3).text_target(Motion::ForwardWord, read),
            Some(SnapshotCell { x: 0, y: 2 })
        );
        assert_eq!(
            copy_at(1, 1, 3).text_target(Motion::BackwardWord, read),
            Some(SnapshotCell { x: 0, y: 0 })
        );
        assert_eq!(
            copy_at(0, 2, 3).text_target(Motion::PreviousWordEnd, read),
            Some(SnapshotCell { x: 2, y: 1 })
        );
        let rows = [row("foo", false), row("", false), row("bar", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        assert_eq!(
            copy_at(0, 0, 3).text_target(Motion::ForwardWord, read),
            Some(SnapshotCell { x: 0, y: 2 })
        );
        assert_eq!(
            copy_at(0, 2, 3).text_target(Motion::BackwardWord, read),
            Some(SnapshotCell { x: 0, y: 0 })
        );
    }

    #[test]
    fn newer_yank_replaces_highlight_and_outlives_old_expiry() {
        let mut mode = CopyMode::default();
        let first = SnapshotCellRange {
            start: SnapshotCell { x: 1, y: 2 },
            end: SnapshotCell { x: 3, y: 2 },
        };
        let second = SnapshotCellRange {
            start: SnapshotCell { x: 5, y: 6 },
            end: SnapshotCell { x: 7, y: 6 },
        };
        let started = Instant::now();
        mode.highlight_yank(first, started);
        mode.highlight_yank(second, started + Duration::from_millis(100));
        assert!(!mode.clear_expired_yank_highlight(started + YANK_HIGHLIGHT_DURATION));
        assert_eq!(mode.yank_highlight_range(), Some(second));
        assert_eq!(
            mode.yank_highlight_remaining(started + YANK_HIGHLIGHT_DURATION),
            Some(Duration::from_millis(100))
        );
        assert!(mode.clear_expired_yank_highlight(started + Duration::from_millis(350)));
        assert!(mode.yank_highlight_range().is_none());
    }

    #[test]
    fn visual_range_includes_anchor_and_orders_reverse_motion() {
        let mut mode = CopyMode {
            cursor: SnapshotCell { x: 4, y: 8 },
            ..Default::default()
        };
        mode.toggle_visual(VisualMode::Characterwise);
        assert_eq!(
            mode.visual_range_to(mode.cursor).unwrap().start,
            mode.cursor
        );
        assert_eq!(mode.visual_range_to(mode.cursor).unwrap().end, mode.cursor);
        mode.moved(Motion::Up, SnapshotCell { x: 2, y: 5 });
        assert_eq!(
            mode.visual_range_to(mode.cursor).unwrap().start,
            mode.cursor
        );
        assert_eq!(
            mode.visual_range_to(mode.cursor).unwrap().end,
            SnapshotCell { x: 4, y: 8 }
        );
        mode.toggle_visual(VisualMode::Characterwise);
        assert!(mode.visual_range_to(mode.cursor).is_none());
    }

    #[test]
    fn linewise_visual_range_expands_wrapped_lines_in_both_directions() {
        let rows = [
            row("before", false),
            row("first", true),
            row("last", false),
            row("middle", false),
            row("next", true),
            row("end", false),
        ];
        let mut mode = copy_at(3, 2, rows.len() as u64);
        mode.toggle_visual(VisualMode::Linewise);
        let range = mode
            .linewise_visual_range_to(SnapshotCell { x: 2, y: 4 }, |y| {
                rows.get(y as usize).cloned()
            })
            .unwrap();
        assert_eq!(range.start, SnapshotCell { x: 0, y: 1 });
        assert_eq!(range.end, SnapshotCell { x: 19, y: 5 });

        let range = mode
            .linewise_visual_range_to(SnapshotCell { x: 2, y: 0 }, |y| {
                rows.get(y as usize).cloned()
            })
            .unwrap();
        assert_eq!(range.start, SnapshotCell { x: 0, y: 0 });
        assert_eq!(range.end, SnapshotCell { x: 19, y: 2 });

        mode.toggle_visual(VisualMode::Characterwise);
        assert_eq!(mode.visual_anchor, Some(SnapshotCell { x: 3, y: 2 }));
        mode.toggle_visual(VisualMode::Characterwise);
        assert!(mode.visual_anchor.is_none());
    }

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
    fn yank_word_objects_distinguish_inner_around_and_punctuation() {
        let row = row("foo.bar  baz", false);
        let mode = copy_at(5, 0, 1);
        let read = |_| Some(row.clone());
        assert_eq!(
            mode.word_object_range(1, WordObject::Inner, false, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 4, y: 0 },
                end: SnapshotCell { x: 6, y: 0 },
            })
        );
        assert_eq!(
            mode.word_object_range(1, WordObject::Around, false, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 4, y: 0 },
                end: SnapshotCell { x: 8, y: 0 },
            })
        );
        let mode = copy_at(3, 0, 1);
        assert_eq!(
            mode.word_object_range(1, WordObject::Inner, false, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 3, y: 0 },
                end: SnapshotCell { x: 3, y: 0 },
            })
        );
        let mode = copy_at(7, 0, 1);
        assert_eq!(
            mode.word_object_range(1, WordObject::Around, false, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 7, y: 0 },
                end: SnapshotCell { x: 11, y: 0 },
            })
        );
    }

    #[test]
    fn yank_word_object_spans_soft_wrap_and_counted_words() {
        let rows = [row("foo", true), row("bar baz", false)];
        let mode = copy_at(1, 1, 2);
        let read = |y: u64| rows.get(y as usize).cloned();
        assert_eq!(
            mode.word_object_range(1, WordObject::Inner, false, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 0 },
                end: SnapshotCell { x: 2, y: 1 },
            })
        );
        assert_eq!(
            mode.word_object_range(2, WordObject::Inner, false, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 0 },
                end: SnapshotCell { x: 6, y: 1 },
            })
        );
    }

    #[test]
    fn yank_word_object_uses_cell_columns_for_multibyte_text() {
        let row = TextRow {
            text: "a.界 b".into(),
            columns: vec![0, 1, 2, 2, 2, 4, 5],
            wraps_to_next: false,
        };
        let mode = copy_at(2, 0, 1);
        let range = mode
            .word_object_range(1, WordObject::Around, false, |_| Some(row.clone()))
            .unwrap();
        assert_eq!(range.start, SnapshotCell { x: 2, y: 0 });
        assert_eq!(range.end, SnapshotCell { x: 4, y: 0 });
    }

    #[test]
    fn yank_big_word_includes_punctuation() {
        let row = row("foo.bar baz", false);
        let mode = copy_at(4, 0, 1);
        let read = |_| Some(row.clone());
        assert_eq!(
            mode.word_object_range(1, WordObject::Inner, true, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 0 },
                end: SnapshotCell { x: 6, y: 0 },
            })
        );
        assert_eq!(
            mode.word_object_range(1, WordObject::Around, true, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 0 },
                end: SnapshotCell { x: 7, y: 0 },
            })
        );
    }

    #[test]
    fn yank_quotes_and_nested_brackets_map_to_cells() {
        let rows = [row("prefix (a [界]", true), row(" b) 'one \\' two'", false)];
        let mode = copy_at(10, 0, 2);
        let read = |y: u64| rows.get(y as usize).cloned();
        assert_eq!(
            mode.delimiter_object_range(WordObject::Around, '(', ')', read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 7, y: 0 },
                end: SnapshotCell { x: 2, y: 1 },
            })
        );
        let mode = copy_at(9, 1, 2);
        assert_eq!(
            mode.quote_object_range(WordObject::Around, '\'', read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 4, y: 1 },
                end: SnapshotCell { x: 15, y: 1 },
            })
        );
    }

    #[test]
    fn yank_word_motions_are_exclusive_and_line_motions_use_logical_lines() {
        let rows = [row("foo bar", false), row("wrap", true), row("end", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        let mode = copy_at(1, 0, 3);
        assert_eq!(
            mode.yank_motion_range(
                Motion::ForwardWord.result(SnapshotCell { x: 4, y: 0 }),
                read
            ),
            Some((
                SnapshotCellRange {
                    start: SnapshotCell { x: 1, y: 0 },
                    end: SnapshotCell { x: 3, y: 0 },
                },
                false
            ))
        );
        assert_eq!(
            mode.yank_motion_range(Motion::Down.result(SnapshotCell { x: 1, y: 2 }), read),
            Some((
                SnapshotCellRange {
                    start: SnapshotCell { x: 0, y: 0 },
                    end: SnapshotCell { x: 19, y: 2 },
                },
                true
            ))
        );
    }

    #[test]
    fn yank_command_objects_use_regex_prompt_boundaries() {
        let rows = [
            row("PS C:\\> first", false),
            row("output", false),
            row("12:34> second", false),
            row("more", false),
        ];
        let mode = copy_at(2, 1, 4);
        let boundaries =
            super::find_prompt_boundaries(rows.len() as u64, |y| rows.get(y as usize).cloned())
                .unwrap();
        assert_eq!(boundaries, [0, 2]);
        assert_eq!(
            mode.command_object_range(WordObject::Inner, 1, &boundaries),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 1 },
                end: SnapshotCell { x: 19, y: 1 },
            })
        );
        assert_eq!(
            mode.command_object_range(WordObject::Around, 2, &boundaries),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 0 },
                end: SnapshotCell { x: 19, y: 3 },
            })
        );
    }

    #[test]
    fn command_navigation_skips_current_prompt_counts_and_stops_at_edges() {
        let mode = copy_at(7, 10, 40);
        let boundaries = [0, 10, 20, 30];
        assert_eq!(
            mode.command_target(false, 1, &boundaries),
            Some(SnapshotCell { x: 0, y: 0 })
        );
        assert_eq!(
            mode.command_target(true, 1, &boundaries),
            Some(SnapshotCell { x: 0, y: 20 })
        );
        assert_eq!(
            mode.command_target(true, 2, &boundaries),
            Some(SnapshotCell { x: 0, y: 30 })
        );
        assert_eq!(mode.command_target(false, 2, &boundaries), None);
        assert_eq!(mode.command_target(true, 3, &boundaries), None);
        assert_eq!(mode.command_target(true, 1, &[]), None);
        let mode = copy_at(7, 15, 40);
        assert_eq!(
            mode.command_target(false, 1, &boundaries),
            Some(SnapshotCell { x: 0, y: 10 })
        );
    }

    #[test]
    fn prompt_patterns_match_logical_lines_without_osc_markers() {
        let rows = [
            row("ordinary output", false),
            row("› command", true),
            row(" continued input", false),
            row("────────────────────", false),
        ];
        assert_eq!(
            super::find_prompt_boundaries(rows.len() as u64, |y| { rows.get(y as usize).cloned() }),
            Some(vec![1, 3])
        );
    }

    #[test]
    fn yank_line_ranges_count_logical_lines_and_dollar_uses_current_row() {
        let rows = [
            row("before", false),
            row("wrapped", true),
            row("end", false),
            row("next", false),
        ];
        let mode = copy_at(1, 1, rows.len() as u64);
        let read = |y: u64| rows.get(y as usize).cloned();
        assert_eq!(
            mode.yank_lines_range(2, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 1 },
                end: SnapshotCell { x: 19, y: 3 },
            })
        );
        assert_eq!(
            mode.yank_to_row_end_range(1, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 1, y: 1 },
                end: SnapshotCell { x: 6, y: 1 },
            })
        );
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
    fn word_lookup_preserves_search_and_object_punctuation_rules() {
        let rows = [row("foo ... bar", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        let mode = copy_at(5, 0, 1);
        assert_eq!(mode.word_under_cursor(read).as_deref(), Some("bar"));
        assert_eq!(
            mode.word_object_range(1, WordObject::Inner, false, read),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 4, y: 0 },
                end: SnapshotCell { x: 6, y: 0 },
            })
        );
        assert_eq!(
            copy_at(3, 0, 1).word_under_cursor(read).as_deref(),
            Some("bar")
        );
        assert_eq!(
            copy_at(10, 0, 1).word_under_cursor(read).as_deref(),
            Some("bar")
        );

        let rows = [row("foo", true), row("...", true), row("bar", false)];
        let read = |y: u64| rows.get(y as usize).cloned();
        // A keyword on another physical row is not a search fallback.
        assert_eq!(copy_at(1, 1, 3).word_under_cursor(read), None);
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

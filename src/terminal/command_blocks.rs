//! Copy-mode command objects (`ic`/`ac`) and prompt jumps over the first rows
//! of each command, as `Patterns::boundaries` finds them in a snapshot.

use crate::config::prompts::Patterns;
use crate::ghostty_ffi::{self, Host, SnapshotCell, SnapshotCellRange};
use nfm_egui::copy_mode::{CopyMode, CopySelection, TextRow, WordObject};
use std::cell::RefCell;
use std::rc::Rc;

/// The copy-mode text object for commands: `yic`, `yac`, `vic`, `vac`.
pub(super) const OBJECT_KEY: char = 'c';

/// Copy mode with the command text object registered.
pub(super) fn copy_mode() -> CopyMode {
    let mut mode = CopyMode::default();
    mode.set_host_objects(&[OBJECT_KEY])
        .expect("the command key is not a built-in text object");
    mode
}

/// The first row of each command in a snapshot, read once per generation.
#[derive(Default)]
pub(super) struct Boundaries(RefCell<Option<(u64, Rc<[u64]>)>>);

impl Boundaries {
    pub(super) fn get(
        &self,
        host: *mut Host,
        generation: u64,
        rows: u64,
        patterns: &Patterns,
    ) -> Option<Rc<[u64]>> {
        if let Some((cached, boundaries)) = &*self.0.borrow()
            && *cached == generation
        {
            return Some(boundaries.clone());
        }
        let boundaries: Rc<[u64]> = patterns
            .boundaries(rows, |row| {
                let data = ghostty_ffi::read_snapshot_row(host, generation, row, false)?;
                let prompt = data.prompt;
                Some((
                    TextRow {
                        text: String::from_utf8(data.text).ok()?,
                        columns: data.columns,
                        wraps_to_next: data.wraps_to_next,
                    },
                    prompt,
                ))
            })?
            .into();
        *self.0.borrow_mut() = Some((generation, boundaries.clone()));
        Some(boundaries)
    }

    pub(super) fn clear(&self) {
        *self.0.borrow_mut() = None;
    }
}

/// What a copy-mode document needs to resolve the command text object.
pub(super) struct Commands<'a> {
    pub(super) boundaries: &'a Boundaries,
    pub(super) patterns: &'a Patterns,
    pub(super) columns: u32,
}

impl Commands<'_> {
    /// The linewise selection for `ic`/`ac` at `cursor_row`, if it is in a command.
    pub(super) fn selection(
        &self,
        host: *mut Host,
        generation: u64,
        rows: u64,
        object: WordObject,
        count: u32,
        cursor_row: u64,
    ) -> Result<Option<CopySelection>, String> {
        let boundaries = self
            .boundaries
            .get(host, generation, rows, self.patterns)
            .ok_or("Unable to read command boundaries")?;
        Ok(
            object_range(&boundaries, cursor_row, rows, self.columns, object, count).map(|range| {
                CopySelection {
                    range,
                    linewise: true,
                    trim: true,
                    rectangle: false,
                }
            }),
        )
    }
}

/// The rows of `count` commands starting with the one containing `cursor_row`:
/// `Around` includes the prompt row and `Inner` starts after it.
pub(super) fn object_range(
    boundaries: &[u64],
    cursor_row: u64,
    rows: u64,
    columns: u32,
    object: WordObject,
    count: u32,
) -> Option<SnapshotCellRange> {
    let index = boundaries
        .partition_point(|&y| y <= cursor_row)
        .checked_sub(1)?;
    let prompt = boundaries[index];
    let mut end = rows.checked_sub(1)?;
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
            x: columns.checked_sub(1)?,
            y: end,
        },
    })
}

/// The prompt `count` commands before or after `cursor_row`. Moving back from
/// inside a command first reaches its own prompt.
pub(super) fn jump_target(
    boundaries: &[u64],
    cursor_row: u64,
    forward: bool,
    count: u32,
) -> Option<SnapshotCell> {
    let steps = usize::try_from(count.max(1)).ok()?;
    let index = if forward {
        boundaries
            .partition_point(|&row| row <= cursor_row)
            .checked_add(steps - 1)?
    } else {
        boundaries
            .partition_point(|&row| row < cursor_row)
            .checked_sub(steps)?
    };
    Some(SnapshotCell {
        x: 0,
        y: *boundaries.get(index)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_objects_span_from_the_prompt_to_the_next_one() {
        let range = |object, count| object_range(&[0, 2], 1, 4, 20, object, count);
        assert_eq!(
            range(WordObject::Inner, 1),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 1 },
                end: SnapshotCell { x: 19, y: 1 },
            })
        );
        assert_eq!(
            range(WordObject::Around, 2),
            Some(SnapshotCellRange {
                start: SnapshotCell { x: 0, y: 0 },
                end: SnapshotCell { x: 19, y: 3 },
            })
        );
        // A prompt with no output has no inner rows, and there is nothing
        // before the first prompt.
        assert_eq!(object_range(&[0, 1], 0, 4, 20, WordObject::Inner, 1), None);
        assert_eq!(object_range(&[2], 1, 4, 20, WordObject::Around, 1), None);
    }

    #[test]
    fn jumps_skip_the_current_prompt_count_and_stop_at_edges() {
        let boundaries = [0, 10, 20, 30];
        let target = |row, forward, count| jump_target(&boundaries, row, forward, count);
        assert_eq!(target(10, false, 1), Some(SnapshotCell { x: 0, y: 0 }));
        assert_eq!(target(10, true, 1), Some(SnapshotCell { x: 0, y: 20 }));
        assert_eq!(target(10, true, 2), Some(SnapshotCell { x: 0, y: 30 }));
        assert_eq!(target(10, false, 2), None);
        assert_eq!(target(10, true, 3), None);
        assert_eq!(jump_target(&[], 10, true, 1), None);
        assert_eq!(target(15, false, 1), Some(SnapshotCell { x: 0, y: 10 }));
    }
}

use crate::ffi::{SnapshotCell, SnapshotView};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Motion {
    Left,
    Down,
    Up,
    Right,
    ViewportTop,
    ViewportMiddle,
    ViewportBottom,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
}

impl Motion {
    pub(crate) fn needs_view(self) -> bool {
        !matches!(self, Self::Left | Self::Down | Self::Up | Self::Right)
    }
}

#[derive(Default)]
pub(crate) struct CopyMode {
    pub(crate) generation: Option<u64>,
    pub(crate) cursor: SnapshotCell,
    preferred_x: u32,
    columns: u32,
    rows: u64,
}

impl CopyMode {
    pub(crate) fn start(&mut self, generation: u64, state: SnapshotView) {
        self.generation = Some(generation);
        self.cursor = state.cursor;
        self.preferred_x = state.cursor.x;
        self.columns = state.columns;
        self.rows = state.total_rows;
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
        }
        Some((cell, snap_right))
    }

    pub(crate) fn moved(&mut self, motion: Motion, cell: SnapshotCell) {
        self.cursor = cell;
        if matches!(motion, Motion::Left | Motion::Right) {
            self.preferred_x = cell.x;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

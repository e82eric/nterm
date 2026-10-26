use std::ffi::{c_char, c_void};

unsafe extern "C" {
    pub(crate) fn nterm_host_apply_settings(
        host: *mut Host,
        name: *const c_char,
        family: *const c_char,
        size: f32,
        error: *mut c_char,
        capacity: usize,
    ) -> bool;
    fn ghostty_last_error() -> *const c_char;
    pub(crate) fn ghostty_config_theme_directories(
        userdata: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const c_char),
    ) -> bool;
    pub(crate) fn nterm_host_apply_theme(
        host: *mut Host,
        name: *const c_char,
        error: *mut c_char,
        capacity: usize,
    ) -> bool;
}

pub(crate) fn native_error(operation: &str) -> String {
    let cause = unsafe { ghostty_last_error() };
    if cause.is_null() {
        operation.into()
    } else {
        format!(
            "{operation}: {}",
            unsafe { std::ffi::CStr::from_ptr(cause) }.to_string_lossy()
        )
    }
}

#[repr(C)]
pub(crate) struct Host(c_void);

#[repr(C)]
#[derive(Default)]
pub(crate) struct FontVariation {
    pub(crate) tag: u32,
    pub(crate) value: f32,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct FontData {
    pub(crate) data: *mut u8,
    pub(crate) len: usize,
    pub(crate) face_index: u32,
    pub(crate) variation_count: u32,
    pub(crate) variations: [FontVariation; 32],
}

#[repr(C)]
pub(crate) struct SurfaceSize {
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    pub(crate) cell_width_px: u32,
    pub(crate) cell_height_px: u32,
}

/// A finished terminal frame. `texture` is an exported frame of our own
/// dispatch table (gpu::bridge), which we release there.
#[repr(C)]
#[derive(Default)]
pub(crate) struct GpuFrame {
    pub(crate) texture: usize,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

// Shared copy-mode types have a C layout matching the native snapshot ABI.
pub(crate) use nfm_egui::copy_mode::{
    CellPosition as SnapshotCell, CellRange as SnapshotCellRange, View as SnapshotView,
};

/// Stable snapshot text position, independent of presentation width.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SnapshotLogicalPosition {
    pub line: u64,
    pub byte: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SnapshotLogicalRange {
    pub start: SnapshotLogicalPosition,
    pub end: SnapshotLogicalPosition,
}

/// One run in a frozen terminal's resize translation map.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReflowSegment {
    pub old_row: u64,
    pub new_row: u64,
    pub old_column: u32,
    pub new_column: u32,
    pub count: u32,
    pub step: u32,
}

/// Owns Zig's immutable map buffers until Drop. Never borrows terminal pages.
#[repr(C)]
#[derive(Default)]
pub(crate) struct SnapshotReflowMap {
    pub handle: *mut c_void,
    pub segments: *const ReflowSegment,
    pub segment_count: usize,
    pub row_offsets: *const u32,
    pub old_rows: u64,
    pub new_rows: u64,
    pub old_columns: u32,
    pub new_columns: u32,
}

impl SnapshotReflowMap {
    pub(crate) fn translate(&self, cell: SnapshotCell) -> Option<SnapshotCell> {
        if cell.y >= self.old_rows
            || cell.x >= self.old_columns
            || self.row_offsets.is_null()
            || self.segments.is_null()
        {
            return None;
        }
        let row = usize::try_from(cell.y).ok()?;
        // The C API owns an immutable old_rows+1 offset array and segment_count
        // segment array for this value's entire lifetime.
        let (mut lo, mut hi) = unsafe {
            (
                *self.row_offsets.add(row) as usize,
                *self.row_offsets.add(row + 1) as usize,
            )
        };
        if lo > hi || hi > self.segment_count {
            return None;
        }
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let segment = unsafe { *self.segments.add(mid) };
            if cell.x < segment.old_column {
                hi = mid;
            } else if cell.x - segment.old_column >= segment.count {
                lo = mid + 1;
            } else {
                let x = segment
                    .new_column
                    .checked_add((cell.x - segment.old_column).checked_mul(segment.step)?)?;
                return (x < self.new_columns && segment.new_row < self.new_rows).then_some(
                    SnapshotCell {
                        x,
                        y: segment.new_row,
                    },
                );
            }
        }
        None
    }
}

impl Drop for SnapshotReflowMap {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ghostty_snapshot_reflow_map_free(self) };
        }
    }
}

unsafe extern "C" {
    fn ghostty_snapshot_reflow_map_free(map: *mut SnapshotReflowMap);
}

#[cfg(all(test, target_pointer_width = "64"))]
mod copy_mode_abi_tests {
    use super::{SnapshotCell, SnapshotCellRange, SnapshotView};
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn shared_copy_types_match_native_snapshot_layout() {
        assert_eq!(size_of::<super::ReflowSegment>(), 32);
        assert_eq!(offset_of!(super::ReflowSegment, old_column), 16);
        assert_eq!(offset_of!(super::ReflowSegment, step), 28);
        assert_eq!(size_of::<super::SnapshotReflowMap>(), 56);
        assert_eq!(offset_of!(super::SnapshotReflowMap, row_offsets), 24);
        assert_eq!(offset_of!(super::SnapshotReflowMap, old_columns), 48);
        assert_eq!(size_of::<SnapshotCell>(), 16);
        assert_eq!(align_of::<SnapshotCell>(), 8);
        assert_eq!(offset_of!(SnapshotCell, x), 0);
        assert_eq!(offset_of!(SnapshotCell, y), 8);
        assert_eq!(size_of::<SnapshotCellRange>(), 32);
        assert_eq!(offset_of!(SnapshotCellRange, end), 16);
        assert_eq!(size_of::<super::SnapshotLogicalPosition>(), 16);
        assert_eq!(size_of::<super::SnapshotLogicalRange>(), 32);
        assert_eq!(offset_of!(super::SnapshotLogicalRange, end), 16);
        assert_eq!(size_of::<SnapshotView>(), 40);
        assert_eq!(offset_of!(SnapshotView, cursor), 0);
        assert_eq!(offset_of!(SnapshotView, viewport_top), 16);
        assert_eq!(offset_of!(SnapshotView, total_rows), 24);
        assert_eq!(offset_of!(SnapshotView, viewport_rows), 32);
        assert_eq!(offset_of!(SnapshotView, columns), 36);
        assert_eq!(size_of::<super::SnapshotStyleRun>(), 24);
        assert_eq!(offset_of!(super::SnapshotStyleRun, flags), 20);
        assert_eq!(size_of::<super::SnapshotStyleRuns>(), 16);
    }

    #[test]
    fn reflow_map_indexes_split_rows_and_collapsed_padding() {
        use super::{ReflowSegment, SnapshotReflowMap};
        let segments = [
            ReflowSegment {
                old_row: 0,
                new_row: 0,
                old_column: 0,
                new_column: 0,
                count: 6,
                step: 1,
            },
            ReflowSegment {
                old_row: 0,
                new_row: 1,
                old_column: 6,
                new_column: 0,
                count: 6,
                step: 1,
            },
            ReflowSegment {
                old_row: 1,
                new_row: 2,
                old_column: 0,
                new_column: 0,
                count: 3,
                step: 1,
            },
            ReflowSegment {
                old_row: 1,
                new_row: 2,
                old_column: 3,
                new_column: 2,
                count: 9,
                step: 0,
            },
        ];
        let offsets = [0, 2, 4];
        let map = SnapshotReflowMap {
            segments: segments.as_ptr(),
            segment_count: segments.len(),
            row_offsets: offsets.as_ptr(),
            old_rows: 2,
            new_rows: 3,
            old_columns: 12,
            new_columns: 6,
            ..Default::default()
        };
        assert_eq!(
            map.translate(SnapshotCell { x: 10, y: 0 }),
            Some(SnapshotCell { x: 4, y: 1 })
        );
        assert_eq!(
            map.translate(SnapshotCell { x: 11, y: 1 }),
            Some(SnapshotCell { x: 2, y: 2 })
        );
        assert_eq!(map.translate(SnapshotCell { x: 12, y: 0 }), None);
        assert_eq!(map.translate(SnapshotCell { x: 0, y: 2 }), None);
    }
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct SnapshotSelectionText {
    pub(crate) text: *const u8,
    pub(crate) len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SnapshotDecorationStyle {
    pub(crate) foreground_rgb: u32,
    pub(crate) background_rgb: u32,
    pub(crate) flags: u32,
}

#[repr(C)]
pub(crate) struct SnapshotDecorationLayer {
    pub(crate) ranges: *const SnapshotCellRange,
    pub(crate) range_count: usize,
    pub(crate) style: SnapshotDecorationStyle,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct SnapshotViewportOptions {
    pub(crate) reveal: *const SnapshotCell,
    pub(crate) padding_rows: u32,
    pub(crate) top_row: *const u64,
}

#[repr(C)]
pub(crate) struct SnapshotCellText {
    pub(crate) cell: SnapshotCell,
    pub(crate) codepoint: u32,
    pub(crate) style: SnapshotDecorationStyle,
    pub(crate) bold: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SnapshotTextPoint {
    pub(crate) x: u16,
    pub(crate) y: u32,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct SnapshotTextChunk {
    pub(crate) text: *const u8,
    pub(crate) points: *const SnapshotTextPoint,
    pub(crate) len: usize,
    pub(crate) row_base: u64,
    pub(crate) start_y: u16,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct SnapshotTextRow {
    pub(crate) text: *const u8,
    pub(crate) columns: *const u32,
    pub(crate) len: usize,
    pub(crate) wraps_to_next: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SnapshotStyleRun {
    pub start: u32,
    pub end: u32,
    pub foreground: u32,
    pub background: u32,
    pub underline_color: u32,
    pub flags: u8,
    pub underline: u8,
    pub reserved: u16,
}
#[repr(C)]
#[derive(Default)]
pub(crate) struct SnapshotStyleRuns {
    pub runs: *mut SnapshotStyleRun,
    pub len: usize,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct LiveCursorLine {
    pub(crate) text: *const u8,
    pub(crate) len: usize,
    pub(crate) cursor_byte: usize,
    pub(crate) cursor_column: u32,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct LiveCursorPosition {
    pub(crate) column: u32,
    pub(crate) row: u32,
}

unsafe extern "C" {
    pub(crate) fn nterm_host_read_font(host: *mut Host, bold: bool, result: *mut FontData) -> bool;
    pub(crate) fn ghostty_font_data_free(result: *mut FontData);
    pub(crate) fn nterm_host_surface_size(host: *mut Host) -> SurfaceSize;
    pub(crate) fn nterm_host_create(
        argc: usize,
        argv: *mut *mut c_char,
        userdata: *mut c_void,
        window: *mut c_void,
        driver: *const c_void,
        scale: f64,
        picker_endpoint: *const c_char,
        picker_token: *const c_char,
        error_buffer: *mut c_char,
        error_capacity: usize,
    ) -> *mut Host;
    pub(crate) fn nterm_host_destroy(host: *mut Host);
    pub(crate) fn nterm_host_close_process(host: *mut Host);
    pub(crate) fn nterm_host_create_child(
        parent: *mut Host,
        userdata: *mut c_void,
        window: *mut c_void,
        driver: *const c_void,
        scale: f64,
        picker_endpoint: *const c_char,
        picker_token: *const c_char,
        command: *const c_char,
        cwd: *const c_char,
        error_buffer: *mut c_char,
        error_capacity: usize,
    ) -> *mut Host;
    pub(crate) fn nterm_host_tick(host: *mut Host);
    pub(crate) fn nterm_host_shell_pid(host: *mut Host) -> u64;
    pub(crate) fn nterm_host_shell_creation_time(host: *mut Host) -> u64;
    pub(crate) fn nterm_host_resize(host: *mut Host, width: u32, height: u32, scale: f64);
    pub(crate) fn nterm_host_focus(host: *mut Host, focused: bool);
    pub(crate) fn nterm_host_take_frame(host: *mut Host, frame: *mut GpuFrame) -> bool;
    pub(crate) fn nterm_host_key(
        host: *mut Host,
        action: i32,
        mods: i32,
        usb: u32,
        unshifted: u32,
        text: *const c_char,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_begin(host: *mut Host, snapshot_generation: *mut u64)
    -> bool;
    pub(crate) fn nterm_host_selection_text(
        host: *mut Host,
        result: *mut SnapshotSelectionText,
    ) -> bool;
    pub(crate) fn nterm_host_selection_text_free(result: *mut SnapshotSelectionText);
    pub(crate) fn nterm_host_snapshot_end(host: *mut Host, snapshot_generation: u64) -> bool;
    pub(crate) fn nterm_host_snapshot_view(
        host: *mut Host,
        generation: u64,
        view: *mut SnapshotView,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_resolve_cell(
        host: *mut Host,
        generation: u64,
        requested: SnapshotCell,
        snap_right: bool,
        actual: *mut SnapshotCell,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_resize(
        host: *mut Host,
        generation: u64,
        result: *mut SnapshotReflowMap,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_selection_text(
        host: *mut Host,
        generation: u64,
        range: SnapshotCellRange,
        trim: bool,
        rectangle: bool,
        result: *mut SnapshotSelectionText,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_selection_text_free(result: *mut SnapshotSelectionText);
    pub(crate) fn nterm_host_snapshot_cursor_new(host: *mut Host, generation: u64) -> *mut c_void;
    pub(crate) fn nterm_host_snapshot_cursor_free(cursor: *mut c_void);
    pub(crate) fn nterm_host_snapshot_cursor_reset(cursor: *mut c_void) -> bool;
    pub(crate) fn nterm_host_snapshot_cursor_next(
        cursor: *mut c_void,
        chunk: *mut SnapshotTextChunk,
    ) -> i32;
    pub(crate) fn nterm_host_snapshot_text_row(
        host: *mut Host,
        generation: u64,
        row: u64,
        result: *mut SnapshotTextRow,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_text_row_free(result: *mut SnapshotTextRow);
    pub(crate) fn nterm_host_snapshot_style_runs(
        host: *mut Host,
        generation: u64,
        row: u64,
        result: *mut SnapshotStyleRuns,
    ) -> bool;
    pub(crate) fn ghostty_snapshot_style_runs_free(result: *mut SnapshotStyleRuns);
    pub(crate) fn nterm_host_snapshot_logical_position(
        host: *mut Host,
        generation: u64,
        cell: SnapshotCell,
        result: *mut SnapshotLogicalPosition,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_resolve_logical_range(
        host: *mut Host,
        generation: u64,
        range: SnapshotLogicalRange,
        result: *mut SnapshotCellRange,
    ) -> bool;
    pub(crate) fn nterm_host_live_cursor_line(host: *mut Host, result: *mut LiveCursorLine)
    -> bool;
    pub(crate) fn nterm_host_live_cursor_line_free(result: *mut LiveCursorLine);
    pub(crate) fn nterm_host_live_cursor_position(
        host: *mut Host,
        result: *mut LiveCursorPosition,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_apply_decorations(
        host: *mut Host,
        generation: u64,
        layers: *const SnapshotDecorationLayer,
        count: usize,
        viewport: *const SnapshotViewportOptions,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_decorations_need_refresh(
        host: *mut Host,
        generation: u64,
    ) -> bool;
    pub(crate) fn nterm_host_snapshot_apply_cell_text(
        host: *mut Host,
        generation: u64,
        layers: *const SnapshotDecorationLayer,
        count: usize,
        text: *const SnapshotCellText,
        text_count: usize,
        viewport: *const SnapshotViewportOptions,
    ) -> bool;
    pub(crate) fn nterm_host_text(host: *mut Host, text: *const c_char, len: usize);
    pub(crate) fn nterm_host_preedit(host: *mut Host, text: *const c_char, len: usize);
    pub(crate) fn nterm_host_mouse_pos(host: *mut Host, x: f64, y: f64, mods: i32);
    pub(crate) fn nterm_host_mouse_button(host: *mut Host, state: i32, button: i32, mods: i32);
    pub(crate) fn nterm_host_mouse_scroll(host: *mut Host, x: f64, y: f64, mods: i32);
}

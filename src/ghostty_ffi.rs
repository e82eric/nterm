use std::ffi::{c_char, c_void};

unsafe extern "C" {
    fn ghostty_last_error() -> *const c_char;
    pub(crate) fn ghostty_config_theme_directories(
        userdata: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const c_char),
    ) -> bool;
    pub(crate) fn ghostty_egui_host_apply_theme(
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

#[repr(C)]
#[derive(Default)]
pub(crate) struct GlFrame {
    pub(crate) handle: *mut c_void,
    pub(crate) texture: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) frame_generation: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SnapshotCell {
    pub(crate) x: u32,
    pub(crate) y: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SnapshotView {
    pub(crate) cursor: SnapshotCell,
    pub(crate) viewport_top: u64,
    pub(crate) total_rows: u64,
    pub(crate) viewport_rows: u32,
    pub(crate) columns: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SnapshotCellRange {
    pub(crate) start: SnapshotCell,
    pub(crate) end: SnapshotCell,
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
#[derive(Default)]
pub(crate) struct SnapshotStyledRows {
    pub(crate) text: *const u8,
    pub(crate) len: usize,
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
    pub(crate) fn ghostty_egui_host_read_font(
        host: *mut Host,
        bold: bool,
        result: *mut FontData,
    ) -> bool;
    pub(crate) fn ghostty_font_data_free(result: *mut FontData);
    pub(crate) fn ghostty_egui_host_surface_size(host: *mut Host) -> SurfaceSize;
    pub(crate) fn ghostty_egui_host_create(
        argc: usize,
        argv: *mut *mut c_char,
        userdata: *mut c_void,
        window: *mut c_void,
        display: *mut c_void,
        context: *mut c_void,
        scale: f64,
        osc_token: *const c_char,
        error_buffer: *mut c_char,
        error_capacity: usize,
    ) -> *mut Host;
    pub(crate) fn ghostty_egui_host_destroy(host: *mut Host);
    pub(crate) fn ghostty_egui_host_create_floating(
        parent: *mut Host,
        userdata: *mut c_void,
        scale: f64,
        osc_token: *const c_char,
        error_buffer: *mut c_char,
        error_capacity: usize,
    ) -> *mut Host;
    pub(crate) fn ghostty_egui_host_tick(host: *mut Host);
    pub(crate) fn ghostty_egui_host_resize(host: *mut Host, width: u32, height: u32, scale: f64);
    pub(crate) fn ghostty_egui_host_focus(host: *mut Host, focused: bool);
    pub(crate) fn ghostty_egui_host_take_frame(host: *mut Host, frame: *mut GlFrame) -> bool;
    pub(crate) fn ghostty_egui_host_release_frame(frame: *mut c_void);
    pub(crate) fn ghostty_egui_host_key(
        host: *mut Host,
        action: i32,
        mods: i32,
        usb: u32,
        unshifted: u32,
        text: *const c_char,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_begin(
        host: *mut Host,
        snapshot_generation: *mut u64,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_selection_text(
        host: *mut Host,
        result: *mut SnapshotSelectionText,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_selection_text_free(result: *mut SnapshotSelectionText);
    pub(crate) fn ghostty_egui_host_snapshot_end(host: *mut Host, snapshot_generation: u64)
    -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_view(
        host: *mut Host,
        generation: u64,
        view: *mut SnapshotView,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_resolve_cell(
        host: *mut Host,
        generation: u64,
        requested: SnapshotCell,
        snap_right: bool,
        actual: *mut SnapshotCell,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_selection_text(
        host: *mut Host,
        generation: u64,
        range: SnapshotCellRange,
        trim: bool,
        rectangle: bool,
        result: *mut SnapshotSelectionText,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_selection_text_free(
        result: *mut SnapshotSelectionText,
    );
    pub(crate) fn ghostty_egui_host_snapshot_cursor_new(
        host: *mut Host,
        generation: u64,
    ) -> *mut c_void;
    pub(crate) fn ghostty_egui_host_snapshot_cursor_free(cursor: *mut c_void);
    pub(crate) fn ghostty_egui_host_snapshot_cursor_reset(cursor: *mut c_void) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_cursor_next(
        cursor: *mut c_void,
        chunk: *mut SnapshotTextChunk,
    ) -> i32;
    pub(crate) fn ghostty_egui_host_snapshot_text_row(
        host: *mut Host,
        generation: u64,
        row: u64,
        result: *mut SnapshotTextRow,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_text_row_free(result: *mut SnapshotTextRow);
    pub(crate) fn ghostty_egui_host_snapshot_styled_rows(
        host: *mut Host,
        generation: u64,
        first_row: u64,
        row_count: u32,
        result: *mut SnapshotStyledRows,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_styled_rows_free(result: *mut SnapshotStyledRows);
    pub(crate) fn ghostty_egui_host_live_cursor_line(
        host: *mut Host,
        result: *mut LiveCursorLine,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_live_cursor_line_free(result: *mut LiveCursorLine);
    pub(crate) fn ghostty_egui_host_live_cursor_position(
        host: *mut Host,
        result: *mut LiveCursorPosition,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_apply_decorations(
        host: *mut Host,
        generation: u64,
        layers: *const SnapshotDecorationLayer,
        count: usize,
        viewport: *const SnapshotViewportOptions,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_decorations_need_refresh(
        host: *mut Host,
        generation: u64,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_apply_cell_text(
        host: *mut Host,
        generation: u64,
        layers: *const SnapshotDecorationLayer,
        count: usize,
        text: *const SnapshotCellText,
        text_count: usize,
        viewport: *const SnapshotViewportOptions,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_text(host: *mut Host, text: *const c_char, len: usize);
    pub(crate) fn ghostty_egui_host_preedit(host: *mut Host, text: *const c_char, len: usize);
    pub(crate) fn ghostty_egui_host_mouse_pos(host: *mut Host, x: f64, y: f64, mods: i32);
    pub(crate) fn ghostty_egui_host_mouse_button(
        host: *mut Host,
        state: i32,
        button: i32,
        mods: i32,
    );
    pub(crate) fn ghostty_egui_host_mouse_scroll(host: *mut Host, x: f64, y: f64, mods: i32);
}

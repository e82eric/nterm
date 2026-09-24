use std::ffi::{c_char, c_void};

#[repr(C)]
pub(crate) struct Host(c_void);

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
pub(crate) struct SnapshotCopyState {
    pub(crate) cell: SnapshotCell,
    pub(crate) columns: u32,
    pub(crate) rows: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SnapshotCopyView {
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

unsafe extern "C" {
    pub(crate) fn ghostty_egui_host_create(
        argc: usize,
        argv: *mut *mut c_char,
        userdata: *mut c_void,
        window: *mut c_void,
        display: *mut c_void,
        context: *mut c_void,
        scale: f64,
    ) -> *mut Host;
    pub(crate) fn ghostty_egui_host_destroy(host: *mut Host);
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
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_begin(
        host: *mut Host,
        snapshot_generation: *mut u64,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_end(host: *mut Host, snapshot_generation: u64)
    -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_copy_cursor_origin(
        host: *mut Host,
        generation: u64,
        state: *mut SnapshotCopyState,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_copy_view(
        host: *mut Host,
        generation: u64,
        view: *mut SnapshotCopyView,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_copy_cursor_set(
        host: *mut Host,
        generation: u64,
        requested: SnapshotCell,
        snap_right: bool,
        actual: *mut SnapshotCell,
    ) -> bool;
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
    pub(crate) fn ghostty_egui_host_snapshot_set_cell_highlights(
        host: *mut Host,
        generation: u64,
        ranges: *const SnapshotCellRange,
        count: usize,
        selected: *const SnapshotCellRange,
        reveal_selected: bool,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_select_cell_highlight(
        host: *mut Host,
        generation: u64,
        selected: SnapshotCellRange,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_cell_highlights_need_refresh(
        host: *mut Host,
        generation: u64,
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

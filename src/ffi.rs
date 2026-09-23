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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotRange {
    pub(crate) start_x: u32,
    pub(crate) start_y: u32,
    pub(crate) end_x: u32,
    pub(crate) end_y: u32,
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
    pub(crate) fn ghostty_egui_host_snapshot_size(
        host: *mut Host,
        snapshot_generation: u64,
        columns: *mut u32,
        screen_rows: *mut u32,
        total_rows: *mut u64,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_read_row(
        host: *mut Host,
        snapshot_generation: u64,
        columns: u32,
        row: u32,
        text: *mut *mut c_char,
        len: *mut usize,
    ) -> bool;
    pub(crate) fn ghostty_egui_host_snapshot_free_text(text: *mut c_char);
    pub(crate) fn ghostty_egui_host_snapshot_set_highlights(
        host: *mut Host,
        snapshot_generation: u64,
        ranges: *const SnapshotRange,
        count: usize,
        selected: *const SnapshotRange,
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

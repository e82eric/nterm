//! The GPU context threaded through the UI. Terminal code receives it to
//! create panes and release their frames; only the app and painter use wgpu.
pub(crate) use crate::gpu::bridge::Context;

/// The native window a window's first terminal is created in.
#[derive(Clone, Copy)]
pub(crate) struct PaneWindow {
    pub id: winit::window::WindowId,
    /// The native handle (an HWND on Windows), which the terminal host
    /// subclasses, e.g. to keep Alt+Space from the system menu; null elsewhere.
    pub native: *mut std::ffi::c_void,
}

#[cfg(feature = "vulkan")]
pub use ghostty_egui::gpu::bridge::Context;
#[cfg(not(feature = "vulkan"))]
pub use glow::{COLOR_BUFFER_BIT, Context, HasContext};

#[cfg(feature = "vulkan")]
thread_local! {
    static NATIVE: std::cell::Cell<(*mut std::ffi::c_void, *mut std::ffi::c_void)> = const {
        std::cell::Cell::new((std::ptr::null_mut(), std::ptr::null_mut()))
    };
}
#[cfg(feature = "vulkan")]
pub(crate) fn set_native(window: *mut std::ffi::c_void, driver: *mut std::ffi::c_void) {
    NATIVE.with(|v| v.set((window, driver)));
}
#[cfg(feature = "vulkan")]
pub(crate) fn native_context() -> (*mut std::ffi::c_void, *mut std::ffi::c_void) {
    NATIVE.with(|v| v.get())
}

use crate::app::UserEvent;
#[path = "snapshot_resize.rs"]
mod snapshot_resize;

use crate::ghostty_ffi::{
    self, GlFrame, Host, SnapshotCell, SnapshotCellRange, SnapshotDecorationLayer,
    SnapshotDecorationStyle, SnapshotTextRow, SnapshotView, SnapshotViewportOptions,
};
use crate::graphics as glow;
use crate::input;
use crate::painter::TerminalPainter;
use crate::search::{SearchUi, SnapshotSearchAdapter};
use nfm_egui::copy_mode::{
    CopyAction, CopyMode, CopySelection, CopyTextObject, Motion, TextRow, VisualMode, YankCommand,
    find_prompt_boundaries,
};
#[cfg(test)]
use nfm_egui::copy_mode::{
    ViewportPosition, copy_digit, copy_jump_prefix, copy_motion, copy_motion_with_prefix,
    copy_scroll_direction, copy_text_object_key, copy_viewport_position_key, is_copy_modifier_key,
};
use nfm_egui::quick_select::{self, Purpose as QuickSelectPurpose, QuickSelect};
use std::ffi::{c_char, c_void};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

/// Synchronous copy-controller access; the pane keeps the snapshot alive for
/// this input call. Native pointers are never sent to an asynchronous worker.
struct SnapshotNavigationDocument<'a> {
    host: *mut Host,
    generation: u64,
    rows: u64,
    boundaries: Option<&'a [u64]>,
}
impl nfm_egui::copy_controller::CopyNavigationDocument for SnapshotNavigationDocument<'_> {
    fn text_row(&self, row: u64) -> Result<Option<TextRow>, String> {
        if row >= self.rows {
            return Ok(None);
        }
        TerminalPane::read_snapshot_row_from(self.host, self.generation, row)
            .map(Some)
            .ok_or_else(|| "Unable to read copy-mode snapshot row".into())
    }
    fn resolve_cell(&self, cell: SnapshotCell, right: bool) -> Result<SnapshotCell, String> {
        let mut actual = SnapshotCell::default();
        if unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_resolve_cell(
                self.host,
                self.generation,
                cell,
                right,
                &mut actual,
            )
        } {
            Ok(actual)
        } else {
            Err("Unable to resolve copy-mode snapshot cell".into())
        }
    }
    fn selection_text(&self, selection: &CopySelection) -> Result<String, String> {
        let mut result = ghostty_ffi::SnapshotSelectionText::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_selection_text(
                self.host,
                self.generation,
                selection.range,
                selection.trim,
                selection.rectangle,
                &mut result,
            )
        } {
            return Err("The selected text is no longer available".into());
        }
        let text = if result.len == 0 {
            Ok(String::new())
        } else if result.text.is_null() {
            Err("Unable to read selected text".into())
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(result.text, result.len) };
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|error| error.to_string())
        };
        unsafe { ghostty_ffi::ghostty_egui_host_snapshot_selection_text_free(&mut result) };
        let mut text = text?;
        if selection.linewise {
            text.push('\n');
        }
        Ok(text)
    }
    fn prompt_boundaries(&self, _rows: u64) -> Result<Vec<u64>, String> {
        self.boundaries
            .map(|rows| rows.to_vec())
            .ok_or_else(|| "Command boundaries unavailable".into())
    }
}

struct WakeState {
    #[cfg(windows)]
    pipe: Option<crate::picker::pipe::Service>,
    context: egui::Context,
    proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
    window_id: egui_winit::winit::window::WindowId,
    frame_ready_pending: AtomicBool,
    close: AtomicBool,
    title: std::sync::Mutex<String>,
    clipboard_text: std::sync::Mutex<Option<String>>,
    config_issues: std::sync::Mutex<Vec<String>>,
    runtime_issues: std::sync::Mutex<Vec<(String, String)>>,
}

impl WakeState {
    fn picker_environment(&self) -> (std::ffi::CString, std::ffi::CString) {
        #[cfg(windows)]
        if let Some(pipe) = &self.pipe {
            return (
                std::ffi::CString::new(pipe.endpoint.clone()).unwrap(),
                std::ffi::CString::new(pipe.token.clone()).unwrap(),
            );
        }
        (std::ffi::CString::default(), std::ffi::CString::default())
    }
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_title(userdata: *mut c_void, title: *const c_char) {
    crate::native_error::ffi_boundary("handle terminal title", || {
        if userdata.is_null() || title.is_null() {
            return;
        }
        let state = unsafe { &*(userdata.cast::<WakeState>()) };
        *state.title.lock().unwrap() = unsafe { std::ffi::CStr::from_ptr(title) }
            .to_string_lossy()
            .into_owned();
    });
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_frame_ready(userdata: *mut c_void) {
    crate::native_error::ffi_boundary("handle native callback ghostty_egui_frame_ready", || {
        let state = unsafe { &*(userdata.cast::<WakeState>()) };
        if !state.frame_ready_pending.swap(true, Ordering::AcqRel)
            && state
                .proxy
                .send_event(UserEvent::FrameReady(state.window_id))
                .is_err()
        {
            state.frame_ready_pending.store(false, Ordering::Release);
        }
    });
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_close(userdata: *mut c_void) {
    crate::native_error::ffi_boundary("handle native callback ghostty_egui_close", || {
        let state = unsafe { &*(userdata.cast::<WakeState>()) };
        state.close.store(true, Ordering::Release);
        state.context.request_repaint();
    });
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_write_clipboard(userdata: *mut c_void, bytes: *const u8, len: usize) {
    crate::native_error::ffi_boundary(
        "handle native callback ghostty_egui_write_clipboard",
        || {
            if bytes.is_null() {
                return;
            }
            let state = unsafe { &*(userdata.cast::<WakeState>()) };
            let text = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(bytes, len) })
                .into_owned();
            *state.clipboard_text.lock().unwrap() = Some(text);
            state.context.request_repaint();
        },
    );
}

pub(super) struct TerminalPane {
    host: *mut Host,
    wake: Box<WakeState>,
    painter: Arc<egui::mutex::Mutex<TerminalPainter>>,
    paint_callback: crate::painter::PaintCallback,
    size: [u32; 2],
    scale: f32,
    focused: bool,
    active: bool,
    search: SearchUi,
    pending_search_snapshot: Option<u64>,
    next_search_open: u64,
    search_worker: SnapshotSearchAdapter,
    copy_mode: CopyMode,
    quick_select: Option<QuickSelect>,
    picker: Option<crate::picker::Picker>,
    picker_font: crate::picker::font::PickerFont,
    toasts: crate::toast::Toasts,
    copy_search_cursor: usize,
    copy_search_query: Option<(String, bool)>,
    copy_search_refresh: Option<u64>,
    prompt_boundaries: Option<(u64, Vec<u64>)>,
    pub(super) floating_palette_entries: Vec<crate::actions::Entry>,
    pub(super) pending_callback: Option<(crate::picker::Action, Option<u64>, bool)>,
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_config_issue(userdata: *mut c_void, message: *const c_char) {
    crate::native_error::ffi_boundary("handle native callback ghostty_egui_config_issue", || {
        if message.is_null() {
            return;
        }
        let state = unsafe { &*(userdata.cast::<WakeState>()) };
        let message = unsafe { std::ffi::CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned();
        state.config_issues.lock().unwrap().push(format!("Ghostty configuration issue. Invalid settings were ignored.\nFix the configuration and restart the app.\n\n{message}"));
    });
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_runtime_issue(
    userdata: *mut c_void,
    operation: *const c_char,
    cause: *const c_char,
) {
    crate::native_error::ffi_boundary("report a native operational failure", || {
        if operation.is_null() || cause.is_null() {
            return;
        }
        let state = unsafe { &*(userdata.cast::<WakeState>()) };
        let operation = unsafe { std::ffi::CStr::from_ptr(operation) }
            .to_string_lossy()
            .into_owned();
        let cause = unsafe { std::ffi::CStr::from_ptr(cause) }
            .to_string_lossy()
            .into_owned();
        crate::error_log::error("libghostty", &operation, &cause);
        let mut issues = state
            .runtime_issues
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if issues.len() < 32
            && !issues
                .iter()
                .any(|issue| issue == &(operation.clone(), cause.clone()))
        {
            issues.push((operation, cause));
        }
        state.context.request_repaint();
    });
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_theme_colors(
    userdata: *mut c_void,
    colors: *const crate::colors::ThemeColors,
    name: *const u8,
    name_len: usize,
) {
    crate::native_error::ffi_boundary("handle native callback ghostty_egui_theme_colors", || {
        if colors.is_null() {
            return;
        }
        let state = unsafe { &*(userdata.cast::<WakeState>()) };
        let name = if name.is_null() {
            None
        } else {
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(name, name_len) }).ok()
        };
        crate::colors::COLORS.apply_theme(&state.context, unsafe { &*colors }, name);
    });
}

impl TerminalPane {
    pub(super) fn show_error(&mut self, title: &str, error: String) {
        crate::error_log::error("terminal", title, &error);
        self.toasts.push_error(title.into(), error, 10000);
    }

    pub(super) fn show_notice(&mut self, title: &str, message: String) {
        self.toasts.push(title.into(), message, 3000);
    }

    pub(super) fn open_error_log(&mut self, ctx: &egui::Context) {
        let options = crate::error_log::picker_options();
        self.picker = Some(crate::picker::Picker::start(
            self.host,
            None,
            false,
            0,
            ctx.clone(),
            options,
        ));
    }
    pub(super) fn apply_theme(&self, name: &str) -> Result<(), String> {
        let name = std::ffi::CString::new(name).map_err(|_| "Theme name contains NUL")?;
        let mut error = [0u8; 4096];
        if unsafe {
            ghostty_ffi::ghostty_egui_host_apply_theme(
                self.host,
                name.as_ptr(),
                error.as_mut_ptr().cast(),
                error.len(),
            )
        } {
            self.wake.context.request_repaint();
            Ok(())
        } else {
            Err(
                String::from_utf8_lossy(error.split(|byte| *byte == 0).next().unwrap_or_default())
                    .into_owned(),
            )
        }
    }
    pub(super) fn apply_settings(
        &self,
        config: &crate::config::ReloadedConfig,
    ) -> Result<(), String> {
        let theme = config
            .theme
            .as_deref()
            .map(std::ffi::CString::new)
            .transpose()
            .map_err(|_| "Theme contains NUL")?;
        let family = config
            .font_family
            .as_deref()
            .map(std::ffi::CString::new)
            .transpose()
            .map_err(|_| "Font contains NUL")?;
        let mut error = [0u8; 4096];
        if unsafe {
            ghostty_ffi::ghostty_egui_host_apply_settings(
                self.host,
                theme
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                family
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                config.font_size.unwrap_or(0.0),
                error.as_mut_ptr().cast(),
                error.len(),
            )
        } {
            Ok(())
        } else {
            Err(
                String::from_utf8_lossy(error.split(|byte| *byte == 0).next().unwrap_or_default())
                    .into_owned(),
            )
        }
    }

    pub(super) fn refresh_picker_font(&mut self, ctx: &egui::Context) {
        self.picker_font = crate::picker::font::PickerFont::new(ctx, self.host);
    }
    pub(super) fn take_config_issues(&self) -> Vec<String> {
        std::mem::take(&mut *self.wake.config_issues.lock().unwrap())
    }
    fn new(
        host: *mut Host,
        wake: Box<WakeState>,
        gl: &glow::Context,
    ) -> Result<Self, crate::native_error::FatalError> {
        let painter = match TerminalPainter::new(gl) {
            Ok(painter) => painter,
            Err(error) => {
                // Keep callback userdata alive until the native host is freed.
                unsafe { ghostty_ffi::ghostty_egui_host_destroy(host) };
                return Err(error);
            }
        };
        let painter = Arc::new(egui::mutex::Mutex::new(painter));
        let callback_painter = Arc::clone(&painter);

        let paint_callback = Arc::new(egui_glow::CallbackFn::new(move |_info, glow_painter| {
            callback_painter.lock().paint(glow_painter.gl());
        }));
        let picker_font = crate::picker::font::PickerFont::new(&wake.context, host);
        let search_worker = SnapshotSearchAdapter::new(wake.context.clone());
        Ok(Self {
            host,
            wake,
            painter,
            paint_callback,
            size: [0, 0],
            scale: 0.0,
            focused: false,
            active: false,
            search: SearchUi::default(),
            pending_search_snapshot: None,
            next_search_open: 0,
            search_worker,
            copy_mode: CopyMode::default(),
            quick_select: None,
            picker: None,
            picker_font,
            toasts: crate::toast::Toasts::default(),
            copy_search_cursor: 0,
            copy_search_query: None,
            copy_search_refresh: None,
            prompt_boundaries: None,
            floating_palette_entries: Vec::new(),
            pending_callback: None,
        })
    }

    pub(super) fn acquire_frame(&mut self) -> bool {
        // Clear before taking the mailbox so a concurrent publication cannot
        // lose its notification. The callback always draws the current frame.
        self.wake
            .frame_ready_pending
            .store(false, Ordering::Release);
        let mut frame = GlFrame::default();
        if !unsafe { ghostty_ffi::ghostty_egui_host_take_frame(self.host, &mut frame) } {
            return false;
        }
        self.painter.lock().replace_frame(frame);
        true
    }

    pub(super) fn destroy(&mut self, gl: &glow::Context) {
        self.picker = None;
        #[cfg(windows)]
        {
            self.wake.pipe.take();
        }
        self.cancel_quick_select();
        self.close_search();
        self.close_copy_mode();
        self.painter.lock().destroy(gl);
        unsafe { ghostty_ffi::ghostty_egui_host_destroy(self.host) };
        self.host = std::ptr::null_mut();
    }

    pub(super) fn set_active(&mut self, ctx: &egui::Context, active: bool, window_focused: bool) {
        if self.active != active {
            if !active && (self.search.active || self.picker.is_some()) {
                ctx.memory_mut(|memory| {
                    if let Some(id) = memory.focused() {
                        memory.surrender_focus(id);
                    }
                });
            }
            if active && self.search.active {
                self.search.focus_requested = true;
            }
            self.active = active;
        }
        let focused = active && window_focused;
        if self.focused != focused {
            self.focused = focused;
            unsafe { ghostty_ffi::ghostty_egui_host_focus(self.host, focused) };
        }
    }

    pub(super) fn resize(&mut self, rect: egui::Rect, scale: f32) -> bool {
        let size = [
            (rect.width() * scale).round().max(1.0) as u32,
            (rect.height() * scale).round().max(1.0) as u32,
        ];
        if self.size == size && self.scale == scale {
            return false;
        }
        // Resize the live surface, then coordinate frozen-terminal reflow.
        // Readers are stopped before any frozen pages change.
        self.size = size;
        self.scale = scale;
        unsafe { ghostty_ffi::ghostty_egui_host_resize(self.host, size[0], size[1], scale as f64) };
        if let Err(error) = self.resize_snapshot() {
            self.picker = None;
            self.quick_select = None;
            self.close_search();
            self.close_copy_mode();
            self.show_error("Snapshot resize failed", error);
        }
        true
    }

    pub(super) fn update_modes(&mut self, ctx: &egui::Context) {
        // Font/scale changes can alter the native grid without changing pane pixels.
        if let Err(error) = self.resize_snapshot() {
            self.picker = None;
            self.quick_select = None;
            self.close_search();
            self.close_copy_mode();
            self.show_error("Snapshot resize failed", error);
        }
        if self.pending_search_snapshot.is_some() {
            self.prepare_search_snapshot();
        }
        self.apply_search_result();
        // Native code invalidates decorations when their presentation changes,
        // including a reflow that leaves the viewport at the same row.
        self.refresh_decorations();
        if self.quick_select.is_none()
            && self.picker.is_none()
            && let Some(generation) = self.copy_mode.generation
        {
            let now = Instant::now();
            if self.copy_mode.clear_expired_yank_highlight(now) {
                self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
            } else if let Some(remaining) = self.copy_mode.yank_highlight_remaining(now) {
                ctx.request_repaint_after(remaining);
            }
        }
    }

    pub(super) fn refresh_decorations(&mut self) {
        let refresh = snapshot_decoration_refresh(
            self.quick_select.as_ref().map(|quick| quick.generation),
            self.search.snapshot_generation,
            self.copy_mode.generation,
            |generation| unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_decorations_need_refresh(
                    self.host, generation,
                )
            },
        );
        match refresh {
            Some(SnapshotDecorationOwner::QuickSelect(_)) => {
                self.publish_quick_select();
            }
            Some(SnapshotDecorationOwner::Search(_)) => {
                self.publish_search_matches(false);
            }
            Some(SnapshotDecorationOwner::Copy(generation)) => {
                self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
            }
            None => {}
        }
    }

    pub(super) fn forward_input(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        keyboard_blocked: bool,
        suppress_scroll: bool,
    ) {
        if suppress_scroll {
            return;
        }
        let scrolled = input::forward(self.host, self.size, ui, rect, self.scale, keyboard_blocked);
        if scrolled {
            self.refresh_decorations();
        }
    }

    pub(super) fn drain_clipboard(&self, ctx: &egui::Context) {
        if let Some(text) = self.wake.clipboard_text.lock().unwrap().take() {
            ctx.copy_text(text);
        }
    }
}

pub(super) struct PaneInput {
    pub(super) keyboard_blocked: bool,
    pub(super) suppress_forward: bool,
    pub(super) action: Option<crate::actions::AppAction>,
}

pub(super) struct PaneStatus<'a> {
    pub(super) label: &'a str,
    pub(super) accent: egui::Color32,
    pub(super) separator: egui::Color32,
    pub(super) detail: Option<String>,
    pub(super) pipe_status: Option<bool>,
}

impl TerminalPane {
    pub(super) fn create(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        window_id: egui_winit::winit::window::WindowId,
        argc: usize,
        argv: *mut *mut c_char,
    ) -> Result<Self, crate::native_error::FatalError> {
        let mut wake = Box::new(WakeState {
            #[cfg(windows)]
            pipe: Some(
                crate::picker::pipe::Service::new(egui_ctx.clone()).map_err(|e| {
                    crate::native_error::FatalError::new("create secure picker pipe", e)
                })?,
            ),
            context: egui_ctx.clone(),
            proxy,
            window_id,
            frame_ready_pending: AtomicBool::new(false),
            close: AtomicBool::new(false),
            title: std::sync::Mutex::new(String::new()),
            config_issues: std::sync::Mutex::new(Vec::new()),
            runtime_issues: std::sync::Mutex::new(Vec::new()),
            clipboard_text: std::sync::Mutex::new(None),
        });
        #[cfg(windows)]
        if let Some(pipe) = &wake.pipe {
            pipe.bind_window(wake.proxy.clone(), wake.window_id);
        }
        let native = current_native_context();
        let (picker_endpoint, picker_token) = wake.picker_environment();
        let mut error_buffer = [0 as c_char; 2048];
        let host = unsafe {
            ghostty_ffi::ghostty_egui_host_create(
                argc,
                argv,
                (&mut *wake as *mut WakeState).cast(),
                native.window,
                native.display,
                native.context,
                egui_ctx.pixels_per_point() as f64,
                picker_endpoint.as_ptr(),
                picker_token.as_ptr(),
                error_buffer.as_mut_ptr(),
                error_buffer.len(),
            )
        };
        if host.is_null() {
            let detail =
                unsafe { std::ffi::CStr::from_ptr(error_buffer.as_ptr()) }.to_string_lossy();
            return Err(crate::native_error::FatalError::new(
                "initialize the main terminal",
                detail,
            ));
        }

        Self::new(host, wake, gl)
    }

    pub(super) fn create_floating(
        &self,
        gl: &glow::Context,
        options: &super::floating::CreateOptions,
    ) -> Result<Self, crate::native_error::FatalError> {
        self.create_child(&self.wake.context, gl, self.wake.window_id, false, options)
    }

    pub(super) fn create_window(
        &self,
        ctx: &egui::Context,
        gl: &glow::Context,
        window_id: egui_winit::winit::window::WindowId,
    ) -> Result<Self, crate::native_error::FatalError> {
        self.create_child(ctx, gl, window_id, true, &Default::default())
    }

    fn create_child(
        &self,
        ctx: &egui::Context,
        gl: &glow::Context,
        window_id: egui_winit::winit::window::WindowId,
        new_window: bool,
        options: &super::floating::CreateOptions,
    ) -> Result<Self, crate::native_error::FatalError> {
        let mut wake = Box::new(WakeState {
            #[cfg(windows)]
            pipe: Some(crate::picker::pipe::Service::new(ctx.clone()).map_err(|e| {
                crate::native_error::FatalError::new("create secure picker pipe", e)
            })?),
            context: ctx.clone(),
            proxy: self.wake.proxy.clone(),
            window_id,
            frame_ready_pending: AtomicBool::new(false),
            close: AtomicBool::new(false),
            title: std::sync::Mutex::new(String::new()),
            config_issues: std::sync::Mutex::new(Vec::new()),
            runtime_issues: std::sync::Mutex::new(Vec::new()),
            clipboard_text: std::sync::Mutex::new(None),
        });
        #[cfg(windows)]
        if let Some(pipe) = &wake.pipe {
            pipe.bind_window(wake.proxy.clone(), wake.window_id);
        }
        let (picker_endpoint, picker_token) = wake.picker_environment();
        let mut error_buffer = [0 as c_char; 2048];
        let command = options
            .command
            .as_ref()
            .map(|s| std::ffi::CString::new(s.as_str()))
            .transpose()
            .map_err(|e| crate::native_error::FatalError::new("floating pane command", e))?;
        let cwd = options
            .cwd
            .as_ref()
            .map(|s| std::ffi::CString::new(s.as_str()))
            .transpose()
            .map_err(|e| crate::native_error::FatalError::new("floating pane directory", e))?;
        let host = unsafe {
            let native = current_native_context();
            ghostty_ffi::ghostty_egui_host_create_child(
                self.host,
                (&mut *wake as *mut WakeState).cast(),
                if new_window {
                    native.window
                } else {
                    std::ptr::null_mut()
                },
                native.display,
                native.context,
                ctx.pixels_per_point() as f64,
                picker_endpoint.as_ptr(),
                picker_token.as_ptr(),
                command.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                cwd.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                error_buffer.as_mut_ptr(),
                error_buffer.len(),
            )
        };
        if host.is_null() {
            let detail =
                unsafe { std::ffi::CStr::from_ptr(error_buffer.as_ptr()) }.to_string_lossy();
            return Err(crate::native_error::FatalError::new(
                "create the additional terminal",
                detail,
            ));
        }
        Self::new(host, wake, gl)
    }

    pub(super) fn tick(&mut self) {
        unsafe { ghostty_ffi::ghostty_egui_host_tick(self.host) };
    }

    pub(super) fn should_close(&self) -> bool {
        self.wake.close.load(Ordering::Acquire)
    }

    pub(super) fn title(&self) -> String {
        self.wake.title.lock().unwrap().clone()
    }

    pub(super) fn request_repaint(&self) {
        self.wake.context.request_repaint();
    }

    pub(super) fn paint_callback(&self) -> crate::painter::PaintCallback {
        self.paint_callback.clone()
    }

    pub(super) fn poll_retired(&self, gl: &glow::Context) {
        self.painter.lock().poll_retired(gl);
    }

    pub(super) fn retired_pending(&self) -> bool {
        self.painter.lock().has_retired_frames()
    }
}

impl TerminalPane {
    pub(super) fn status<'a>(&'a self, command_label: &'a str) -> PaneStatus<'a> {
        let copying = self.copy_mode.generation.is_some();
        let visual =
            copying && self.quick_select.is_none() && self.copy_mode.visual_anchor.is_some();
        let label = if let Some(picker) = &self.picker {
            picker.label()
        } else if let Some(quick) = &self.quick_select {
            if matches!(quick.purpose, QuickSelectPurpose::YankLines { .. }) {
                "YANK LINES"
            } else {
                "QUICK SELECT"
            }
        } else if self.search.active {
            "SEARCH"
        } else if visual {
            match self.copy_mode.visual_mode {
                VisualMode::Characterwise => "VISUAL",
                VisualMode::Linewise => "V-LINE",
                VisualMode::Blockwise => "V-BLOCK",
            }
        } else if copying {
            "COPY"
        } else {
            command_label
        };
        let accent = if visual {
            crate::colors::palette().accent
        } else if copying || self.search.active {
            crate::colors::palette().accent
        } else {
            crate::colors::palette().muted_text
        };
        let separator = if copying || self.search.active {
            accent
        } else {
            crate::colors::palette().panel_background
        };
        let detail = self
            .quick_select
            .as_ref()
            .map(QuickSelect::status_text)
            .or_else(|| {
                copying.then(|| {
                    format!(
                        "{}:{}",
                        self.copy_mode.cursor.y.saturating_add(1),
                        self.copy_mode.cursor.x.saturating_add(1),
                    )
                })
            });
        PaneStatus {
            label,
            accent,
            separator,
            detail,
            pipe_status: {
                #[cfg(windows)]
                {
                    self.wake
                        .pipe
                        .as_ref()
                        .filter(|pipe| pipe.enabled())
                        .map(|pipe| pipe.connected())
                }
                #[cfg(not(windows))]
                {
                    None
                }
            },
        }
    }

    pub(super) fn handle_input(
        &mut self,
        ui: &egui::Ui,
        bounds: egui::Rect,
        bindings: &mut crate::picker::config::Bindings,
    ) -> PaneInput {
        self.copy_mode
            .set_scroll_padding(bindings.cursor_scroll_padding);
        let (picker_consumed, action) = self.handle_picker(ui, bounds, bindings);
        let quick_consumed = !picker_consumed && self.handle_quick_select_keys(ui);
        let copy_consumed =
            !picker_consumed && !quick_consumed && self.handle_copy_keys(ui, bindings);
        PaneInput {
            keyboard_blocked: picker_consumed
                || quick_consumed
                || copy_consumed
                || self.search.active,
            suppress_forward: picker_consumed || quick_consumed,
            action,
        }
    }

    fn handle_quick_select_keys(&mut self, ui: &egui::Ui) -> bool {
        if self.quick_select.is_none() {
            return false;
        }
        let events = ui.input(|input| input.events.clone());
        for index in 0..events.len() {
            let action = self
                .quick_select
                .as_mut()
                .expect("quick select is active")
                .input(&events, index);
            match action {
                quick_select::InputAction::Cancel => {
                    self.cancel_quick_select();
                    break;
                }
                quick_select::InputAction::Page(down) => {
                    if !self.page_quick_select(down) {
                        self.cancel_quick_select();
                        break;
                    }
                }
                quick_select::InputAction::Changed => {
                    self.publish_quick_select();
                }
                quick_select::InputAction::Select { cell, shifted } => {
                    if matches!(
                        self.quick_select.as_ref().map(|quick| quick.purpose),
                        Some(QuickSelectPurpose::YankLines { .. })
                    ) {
                        self.complete_quick_select(ui.ctx(), cell);
                    } else if shifted {
                        self.paste_quick_select(cell);
                    } else {
                        self.complete_quick_select(ui.ctx(), cell);
                    }
                    break;
                }
                quick_select::InputAction::None => {}
            }
        }
        true
    }
}

impl TerminalPane {
    pub(super) fn show_background_overlays(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if let Some(picker) = &mut self.picker {
            picker.typography = self.picker_font.typography(ctx, self.host);
            picker.show_background_in_bounds(ctx, bounds);
        }
        if self.search.active {
            self.search
                .show(ctx, bounds, self.pending_search_snapshot.is_some(), false);
        }
    }

    // Text is typed input; line breaks are Enter keys, never bracketed paste.
    fn send_picker_text(host: *mut Host, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let enter_usb = crate::input::key_to_usb(egui::Key::Enter)
            .expect("Enter must have a USB key code")
            .0;
        for (index, chunk) in normalized.split('\n').enumerate() {
            if index > 0 {
                unsafe {
                    ghostty_ffi::ghostty_egui_host_key(host, 1, 0, enter_usb, 0, std::ptr::null());
                    ghostty_ffi::ghostty_egui_host_key(host, 0, 0, enter_usb, 0, std::ptr::null());
                }
            }
            if !chunk.is_empty() {
                if let Ok(text) = std::ffi::CString::new(chunk) {
                    unsafe {
                        ghostty_ffi::ghostty_egui_host_key(host, 1, 0, 0, 0, text.as_ptr());
                    }
                }
            }
        }
    }

    fn replace_picker_prompt(host: *mut Host, text: &str, before: u32, after: u32) {
        for (key, count) in [(egui::Key::Delete, after), (egui::Key::Backspace, before)] {
            let usb = crate::input::key_to_usb(key)
                .expect("prompt editing key must have a USB code")
                .0;
            for _ in 0..count {
                unsafe {
                    ghostty_ffi::ghostty_egui_host_key(host, 1, 0, usb, 0, std::ptr::null());
                    ghostty_ffi::ghostty_egui_host_key(host, 0, 0, usb, 0, std::ptr::null());
                }
            }
        }
        unsafe { ghostty_ffi::ghostty_egui_host_text(host, text.as_ptr().cast(), text.len()) };
    }
    pub(super) fn open_picker(
        &mut self,
        ctx: &egui::Context,
        entry: crate::picker::config::PickerEntry,
        request_params: Option<std::collections::BTreeMap<String, String>>,
        bindings: &crate::picker::config::Bindings,
        has_floating: bool,
    ) {
        self.open_picker_impl(ctx, entry, request_params, bindings, has_floating, false);
    }

    fn open_picker_impl(
        &mut self,
        ctx: &egui::Context,
        entry: crate::picker::config::PickerEntry,
        request_params: Option<std::collections::BTreeMap<String, String>>,
        bindings: &crate::picker::config::Bindings,
        has_floating: bool,
        replace: bool,
    ) {
        let host = self.host;
        let context = if entry.needs_context() {
            let mut row = ghostty_ffi::LiveCursorLine::default();
            if !unsafe { ghostty_ffi::ghostty_egui_host_live_cursor_line(host, &mut row) } {
                self.show_error(
                    "Could not open picker",
                    "Unable to read live cursor line".into(),
                );
                return;
            }
            let bytes = if row.len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(row.text, row.len) }
            };
            let line = String::from_utf8_lossy(bytes).into_owned();
            let before =
                String::from_utf8_lossy(&bytes[..row.cursor_byte.min(bytes.len())]).into_owned();
            let context = crate::config::PickerContext {
                line,
                line_before_cursor: before,
                cursor_column: row.cursor_column,
                recent_lines: self.recent_picker_lines(),
            };
            unsafe { ghostty_ffi::ghostty_egui_host_live_cursor_line_free(&mut row) };
            context
        } else {
            crate::config::PickerContext::default()
        };
        let mut options = match entry
            .resolve(&context)
            .and_then(|options| match request_params {
                Some(params) => options.with_request(params),
                None => Ok(options),
            }) {
            Ok(options) => options,
            Err(error) => {
                self.show_error("Could not open picker", error);
                return;
            }
        };
        if matches!(options.source, crate::picker::config::Source::ErrorLog) {
            options.source = crate::error_log::picker_options().source;
            options
                .params
                .insert("__error_history".into(), "true".into());
        } else if matches!(options.source, crate::picker::config::Source::Themes) {
            let names = match crate::themes::installed_themes() {
                Ok(names) if !names.is_empty() => names,
                Ok(_) => {
                    self.show_error(
                        "No themes found",
                        "Check your Ghostty resources/themes directory".into(),
                    );
                    return;
                }
                Err(error) => {
                    self.show_error("Could not load themes", error);
                    return;
                }
            };
            options.source = crate::themes::picker_options(names).source;
        } else if let crate::picker::config::Source::CommandPalette(entries) = &mut options.source {
            *entries = crate::actions::catalog(bindings, has_floating, self.copying());
            entries.extend(self.floating_palette_entries.iter().cloned());
            // A factory's source is only known after resolving it. Exclude
            // the current factory too, without evaluating other factories.
            if let crate::picker::config::PickerEntry::Factory(factory, _) = &entry {
                entries.retain(|entry| !matches!(&entry.action, crate::actions::AppAction::OpenPicker(name) if name == factory.name()));
            }
        }
        let copy_generation = self.copy_mode.generation;
        let from_copy = if replace {
            self.picker
                .as_ref()
                .map_or(copy_generation.is_some(), |picker| picker.from_copy)
        } else {
            copy_generation.is_some()
        };
        let cursor_row = if options.position == crate::picker::config::Position::Cursor {
            self.picker_cursor_row()
        } else {
            None
        };
        if matches!(
            options.source,
            crate::picker::config::Source::CommandPalette(_)
                | crate::picker::config::Source::FileSystem(_)
                | crate::picker::config::Source::Items(_)
                | crate::picker::config::Source::Command(_)
        ) {
            let previous = if replace { self.picker.take() } else { None };
            self.picker = Some(crate::picker::Picker::start_replacing(
                host,
                if matches!(
                    options.source,
                    crate::picker::config::Source::CommandPalette(_)
                ) {
                    None
                } else {
                    copy_generation
                },
                from_copy,
                0,
                ctx.clone(),
                options,
                previous,
            ));
        } else {
            let generation = if let Some(generation) = copy_generation {
                generation
            } else {
                let mut generation = 0;
                if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_begin(host, &mut generation) }
                {
                    self.show_error(
                        "Picker unavailable",
                        "Unable to create the scrollback snapshot".into(),
                    );
                    return;
                }
                generation
            };
            let mut view = SnapshotView::default();
            if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view) }
            {
                if !from_copy {
                    unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(host, generation) };
                }
                if from_copy {
                    self.close_copy_mode();
                }
                self.show_error(
                    "Picker unavailable",
                    "Unable to read the scrollback snapshot".into(),
                );
                return;
            }
            if !from_copy {
                self.copy_mode.start(generation, view);
            }
            let previous = if replace { self.picker.take() } else { None };
            self.picker = Some(crate::picker::Picker::start_replacing(
                host,
                Some(generation),
                from_copy,
                view.total_rows,
                ctx.clone(),
                options,
                previous,
            ));
        }
        if let Some(picker) = self.picker.as_mut() {
            picker.cursor_row = cursor_row;
        }
    }
    pub(super) fn action_context(&self) -> crate::actions::ActionContext {
        crate::actions::ActionContext {
            copying: self.copying(),
            picker: self.picker.is_some(),
            search: self.search.active,
            quick_select: self.quick_select.is_some(),
            palette: self
                .picker
                .as_ref()
                .is_some_and(crate::picker::Picker::is_command_palette),
        }
    }
    pub(super) fn copying(&self) -> bool {
        self.copy_mode.generation.is_some()
    }

    pub(super) fn show_toasts(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let issues = std::mem::take(
            &mut *self
                .wake
                .runtime_issues
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for (operation, cause) in issues {
            if matches!(cause.as_str(), "SnapshotExpired" | "SnapshotUnavailable") {
                self.picker = None;
                self.cancel_quick_select();
                self.close_search();
                self.close_copy_mode();
            }
            self.toasts.push_error(
                "Terminal operation failed".into(),
                format!("{operation}: {cause}"),
                10000,
            );
        }
        if self.toasts.is_empty() {
            return;
        }
        let cursor_row = self.picker_cursor_row();
        let size = unsafe { ghostty_ffi::ghostty_egui_host_surface_size(self.host) };
        let row_height = (size.cell_height_px as f32 / ctx.pixels_per_point()).max(1.0);
        self.toasts.show(
            ctx,
            bounds,
            egui::Id::new(("terminal-toasts", self.host as usize)),
            cursor_row,
            row_height,
        );
    }

    fn picker_cursor_row(&self) -> Option<u32> {
        if let Some(generation) = self.copy_mode.generation {
            let mut view = SnapshotView::default();
            if !unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view)
            } {
                return None;
            }
            self.copy_mode
                .cursor
                .y
                .checked_sub(view.viewport_top)
                .filter(|row| *row < u64::from(view.viewport_rows))
                .and_then(|row| u32::try_from(row).ok())
        } else {
            let mut cursor = ghostty_ffi::LiveCursorPosition::default();
            unsafe { ghostty_ffi::ghostty_egui_host_live_cursor_position(self.host, &mut cursor) }
                .then_some(cursor.row)
        }
    }

    pub(super) fn selected_text(&self) -> String {
        if let Some(text) = self.copy_mode.reflow_selection_text() {
            return text.to_owned();
        }
        if let Some(generation) = self.copy_mode.generation {
            let document = SnapshotNavigationDocument {
                host: self.host,
                generation,
                rows: self.copy_mode.total_rows(),
                boundaries: None,
            };
            let selection = self
                .copy_mode
                .visual_selection_to(self.copy_mode.cursor, |row| {
                    self.read_snapshot_row(generation, row)
                });
            use nfm_egui::copy_controller::CopyNavigationDocument;
            return selection
                .and_then(|selection| document.selection_text(&selection).ok())
                .unwrap_or_default();
        }
        let mut result = ghostty_ffi::SnapshotSelectionText::default();
        let copied =
            unsafe { ghostty_ffi::ghostty_egui_host_selection_text(self.host, &mut result) };
        if !copied {
            return String::new();
        }
        let text = if result.len == 0 {
            String::new()
        } else {
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(result.text, result.len) })
                .into_owned()
        };
        unsafe {
            if self.copy_mode.generation.is_some() {
                ghostty_ffi::ghostty_egui_host_snapshot_selection_text_free(&mut result);
            } else {
                ghostty_ffi::ghostty_egui_host_selection_text_free(&mut result);
            }
        }
        text
    }

    pub(super) fn finish_lua_action(
        &mut self,
        result: Result<Vec<crate::lua_callbacks::Command>, String>,
        ctx: &egui::Context,
    ) {
        match result {
            Ok(commands) => {
                for command in commands {
                    match command {
                        crate::lua_callbacks::Command::ShowToast {
                            title,
                            message,
                            duration_ms,
                        } => self.toasts.push(title, message, duration_ms),
                        crate::lua_callbacks::Command::Paste(text) => {
                            self.close_copy_mode();
                            unsafe {
                                ghostty_ffi::ghostty_egui_host_text(
                                    self.host,
                                    text.as_ptr().cast(),
                                    text.len(),
                                );
                            }
                        }
                        crate::lua_callbacks::Command::SendText(text) => {
                            self.close_copy_mode();
                            Self::send_picker_text(self.host, &text);
                        }
                        crate::lua_callbacks::Command::ReplacePrompt {
                            text,
                            before,
                            after,
                        } => {
                            self.close_copy_mode();
                            Self::replace_picker_prompt(self.host, &text, before, after);
                        }
                        crate::lua_callbacks::Command::Navigate(_) => {}
                        crate::lua_callbacks::Command::ReplacePicker { .. } => {
                            self.show_error(
                                "Picker replacement failed",
                                "replace_picker requires a picker callback".into(),
                            );
                        }
                        crate::lua_callbacks::Command::ClosePicker => {
                            if let Some(picker) = self.picker.take() {
                                picker.surrender_focus();
                                let (generation, from_copy) = (picker.generation, picker.from_copy);
                                drop(picker);
                                self.handle_picker_action(
                                    crate::picker::Action::Cancel,
                                    generation,
                                    from_copy,
                                );
                            }
                        }
                    }
                }
            }
            Err(error) => self.show_error("Action failed", error),
        }
        ctx.request_repaint();
    }

    pub(super) fn execute_action(
        &mut self,
        action: &crate::actions::AppAction,
        ctx: &egui::Context,
    ) {
        use crate::actions::AppAction;
        match action {
            AppAction::OpenSearch => {
                if self.search.active {
                    self.search.focus_requested = true;
                } else {
                    self.open_search(String::new());
                }
            }
            AppAction::EnterCopyMode => {
                if !self.copying() {
                    self.open_copy_mode();
                }
            }
            AppAction::ExitCopyMode => self.close_copy_mode(),
            AppAction::PreviousCommand | AppAction::NextCommand => {
                if !self.copying() {
                    self.open_copy_mode();
                }
                if self.copying() {
                    let count = self.copy_mode.command_navigation_count();
                    self.navigate_copy_command(matches!(action, AppAction::NextCommand), count);
                }
            }
            AppAction::OpenQuickSelect => {
                if self.quick_select.is_some() {
                    self.cancel_quick_select();
                } else {
                    self.open_quick_select();
                }
            }
            _ => {}
        }
        ctx.request_repaint();
    }
    #[cfg(windows)]
    pub(super) fn handle_pipe_requests(
        &mut self,
        ctx: &egui::Context,
        bindings: &crate::picker::config::Bindings,
        has_floating: bool,
        interactive: bool,
    ) {
        use crate::picker::pipe::{Event, Open};
        if let Some(pipe) = &self.wake.pipe {
            pipe.update(
                &bindings.pipe_policy,
                unsafe { ghostty_ffi::ghostty_egui_host_shell_pid(self.host) } as u32,
                unsafe { ghostty_ffi::ghostty_egui_host_shell_creation_time(self.host) },
            );
        }
        if self.picker.as_ref().is_some_and(|p| p.pipe_cancelled()) {
            let picker = self.picker.take().unwrap();
            let (generation, from_copy) = (picker.generation, picker.from_copy);
            drop(picker);
            self.handle_picker_action(crate::picker::Action::Cancel, generation, from_copy);
        }
        // A producer can refill the queue as batches are acknowledged. Yield to
        // input and preview painting instead of draining it indefinitely.
        let pipe_deadline = std::time::Instant::now() + std::time::Duration::from_millis(4);
        let mut pipe_events_processed = 0;
        loop {
            if pipe_events_processed >= 8 || std::time::Instant::now() >= pipe_deadline {
                ctx.request_repaint();
                break;
            }
            let event = self
                .wake
                .pipe
                .as_ref()
                .and_then(|pipe| pipe.events.try_recv().ok());
            let Some(event) = event else {
                break;
            };
            pipe_events_processed += 1;
            match event {
                Event::Open { request, session } => {
                    if session.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    if !interactive
                        || self.picker.is_some()
                        || self.search.active
                        || self.quick_select.is_some()
                    {
                        session.finish("error",serde_json::json!({"code":"busy","message":"another overlay is active"}));
                        continue;
                    }
                    if let Err(message) = bindings.pipe_policy.authorize(&request) {
                        session.finish(
                            "error",
                            serde_json::json!({"code":"capability_denied","message":message}),
                        );
                        continue;
                    }
                    let (entry, params) = match &request {
                        Open::Named { name, params } => (
                            bindings
                                .named(name)
                                .ok_or_else(|| "unknown named picker".to_owned()),
                            Some(params.clone()),
                        ),
                        Open::Stream { options } => {
                            (crate::picker::config::Bindings::pipe_stream(options), None)
                        }
                    };
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(message) => {
                            session.finish(
                                "error",
                                serde_json::json!({"code":"invalid_definition","message":message}),
                            );
                            continue;
                        }
                    };
                    let mut params = params.unwrap_or_default();
                    params.insert("__ipc_request".into(), "1".into());
                    self.open_picker(ctx, entry, Some(params), bindings, has_floating);
                    if let Some(picker) = &mut self.picker {
                        picker.attach_pipe(
                            ctx,
                            session.clone(),
                            match &request {
                                Open::Stream { options } => Some(options),
                                _ => None,
                            },
                        );
                        picker.bind_pipe_search_wake(self.wake.proxy.clone(), self.wake.window_id);
                        session.reply(serde_json::json!({"type":"opened","request_id":session.id}));
                    } else {
                        session.finish("error",serde_json::json!({"code":"open_failed","message":"unable to open picker"}));
                    }
                }
                other => {
                    if let Some(picker) = &mut self.picker {
                        picker.pipe_event(ctx, other);
                    }
                }
            }
        }
    }

    fn handle_picker(
        &mut self,
        ui: &egui::Ui,
        bounds: egui::Rect,
        bindings: &mut crate::picker::config::Bindings,
    ) -> (bool, Option<crate::actions::AppAction>) {
        let was_active = self.picker.as_ref().is_some();
        let consumed = false;
        let active = self.picker.as_ref().is_some();
        let host = self.host;
        let typography = self.picker_font.typography(ui.ctx(), host);
        if let Some(picker) = self.picker.as_mut() {
            picker.typography = typography;
            picker.builtin_pickers = bindings.builtin_pickers;
            picker.set_cursor_scroll_padding(bindings.cursor_scroll_padding);
        }
        let action = self
            .picker
            .as_mut()
            .and_then(|picker| picker.show_in_bounds(ui.ctx(), bounds));
        let mut application_action = None;
        if let Some(action) = action {
            if matches!(&action, crate::picker::Action::Custom(..)) {
                let picker = self
                    .picker
                    .as_ref()
                    .expect("picker key action requires active picker");
                self.pending_callback = Some((action, picker.generation, picker.from_copy));
                ui.ctx().request_repaint();
                return (true, None);
            }
            let picker = self
                .picker
                .take()
                .expect("picker action requires active picker");
            let (generation, from_copy) = (picker.generation, picker.from_copy);
            drop(picker);
            // Surrender query focus before returning terminal/copy input.
            ui.ctx().memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            application_action = self.handle_picker_action(action, generation, from_copy);
            ui.ctx().request_repaint();
        }
        (was_active || active || consumed, application_action)
    }

    fn handle_picker_action(
        &mut self,
        action: crate::picker::Action,
        generation: Option<u64>,
        from_copy: bool,
    ) -> Option<crate::actions::AppAction> {
        let restore_copy = |pane: &mut Self| {
            if from_copy {
                if let Some(generation) = generation {
                    pane.publish_copy_decoration(generation, pane.copy_mode.cursor, false);
                }
            } else {
                pane.close_copy_mode();
            }
        };
        match action {
            crate::picker::Action::Execute(action) => return Some(action),
            crate::picker::Action::Cancel => restore_copy(self),
            crate::picker::Action::Navigate(target) => {
                if let Some(generation) = generation {
                    self.accept_picker_cell(generation, target);
                }
            }
            crate::picker::Action::SendInput(text) => {
                self.close_copy_mode();
                unsafe {
                    ghostty_ffi::ghostty_egui_host_text(self.host, text.as_ptr().cast(), text.len())
                };
            }
            action @ crate::picker::Action::Custom(..) => {
                self.pending_callback = Some((action, generation, from_copy));
            }
        }
        None
    }

    pub(super) fn finish_picker_callback(
        &mut self,
        result: Result<Vec<crate::lua_callbacks::Command>, String>,
        generation: Option<u64>,
        from_copy: bool,
        ctx: &egui::Context,
        bindings: &crate::picker::config::Bindings,
        has_floating: bool,
    ) {
        let restore_copy = |pane: &mut Self| {
            if from_copy {
                if let Some(generation) = generation {
                    pane.publish_copy_decoration(generation, pane.copy_mode.cursor, false);
                }
            } else {
                pane.close_copy_mode();
            }
        };
        match result {
            Ok(commands) => {
                // Accept callbacks normally close before executing commands.
                // Keep the old control only when it is needed for replacement.
                if !commands.iter().any(|command| {
                    matches!(command, crate::lua_callbacks::Command::ReplacePicker { .. })
                }) && self
                    .picker
                    .as_ref()
                    .is_some_and(|picker| picker.is_closed())
                {
                    if let Some(picker) = self.picker.take() {
                        picker.surrender_focus();
                    }
                }
                let mut navigated = false;
                let mut sent = false;
                for command in commands {
                    match command {
                        crate::lua_callbacks::Command::ReplacePicker { name, params } => {
                            if let Some(entry) = bindings.named(&name) {
                                self.open_picker_impl(
                                    ctx,
                                    entry,
                                    Some(params),
                                    bindings,
                                    has_floating,
                                    true,
                                );
                            } else {
                                self.show_error(
                                    "Picker replacement failed",
                                    format!("Unknown picker {name}"),
                                );
                            }
                        }
                        crate::lua_callbacks::Command::ClosePicker => {
                            if let Some(picker) = &self.picker {
                                picker.surrender_focus();
                            }
                            self.picker = None;
                            if sent {
                                self.close_copy_mode();
                            } else if !navigated {
                                restore_copy(self);
                            }
                        }
                        crate::lua_callbacks::Command::ShowToast {
                            title,
                            message,
                            duration_ms,
                        } => self.toasts.push(title, message, duration_ms),
                        crate::lua_callbacks::Command::Navigate(target) => {
                            if let Some(generation) =
                                generation.filter(|g| self.copy_mode.generation == Some(*g))
                            {
                                self.accept_picker_cell(generation, target);
                                navigated = true;
                            } else {
                                self.show_error(
                                    "Picker navigation failed",
                                    "The saved terminal view has expired. Open the picker again."
                                        .into(),
                                );
                            }
                        }
                        crate::lua_callbacks::Command::Paste(text) => {
                            if self.picker.is_none() {
                                self.close_copy_mode();
                            }
                            sent = true;
                            unsafe {
                                ghostty_ffi::ghostty_egui_host_text(
                                    self.host,
                                    text.as_ptr().cast(),
                                    text.len(),
                                );
                            }
                        }
                        crate::lua_callbacks::Command::SendText(text) => {
                            if self.picker.is_none() {
                                self.close_copy_mode();
                            }
                            sent = true;
                            Self::send_picker_text(self.host, &text);
                        }
                        crate::lua_callbacks::Command::ReplacePrompt {
                            text,
                            before,
                            after,
                        } => {
                            if self.picker.is_none() {
                                self.close_copy_mode();
                            }
                            sent = true;
                            Self::replace_picker_prompt(self.host, &text, before, after);
                        }
                    }
                }
                if self
                    .picker
                    .as_ref()
                    .is_some_and(|picker| picker.is_closed())
                {
                    if let Some(picker) = self.picker.take() {
                        picker.surrender_focus();
                    }
                }
                if !sent && !navigated && self.picker.is_none() {
                    restore_copy(self);
                }
            }
            Err(error) => {
                self.show_error("Picker action failed", error);
                if self
                    .picker
                    .as_ref()
                    .is_some_and(|picker| picker.is_closed())
                {
                    if let Some(picker) = self.picker.take() {
                        picker.surrender_focus();
                    }
                }
                if self.picker.is_none() {
                    restore_copy(self);
                }
            }
        }
    }

    fn accept_picker_cell(&mut self, generation: u64, target: SnapshotCell) {
        // A pending copy-mode word search must not move the cursor
        // again after accepting this picker result.
        self.search_worker.cancel();
        self.copy_mode.cancel_pending_search();
        self.copy_mode.clear_pending_for_navigation();
        self.apply_copy_target(generation, Motion::CharacterJump, target, false);
    }

    fn open_quick_select(&mut self) {
        let host = self.host;
        let copy_generation = self.copy_mode.generation;
        let from_copy = copy_generation.is_some();
        let generation = if let Some(generation) = copy_generation {
            generation
        } else {
            let mut generation = 0;
            if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_begin(host, &mut generation) } {
                self.show_error(
                    "Quick select unavailable",
                    "Unable to create the terminal snapshot".into(),
                );
                return;
            }
            generation
        };
        let mut view = SnapshotView::default();
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view) } {
            if !from_copy {
                unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(host, generation) };
            }
            self.show_error(
                "Quick select unavailable",
                "Unable to read the terminal snapshot".into(),
            );
            return;
        }
        if !from_copy {
            self.copy_mode.start(generation, view);
        }
        self.quick_select = self.build_quick_select(generation, from_copy, view, false);
        if self.quick_select.is_none() {
            if !from_copy {
                self.close_quick_copy_mode();
            }
            return;
        }
        if !self.publish_quick_select() {
            self.cancel_quick_select();
            self.show_error(
                "Quick select failed",
                "Unable to display selection labels".into(),
            );
        }
        self.wake.context.request_repaint();
    }

    fn publish_quick_select(&self) -> bool {
        self.publish_quick_select_with_viewport(std::ptr::null())
    }

    fn build_quick_select(
        &self,
        generation: u64,
        from_copy: bool,
        view: SnapshotView,
        _paged: bool,
    ) -> Option<QuickSelect> {
        QuickSelect::new(generation, from_copy, view, |row| {
            Self::read_snapshot_row_from(self.quick_host(), generation, row)
        })
    }

    fn page_quick_select(&mut self, down: bool) -> bool {
        let Some(quick) = &self.quick_select else {
            return false;
        };
        let (generation, from_copy, purpose) = (quick.generation, quick.from_copy, quick.purpose);
        let host = self.quick_host();
        let mut view = SnapshotView::default();
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view) } {
            self.cancel_quick_select();
            self.show_error(
                "Quick select failed",
                "The saved terminal view has expired. Open quick select again.".into(),
            );
            return false;
        }
        let view = QuickSelect::paged_view(view, down);
        let Some(mut quick) = self.build_quick_select(generation, from_copy, view, true) else {
            return false;
        };
        quick.purpose = purpose;
        self.quick_select = Some(quick);
        // Move the viewport and replace labels in the same Ghostty operation.
        let viewport = SnapshotViewportOptions {
            top_row: &view.viewport_top,
            ..Default::default()
        };
        let published = self.publish_quick_select_with_viewport(&viewport);
        self.wake.context.request_repaint();
        published
    }

    fn publish_quick_select_with_viewport(&self, viewport: *const SnapshotViewportOptions) -> bool {
        let Some(quick) = &self.quick_select else {
            return false;
        };
        let palette = crate::colors::palette();
        let ranges: Vec<_> = quick.visible_hints().map(|hint| hint.range).collect();
        let text: Vec<_> = quick
            .cell_text()
            .into_iter()
            .map(|hint| ghostty_ffi::SnapshotCellText {
                cell: hint.cell,
                codepoint: hint.codepoint,
                style: SnapshotDecorationStyle {
                    foreground_rgb: crate::colors::rgb(palette.accent),
                    background_rgb: crate::colors::rgb(palette.background),
                    flags: 3,
                },
                bold: true,
            })
            .collect();
        let style = SnapshotDecorationStyle {
            foreground_rgb: crate::colors::rgb(palette.selection_text),
            background_rgb: crate::colors::rgb(palette.selection_background),
            flags: 3,
        };
        let layer = SnapshotDecorationLayer {
            ranges: ranges.as_ptr(),
            range_count: ranges.len(),
            style,
        };
        unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_apply_cell_text(
                self.quick_host(),
                quick.generation,
                &layer,
                1,
                text.as_ptr(),
                text.len(),
                viewport,
            )
        }
    }

    fn cancel_quick_select(&mut self) {
        let Some(quick) = self.quick_select.take() else {
            return;
        };
        if quick.from_copy {
            self.publish_copy_decoration(quick.generation, self.copy_mode.cursor, false);
        } else {
            self.close_quick_copy_mode();
        }
        self.wake.context.request_repaint();
    }

    fn complete_quick_select(&mut self, ctx: &egui::Context, target: SnapshotCell) {
        let Some(quick) = self.quick_select.take() else {
            return;
        };
        let mut view = SnapshotView::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, quick.generation, &mut view)
        } {
            self.copy_controller_error("Quick-select snapshot expired".into());
            return;
        }
        let document = SnapshotNavigationDocument {
            host: self.host,
            generation: quick.generation,
            rows: view.total_rows,
            boundaries: None,
        };
        match nfm_egui::copy_controller::CopyController::new(&mut self.copy_mode, &document, view)
            .select_quick(quick.purpose, target)
        {
            Ok(update) => {
                self.apply_copy_update(update, quick.generation, ctx, None);
            }
            Err(error) => self.copy_controller_error(error),
        }
    }

    fn paste_quick_select(&mut self, target: SnapshotCell) {
        let Some(quick) = &self.quick_select else {
            return;
        };
        let Some(hint) = quick.hints.iter().find(|hint| hint.range.start == target) else {
            return;
        };
        let mut result = ghostty_ffi::SnapshotSelectionText::default();
        let host = self.quick_host();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_selection_text(
                host,
                quick.generation,
                hint.range,
                false,
                false,
                &mut result,
            )
        } {
            self.cancel_quick_select();
            self.show_error(
                "Quick select paste failed",
                "Unable to read the selected text".into(),
            );
            return;
        }
        // Own the original text before releasing the snapshot. Ghostty's
        // selection formatter handles Unicode and joins soft-wrapped rows.
        let text = if result.len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(result.text, result.len) }.to_vec()
        };
        unsafe { ghostty_ffi::ghostty_egui_host_snapshot_selection_text_free(&mut result) };
        self.quick_select = None;
        self.close_quick_copy_mode();
        unsafe { ghostty_ffi::ghostty_egui_host_text(host, text.as_ptr().cast(), text.len()) };
        self.wake.context.request_repaint();
    }

    fn quick_host(&self) -> *mut Host {
        self.host
    }

    fn close_quick_copy_mode(&mut self) {
        self.close_copy_mode();
    }

    fn open_search(&mut self, needle: String) {
        self.copy_mode.cancel_pending_search();
        self.close_copy_mode();
        self.close_search();

        self.search = SearchUi {
            active: true,
            query: needle,
            focus_requested: true,
            ..SearchUi::default()
        };
        self.next_search_open = self.next_search_open.wrapping_add(1);
        self.pending_search_snapshot = Some(self.next_search_open);
        self.wake.context.request_repaint();
    }

    fn open_copy_search(&mut self, backward: bool) {
        let Some(generation) = self.copy_mode.generation else {
            return;
        };
        self.search_worker.cancel_and_wait();
        let cursor = if self.copy_search_cursor != 0 {
            self.copy_search_cursor
        } else {
            (unsafe { ghostty_ffi::ghostty_egui_host_snapshot_cursor_new(self.host, generation) })
                as usize
        };
        if cursor == 0 {
            self.show_error(
                "Search unavailable",
                "Unable to create snapshot cursor".into(),
            );
            return;
        }
        self.copy_mode.cancel_pending_search();
        let padding = self.copy_mode.scroll_padding();
        let origin = std::mem::take(&mut self.copy_mode);
        self.copy_mode.set_scroll_padding(padding);
        self.copy_search_cursor = 0;
        self.search = SearchUi {
            active: true,
            focus_requested: true,
            snapshot_generation: Some(generation),
            cursor,
            copy_origin: Some(origin),
            backward,
            ..Default::default()
        };
        self.publish_search_matches(false);
        self.queue_search();
        self.wake.context.request_repaint();
    }

    fn cancel_search(&mut self) {
        if self.search.copy_origin.is_some() {
            self.search_worker.cancel_and_wait();
            let mut search = std::mem::take(&mut self.search);
            self.copy_mode = search.copy_origin.take().unwrap();
            self.copy_search_cursor = search.cursor;
            if let Some(generation) = self.copy_mode.generation {
                self.apply_copy_target(
                    generation,
                    Motion::CharacterJump,
                    self.copy_mode.cursor,
                    false,
                );
            }
            self.wake.context.request_repaint();
        } else {
            self.close_search();
        }
    }

    fn prepare_search_snapshot(&mut self) {
        self.pending_search_snapshot = None;
        let host = self.search_host();
        let mut snapshot_generation = 0;
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_begin(host, &mut snapshot_generation) }
        {
            self.close_search();
            self.show_error(
                "Search unavailable",
                "Unable to create terminal snapshot".into(),
            );
            return;
        }

        let cursor = unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_cursor_new(host, snapshot_generation)
        };
        if cursor.is_null() {
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(host, snapshot_generation) };
            self.close_search();
            self.show_error(
                "Search unavailable",
                "Unable to create snapshot cursor".into(),
            );
            return;
        }

        self.search.snapshot_generation = Some(snapshot_generation);
        self.search.cursor = cursor as usize;
        self.queue_search();
    }

    fn close_search(&mut self) {
        self.pending_search_snapshot = None;
        let host = self.search_host();
        if self.search.cursor != 0 {
            self.search_worker.cancel_and_wait();
            unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_cursor_free(self.search.cursor as *mut _)
            };
        } else {
            self.search_worker.cancel();
        }
        if let Some(snapshot_generation) = self.search.snapshot_generation.take() {
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(host, snapshot_generation) };
        }
        self.search = SearchUi::default();
    }

    fn search_host(&self) -> *mut Host {
        self.host
    }

    fn open_copy_mode(&mut self) {
        self.close_search();
        let mut generation = 0;
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_begin(self.host, &mut generation) } {
            self.show_error(
                "Copy mode failed",
                "Could not capture the terminal contents. Try again.".into(),
            );
            return;
        }
        let mut state = SnapshotView::default();
        let mut actual = SnapshotCell::default();
        let ready = unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut state)
                && ghostty_ffi::ghostty_egui_host_snapshot_resolve_cell(
                    self.host,
                    generation,
                    state.cursor,
                    false,
                    &mut actual,
                )
        };
        if !ready || !self.publish_copy_decoration(generation, actual, true) {
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
            self.show_error(
                "Copy mode failed",
                "Could not locate the cursor in the saved terminal view. Try again.".into(),
            );
            return;
        }
        state.cursor = actual;
        self.copy_mode.start(generation, state);
        self.prompt_boundaries = None;
        self.wake.context.request_repaint();
    }

    fn accept_search(&mut self) {
        // A single-line TextEdit can surrender focus on Enter. Keep search
        // usable if no match is ready yet or the query has no valid result.
        self.search.focus_requested = true;
        self.wake.context.request_repaint();
        if self.search.waiting || self.pending_search_snapshot.is_some() {
            self.search.accept_requested = true;
            return;
        }
        self.search.accept_requested = false;
        let Some(target) = self.search.copy_target() else {
            return;
        };
        let Some(generation) = self.search.snapshot_generation else {
            return;
        };
        let host = self.search_host();
        let mut view = SnapshotView::default();
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view) } {
            self.close_search();
            self.show_error(
                "Search navigation failed",
                "The saved search view has expired. Start a new search.".into(),
            );
            return;
        }
        let mut actual = SnapshotCell::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_resolve_cell(
                host,
                generation,
                target,
                false,
                &mut actual,
            )
        } {
            self.close_search();
            self.show_error(
                "Search navigation failed",
                "Unable to resolve the selected match".into(),
            );
            return;
        }
        self.search_worker.cancel_and_wait();
        // Transfer ownership instead of closing search: ending this snapshot
        // would invalidate the match coordinates and briefly show live output.
        let search = std::mem::take(&mut self.search);
        self.copy_search_query = Some((search.query.clone(), search.regex));
        view.cursor = actual;
        self.copy_mode.start(generation, view);
        self.copy_search_cursor = search.cursor;
        if search.copy_origin.is_some() {
            self.copy_mode
                .begin_search(search.query_revision, !search.backward, 1);
            self.copy_mode
                .finish_search(search.query_revision, search.matches);
        } else {
            self.copy_mode
                .adopt_search(search.query_revision, search.matches);
        }
        self.pending_search_snapshot = None;
        self.prompt_boundaries = None;
        self.wake.context.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        if !self.publish_copy_decoration(generation, actual, true) {
            self.close_copy_mode();
        }
        self.wake.context.request_repaint();
    }

    fn close_copy_mode(&mut self) {
        self.picker = None;
        self.quick_select = None;
        if let Some(generation) = self.copy_mode.generation {
            if self.copy_search_cursor != 0 {
                self.search_worker.cancel_and_wait();
                unsafe {
                    ghostty_ffi::ghostty_egui_host_snapshot_cursor_free(
                        self.copy_search_cursor as *mut _,
                    )
                };
            }
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
            let padding = self.copy_mode.scroll_padding();
            self.copy_mode = CopyMode::default();
            self.copy_mode.set_scroll_padding(padding);
            self.copy_search_cursor = 0;
            self.copy_search_query = None;
            self.copy_search_refresh = None;
            self.prompt_boundaries = None;
            self.wake.context.request_repaint();
        }
    }

    fn handle_copy_keys(
        &mut self,
        ui: &egui::Ui,
        bindings: &crate::picker::config::Bindings,
    ) -> bool {
        if self.search.active {
            return false;
        }
        let was_active = self.copy_mode.generation.is_some();
        let events = ui.input(|input| input.events.clone());
        self.copy_mode.begin_input_batch();
        for (event_index, event) in events.iter().enumerate() {
            if self.copy_mode.generation.is_none() {
                break;
            }
            let action = self.copy_mode.input(event);
            let generation = self.copy_mode.generation.expect("copy mode is active");
            let mut view = SnapshotView::default();
            if !unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view)
            } {
                self.copy_controller_error("The copy-mode snapshot is no longer available".into());
                break;
            }
            if matches!(
                &action,
                CopyAction::SelectObject {
                    kind: CopyTextObject::Command,
                    ..
                } | CopyAction::Yank(YankCommand::Object {
                    kind: CopyTextObject::Command,
                    ..
                })
            ) && !self.ensure_prompt_boundaries(generation)
            {
                self.copy_controller_error("Unable to read command boundaries".into());
                break;
            }
            let document = SnapshotNavigationDocument {
                host: self.host,
                generation,
                rows: view.total_rows,
                boundaries: self
                    .prompt_boundaries
                    .as_ref()
                    .map(|(_, rows)| rows.as_slice()),
            };
            let update = match nfm_egui::copy_controller::CopyController::new(
                &mut self.copy_mode,
                &document,
                view,
            )
            .execute(action)
            {
                Ok(update) => update,
                Err(error) => {
                    self.copy_controller_error(error);
                    break;
                }
            };
            let begins_search = update.effects.iter().any(|effect| {
                matches!(
                    effect,
                    nfm_egui::copy_controller::CopyEffect::BeginSearch { .. }
                )
            });
            let success = self.apply_copy_update(update, generation, ui.ctx(), Some(bindings));
            if begins_search {
                // Egui may emit a key and its text together. Neither trigger
                // belongs in the newly opened search prompt.
                let mut consumed = event_index + 1;
                if matches!(event, egui::Event::Key { .. })
                    && matches!(events.get(consumed), Some(egui::Event::Text(text)) if text == "/" || text == "?")
                {
                    consumed += 1;
                }
                ui.ctx().input_mut(|input| {
                    input.events.drain(..consumed);
                });
            }
            if !success {
                break;
            }
        }
        was_active || self.copy_mode.generation.is_some()
    }

    fn copy_controller_error(&mut self, error: String) {
        self.close_copy_mode();
        self.show_error("Copy navigation failed", error);
    }

    fn apply_copy_update(
        &mut self,
        update: nfm_egui::copy_controller::CopyUpdate,
        generation: u64,
        ctx: &egui::Context,
        bindings: Option<&crate::picker::config::Bindings>,
    ) -> bool {
        if update.changed {
            let viewport = SnapshotViewportOptions {
                reveal: update.reveal.as_ref().map_or(std::ptr::null(), |cell| cell),
                top_row: update
                    .viewport_top
                    .as_ref()
                    .filter(|_| update.reveal.is_none())
                    .map_or(std::ptr::null(), |top| top),
                padding_rows: self.copy_mode.scroll_padding(),
                ..Default::default()
            };
            if !self.publish_copy_decoration_with_viewport(
                generation,
                self.copy_mode.cursor,
                &viewport,
            ) {
                self.copy_controller_error("Unable to publish copy-mode viewport".into());
                return false;
            }
            ctx.request_repaint();
        }
        if let Some(delay) = update.repaint_after {
            ctx.request_repaint_after(delay);
        }
        for effect in update.effects {
            use nfm_egui::copy_controller::CopyEffect;
            match effect {
                CopyEffect::BeginSearch { forward } => {
                    self.open_copy_search(!forward);
                    return false;
                }
                CopyEffect::Exit => {
                    self.close_copy_mode();
                    return false;
                }
                CopyEffect::CopyText { text, .. } => ctx.copy_text(text),
                CopyEffect::Search {
                    pattern,
                    forward,
                    count,
                    regex,
                } => {
                    self.submit_copy_search(pattern, forward, count, regex);
                }
                CopyEffect::ClearSearch { cancel_pending } => {
                    self.copy_search_query = None;
                    self.copy_search_refresh = None;
                    if cancel_pending {
                        self.search_worker.cancel();
                    }
                }
                CopyEffect::QuickSelect { origin } => {
                    self.open_quick_select();
                    if let Some(quick) = &mut self.quick_select {
                        quick.purpose = QuickSelectPurpose::YankLines { origin };
                    }
                }
                CopyEffect::OpenLineSplit { cursor } => {
                    let Some(bindings) = bindings else {
                        self.show_error(
                            "Could not split this line",
                            "Picker bindings unavailable".into(),
                        );
                        return false;
                    };
                    let mut view = SnapshotView::default();
                    let picker = if unsafe {
                        ghostty_ffi::ghostty_egui_host_snapshot_view(
                            self.host, generation, &mut view,
                        )
                    } {
                        crate::picker::Picker::start_line_split(
                            self.host,
                            generation,
                            view,
                            cursor.y,
                            ctx.clone(),
                            bindings.builtin_pickers,
                        )
                    } else {
                        Err("Unable to read the copy-mode snapshot".into())
                    };
                    match picker {
                        Ok(picker) => self.picker = Some(picker),
                        Err(error) => self.show_error("Could not split this line", error),
                    }
                    ctx.request_repaint();
                    return false;
                }
            }
        }
        true
    }

    fn apply_copy_target(
        &mut self,
        generation: u64,
        motion: Motion,
        requested: SnapshotCell,
        snap_right: bool,
    ) -> bool {
        if let Some(actual) = self.resolve_copy_cell(generation, requested, snap_right)
            && self.publish_copy_decoration(generation, actual, true)
        {
            let host = self.host;
            self.copy_mode.moved_with_rows(motion, actual, |row| {
                Self::read_snapshot_row_from(host, generation, row)
            });
            true
        } else {
            self.close_copy_mode();
            self.show_error(
                "Copy navigation failed",
                "This position is no longer available. Exit copy mode and try again.".into(),
            );
            false
        }
    }

    fn resolve_copy_cell(
        &self,
        generation: u64,
        requested: SnapshotCell,
        snap_right: bool,
    ) -> Option<SnapshotCell> {
        let mut actual = SnapshotCell::default();
        unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_resolve_cell(
                self.host,
                generation,
                requested,
                snap_right,
                &mut actual,
            )
        }
        .then_some(actual)
    }

    fn recent_picker_lines(&self) -> Vec<String> {
        let mut generation = self.copy_mode.generation.unwrap_or_default();
        let temporary = self.copy_mode.generation.is_none();
        if temporary
            && !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_begin(self.host, &mut generation) }
        {
            return Vec::new();
        }
        let mut view = SnapshotView::default();
        let mut lines = Vec::new();
        if unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view) }
        {
            // Bound the synchronous read even when the terminal has huge scrollback.
            for row in (view.total_rows.saturating_sub(256)..view.total_rows).rev() {
                if let Some(row) = self.read_snapshot_row(generation, row) {
                    let text = row.text.trim_end().to_owned();
                    if !text.trim().is_empty() {
                        lines.push(text);
                        if lines.len() == 10 {
                            break;
                        }
                    }
                }
            }
            lines.reverse();
        }
        if temporary {
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
        }
        lines
    }

    fn read_snapshot_row(&self, generation: u64, row: u64) -> Option<TextRow> {
        Self::read_snapshot_row_from(self.host, generation, row)
    }

    fn read_snapshot_row_from(host: *mut Host, generation: u64, row: u64) -> Option<TextRow> {
        let mut result = SnapshotTextRow::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_text_row(host, generation, row, &mut result)
        } {
            return None;
        }
        let (text, columns) = if result.len == 0 {
            (Vec::new(), Vec::new())
        } else {
            (
                unsafe { std::slice::from_raw_parts(result.text, result.len) }.to_vec(),
                unsafe { std::slice::from_raw_parts(result.columns, result.len) }.to_vec(),
            )
        };
        let wraps_to_next = result.wraps_to_next;
        unsafe { ghostty_ffi::ghostty_egui_host_snapshot_text_row_free(&mut result) };
        Some(TextRow {
            text: String::from_utf8(text).ok()?,
            columns,
            wraps_to_next,
        })
    }

    fn ensure_prompt_boundaries(&mut self, generation: u64) -> bool {
        if self
            .prompt_boundaries
            .as_ref()
            .is_some_and(|(cached, _)| *cached == generation)
        {
            return true;
        }
        let Some(rows) = find_prompt_boundaries(self.copy_mode.total_rows(), |row| {
            self.read_snapshot_row(generation, row)
        }) else {
            return false;
        };
        self.prompt_boundaries = Some((generation, rows));
        true
    }

    fn navigate_copy_command(&mut self, forward: bool, count: u32) -> bool {
        let Some(generation) = self.copy_mode.generation else {
            return false;
        };
        if !self.ensure_prompt_boundaries(generation) {
            return false;
        }
        let target = self.copy_mode.command_target(
            forward,
            count,
            &self.prompt_boundaries.as_ref().unwrap().1,
        );
        // All configured prompt patterns are anchored at column zero. Reset
        // the preferred column just as a row-start motion does.
        target.is_none_or(|cell| self.apply_copy_target(generation, Motion::RowStart, cell, false))
    }

    fn queue_search(&mut self) {
        let Some(snapshot_generation) = self.search.snapshot_generation else {
            return;
        };
        let (query, regex, cursor) = (
            self.search.query.clone(),
            self.search.regex,
            self.search.cursor,
        );
        self.search.query_revision =
            match self
                .search_worker
                .submit(snapshot_generation, query, regex, cursor)
            {
                Ok(revision) => revision,
                Err(error) => {
                    self.close_search();
                    self.show_error("Search unavailable", error);
                    return;
                }
            };
        self.search.waiting = true;
    }

    fn apply_search_result(&mut self) {
        if self.quick_select.is_some() || self.picker.is_some() {
            return;
        }
        let result = match self.search_worker.take_latest_result() {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                self.copy_mode.cancel_pending_search();
                self.close_search();
                self.show_error("Search failed", error);
                return;
            }
        };
        if self.copy_mode.generation == Some(result.document_revision)
            && self.copy_search_refresh == Some(result.query_revision)
        {
            self.copy_search_refresh = None;
            if let Some(error) = result.error {
                self.show_error("Copy search failed", error);
            } else {
                self.copy_mode
                    .adopt_search(result.query_revision, result.matches);
                self.publish_copy_decoration(
                    result.document_revision,
                    self.copy_mode.cursor,
                    false,
                );
            }
            return;
        }
        if self.copy_mode.generation == Some(result.document_revision)
            && self.copy_mode.search_accepts_result(result.query_revision)
        {
            self.finish_copy_search_result(result);
            return;
        }
        if !self.search.active
            || self.search.snapshot_generation != Some(result.document_revision)
            || self.search.query_revision != result.query_revision
        {
            return;
        }
        self.search.apply_result(result.matches, result.error);
        self.publish_search_matches(true);
        if self.search.accept_requested {
            self.accept_search();
        }
    }

    fn finish_copy_search_result(&mut self, result: crate::search::SearchResult) {
        if let Some(error) = result.error {
            self.copy_mode.cancel_pending_search();
            self.show_error("Copy search failed", error);
            return;
        }
        let mut view = SnapshotView::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(
                self.host,
                result.document_revision,
                &mut view,
            )
        } {
            self.copy_controller_error("Copy-search snapshot expired".into());
            return;
        }
        let document = SnapshotNavigationDocument {
            host: self.host,
            generation: result.document_revision,
            rows: view.total_rows,
            boundaries: None,
        };
        let update =
            nfm_egui::copy_controller::CopyController::new(&mut self.copy_mode, &document, view)
                .finish_search(result.query_revision, result.matches, None);
        match update {
            Ok(update) => {
                let ctx = self.wake.context.clone();
                self.apply_copy_update(update, result.document_revision, &ctx, None);
            }
            Err(error) => self.copy_controller_error(error),
        }
    }

    fn navigate_search(&mut self, previous: bool) {
        self.search.navigate(previous);
        self.publish_search_matches(true);
    }

    fn submit_copy_search(&mut self, pattern: String, forward: bool, count: u32, regex: bool) {
        self.copy_search_query = Some((pattern.clone(), regex));
        self.copy_search_refresh = None;
        let Some(generation) = self.copy_mode.generation else {
            return;
        };
        if self.copy_search_cursor == 0 {
            let cursor = unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_cursor_new(self.host, generation)
            };
            if cursor.is_null() {
                return;
            }
            self.copy_search_cursor = cursor as usize;
        }
        let query_revision =
            match self
                .search_worker
                .submit(generation, pattern, regex, self.copy_search_cursor)
            {
                Ok(revision) => revision,
                Err(error) => {
                    self.copy_mode.cancel_pending_search();
                    self.show_error("Search unavailable", error);
                    return;
                }
            };
        self.copy_mode.begin_search(query_revision, forward, count);
        self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
    }

    fn publish_copy_decoration(&self, generation: u64, cell: SnapshotCell, reveal: bool) -> bool {
        let viewport = SnapshotViewportOptions {
            reveal: if reveal { &cell } else { std::ptr::null() },
            padding_rows: self.copy_mode.scroll_padding(),
            ..Default::default()
        };
        self.publish_copy_decoration_with_viewport(generation, cell, &viewport)
    }

    fn publish_copy_decoration_with_viewport(
        &self,
        generation: u64,
        cell: SnapshotCell,
        viewport: *const SnapshotViewportOptions,
    ) -> bool {
        let range = SnapshotCellRange {
            start: cell,
            end: cell,
        };
        let caret_layer = SnapshotDecorationLayer {
            ranges: &range,
            range_count: 1,
            style: SnapshotDecorationStyle {
                foreground_rgb: crate::colors::rgb(crate::colors::palette().background),
                background_rgb: crate::colors::rgb(
                    crate::colors::palette()
                        .copy_cursor_background
                        .unwrap_or(crate::colors::palette().accent),
                ),
                flags: 3,
            },
        };
        let selection = self
            .copy_mode
            .visual_selection_to(cell, |row| self.read_snapshot_row(generation, row));
        let selection_ranges = self
            .copy_mode
            .reflow_selection_ranges()
            .map(<[_]>::to_vec)
            .unwrap_or_else(|| {
                self.copy_decoration_ranges(
                    generation,
                    selection.as_ref().map(|selection| selection.range),
                    selection
                        .as_ref()
                        .is_some_and(|selection| selection.rectangle),
                    viewport,
                )
            });
        let selection_layer = selection.map(|_| SnapshotDecorationLayer {
            ranges: selection_ranges.as_ptr(),
            range_count: selection_ranges.len(),
            style: copy_selection_style(),
        });
        let matches_layer = SnapshotDecorationLayer {
            ranges: self.copy_mode.search_matches().as_ptr(),
            range_count: self.copy_mode.search_matches().len(),
            style: SnapshotDecorationStyle {
                foreground_rgb: crate::colors::rgb(crate::colors::palette().search_match_text),
                background_rgb: crate::colors::rgb(
                    crate::colors::palette().search_match_background,
                ),
                flags: 3,
            },
        };
        let yank = self.copy_mode.yank_highlight_range();
        let yank_ranges = self.copy_decoration_ranges(
            generation,
            yank,
            self.copy_mode.yank_highlight_is_rectangle(),
            viewport,
        );
        let yank_layer = yank.map(|_| SnapshotDecorationLayer {
            ranges: yank_ranges.as_ptr(),
            range_count: yank_ranges.len(),
            style: SnapshotDecorationStyle {
                foreground_rgb: crate::colors::rgb(crate::colors::palette().yank_text),
                background_rgb: crate::colors::rgb(crate::colors::palette().yank_background),
                flags: 3,
            },
        });
        let active_match = self.copy_mode.search_matches().iter().find(|range| {
            (range.start.y, range.start.x) <= (cell.y, cell.x)
                && (cell.y, cell.x) <= (range.end.y, range.end.x)
        });
        let mut layers =
            copy_decoration_layers(caret_layer, selection_layer, yank_layer, matches_layer);
        if let Some(range) = active_match {
            // Earlier layers win. Keep cursor/yank precedence, but color the
            // whole active match before ordinary search matches.
            let index = layers.len() - 1;
            layers.insert(index, active_search_layer(range));
        }
        unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_apply_decorations(
                self.host,
                generation,
                layers.as_ptr(),
                layers.len(),
                viewport,
            )
        }
    }

    fn copy_decoration_ranges(
        &self,
        generation: u64,
        range: Option<SnapshotCellRange>,
        rectangle: bool,
        viewport: *const SnapshotViewportOptions,
    ) -> Vec<SnapshotCellRange> {
        let Some(range) = range else {
            return Vec::new();
        };
        if !rectangle {
            return vec![range];
        }
        let mut view = SnapshotView::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view)
        } || view.viewport_rows == 0
        {
            return Vec::new();
        }
        let height = u64::from(view.viewport_rows);
        let max_top = view.total_rows.saturating_sub(height);
        let mut top = view.viewport_top.min(max_top);
        // These pointers borrow local values for this decoration call only.
        if let Some(options) = unsafe { viewport.as_ref() } {
            if let Some(requested) = unsafe { options.top_row.as_ref() } {
                top = (*requested).min(max_top);
            } else if let Some(cell) = unsafe { options.reveal.as_ref() } {
                let padding = u64::from(options.padding_rows).min((height - 1) / 2);
                if cell.y < top + padding {
                    top = cell.y.saturating_sub(padding);
                } else if cell.y > top + height - 1 - padding {
                    top = cell.y.saturating_sub(height - 1 - padding);
                }
                top = top.min(max_top);
            }
        }
        nfm_egui::copy_mode::rectangle_row_ranges(range, top, height)
    }

    fn publish_search_matches(&self, reveal_selected: bool) {
        let Some(snapshot_generation) = self.search.snapshot_generation else {
            return;
        };
        let selected = self.search.selected_range();
        let viewport = SnapshotViewportOptions {
            reveal: selected
                .filter(|_| reveal_selected)
                .map_or(std::ptr::null(), |range| &range.start),
            padding_rows: self.copy_mode.scroll_padding(),
            ..Default::default()
        };
        let selected_layers = selected.map(active_search_layer);
        let matches_layer = SnapshotDecorationLayer {
            ranges: self.search.matches.as_ptr(),
            range_count: self.search.matches.len(),
            style: SnapshotDecorationStyle {
                foreground_rgb: crate::colors::rgb(crate::colors::palette().search_match_text),
                background_rgb: crate::colors::rgb(
                    crate::colors::palette().search_match_background,
                ),
                flags: 3,
            },
        };
        let mut layers = Vec::with_capacity(2);
        if let Some(layer) = selected_layers {
            layers.push(layer);
        }
        layers.push(matches_layer);
        unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_apply_decorations(
                self.search_host(),
                snapshot_generation,
                layers.as_ptr(),
                layers.len(),
                &viewport,
            );
        }
    }

    pub(super) fn show_search_overlay(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let action = self
            .search
            .show(ctx, bounds, self.pending_search_snapshot.is_some(), true);
        if action.changed {
            self.search.accept_requested = false;
            self.queue_search();
        }
        if let Some(previous) = action.navigate {
            self.navigate_search(previous);
        }
        if action.close {
            self.cancel_search();
        } else if action.accept {
            self.accept_search();
        }
    }
}

// The renderer uses the first matching decoration. Yank feedback must cover the
// caret, even when the copied range contains only the caret's cell.
fn copy_selection_style() -> SnapshotDecorationStyle {
    SnapshotDecorationStyle {
        foreground_rgb: 0,
        background_rgb: crate::colors::rgb(
            nfm_egui::Palette::default().preview_selection_background,
        ),
        // Background only: let Ghostty retain each cell's syntax/ANSI foreground.
        flags: 2,
    }
}

fn active_search_layer(range: &SnapshotCellRange) -> SnapshotDecorationLayer {
    let palette = crate::colors::palette();
    SnapshotDecorationLayer {
        ranges: std::ptr::from_ref(range),
        range_count: 1,
        style: SnapshotDecorationStyle {
            foreground_rgb: crate::colors::rgb(palette.background),
            background_rgb: crate::colors::rgb(palette.match_highlight),
            flags: 3,
        },
    }
}

fn copy_decoration_layers(
    caret: SnapshotDecorationLayer,
    selection: Option<SnapshotDecorationLayer>,
    yank: Option<SnapshotDecorationLayer>,
    matches: SnapshotDecorationLayer,
) -> Vec<SnapshotDecorationLayer> {
    let mut layers = Vec::with_capacity(4);
    if let Some(layer) = yank {
        layers.push(layer);
    }
    layers.push(caret);
    if let Some(layer) = selection {
        layers.push(layer);
    }
    layers.push(matches);
    layers
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotDecorationOwner {
    QuickSelect(u64),
    Search(u64),
    Copy(u64),
}

fn snapshot_decoration_refresh(
    quick: Option<u64>,
    search: Option<u64>,
    copy: Option<u64>,
    needs_refresh: impl FnOnce(u64) -> bool,
) -> Option<SnapshotDecorationOwner> {
    let owner = quick
        .map(SnapshotDecorationOwner::QuickSelect)
        .or_else(|| search.map(SnapshotDecorationOwner::Search))
        .or_else(|| copy.map(SnapshotDecorationOwner::Copy))?;
    let generation = match owner {
        SnapshotDecorationOwner::QuickSelect(generation)
        | SnapshotDecorationOwner::Search(generation)
        | SnapshotDecorationOwner::Copy(generation) => generation,
    };
    needs_refresh(generation).then_some(owner)
}

#[cfg(test)]
mod copy_shortcut_tests {
    #[test]
    fn quick_select_labels_refresh_after_live_snapshot_handoff_without_navigation() {
        use super::*;
        // Quick select also holds copy-mode state for the same snapshot. Once
        // native code invalidates that snapshot's decorations, only hints own
        // the refresh; a copy repaint would erase them.
        assert_eq!(
            snapshot_decoration_refresh(Some(7), None, Some(7), |_| false),
            None
        );
        assert_eq!(
            snapshot_decoration_refresh(Some(7), None, Some(7), |generation| generation == 7),
            Some(SnapshotDecorationOwner::QuickSelect(7))
        );
        assert_eq!(
            snapshot_decoration_refresh(Some(7), Some(6), Some(7), |generation| generation == 7),
            Some(SnapshotDecorationOwner::QuickSelect(7))
        );
        assert_eq!(
            snapshot_decoration_refresh(None, None, Some(7), |_| true),
            Some(SnapshotDecorationOwner::Copy(7))
        );
    }

    #[test]
    fn unchanged_snapshot_does_not_republish_or_fall_through_to_other_modes() {
        use super::*;
        assert_eq!(
            snapshot_decoration_refresh(Some(7), Some(6), Some(7), |generation| {
                assert_eq!(generation, 7);
                false
            }),
            None
        );
        assert_eq!(
            snapshot_decoration_refresh(None, Some(7), Some(7), |_| true),
            Some(SnapshotDecorationOwner::Search(7))
        );
        assert_eq!(
            snapshot_decoration_refresh(None, None, None, |_| panic!("no active snapshot")),
            None
        );
    }
    #[test]
    fn active_search_layer_uses_full_match_and_distinct_configured_color() {
        use super::*;
        let range = SnapshotCellRange {
            start: SnapshotCell { x: 3, y: 2 },
            end: SnapshotCell { x: 8, y: 3 },
        };
        let layer = active_search_layer(&range);
        assert_eq!(layer.ranges, std::ptr::from_ref(&range));
        assert_eq!(layer.range_count, 1);
        assert_eq!(layer.style.flags, 3);
        assert_eq!(
            layer.style.background_rgb,
            crate::colors::rgb(crate::colors::palette().match_highlight)
        );
        assert_eq!(
            layer.style.foreground_rgb,
            crate::colors::rgb(crate::colors::palette().background)
        );
    }

    #[test]
    fn single_cell_yank_takes_precedence_over_caret_and_restores_after_expiry() {
        use super::*;
        let range = SnapshotCellRange {
            start: SnapshotCell { x: 0, y: 0 },
            end: SnapshotCell { x: 0, y: 0 },
        };
        let layer = |color| SnapshotDecorationLayer {
            ranges: &range,
            range_count: 1,
            style: SnapshotDecorationStyle {
                foreground_rgb: 0,
                background_rgb: color,
                flags: 3,
            },
        };
        let active = copy_decoration_layers(layer(1), Some(layer(2)), Some(layer(3)), layer(4));
        assert_eq!(active[0].style.background_rgb, 3);
        assert_eq!(active[1].style.background_rgb, 1);
        assert_eq!(active[0].ranges, &range as *const _);
        assert_eq!(active[0].range_count, 1);
        let expired = copy_decoration_layers(layer(1), Some(layer(2)), None, layer(4));
        assert_eq!(expired[0].style.background_rgb, 1);
        assert_eq!(expired.len(), 3);
    }
    #[test]
    fn scroll_shortcuts_accept_platform_command_alias_for_control() {
        use super::copy_scroll_direction;
        for command in [false, true] {
            let modifiers = egui::Modifiers {
                ctrl: true,
                command,
                ..Default::default()
            };
            assert_eq!(copy_scroll_direction(egui::Key::E, modifiers), Some(true));
            assert_eq!(copy_scroll_direction(egui::Key::Y, modifiers), Some(false));
            assert_eq!(copy_scroll_direction(egui::Key::U, modifiers), None);
            for invalid in [
                egui::Modifiers {
                    shift: true,
                    ..modifiers
                },
                egui::Modifiers {
                    alt: true,
                    ..modifiers
                },
                egui::Modifiers {
                    ctrl: false,
                    ..modifiers
                },
            ] {
                assert_eq!(copy_scroll_direction(egui::Key::E, invalid), None);
                assert_eq!(copy_scroll_direction(egui::Key::Y, invalid), None);
            }
        }
    }

    #[test]
    fn viewport_position_keys_require_unmodified_z_suffixes() {
        use super::{ViewportPosition, copy_viewport_position_key};
        for (key, position) in [
            (egui::Key::T, ViewportPosition::Top),
            (egui::Key::Z, ViewportPosition::Center),
            (egui::Key::B, ViewportPosition::Bottom),
        ] {
            assert_eq!(
                copy_viewport_position_key(key, egui::Modifiers::NONE),
                Some(position)
            );
            assert_eq!(copy_viewport_position_key(key, egui::Modifiers::CTRL), None);
            assert_eq!(
                copy_viewport_position_key(key, egui::Modifiers::SHIFT),
                None
            );
        }
        assert_eq!(
            copy_viewport_position_key(egui::Key::W, egui::Modifiers::NONE),
            None
        );
    }

    use super::{
        CopyTextObject, copy_digit, copy_jump_prefix, copy_motion, copy_motion_with_prefix,
        copy_text_object_key, is_copy_modifier_key,
    };

    #[test]
    fn standalone_modifier_events_do_not_consume_yank_targets() {
        for key in [
            egui::Key::ShiftLeft,
            egui::Key::ShiftRight,
            egui::Key::ControlLeft,
            egui::Key::ControlRight,
            egui::Key::AltLeft,
            egui::Key::AltRight,
            egui::Key::SuperLeft,
            egui::Key::SuperRight,
        ] {
            assert!(is_copy_modifier_key(key));
        }
        assert!(!is_copy_modifier_key(egui::Key::W));
        assert!(!is_copy_modifier_key(egui::Key::Escape));
    }

    #[test]
    fn yank_text_object_keys_include_shifted_big_words_and_delimiters() {
        let shift = egui::Modifiers::SHIFT;
        assert!(matches!(
            copy_text_object_key(egui::Key::W, shift),
            Some(CopyTextObject::Word(true))
        ));
        assert!(matches!(
            copy_text_object_key(egui::Key::W, egui::Modifiers::NONE),
            Some(CopyTextObject::Word(false))
        ));
        assert!(matches!(
            copy_text_object_key(egui::Key::Quote, shift),
            Some(CopyTextObject::Quote('"'))
        ));
        assert!(matches!(
            copy_text_object_key(egui::Key::OpenCurlyBracket, shift),
            Some(CopyTextObject::Delimiter('{', '}'))
        ));
        assert!(matches!(
            copy_text_object_key(egui::Key::C, egui::Modifiers::NONE),
            Some(CopyTextObject::Command)
        ));
    }

    #[test]
    fn viewport_and_page_keys_respect_modifiers() {
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        assert!(matches!(
            copy_motion(egui::Key::H, shift),
            Some(nfm_egui::copy_mode::Motion::ViewportTop)
        ));
        assert!(matches!(
            copy_motion(egui::Key::M, shift),
            Some(nfm_egui::copy_mode::Motion::ViewportMiddle)
        ));
        assert!(matches!(
            copy_motion(egui::Key::L, shift),
            Some(nfm_egui::copy_mode::Motion::ViewportBottom)
        ));
        assert!(matches!(
            copy_motion(egui::Key::PageUp, egui::Modifiers::NONE),
            Some(nfm_egui::copy_mode::Motion::PageUp)
        ));
        assert!(matches!(
            copy_motion(egui::Key::PageDown, egui::Modifiers::NONE),
            Some(nfm_egui::copy_mode::Motion::PageDown)
        ));
        assert!(matches!(
            copy_motion(egui::Key::U, ctrl),
            Some(nfm_egui::copy_mode::Motion::HalfPageUp)
        ));
        assert!(matches!(
            copy_motion(egui::Key::D, ctrl),
            Some(nfm_egui::copy_mode::Motion::HalfPageDown)
        ));
        assert!(copy_motion(egui::Key::H, ctrl).is_none());
    }

    #[test]
    fn text_motion_keys_are_copy_mode_only() {
        use nfm_egui::copy_mode::Motion;
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            copy_motion(egui::Key::Num6, shift),
            Some(Motion::FirstNonblank)
        );
        assert_eq!(
            copy_motion(egui::Key::Num4, shift),
            Some(Motion::LastNonblank)
        );
        assert_eq!(
            copy_motion(egui::Key::W, egui::Modifiers::NONE),
            Some(Motion::ForwardWord)
        );
        assert_eq!(
            copy_motion(egui::Key::B, egui::Modifiers::NONE),
            Some(Motion::BackwardWord)
        );
        assert_eq!(
            copy_motion(egui::Key::E, egui::Modifiers::NONE),
            Some(Motion::ForwardWordEnd)
        );
        assert_eq!(
            copy_motion(egui::Key::Num0, egui::Modifiers::NONE),
            Some(Motion::RowStart)
        );
        assert_eq!(
            copy_motion(egui::Key::W, shift),
            Some(Motion::ForwardBigWord)
        );
        assert_eq!(
            copy_motion(egui::Key::B, shift),
            Some(Motion::BackwardBigWord)
        );
        assert_eq!(
            copy_motion(egui::Key::E, shift),
            Some(Motion::ForwardBigWordEnd)
        );
        assert_eq!(
            copy_motion(egui::Key::G, shift),
            Some(Motion::ScrollbackBottom)
        );
        assert_eq!(
            copy_motion(egui::Key::Num5, shift),
            Some(Motion::MatchingBracket)
        );
        assert_eq!(
            copy_jump_prefix(egui::Key::F, egui::Modifiers::NONE),
            Some((true, false))
        );
        assert_eq!(copy_jump_prefix(egui::Key::F, shift), Some((false, false)));
        assert_eq!(
            copy_jump_prefix(egui::Key::T, egui::Modifiers::NONE),
            Some((true, true))
        );
        assert_eq!(copy_jump_prefix(egui::Key::T, shift), Some((false, true)));
    }

    #[test]
    fn gg_prefix_moves_to_scrollback_top_and_other_keys_cancel_it() {
        use nfm_egui::copy_mode::{CopyMode, Motion};
        let mut mode = CopyMode::default();
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::G, egui::Modifiers::NONE),
            None
        );
        assert!(mode.pending_g);
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::G, egui::Modifiers::NONE),
            Some(Motion::ScrollbackTop)
        );
        assert!(!mode.pending_g);
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::G, egui::Modifiers::NONE),
            None
        );
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::H, egui::Modifiers::NONE),
            Some(Motion::Left)
        );
        assert!(!mode.pending_g);
    }

    #[test]
    fn next_copy_motions_and_digits_have_distinct_bindings() {
        use nfm_egui::copy_mode::{CopyMode, Motion};
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(copy_digit(egui::Key::Num0), Some(0));
        assert_eq!(copy_digit(egui::Key::Num9), Some(9));
        assert_eq!(
            copy_motion(egui::Key::Plus, egui::Modifiers::NONE),
            Some(Motion::NextLineNonblank)
        );
        assert_eq!(
            copy_motion(egui::Key::Minus, egui::Modifiers::NONE),
            Some(Motion::PreviousLineNonblank)
        );
        assert_eq!(
            copy_motion(egui::Key::Minus, shift),
            Some(Motion::LineNonblank)
        );
        let mut mode = CopyMode::default();
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::G, egui::Modifiers::NONE),
            None
        );
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::E, egui::Modifiers::NONE),
            Some(Motion::PreviousWordEnd)
        );
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::G, egui::Modifiers::NONE),
            None
        );
        assert_eq!(
            copy_motion_with_prefix(&mut mode, egui::Key::E, shift),
            Some(Motion::PreviousBigWordEnd)
        );
    }
}

struct NativeContext {
    window: *mut c_void,
    display: *mut c_void,
    context: *mut c_void,
}

#[cfg(target_os = "windows")]
fn current_native_context() -> NativeContext {
    unsafe extern "system" {
        fn wglGetCurrentContext() -> *mut c_void;
        fn wglGetCurrentDC() -> *mut c_void;
        fn WindowFromDC(dc: *mut c_void) -> *mut c_void;
    }
    unsafe {
        NativeContext {
            window: WindowFromDC(wglGetCurrentDC()),
            display: std::ptr::null_mut(),
            context: wglGetCurrentContext(),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn current_native_context() -> NativeContext {
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetCurrentDisplay() -> *mut c_void;
        fn eglGetCurrentContext() -> *mut c_void;
    }
    unsafe {
        NativeContext {
            window: std::ptr::null_mut(),
            display: eglGetCurrentDisplay(),
            context: eglGetCurrentContext(),
        }
    }
}

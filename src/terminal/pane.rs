use crate::app::UserEvent;
use crate::copy_mode::{
    CopyAction, CopyMode, CopySelection, CopyTextObject, Jump, Motion, MotionEvaluation,
    MotionResult, SCROLL_PADDING_ROWS, TextRow, ViewportPosition, VisualMode, WordObject,
    YANK_HIGHLIGHT_DURATION, YankCommand, find_prompt_boundaries, word_search_pattern,
};
#[cfg(test)]
use crate::copy_mode::{
    copy_digit, copy_jump_prefix, copy_motion, copy_motion_with_prefix, copy_scroll_direction,
    copy_text_object_key, copy_viewport_position_key, is_copy_modifier_key,
};
use crate::ghostty_ffi::{
    self, GlFrame, Host, SnapshotCell, SnapshotCellRange, SnapshotDecorationLayer,
    SnapshotDecorationStyle, SnapshotTextRow, SnapshotView, SnapshotViewportOptions,
};
use crate::input;
use crate::painter::TerminalPainter;
use crate::quick_select::{self, Purpose as QuickSelectPurpose, QuickSelect};
use crate::search::{SearchUi, SearchWorker};
use std::ffi::{c_char, c_void};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

struct WakeState {
    context: egui::Context,
    proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
    wake_pending: AtomicBool,
    frame_ready_pending: AtomicBool,
    close: AtomicBool,
    osc_token: String,
    protocol_request: std::sync::Mutex<Option<crate::picker::protocol::PickerRequest>>,
    clipboard_text: std::sync::Mutex<Option<String>>,
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_wakeup(userdata: *mut c_void) {
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    if !state.wake_pending.swap(true, Ordering::AcqRel)
        && state.proxy.send_event(UserEvent::GhosttyWake).is_err()
    {
        state.wake_pending.store(false, Ordering::Release);
    }
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_frame_ready(userdata: *mut c_void) {
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    if !state.frame_ready_pending.swap(true, Ordering::AcqRel)
        && state.proxy.send_event(UserEvent::FrameReady).is_err()
    {
        state.frame_ready_pending.store(false, Ordering::Release);
    }
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_close(userdata: *mut c_void) {
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    state.close.store(true, Ordering::Release);
    state.context.request_repaint();
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_protocol(userdata: *mut c_void, bytes: *const u8, len: usize) {
    if bytes.is_null() || len > crate::picker::protocol::MAX_PAYLOAD {
        return;
    }
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    let payload = unsafe { std::slice::from_raw_parts(bytes, len) };
    match crate::picker::protocol::parse(payload, &state.osc_token) {
        Ok(request) => {
            *state.protocol_request.lock().unwrap() = Some(request);
            state.context.request_repaint();
        }
        Err(error) => eprintln!("{error}"),
    }
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_write_clipboard(userdata: *mut c_void, bytes: *const u8, len: usize) {
    if bytes.is_null() {
        return;
    }
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    let text =
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(bytes, len) }).into_owned();
    *state.clipboard_text.lock().unwrap() = Some(text);
    state.context.request_repaint();
}

pub(super) struct TerminalPane {
    host: *mut Host,
    wake: Box<WakeState>,
    painter: Arc<egui::mutex::Mutex<TerminalPainter>>,
    paint_callback: Arc<egui_glow::CallbackFn>,
    size: [u32; 2],
    scale: f32,
    focused: bool,
    active: bool,
    search: SearchUi,
    pending_search_snapshot: Option<u64>,
    next_search_open: u64,
    search_worker: SearchWorker,
    copy_mode: CopyMode,
    quick_select: Option<QuickSelect>,
    picker: Option<crate::picker::Picker>,
    picker_font: crate::picker::font::PickerFont,
    copy_search_cursor: usize,
    prompt_boundaries: Option<(u64, Vec<u64>)>,
}

impl TerminalPane {
    fn new(host: *mut Host, wake: Box<WakeState>, gl: &glow::Context) -> Self {
        let painter = Arc::new(egui::mutex::Mutex::new(TerminalPainter::new(gl)));
        let callback_painter = Arc::clone(&painter);
        let paint_callback = Arc::new(egui_glow::CallbackFn::new(move |_info, glow_painter| {
            callback_painter.lock().paint(glow_painter.gl());
        }));
        let picker_font = crate::picker::font::PickerFont::new(&wake.context, host);
        let search_worker = SearchWorker::new(wake.context.clone());
        Self {
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
            copy_search_cursor: 0,
            prompt_boundaries: None,
        }
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
        // Resize invalidates presentation snapshots and their text cursors.
        if self.copy_mode.generation.is_some() {
            self.picker = None;
        }
        self.cancel_quick_select();
        self.close_search();
        self.close_copy_mode();
        self.size = size;
        self.scale = scale;
        unsafe { ghostty_ffi::ghostty_egui_host_resize(self.host, size[0], size[1], scale as f64) };
        true
    }

    pub(super) fn update_modes(&mut self, ctx: &egui::Context) {
        if self.pending_search_snapshot.is_some() {
            self.prepare_search_snapshot();
        }
        self.apply_search_result();
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

    pub(super) fn refresh_decorations(&mut self, scrolled: bool, resized: bool) {
        if (scrolled || resized)
            && let Some(generation) = self.search.snapshot_generation
            && unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_decorations_need_refresh(
                    self.host, generation,
                )
            }
        {
            self.publish_search_matches(false);
        }
        if scrolled
            && let Some(generation) = self.copy_mode.generation
            && unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_decorations_need_refresh(
                    self.host, generation,
                )
            }
        {
            self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
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
        self.refresh_decorations(scrolled, false);
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
}

impl TerminalPane {
    pub(super) fn create(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        argc: usize,
        argv: *mut *mut c_char,
    ) -> Self {
        let mut wake = Box::new(WakeState {
            context: egui_ctx.clone(),
            proxy,
            wake_pending: AtomicBool::new(false),
            frame_ready_pending: AtomicBool::new(false),
            close: AtomicBool::new(false),
            osc_token: crate::picker::protocol::new_token(),
            protocol_request: std::sync::Mutex::new(None),
            clipboard_text: std::sync::Mutex::new(None),
        });
        let native = current_native_context();
        let osc_token = std::ffi::CString::new(wake.osc_token.clone()).unwrap();
        let host = unsafe {
            ghostty_ffi::ghostty_egui_host_create(
                argc,
                argv,
                (&mut *wake as *mut WakeState).cast(),
                native.window,
                native.display,
                native.context,
                egui_ctx.pixels_per_point() as f64,
                osc_token.as_ptr(),
            )
        };
        assert!(
            !host.is_null(),
            "failed to initialize the embedded Ghostty surface"
        );

        Self::new(host, wake, gl)
    }

    pub(super) fn create_floating(&self, gl: &glow::Context) -> Option<Self> {
        let mut wake = Box::new(WakeState {
            context: self.wake.context.clone(),
            proxy: self.wake.proxy.clone(),
            wake_pending: AtomicBool::new(false),
            frame_ready_pending: AtomicBool::new(false),
            close: AtomicBool::new(false),
            osc_token: crate::picker::protocol::new_token(),
            protocol_request: std::sync::Mutex::new(None),
            clipboard_text: std::sync::Mutex::new(None),
        });
        let osc_token = std::ffi::CString::new(wake.osc_token.clone()).ok()?;
        let host = unsafe {
            ghostty_ffi::ghostty_egui_host_create_floating(
                self.host,
                (&mut *wake as *mut WakeState).cast(),
                self.scale as f64,
                osc_token.as_ptr(),
            )
        };
        if host.is_null() {
            return None;
        }
        Some(Self::new(host, wake, gl))
    }

    pub(super) fn tick(&mut self) {
        self.wake.wake_pending.store(false, Ordering::Release);
        unsafe { ghostty_ffi::ghostty_egui_host_tick(self.host) };
    }

    pub(super) fn should_close(&self) -> bool {
        self.wake.close.load(Ordering::Acquire)
    }

    pub(super) fn request_repaint(&self) {
        self.wake.context.request_repaint();
    }

    pub(super) fn paint_callback(&self) -> Arc<egui_glow::CallbackFn> {
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
            egui::Color32::from_rgb(0xd6, 0x5d, 0x0e)
        } else if copying || self.search.active {
            egui::Color32::from_rgb(0xd7, 0x99, 0x21)
        } else {
            egui::Color32::from_rgb(0x92, 0x83, 0x74)
        };
        let separator = if copying || self.search.active {
            accent
        } else {
            egui::Color32::from_rgb(0x3c, 0x38, 0x36)
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
        }
    }

    pub(super) fn handle_input(
        &mut self,
        ui: &egui::Ui,
        bounds: egui::Rect,
        bindings: &mut crate::picker::config::Bindings,
        has_floating: bool,
    ) -> PaneInput {
        let (picker_consumed, action) = self.handle_picker(ui, bounds, bindings, has_floating);
        let quick_consumed = !picker_consumed && self.handle_quick_select_keys(ui);
        let copy_consumed = !picker_consumed && !quick_consumed && self.handle_copy_keys(ui);
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
                        self.yank_quick_select_lines(ui.ctx(), cell);
                    } else if shifted {
                        self.paste_quick_select(cell);
                    } else {
                        self.accept_quick_select(cell);
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
        let host = self.host;
        let context = if entry.needs_context() {
            let mut row = ghostty_ffi::LiveCursorLine::default();
            if !unsafe { ghostty_ffi::ghostty_egui_host_live_cursor_line(host, &mut row) } {
                eprintln!("unable to read live cursor line for picker");
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
                eprintln!("picker: {error}");
                return;
            }
        };
        if let crate::picker::config::Source::CommandPalette(entries) = &mut options.source {
            *entries = crate::actions::catalog(bindings, has_floating, self.copying());
            // A factory's source is only known after resolving it. Exclude
            // the current factory too, without evaluating other factories.
            if let crate::picker::config::PickerEntry::Factory(factory, _) = &entry {
                entries.retain(|entry| !matches!(&entry.action, crate::actions::AppAction::OpenPicker(name) if name == factory.name()));
            }
        }
        let copy_generation = self.copy_mode.generation;
        let from_copy = copy_generation.is_some();
        let cursor_row = if options.position == crate::picker::config::Position::Cursor {
            if let Some(generation) = copy_generation {
                let copy_cursor = Some(self.copy_mode.cursor);
                let mut view = SnapshotView::default();
                if unsafe {
                    ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view)
                } {
                    copy_cursor
                        .and_then(|cursor| cursor.y.checked_sub(view.viewport_top))
                        .filter(|row| *row < u64::from(view.viewport_rows))
                        .and_then(|row| u32::try_from(row).ok())
                } else {
                    None
                }
            } else {
                let mut cursor = ghostty_ffi::LiveCursorPosition::default();
                unsafe { ghostty_ffi::ghostty_egui_host_live_cursor_position(host, &mut cursor) }
                    .then_some(cursor.row)
            }
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
            self.picker = Some(crate::picker::Picker::start(
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
            ));
        } else {
            let generation = if let Some(generation) = copy_generation {
                generation
            } else {
                let mut generation = 0;
                if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_begin(host, &mut generation) }
                {
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
                return;
            }
            if !from_copy {
                self.copy_mode.start(generation, view);
            }
            self.picker = Some(crate::picker::Picker::start(
                host,
                Some(generation),
                from_copy,
                view.total_rows,
                ctx.clone(),
                options,
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
    fn handle_picker(
        &mut self,
        ui: &egui::Ui,
        bounds: egui::Rect,
        bindings: &mut crate::picker::config::Bindings,
        has_floating: bool,
    ) -> (bool, Option<crate::actions::AppAction>) {
        let was_active = self.picker.as_ref().is_some();
        let modal_active = self.search.active || self.quick_select.is_some();
        let mut shortcut = None;
        let mut consumed = false;
        let mut request_params = None;
        let request = self.wake.protocol_request.lock().unwrap().take();
        if let Some(request) = request {
            consumed = true;
            if !was_active && !modal_active {
                if let Some(entry) = bindings.named(&request.name) {
                    shortcut = Some(entry);
                    request_params = Some(request.params);
                } else {
                    eprintln!("unknown OSC picker {:?}", request.name);
                }
            } else {
                eprintln!("OSC picker request ignored while another overlay is active");
            }
        }
        if let Some(entry) = shortcut.filter(|_| !was_active && !modal_active) {
            self.open_picker(ui.ctx(), entry, request_params, bindings, has_floating);
        }
        let active = self.picker.as_ref().is_some();
        let host = self.host;
        let typography = self.picker_font.typography(ui.ctx(), host);
        if let Some(picker) = self.picker.as_mut() {
            picker.typography = typography;
        }
        let action = self
            .picker
            .as_mut()
            .and_then(|picker| picker.show_in_bounds(ui.ctx(), bounds));
        let mut application_action = None;
        if let Some(action) = action {
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
            crate::picker::Action::Accept(target) => {
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
            crate::picker::Action::Custom(callback, text, filesystem, range, value, params) => {
                match callback.call(&text, &value, filesystem, generation, range, &params) {
                    Ok(commands) => {
                        let mut navigated = false;
                        let mut sent = false;
                        for command in commands {
                            match command {
                                crate::picker::action::Command::Navigate(target) => {
                                    if let Some(generation) =
                                        generation.filter(|g| self.copy_mode.generation == Some(*g))
                                    {
                                        self.accept_picker_cell(generation, target);
                                        navigated = true;
                                    } else {
                                        eprintln!("picker navigation snapshot is no longer active");
                                    }
                                }
                                crate::picker::action::Command::Paste(text) => {
                                    self.close_copy_mode();
                                    sent = true;
                                    unsafe {
                                        ghostty_ffi::ghostty_egui_host_text(
                                            self.host,
                                            text.as_ptr().cast(),
                                            text.len(),
                                        );
                                    }
                                }
                                crate::picker::action::Command::SendText(text) => {
                                    self.close_copy_mode();
                                    sent = true;
                                    Self::send_picker_text(self.host, &text);
                                }
                                crate::picker::action::Command::ReplacePrompt {
                                    text,
                                    before,
                                    after,
                                } => {
                                    self.close_copy_mode();
                                    sent = true;
                                    Self::replace_picker_prompt(self.host, &text, before, after);
                                }
                            }
                        }
                        if !sent && !navigated {
                            restore_copy(self);
                        }
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        restore_copy(self);
                    }
                }
            }
        }
        None
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
                return;
            }
            generation
        };
        let mut view = SnapshotView::default();
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view) } {
            if !from_copy {
                unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(host, generation) };
            }
            return;
        }
        if !from_copy {
            self.copy_mode.start(generation, view);
        }
        self.quick_select = QuickSelect::new(generation, from_copy, view, |row| {
            Self::read_snapshot_row_from(host, generation, row)
        });
        if self.quick_select.is_none() {
            if !from_copy {
                self.close_quick_copy_mode();
            }
            return;
        }
        if !self.publish_quick_select() {
            self.cancel_quick_select();
        }
        self.wake.context.request_repaint();
    }

    fn publish_quick_select(&self) -> bool {
        self.publish_quick_select_with_viewport(std::ptr::null())
    }

    fn page_quick_select(&mut self, down: bool) -> bool {
        let Some(quick) = &self.quick_select else {
            return false;
        };
        let (generation, from_copy, purpose) = (quick.generation, quick.from_copy, quick.purpose);
        let host = self.quick_host();
        let mut view = SnapshotView::default();
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, generation, &mut view) } {
            return false;
        }
        let view = QuickSelect::paged_view(view, down);
        let Some(mut quick) = QuickSelect::new(generation, from_copy, view, |row| {
            Self::read_snapshot_row_from(host, generation, row)
        }) else {
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
        let (ranges, text, style) = quick.decorations();
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

    fn accept_quick_select(&mut self, target: SnapshotCell) {
        let Some(quick) = self.quick_select.take() else {
            return;
        };
        self.copy_mode.clear_pending_for_navigation();
        self.apply_copy_target(quick.generation, Motion::CharacterJump, target, false);
        self.wake.context.request_repaint();
    }

    fn yank_quick_select_lines(&mut self, ctx: &egui::Context, target: SnapshotCell) {
        let Some(quick) = self.quick_select.take() else {
            return;
        };
        let QuickSelectPurpose::YankLines { origin } = quick.purpose else {
            return;
        };
        let range = self
            .copy_mode
            .linewise_range_between(origin, target, |row| {
                self.read_snapshot_row(quick.generation, row)
            });
        if let Some(range) = range
            && self.yank_range(ctx, quick.generation, range, true, true)
        {
            self.copy_mode.visual_anchor = None;
        }
        self.publish_copy_decoration(quick.generation, self.copy_mode.cursor, false);
        self.wake.context.request_repaint();
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

    fn prepare_search_snapshot(&mut self) {
        self.pending_search_snapshot = None;
        let host = self.search_host();
        let mut snapshot_generation = 0;
        if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_begin(host, &mut snapshot_generation) }
        {
            self.close_search();
            return;
        }

        let cursor = unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_cursor_new(host, snapshot_generation)
        };
        if cursor.is_null() {
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_end(host, snapshot_generation) };
            self.close_search();
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
            eprintln!("copy mode: failed to create terminal snapshot");
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
            eprintln!("copy mode: failed to initialize snapshot cursor");
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
            return;
        }
        self.search_worker.cancel_and_wait();
        // Transfer ownership instead of closing search: ending this snapshot
        // would invalidate the match coordinates and briefly show live output.
        let search = std::mem::take(&mut self.search);
        view.cursor = actual;
        self.copy_mode.start(generation, view);
        self.copy_search_cursor = search.cursor;
        self.copy_mode
            .adopt_search(search.query_revision, search.matches);
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
            self.copy_mode = CopyMode::default();
            self.copy_search_cursor = 0;
            self.prompt_boundaries = None;
            self.wake.context.request_repaint();
        }
    }

    fn handle_copy_keys(&mut self, ui: &egui::Ui) -> bool {
        let was_active = self.copy_mode.generation.is_some();
        let events = ui.input(|input| input.events.clone());
        self.copy_mode.begin_input_batch();
        for event in &events {
            if self.copy_mode.generation.is_none() {
                break;
            }
            let action = self.copy_mode.input(event);
            let generation = self.copy_mode.generation.expect("copy mode is active");
            let success = match action {
                CopyAction::None => true,
                CopyAction::Exit => {
                    self.close_copy_mode();
                    break;
                }
                CopyAction::ClearSearch { cancel_pending } => {
                    if cancel_pending {
                        self.search_worker.cancel();
                    }
                    self.publish_copy_decoration(generation, self.copy_mode.cursor, false)
                }
                CopyAction::Redraw | CopyAction::ToggleVisual => {
                    self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                    true
                }
                CopyAction::Position(position) => self.position_copy_viewport(position),
                CopyAction::SelectObject {
                    object,
                    kind,
                    count,
                } => {
                    if let Some(selection) =
                        self.copy_text_object_selection(generation, count, object, kind)
                    {
                        self.select_copy_visual_object(generation, selection.range, selection.trim)
                    } else {
                        true
                    }
                }
                CopyAction::SwapEndpoint => self.apply_copy_target(
                    generation,
                    Motion::CharacterJump,
                    self.copy_mode.cursor,
                    false,
                ),
                CopyAction::Scroll { down, count } => self.scroll_copy_viewport(down, count),
                CopyAction::HintYank { origin } => {
                    self.open_quick_select();
                    if let Some(quick) = &mut self.quick_select {
                        quick.purpose = QuickSelectPurpose::YankLines { origin };
                    }
                    true
                }
                CopyAction::YankVisual => {
                    if self.yank_visual(ui.ctx()) {
                        self.copy_mode.visual_anchor = None;
                        self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                    }
                    true
                }
                CopyAction::Yank(command) => self.execute_copy_yank(ui.ctx(), generation, command),
                CopyAction::Jump {
                    jump,
                    repeat,
                    count,
                } => self.perform_copy_jump(&jump, repeat, count),
                CopyAction::SearchWord { forward, count } => {
                    self.search_copy_word(forward, count);
                    true
                }
                CopyAction::NavigateSearch { reverse, count } => {
                    self.navigate_copy_search(reverse, count);
                    true
                }
                CopyAction::Motion {
                    motion,
                    count,
                    explicit_count,
                } => self.perform_copy_motion(motion, count, explicit_count),
            };
            if !success {
                break;
            }
        }
        was_active || self.copy_mode.generation.is_some()
    }

    fn execute_copy_yank(
        &mut self,
        ctx: &egui::Context,
        generation: u64,
        command: YankCommand,
    ) -> bool {
        let (range, linewise, trim) = match command {
            YankCommand::Lines(count) => (
                self.copy_mode
                    .yank_lines_range(count, |row| self.read_snapshot_row(generation, row)),
                true,
                true,
            ),
            YankCommand::RowEnd(count) => (
                self.copy_mode
                    .yank_to_row_end_range(count, |row| self.read_snapshot_row(generation, row)),
                false,
                true,
            ),
            YankCommand::Object {
                object,
                kind,
                count,
            } => self
                .copy_text_object_selection(generation, count, object, kind)
                .map_or((None, false, true), |selection| {
                    (Some(selection.range), selection.linewise, selection.trim)
                }),
            YankCommand::Motion {
                motion,
                count,
                explicit_count,
            } => {
                let target = self
                    .copy_motion_target(generation, motion, count, explicit_count)
                    .and_then(|target| {
                        self.copy_mode.yank_motion_range(target, |row| {
                            self.read_snapshot_row(generation, row)
                        })
                    });
                match target {
                    Some((range, linewise)) => (Some(range), linewise, true),
                    None => (None, false, true),
                }
            }
            YankCommand::Jump { jump, count } => {
                let target = self
                    .copy_jump_target(generation, &jump, count)
                    .and_then(|cell| {
                        self.copy_mode
                            .yank_motion_range(Motion::CharacterJump.result(cell), |row| {
                                self.read_snapshot_row(generation, row)
                            })
                    });
                match target {
                    Some((range, linewise)) => (Some(range), linewise, true),
                    None => (None, false, true),
                }
            }
        };
        if let Some(range) = range
            && self.yank_range(ctx, generation, range, linewise, trim)
        {
            self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
        }
        true
    }
    fn scroll_copy_viewport(&mut self, down: bool, count: u32) -> bool {
        let generation = self.copy_mode.generation.expect("copy mode is active");
        let mut view = SnapshotView::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view)
        } {
            self.close_copy_mode();
            return false;
        }
        let Some((top, requested)) = self.copy_mode.scrolled_viewport_target(down, count, view)
        else {
            self.close_copy_mode();
            return false;
        };
        let Some(actual) = self.resolve_copy_cell(generation, requested, false) else {
            self.close_copy_mode();
            return false;
        };
        let viewport = SnapshotViewportOptions {
            top_row: &top,
            ..Default::default()
        };
        if !self.publish_copy_decoration_with_viewport(generation, actual, &viewport) {
            self.close_copy_mode();
            return false;
        }
        self.copy_mode
            .moved(if down { Motion::Down } else { Motion::Up }, actual);
        self.wake.context.request_repaint();
        true
    }

    fn position_copy_viewport(&mut self, position: ViewportPosition) -> bool {
        let generation = self.copy_mode.generation.expect("copy mode is active");
        let mut view = SnapshotView::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view)
        } {
            self.close_copy_mode();
            return false;
        }
        let Some(top) = self.copy_mode.positioned_viewport_top(position, view) else {
            self.close_copy_mode();
            return false;
        };
        let viewport = SnapshotViewportOptions {
            top_row: &top,
            ..Default::default()
        };
        if !self.publish_copy_decoration_with_viewport(generation, self.copy_mode.cursor, &viewport)
        {
            self.close_copy_mode();
            return false;
        }
        self.wake.context.request_repaint();
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
            self.copy_mode.moved(motion, actual);
            true
        } else {
            self.close_copy_mode();
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

    fn evaluate_copy_motion(
        &self,
        generation: u64,
        motion: Motion,
        count: u32,
        explicit_count: bool,
    ) -> MotionEvaluation {
        let view = if motion.needs_view() {
            let mut view = SnapshotView::default();
            if !unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view)
            } {
                return MotionEvaluation::InvalidSnapshot;
            }
            Some(view)
        } else {
            None
        };
        self.copy_mode.evaluate_motion(
            motion,
            count,
            explicit_count,
            view,
            |row| self.read_snapshot_row(generation, row),
            |cell, right| self.resolve_copy_cell(generation, cell, right),
        )
    }

    fn perform_copy_motion(&mut self, motion: Motion, count: u32, explicit_count: bool) -> bool {
        let generation = self.copy_mode.generation.expect("copy mode is active");
        match self.evaluate_copy_motion(generation, motion, count, explicit_count) {
            MotionEvaluation::NoMovement => true,
            MotionEvaluation::Complete(result) | MotionEvaluation::Partial(result) => {
                self.apply_copy_target(generation, motion, result.target, false)
            }
            MotionEvaluation::InvalidSnapshot => {
                self.close_copy_mode();
                false
            }
        }
    }

    fn copy_motion_target(
        &self,
        generation: u64,
        motion: Motion,
        count: u32,
        explicit_count: bool,
    ) -> Option<MotionResult> {
        match self.evaluate_copy_motion(generation, motion, count, explicit_count) {
            MotionEvaluation::Complete(result) => Some(result),
            // A failed counted operator must not yank an incomplete range.
            _ => None,
        }
    }

    fn perform_copy_jump(&mut self, jump: &Jump, repeat: bool, count: u32) -> bool {
        let generation = self.copy_mode.generation.expect("copy mode is active");
        let mut working = self.copy_mode.clone();
        for index in 0..count {
            let Some(cell) = working.jump_target(jump, repeat || index > 0, |row| {
                self.read_snapshot_row(generation, row)
            }) else {
                return true;
            };
            working.moved(Motion::CharacterJump, cell);
        }
        self.apply_copy_target(generation, Motion::CharacterJump, working.cursor, false)
    }

    fn copy_jump_target(&self, generation: u64, jump: &Jump, count: u32) -> Option<SnapshotCell> {
        let mut working = self.copy_mode.clone();
        for index in 0..count {
            let cell = working.jump_target(jump, index > 0, |row| {
                self.read_snapshot_row(generation, row)
            })?;
            working.moved(Motion::CharacterJump, cell);
        }
        Some(working.cursor)
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
            self.search_worker
                .submit(snapshot_generation, query, regex, cursor);
        self.search.waiting = true;
    }

    fn apply_search_result(&mut self) {
        if self.quick_select.is_some() || self.picker.is_some() {
            return;
        }
        let Some(result) = self.search_worker.take_latest_result() else {
            return;
        };
        if self.copy_mode.generation == Some(result.snapshot_generation)
            && self.copy_mode.search_accepts_result(result.query_revision)
        {
            self.finish_copy_search_result(result);
            return;
        }
        if !self.search.active
            || self.search.snapshot_generation != Some(result.snapshot_generation)
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
        let Some(target) = self
            .copy_mode
            .finish_search(result.query_revision, result.matches)
        else {
            return;
        };
        self.apply_copy_target(
            result.snapshot_generation,
            Motion::CharacterJump,
            target,
            false,
        );
    }

    fn navigate_search(&mut self, previous: bool) {
        self.search.navigate(previous);
        self.publish_search_matches(true);
    }

    fn search_copy_word(&mut self, forward: bool, count: u32) {
        let Some(generation) = self.copy_mode.generation else {
            return;
        };
        let Some(word) = self
            .copy_mode
            .word_under_cursor(|row| self.read_snapshot_row(generation, row))
        else {
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
        let pattern = word_search_pattern(&word);
        let query_revision =
            self.search_worker
                .submit(generation, pattern, true, self.copy_search_cursor);
        self.copy_mode.begin_search(query_revision, forward, count);
        self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
    }

    fn navigate_copy_search(&mut self, reverse: bool, count: u32) {
        let Some(generation) = self.copy_mode.generation else {
            return;
        };
        if let Some(target) = self.copy_mode.navigate_search(reverse, count) {
            self.apply_copy_target(generation, Motion::CharacterJump, target, false);
        }
    }

    fn publish_copy_decoration(&self, generation: u64, cell: SnapshotCell, reveal: bool) -> bool {
        let viewport = SnapshotViewportOptions {
            reveal: if reveal { &cell } else { std::ptr::null() },
            padding_rows: SCROLL_PADDING_ROWS,
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
                foreground_rgb: 0x282828,
                background_rgb: if self.copy_mode.visual_anchor.is_some() {
                    0xd65d0e // Gruvbox neutral orange, matching the visual statusline.
                } else {
                    0xd79921 // Gruvbox neutral yellow, matching the normal statusline.
                },
                flags: 3,
            },
        };
        let selection = self
            .copy_mode
            .visual_selection_to(cell, |row| self.read_snapshot_row(generation, row));
        let selection_ranges = self.copy_decoration_ranges(
            generation,
            selection.as_ref().map(|selection| selection.range),
            selection
                .as_ref()
                .is_some_and(|selection| selection.rectangle),
            viewport,
        );
        let selection_layer = selection.map(|_| SnapshotDecorationLayer {
            ranges: selection_ranges.as_ptr(),
            range_count: selection_ranges.len(),
            style: SnapshotDecorationStyle {
                foreground_rgb: 0xebdbb2,
                background_rgb: 0x504945,
                flags: 3,
            },
        });
        let matches_layer = SnapshotDecorationLayer {
            ranges: self.copy_mode.search_matches().as_ptr(),
            range_count: self.copy_mode.search_matches().len(),
            style: SnapshotDecorationStyle {
                foreground_rgb: 0xebdbb2,
                background_rgb: 0x665c54,
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
                foreground_rgb: 0x282828,
                background_rgb: 0xd79921,
                flags: 3,
            },
        });
        let mut layers = Vec::with_capacity(4);
        layers.push(caret_layer);
        if let Some(layer) = selection_layer {
            layers.push(layer);
        }
        if let Some(layer) = yank_layer {
            layers.push(layer);
        }
        layers.push(matches_layer);
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
        crate::copy_mode::rectangle_row_ranges(range, top, height)
    }

    fn copy_text_object_selection(
        &mut self,
        generation: u64,
        count: u32,
        object: WordObject,
        kind: CopyTextObject,
    ) -> Option<CopySelection> {
        if matches!(kind, CopyTextObject::Command) && !self.ensure_prompt_boundaries(generation) {
            return None;
        }
        let boundaries = self
            .prompt_boundaries
            .as_ref()
            .map_or(&[][..], |(_, rows)| rows.as_slice());
        self.copy_mode
            .text_object_selection(count, object, kind, boundaries, |row| {
                self.read_snapshot_row(generation, row)
            })
    }

    fn select_copy_visual_object(
        &mut self,
        generation: u64,
        range: SnapshotCellRange,
        trim: bool,
    ) -> bool {
        let Some(start) = self.resolve_copy_cell(generation, range.start, false) else {
            self.close_copy_mode();
            return false;
        };
        let Some(end) = self.resolve_copy_cell(generation, range.end, false) else {
            self.close_copy_mode();
            return false;
        };
        self.copy_mode
            .select_visual_object(SnapshotCellRange { start, end }, trim);
        if !self.publish_copy_decoration(generation, end, true) {
            self.close_copy_mode();
            return false;
        }
        self.wake.context.request_repaint();
        true
    }

    fn yank_visual(&mut self, ctx: &egui::Context) -> bool {
        let Some(generation) = self.copy_mode.generation else {
            return false;
        };
        let Some(selection) = self
            .copy_mode
            .visual_selection_to(self.copy_mode.cursor, |row| {
                self.read_snapshot_row(generation, row)
            })
        else {
            return false;
        };
        self.yank_selection(
            ctx,
            generation,
            selection.range,
            selection.linewise,
            selection.trim,
            selection.rectangle,
        )
    }

    fn yank_range(
        &mut self,
        ctx: &egui::Context,
        generation: u64,
        range: SnapshotCellRange,
        linewise: bool,
        trim: bool,
    ) -> bool {
        self.yank_selection(ctx, generation, range, linewise, trim, false)
    }

    fn yank_selection(
        &mut self,
        ctx: &egui::Context,
        generation: u64,
        range: SnapshotCellRange,
        linewise: bool,
        trim: bool,
        rectangle: bool,
    ) -> bool {
        let mut result = ghostty_ffi::SnapshotSelectionText::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_selection_text(
                self.host,
                generation,
                range,
                trim,
                rectangle,
                &mut result,
            )
        } {
            return false;
        }
        let text = if result.text.is_null() {
            None
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(result.text, result.len) };
            std::str::from_utf8(bytes).ok().map(str::to_owned)
        };
        unsafe { ghostty_ffi::ghostty_egui_host_snapshot_selection_text_free(&mut result) };
        if let Some(mut text) = text {
            if linewise {
                text.push('\n');
            }
            ctx.copy_text(text);
            if rectangle {
                self.copy_mode
                    .highlight_rectangle_yank(range, Instant::now());
            } else {
                self.copy_mode.highlight_yank(range, Instant::now());
            }
            ctx.request_repaint_after(YANK_HIGHLIGHT_DURATION);
            true
        } else {
            false
        }
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
            padding_rows: SCROLL_PADDING_ROWS,
            ..Default::default()
        };
        let selected_layers = selected.map(|range| SnapshotDecorationLayer {
            ranges: std::ptr::from_ref(range),
            range_count: 1,
            style: SnapshotDecorationStyle {
                foreground_rgb: 0x282828,
                background_rgb: 0xfabd2f,
                flags: 3,
            },
        });
        let matches_layer = SnapshotDecorationLayer {
            ranges: self.search.matches.as_ptr(),
            range_count: self.search.matches.len(),
            style: SnapshotDecorationStyle {
                foreground_rgb: 0xebdbb2,
                background_rgb: 0x665c54,
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
            self.close_search();
        } else if action.accept {
            self.accept_search();
        }
    }
}

#[cfg(test)]
mod copy_shortcut_tests {
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
                    mac_cmd: true,
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
            Some(crate::copy_mode::Motion::ViewportTop)
        ));
        assert!(matches!(
            copy_motion(egui::Key::M, shift),
            Some(crate::copy_mode::Motion::ViewportMiddle)
        ));
        assert!(matches!(
            copy_motion(egui::Key::L, shift),
            Some(crate::copy_mode::Motion::ViewportBottom)
        ));
        assert!(matches!(
            copy_motion(egui::Key::PageUp, egui::Modifiers::NONE),
            Some(crate::copy_mode::Motion::PageUp)
        ));
        assert!(matches!(
            copy_motion(egui::Key::PageDown, egui::Modifiers::NONE),
            Some(crate::copy_mode::Motion::PageDown)
        ));
        assert!(matches!(
            copy_motion(egui::Key::U, ctrl),
            Some(crate::copy_mode::Motion::HalfPageUp)
        ));
        assert!(matches!(
            copy_motion(egui::Key::D, ctrl),
            Some(crate::copy_mode::Motion::HalfPageDown)
        ));
        assert!(copy_motion(egui::Key::H, ctrl).is_none());
    }

    #[test]
    fn text_motion_keys_are_copy_mode_only() {
        use crate::copy_mode::Motion;
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
        use crate::copy_mode::{CopyMode, Motion};
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
        use crate::copy_mode::{CopyMode, Motion};
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

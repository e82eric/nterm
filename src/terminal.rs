use crate::app::UserEvent;
use crate::copy_mode::{CopyMode, Motion};
use crate::ffi::{self, GlFrame, Host, SnapshotCell, SnapshotCopyState, SnapshotCopyView};
use crate::input;
use crate::painter::TerminalPainter;
use crate::search::{SearchEvents, SearchUi, SearchWorker};
use std::ffi::{c_char, c_void};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct WakeState {
    context: egui::Context,
    proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
    wake_pending: AtomicBool,
    frame_ready_pending: AtomicBool,
    close: AtomicBool,
    search: Mutex<SearchEvents>,
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
        && state
            .proxy
            .send_event(UserEvent::FrameReady(std::time::Instant::now()))
            .is_err()
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
extern "C" fn ghostty_egui_search_open(userdata: *mut c_void, needle: *const c_char, len: usize) {
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    let needle = if needle.is_null() {
        String::new()
    } else {
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(needle.cast(), len) })
            .into_owned()
    };
    let mut events = state.search.lock().expect("search event mutex poisoned");
    events.open = Some(needle);
    events.close = false;
    drop(events);
    state.context.request_repaint();
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_search_close(userdata: *mut c_void) {
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    state
        .search
        .lock()
        .expect("search event mutex poisoned")
        .close = true;
    state.context.request_repaint();
}

pub(crate) struct TerminalView {
    host: *mut Host,
    wake: Box<WakeState>,
    painter: Arc<egui::mutex::Mutex<TerminalPainter>>,
    paint_callback: Arc<egui_glow::CallbackFn>,
    size: [u32; 2],
    scale: f32,
    focused: bool,
    search: SearchUi,
    pending_search_snapshot: Option<u64>,
    next_search_open: u64,
    search_worker: SearchWorker,
    copy_mode: CopyMode,
}

impl TerminalView {
    pub(crate) fn new(
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
            search: Mutex::new(SearchEvents::default()),
        });
        let native = current_native_context();
        let host = unsafe {
            ffi::ghostty_egui_host_create(
                argc,
                argv,
                (&mut *wake as *mut WakeState).cast(),
                native.window,
                native.display,
                native.context,
                egui_ctx.pixels_per_point() as f64,
            )
        };
        assert!(
            !host.is_null(),
            "failed to initialize the embedded Ghostty surface"
        );

        let painter = Arc::new(egui::mutex::Mutex::new(TerminalPainter::new(gl)));
        let callback_painter = Arc::clone(&painter);
        let paint_callback = Arc::new(egui_glow::CallbackFn::new(move |_info, glow_painter| {
            callback_painter.lock().paint(glow_painter.gl());
        }));
        Self {
            host,
            wake,
            painter,
            paint_callback,
            size: [0, 0],
            scale: 0.0,
            focused: false,
            search: SearchUi::default(),
            pending_search_snapshot: None,
            next_search_open: 0,
            search_worker: SearchWorker::new(egui_ctx.clone()),
            copy_mode: CopyMode::default(),
        }
    }

    pub(crate) fn tick(&mut self) {
        self.wake.wake_pending.store(false, Ordering::Release);
        unsafe { ffi::ghostty_egui_host_tick(self.host) };
    }

    pub(crate) fn acquire_frame(&mut self) -> bool {
        // Clear before taking the mailbox so a concurrent publication cannot
        // lose its notification. The callback always draws the current frame.
        self.wake
            .frame_ready_pending
            .store(false, Ordering::Release);
        let mut next = GlFrame::default();
        if !unsafe { ffi::ghostty_egui_host_take_frame(self.host, &mut next) } {
            return false;
        }
        self.painter.lock().replace_frame(next);
        true
    }

    pub(crate) fn update(&mut self, ui: &mut egui::Ui) {
        let pending_at_frame_start = self.pending_search_snapshot;
        self.apply_search_events();
        if pending_at_frame_start.is_some()
            && self.pending_search_snapshot == pending_at_frame_start
        {
            self.prepare_search_snapshot();
        }
        self.apply_search_result();

        let rect = ui.max_rect();
        let scale = ui.ctx().pixels_per_point();
        let width = (rect.width() * scale).round().max(1.0) as u32;
        let height = (rect.height() * scale).round().max(1.0) as u32;
        let resized = self.size != [width, height] || self.scale != scale;
        if resized {
            // Ghostty ends presentation snapshots during resize. Stop the
            // worker before that can free the pages its cursor reads.
            if self.search.snapshot_generation.is_some() {
                self.close_search();
            }
            self.close_copy_mode();
            self.size = [width, height];
            self.scale = scale;
            unsafe { ffi::ghostty_egui_host_resize(self.host, width, height, scale as f64) };
        }

        let focused = ui.input(|input| input.viewport().focused.unwrap_or(false));
        if self.focused != focused {
            self.focused = focused;
            unsafe { ffi::ghostty_egui_host_focus(self.host, focused) };
        }

        ui.painter().add(egui::PaintCallback {
            rect,
            callback: self.paint_callback.clone(),
        });

        let keyboard_blocked = self.handle_copy_keys(ui);
        self.show_search_overlay(ui.ctx());
        let scrolled = input::forward(self.host, self.size, ui, rect, scale, keyboard_blocked);
        if (scrolled || resized)
            && let Some(snapshot_generation) = self.search.snapshot_generation
            && unsafe {
                ffi::ghostty_egui_host_snapshot_cell_highlights_need_refresh(
                    self.host,
                    snapshot_generation,
                )
            }
        {
            self.publish_search_matches(false);
        }

        if !self.search.active {
            egui::Area::new("sample-overlay".into())
                .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -12.0])
                .fade_in(false)
                .movable(false)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        if self.copy_mode.generation.is_some() {
                            ui.small("COPY · hjkl / arrows · H M L · PgUp/PgDn · Ctrl+U/D · Esc");
                        } else {
                            ui.small("egui shell · Ghostty OpenGL texture");
                        }
                    });
                });
        }

        if self.should_close() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(crate) fn should_close(&self) -> bool {
        self.wake.close.load(Ordering::Acquire)
    }

    pub(crate) fn frame_generation(&self) -> Option<u64> {
        self.painter.lock().frame_generation()
    }

    pub(crate) fn poll_retired(&self, gl: &glow::Context) {
        self.painter.lock().poll_retired(gl);
    }

    pub(crate) fn retired_pending(&self) -> bool {
        self.painter.lock().has_retired_frames()
    }

    pub(crate) fn paint_trace(&self) -> crate::painter::PaintTrace {
        self.painter.lock().paint_trace()
    }

    pub(crate) fn destroy(&mut self, gl: &glow::Context) {
        self.close_search();
        self.close_copy_mode();
        self.painter.lock().destroy(gl);
        unsafe { ffi::ghostty_egui_host_destroy(self.host) };
        self.host = std::ptr::null_mut();
    }

    fn apply_search_events(&mut self) {
        let events = {
            let mut pending = self
                .wake
                .search
                .lock()
                .expect("search event mutex poisoned");
            std::mem::take(&mut *pending)
        };

        if let Some(needle) = events.open {
            self.open_search(needle);
        }
        if events.close {
            self.close_search();
        }
    }

    fn open_search(&mut self, needle: String) {
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
        let mut snapshot_generation = 0;
        if !unsafe { ffi::ghostty_egui_host_snapshot_begin(self.host, &mut snapshot_generation) } {
            self.close_search();
            return;
        }

        let cursor =
            unsafe { ffi::ghostty_egui_host_snapshot_cursor_new(self.host, snapshot_generation) };
        if cursor.is_null() {
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, snapshot_generation) };
            self.close_search();
            return;
        }

        self.search.snapshot_generation = Some(snapshot_generation);
        self.search.cursor = cursor as usize;
        self.queue_search();
    }

    fn close_search(&mut self) {
        self.pending_search_snapshot = None;
        if self.search.cursor != 0 {
            self.search_worker.cancel_and_wait();
            unsafe { ffi::ghostty_egui_host_snapshot_cursor_free(self.search.cursor as *mut _) };
        } else {
            self.search_worker.cancel();
        }
        if let Some(snapshot_generation) = self.search.snapshot_generation.take() {
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, snapshot_generation) };
        }
        self.search = SearchUi::default();
    }

    fn open_copy_mode(&mut self) {
        self.close_search();
        let mut generation = 0;
        if !unsafe { ffi::ghostty_egui_host_snapshot_begin(self.host, &mut generation) } {
            eprintln!("copy mode: failed to create terminal snapshot");
            return;
        }
        let mut state = SnapshotCopyState::default();
        let mut actual = SnapshotCell::default();
        let ready = unsafe {
            ffi::ghostty_egui_host_snapshot_copy_cursor_origin(self.host, generation, &mut state)
                && ffi::ghostty_egui_host_snapshot_copy_cursor_set(
                    self.host,
                    generation,
                    state.cell,
                    false,
                    &mut actual,
                )
        };
        if !ready {
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
            eprintln!("copy mode: failed to initialize snapshot cursor");
            return;
        }
        state.cell = actual;
        self.copy_mode.start(generation, state);
        self.wake.context.request_repaint();
    }

    fn close_copy_mode(&mut self) {
        if let Some(generation) = self.copy_mode.generation {
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
            self.copy_mode = CopyMode::default();
            self.wake.context.request_repaint();
        }
    }

    fn handle_copy_keys(&mut self, ui: &egui::Ui) -> bool {
        let was_active = self.copy_mode.generation.is_some();
        let events = ui.input(|input| input.events.clone());
        let mut handled = false;
        for event in events {
            if self.copy_mode.generation.is_none() {
                if !self.search.active && is_copy_mode_shortcut(&event) {
                    self.open_copy_mode();
                    handled = true;
                }
                continue;
            }
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            handled = true;
            if key == egui::Key::Escape {
                self.close_copy_mode();
                break;
            }
            let Some(motion) = copy_motion(key, modifiers) else {
                continue;
            };
            let generation = self.copy_mode.generation.expect("copy mode is active");
            let view = if motion.needs_view() {
                let mut view = SnapshotCopyView::default();
                if !unsafe {
                    ffi::ghostty_egui_host_snapshot_copy_view(self.host, generation, &mut view)
                } {
                    self.close_copy_mode();
                    break;
                }
                Some(view)
            } else {
                None
            };
            let Some((requested, snap_right)) = self.copy_mode.move_to(motion, view) else {
                continue;
            };
            let mut actual = SnapshotCell::default();
            if unsafe {
                ffi::ghostty_egui_host_snapshot_copy_cursor_set(
                    self.host,
                    generation,
                    requested,
                    snap_right,
                    &mut actual,
                )
            } {
                self.copy_mode.moved(motion, actual);
            } else {
                self.close_copy_mode();
                break;
            }
        }
        was_active || self.copy_mode.generation.is_some() || handled
    }

    fn queue_search(&mut self) {
        let Some(snapshot_generation) = self.search.snapshot_generation else {
            return;
        };
        self.search.query_revision = self.search_worker.submit(
            snapshot_generation,
            self.search.query.clone(),
            self.search.regex,
            self.search.cursor,
        );
    }

    fn apply_search_result(&mut self) {
        let Some(result) = self.search_worker.take_latest_result() else {
            return;
        };
        if !self.search.active
            || self.search.snapshot_generation != Some(result.snapshot_generation)
            || self.search.query_revision != result.query_revision
        {
            return;
        }
        self.search.apply_result(result.matches, result.error);
        self.publish_search_matches(true);
    }

    fn navigate_search(&mut self, previous: bool) {
        self.search.navigate(previous);
        let Some(snapshot_generation) = self.search.snapshot_generation else {
            return;
        };
        let Some(selected) = self.search.selected_range() else {
            return;
        };
        if !unsafe {
            ffi::ghostty_egui_host_snapshot_select_cell_highlight(
                self.host,
                snapshot_generation,
                *selected,
            )
        } {
            self.publish_search_matches(true);
        }
    }

    fn publish_search_matches(&self, reveal_selected: bool) {
        let Some(snapshot_generation) = self.search.snapshot_generation else {
            return;
        };
        let selected = self.search.selected_range();
        let ranges = if self.search.matches.is_empty() {
            std::ptr::null()
        } else {
            self.search.matches.as_ptr()
        };
        unsafe {
            ffi::ghostty_egui_host_snapshot_set_cell_highlights(
                self.host,
                snapshot_generation,
                ranges,
                self.search.matches.len(),
                selected.map_or(std::ptr::null(), std::ptr::from_ref),
                reveal_selected,
            );
        }
    }

    fn show_search_overlay(&mut self, ctx: &egui::Context) {
        if !self.search.active {
            return;
        }

        let mut query_changed = false;
        let mut mode_changed = false;
        let mut navigate = None;
        let mut close = false;
        let available_width = ctx.content_rect().width();
        let search_width = (available_width - 24.0).max(1.0);
        // Gruvbox dark: lift the search box one shade above the terminal background.
        let fill = egui::Color32::from_rgb(0x3c, 0x38, 0x36); // dark1
        let border = egui::Color32::from_rgb(0x66, 0x5c, 0x54); // dark3
        let accent = egui::Color32::from_rgb(0xfa, 0xbd, 0x2f); // bright yellow
        let text_color = egui::Color32::from_rgb(0xeb, 0xdb, 0xb2); // light1
        let muted_text = egui::Color32::from_rgb(0xd5, 0xc4, 0xa1); // light2
        let selection = egui::Color32::from_rgb(0x66, 0x5c, 0x54); // dark3

        egui::Area::new("terminal-search".into())
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, 0.0])
            .order(egui::Order::Foreground)
            .fade_in(false)
            .movable(false)
            .show(ctx, |ui| {
                let frame = egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(12, 3));
                let frame_response = frame.show(ui, |ui| {
                    ui.set_width(search_width);
                    ui.visuals_mut().text_cursor.stroke = egui::Stroke::new(2.0, text_color);
                    ui.visuals_mut().selection.bg_fill = selection;
                    ui.visuals_mut().selection.stroke.color = text_color;
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add_sized(
                            [16.0, 24.0],
                            egui::Label::new(
                                egui::RichText::new("/")
                                    .font(egui::FontId::monospace(16.0))
                                    .color(accent),
                            ),
                        );

                        let match_label = if self.pending_search_snapshot.is_some() {
                            "…".to_owned()
                        } else if self.search.error.is_some() {
                            "INVALID REGEX".to_owned()
                        } else {
                            self.search.match_label()
                        };
                        let match_label = if available_width >= 600.0 {
                            let mode = if self.search.regex {
                                "REGEX"
                            } else {
                                "LITERAL"
                            };
                            format!("{match_label}   ·   {mode}")
                        } else {
                            match_label
                        };
                        let match_width = if available_width >= 600.0 {
                            210.0
                        } else {
                            74.0
                        };
                        let input_width = (ui.available_width() - match_width - 10.0).max(1.0);
                        let response = ui.add_sized(
                            [input_width, 24.0],
                            egui::TextEdit::singleline(&mut self.search.query)
                                .frame(egui::Frame::NONE)
                                .font(egui::FontId::monospace(15.0))
                                .vertical_align(egui::Align::Center)
                                .text_color(text_color),
                        );
                        if self.search.focus_requested {
                            response.request_focus();
                            self.search.focus_requested = false;
                        }
                        query_changed = response.changed();

                        let (count_rect, count_response) = ui.allocate_exact_size(
                            egui::vec2(match_width, response.rect.height()),
                            egui::Sense::hover(),
                        );
                        if let Some(error) = &self.search.error {
                            count_response.on_hover_text(error);
                        }
                        let match_color = if self.search.error.is_some() {
                            accent
                        } else {
                            muted_text
                        };
                        ui.painter().text(
                            count_rect.right_center(),
                            egui::Align2::RIGHT_CENTER,
                            match_label,
                            egui::FontId::monospace(14.0),
                            match_color,
                        );

                        if response.has_focus() {
                            if ui.input(|input| {
                                input.modifiers.ctrl && input.key_pressed(egui::Key::R)
                            }) {
                                self.search.regex = !self.search.regex;
                                self.search.error = None;
                                mode_changed = true;
                            }
                            let search_key = ui.input(|input| {
                                if input.modifiers.ctrl && input.key_pressed(egui::Key::N) {
                                    Some(false)
                                } else if input.modifiers.ctrl && input.key_pressed(egui::Key::P) {
                                    Some(true)
                                } else if input.key_pressed(egui::Key::Enter) {
                                    Some(input.modifiers.shift)
                                } else {
                                    None
                                }
                            });
                            if search_key.is_some() {
                                navigate = search_key;
                            }
                            if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                                close = true;
                            }
                        }
                    });
                });
                ui.painter().line_segment(
                    [
                        frame_response.response.rect.left_top(),
                        frame_response.response.rect.right_top(),
                    ],
                    egui::Stroke::new(1.0, border),
                );
            });

        if query_changed || mode_changed {
            self.queue_search();
        }
        if let Some(previous) = navigate {
            self.navigate_search(previous);
        }
        if close {
            self.close_search();
        }
    }
}

fn is_copy_mode_shortcut(event: &egui::Event) -> bool {
    matches!(event, egui::Event::Key {
        key: egui::Key::M,
        pressed: true,
        modifiers,
        ..
    } if modifiers.ctrl && modifiers.shift)
}

fn copy_motion(key: egui::Key, modifiers: egui::Modifiers) -> Option<Motion> {
    use egui::Key;
    if modifiers.alt || modifiers.mac_cmd {
        return None;
    }
    if modifiers.ctrl {
        return if modifiers.shift {
            None
        } else {
            match key {
                Key::U => Some(Motion::HalfPageUp),
                Key::D => Some(Motion::HalfPageDown),
                _ => None,
            }
        };
    }
    if modifiers.command {
        return None;
    }
    if modifiers.shift {
        return match key {
            Key::H => Some(Motion::ViewportTop),
            Key::M => Some(Motion::ViewportMiddle),
            Key::L => Some(Motion::ViewportBottom),
            _ => None,
        };
    }
    match key {
        Key::H | Key::ArrowLeft => Some(Motion::Left),
        Key::J | Key::ArrowDown => Some(Motion::Down),
        Key::K | Key::ArrowUp => Some(Motion::Up),
        Key::L | Key::ArrowRight => Some(Motion::Right),
        Key::PageUp => Some(Motion::PageUp),
        Key::PageDown => Some(Motion::PageDown),
        _ => None,
    }
}

#[cfg(test)]
mod copy_shortcut_tests {
    use super::{copy_motion, is_copy_mode_shortcut};

    #[test]
    fn copy_mode_shortcut_requires_ctrl_shift_m() {
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        let key_event = |key, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        };
        assert!(is_copy_mode_shortcut(&key_event(egui::Key::M, ctrl_shift)));
        assert!(!is_copy_mode_shortcut(&key_event(
            egui::Key::M,
            egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        )));
        assert!(!is_copy_mode_shortcut(&key_event(egui::Key::X, ctrl_shift)));
        assert!(!is_copy_mode_shortcut(&egui::Event::Cut));
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

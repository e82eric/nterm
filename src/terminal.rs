use crate::ffi::{self, GlFrame, Host};
use crate::input;
use crate::painter::TerminalPainter;
use crate::search::{SearchEvents, SearchUi};
use glow::HasContext as _;
use std::ffi::{c_char, c_void};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct WakeState {
    context: egui::Context,
    close: AtomicBool,
    search: Mutex<SearchEvents>,
}

#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_wakeup(userdata: *mut c_void) {
    let state = unsafe { &*(userdata.cast::<WakeState>()) };
    state.context.request_repaint();
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
    frame: Option<GlFrame>,
    painter: Arc<egui::mutex::Mutex<TerminalPainter>>,
    size: [u32; 2],
    scale: f32,
    focused: bool,
    search: SearchUi,
}

impl TerminalView {
    pub(crate) fn new(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        argc: usize,
        argv: *mut *mut c_char,
    ) -> Self {
        let mut wake = Box::new(WakeState {
            context: egui_ctx.clone(),
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

        Self {
            host,
            wake,
            frame: None,
            painter: Arc::new(egui::mutex::Mutex::new(TerminalPainter::new(gl))),
            size: [0, 0],
            scale: 0.0,
            focused: false,
            search: SearchUi::default(),
        }
    }

    pub(crate) fn update(&mut self, ui: &mut egui::Ui) {
        unsafe { ffi::ghostty_egui_host_tick(self.host) };
        self.apply_search_events();

        let rect = ui.max_rect();
        let scale = ui.ctx().pixels_per_point();
        let width = (rect.width() * scale).round().max(1.0) as u32;
        let height = (rect.height() * scale).round().max(1.0) as u32;
        if self.size != [width, height] || self.scale != scale {
            self.size = [width, height];
            self.scale = scale;
            unsafe { ffi::ghostty_egui_host_resize(self.host, width, height, scale as f64) };
        }

        let focused = ui.input(|input| input.viewport().focused.unwrap_or(false));
        if self.focused != focused {
            self.focused = focused;
            unsafe { ffi::ghostty_egui_host_focus(self.host, focused) };
        }

        let mut next = GlFrame::default();
        if unsafe { ffi::ghostty_egui_host_take_frame(self.host, &mut next) }
            && let Some(old) = self.frame.replace(next)
        {
            self.painter.lock().retire(old.handle);
        }

        if let Some(frame) = self.frame.as_ref() {
            let painter = Arc::clone(&self.painter);
            let texture = frame.texture;
            let handle = frame.handle as usize;
            ui.painter().add(egui::PaintCallback {
                rect,
                callback: Arc::new(egui_glow::CallbackFn::new(move |_info, glow_painter| {
                    painter.lock().paint(glow_painter.gl(), texture, handle);
                })),
            });
        } else {
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
        }

        self.show_search_overlay(ui.ctx());
        input::forward(self.host, self.size, ui, rect, scale);

        if !self.search.active {
            egui::Area::new("sample-overlay".into())
                .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -12.0])
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.small("egui shell · Ghostty OpenGL texture");
                    });
                });
        }

        if self.should_close() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.painter.lock().has_retired_frames() {
            ui.ctx().request_repaint();
        }
    }

    pub(crate) fn should_close(&self) -> bool {
        self.wake.close.load(Ordering::Acquire)
    }

    pub(crate) fn destroy(&mut self, gl: &glow::Context) {
        self.close_search();
        unsafe { gl.finish() };
        self.painter.lock().destroy(gl);
        if let Some(frame) = self.frame.take() {
            unsafe { ffi::ghostty_egui_host_release_frame(frame.handle) };
        }
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
        self.close_search();

        let mut generation = 0;
        if !unsafe { ffi::ghostty_egui_host_snapshot_begin(self.host, &mut generation) } {
            return;
        }

        let mut columns = 0;
        let mut screen_rows = 0;
        let mut total_rows = 0;
        if !unsafe {
            ffi::ghostty_egui_host_snapshot_size(
                self.host,
                generation,
                &mut columns,
                &mut screen_rows,
                &mut total_rows,
            )
        } || columns == 0
            || total_rows > u32::MAX as u64
        {
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
            return;
        }

        let mut rows = Vec::with_capacity(total_rows as usize);
        for row in 0..total_rows as u32 {
            let mut text = std::ptr::null_mut();
            let mut len = 0;
            if !unsafe {
                ffi::ghostty_egui_host_snapshot_read_row(
                    self.host, generation, columns, row, &mut text, &mut len,
                )
            } {
                unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
                return;
            }
            let value = String::from_utf8_lossy(unsafe {
                std::slice::from_raw_parts(text.cast::<u8>(), len)
            })
            .trim_end_matches(['\r', '\n'])
            .to_owned();
            unsafe { ffi::ghostty_egui_host_snapshot_free_text(text) };
            rows.push(value);
        }

        self.search = SearchUi {
            active: true,
            query: needle,
            generation: Some(generation),
            columns,
            rows,
            matches: Vec::new(),
            selected: None,
            focus_requested: true,
        };
        self.rebuild_search_matches();
    }

    fn close_search(&mut self) {
        if let Some(generation) = self.search.generation.take() {
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
        }
        self.search = SearchUi::default();
    }

    fn rebuild_search_matches(&mut self) {
        self.search.rebuild_matches();
        self.publish_search_matches();
    }

    fn navigate_search(&mut self, previous: bool) {
        self.search.navigate(previous);
        self.publish_search_matches();
    }

    fn publish_search_matches(&self) {
        let Some(generation) = self.search.generation else {
            return;
        };
        let selected = self.search.selected_range();
        let ranges = if self.search.matches.is_empty() {
            std::ptr::null()
        } else {
            self.search.matches.as_ptr()
        };
        unsafe {
            ffi::ghostty_egui_host_snapshot_set_highlights(
                self.host,
                generation,
                ranges,
                self.search.matches.len(),
                selected.map_or(std::ptr::null(), std::ptr::from_ref),
            );
        }
    }

    fn show_search_overlay(&mut self, ctx: &egui::Context) {
        if !self.search.active {
            return;
        }

        let mut query_changed = false;
        let mut navigate = None;
        let mut close = false;
        let available_width = ctx.content_rect().width();
        let search_width = (available_width * 0.86).clamp(320.0, 1650.0);
        let fill = egui::Color32::from_rgb(39, 39, 39);
        let border = egui::Color32::from_rgb(143, 136, 129);
        let accent = egui::Color32::from_rgb(197, 211, 22);

        egui::Area::new("terminal-search".into())
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -12.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(search_width);
                let frame = egui::Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(2.0, border))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(10, 7));
                let frame_response = frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let match_label = self.search.match_label();
                        let match_width = 64.0;
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.search.query)
                                .hint_text("Search")
                                .frame(egui::Frame::NONE)
                                .desired_width((search_width - match_width).max(1.0)),
                        );
                        if self.search.focus_requested {
                            response.request_focus();
                            self.search.focus_requested = false;
                        }
                        query_changed = response.changed();

                        ui.add_sized(
                            [match_width, response.rect.height()],
                            egui::Label::new(
                                egui::RichText::new(match_label).small().color(border),
                            ),
                        );

                        if response.has_focus() {
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

                let title_center = frame_response.response.rect.center_top();
                let title_rect = egui::Rect::from_center_size(title_center, egui::vec2(76.0, 20.0));
                ui.painter().rect_filled(title_rect, 0.0, fill);
                ui.painter().text(
                    title_center,
                    egui::Align2::CENTER_CENTER,
                    "?Search",
                    egui::FontId::proportional(14.0),
                    accent,
                );
            });

        if query_changed {
            self.rebuild_search_matches();
        }
        if let Some(previous) = navigate {
            self.navigate_search(previous);
        }
        if close {
            self.close_search();
        }
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

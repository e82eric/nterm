use crate::app::UserEvent;
use crate::copy_mode::{
    CopyMode, Jump, Motion, MotionEvaluation, MotionResult, SCROLL_PADDING_ROWS, TextRow,
    ViewportPosition, VisualMode, WordObject, YANK_HIGHLIGHT_DURATION, YankPending,
    find_prompt_boundaries, next_match_cell, word_search_pattern,
};
use crate::ffi::{
    self, GlFrame, Host, SnapshotCell, SnapshotCellRange, SnapshotDecorationLayer,
    SnapshotDecorationStyle, SnapshotTextRow, SnapshotView, SnapshotViewportOptions,
};
use crate::input;
use crate::painter::TerminalPainter;
use crate::quick_select::{self, QuickSelect};
use crate::search::{SearchUi, SearchWorker};
use std::ffi::{c_char, c_void};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

struct WakeState {
    context: egui::Context,
    proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
    wake_pending: AtomicBool,
    frame_ready_pending: AtomicBool,
    close: AtomicBool,
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
    quick_select: Option<QuickSelect>,
    copy_search: CopySearch,
    prompt_boundaries: Option<(u64, Vec<u64>)>,
}

#[derive(Default)]
struct CopySearch {
    cursor: usize,
    query_revision: u64,
    matches: Vec<SnapshotCellRange>,
    forward: bool,
    waiting: bool,
    pending_moves: Vec<(bool, u32)>,
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
            quick_select: None,
            copy_search: CopySearch::default(),
            prompt_boundaries: None,
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
        if self.pending_search_snapshot.is_some() {
            self.prepare_search_snapshot();
        }
        self.apply_search_result();

        // Reserve the same status height in every mode. Toggling copy mode
        // must not resize the terminal and invalidate its frozen snapshot.
        let available = ui.max_rect();
        let status_height = 24.0_f32.min((available.height() - 1.0).max(0.0));
        let rect = egui::Rect::from_min_max(
            available.min,
            egui::pos2(available.right(), available.bottom() - status_height),
        );
        let status_rect = egui::Rect::from_min_max(rect.left_bottom(), available.max);
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

        if let Some(generation) = self.copy_mode.generation
            && self.quick_select.is_none()
        {
            let now = Instant::now();
            if self.copy_mode.clear_expired_yank_highlight(now) {
                self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
            } else if let Some(remaining) = self.copy_mode.yank_highlight_remaining(now) {
                ui.ctx().request_repaint_after(remaining);
            }
        }

        ui.painter().add(egui::PaintCallback {
            rect,
            callback: self.paint_callback.clone(),
        });

        // Search may surrender text-field focus when Enter transfers its
        // snapshot to copy mode. Consume that frame's input even after the
        // overlay closes, so Enter is never also sent to the PTY.
        let quick_select_consumed = self.handle_quick_select_keys(ui);
        let search_shortcut_consumed = !quick_select_consumed && self.handle_search_shortcut(ui);
        let copy_input_consumed = !quick_select_consumed && self.handle_copy_keys(ui);
        let keyboard_blocked = quick_select_consumed
            || copy_input_consumed
            || self.search.active
            || search_shortcut_consumed;
        self.show_search_overlay(ui.ctx(), status_height);
        let scrolled = if quick_select_consumed {
            false // Hint labels are stable until this mode is completed or canceled.
        } else {
            input::forward(self.host, self.size, ui, rect, scale, keyboard_blocked)
        };
        if (scrolled || resized)
            && let Some(snapshot_generation) = self.search.snapshot_generation
            && unsafe {
                ffi::ghostty_egui_host_snapshot_decorations_need_refresh(
                    self.host,
                    snapshot_generation,
                )
            }
        {
            self.publish_search_matches(false);
        }
        if scrolled
            && let Some(generation) = self.copy_mode.generation
            && unsafe {
                ffi::ghostty_egui_host_snapshot_decorations_need_refresh(self.host, generation)
            }
        {
            self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
        }

        self.show_mode_status(ui, status_rect);

        if self.should_close() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(crate) fn should_close(&self) -> bool {
        self.wake.close.load(Ordering::Acquire)
    }

    fn show_mode_status(&self, ui: &egui::Ui, rect: egui::Rect) {
        let copying = self.copy_mode.generation.is_some();
        let visual =
            copying && self.quick_select.is_none() && self.copy_mode.visual_anchor.is_some();
        let label = if self.quick_select.is_some() {
            "QUICK SELECT"
        } else if self.search.active {
            "SEARCH"
        } else if visual {
            match self.copy_mode.visual_mode {
                VisualMode::Characterwise => "VISUAL",
                VisualMode::Linewise => "V-LINE",
                VisualMode::Blockwise => "V-BLOCK",
            }
        } else if copying {
            "NORMAL"
        } else {
            "TERMINAL"
        };
        let accent = if visual {
            egui::Color32::from_rgb(0xd6, 0x5d, 0x0e)
        } else if copying || self.search.active {
            egui::Color32::from_rgb(0xd7, 0x99, 0x21)
        } else {
            egui::Color32::from_rgb(0x92, 0x83, 0x74)
        };
        let background = egui::Color32::from_rgb(0x1d, 0x20, 0x21);
        let foreground = egui::Color32::from_rgb(0xeb, 0xdb, 0xb2);
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, background);
        let separator = if copying || self.search.active {
            accent
        } else {
            egui::Color32::from_rgb(0x3c, 0x38, 0x36)
        };
        painter.line_segment(
            [
                rect.left_top() + egui::vec2(0.0, 0.75),
                rect.right_top() + egui::vec2(0.0, 0.75),
            ],
            egui::Stroke::new(1.5, separator),
        );
        let font = egui::FontId::monospace(15.0);
        let label_rect = painter.text(
            egui::pos2(rect.left() + 10.0, rect.center().y + 1.0),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            accent,
        );
        if let Some(quick) = &self.quick_select {
            let text = if quick.hints.is_empty() {
                "NO MATCHES · Esc".to_owned()
            } else {
                format!(
                    "{}  ·  {} hints",
                    quick.prefix.to_ascii_uppercase(),
                    quick.visible_hints().count()
                )
            };
            painter.text(
                egui::pos2(rect.right() - 10.0, rect.center().y + 1.0),
                egui::Align2::RIGHT_CENTER,
                text,
                font,
                foreground,
            );
        } else if copying {
            // One-based physical scrollback row and terminal column.
            let cursor = self.copy_mode.cursor;
            let position = painter.layout_no_wrap(
                format!(
                    "{}:{}",
                    cursor.y.saturating_add(1),
                    cursor.x.saturating_add(1)
                ),
                font,
                foreground,
            );
            let left = rect.right() - 10.0 - position.size().x;
            if left > label_rect.right() + 8.0 {
                painter.galley(
                    egui::pos2(left, rect.center().y + 1.0 - position.size().y / 2.0),
                    position,
                    foreground,
                );
            }
        }
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

    fn handle_quick_select_keys(&mut self, ui: &egui::Ui) -> bool {
        let was_active = self.quick_select.is_some();
        let (open, consumed) =
            ui.input_mut(|input| quick_select::consume_shortcut(&mut input.events));
        if open && !self.search.active {
            if was_active {
                self.cancel_quick_select();
            } else {
                self.open_quick_select();
            }
        }
        if self.quick_select.is_none() {
            return was_active || consumed;
        }
        let events = ui.input(|input| input.events.clone());
        for event in events {
            match event {
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                } => {
                    self.cancel_quick_select();
                    break;
                }
                egui::Event::Key {
                    key: egui::Key::Backspace,
                    pressed: true,
                    ..
                } => {
                    self.quick_select
                        .as_mut()
                        .expect("quick select is active")
                        .prefix
                        .pop();
                    self.publish_quick_select();
                }
                egui::Event::Text(text) => {
                    let target = self
                        .quick_select
                        .as_mut()
                        .expect("quick select is active")
                        .type_text(&text);
                    if let Some(target) = target {
                        self.accept_quick_select(target);
                        break;
                    }
                    self.publish_quick_select();
                }
                _ => {}
            }
        }
        true
    }

    fn open_quick_select(&mut self) {
        let from_copy = self.copy_mode.generation.is_some();
        let generation = if let Some(generation) = self.copy_mode.generation {
            generation
        } else {
            let mut generation = 0;
            if !unsafe { ffi::ghostty_egui_host_snapshot_begin(self.host, &mut generation) } {
                return;
            }
            generation
        };
        let mut view = SnapshotView::default();
        if !unsafe { ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view) } {
            if !from_copy {
                unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
            }
            return;
        }
        if !from_copy {
            self.copy_mode.start(generation, view);
        }
        self.quick_select = QuickSelect::new(generation, from_copy, view, |row| {
            self.read_snapshot_row(generation, row)
        });
        if self.quick_select.is_none() {
            if !from_copy {
                self.close_copy_mode();
            }
            return;
        }
        if !self.publish_quick_select() {
            self.cancel_quick_select();
        }
        self.wake.context.request_repaint();
    }

    fn publish_quick_select(&self) -> bool {
        let Some(quick) = &self.quick_select else {
            return false;
        };
        let ranges: Vec<_> = quick.visible_hints().map(|hint| hint.range).collect();
        let text = quick.cell_text();
        let layer = SnapshotDecorationLayer {
            ranges: ranges.as_ptr(),
            range_count: ranges.len(),
            style: SnapshotDecorationStyle {
                foreground_rgb: 0xebdbb2,
                background_rgb: 0x3c3836,
                flags: 2, // Preserve the underlying terminal's foreground colors.
            },
        };
        unsafe {
            ffi::ghostty_egui_host_snapshot_apply_cell_text(
                self.host,
                quick.generation,
                &layer,
                1,
                text.as_ptr(),
                text.len(),
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
            self.close_copy_mode();
        }
        self.wake.context.request_repaint();
    }

    fn accept_quick_select(&mut self, target: SnapshotCell) {
        let Some(quick) = self.quick_select.take() else {
            return;
        };
        self.copy_mode.visual_anchor = None;
        self.copy_mode.pending_yank = None;
        self.copy_mode.pending_visual_object = None;
        self.copy_mode.pending_jump = None;
        self.copy_mode.pending_count = None;
        self.copy_mode.pending_g = false;
        self.copy_mode.pending_z = false;
        self.apply_copy_target(quick.generation, Motion::CharacterJump, target, false);
        self.wake.context.request_repaint();
    }

    fn handle_search_shortcut(&mut self, ui: &egui::Ui) -> bool {
        let (open, consumed) = ui.input_mut(|input| consume_search_shortcut(&mut input.events));
        if open {
            if self.search.active {
                self.search.focus_requested = true;
                ui.ctx().request_repaint();
            } else {
                self.open_search(String::new());
            }
        }
        consumed
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
        let mut state = SnapshotView::default();
        let mut actual = SnapshotCell::default();
        let ready = unsafe {
            ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut state)
                && ffi::ghostty_egui_host_snapshot_resolve_cell(
                    self.host,
                    generation,
                    state.cursor,
                    false,
                    &mut actual,
                )
        };
        if !ready || !self.publish_copy_decoration(generation, actual, true) {
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
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
        let mut view = SnapshotView::default();
        if !unsafe { ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view) } {
            return;
        }
        let Some(actual) = self.resolve_copy_cell(generation, target, false) else {
            return;
        };
        self.search_worker.cancel_and_wait();
        // Transfer ownership instead of closing search: ending this snapshot
        // would invalidate the match coordinates and briefly show live output.
        let mut search = std::mem::take(&mut self.search);
        search.matches.reverse();
        view.cursor = actual;
        self.copy_mode.start(generation, view);
        self.copy_search = CopySearch {
            cursor: search.cursor,
            query_revision: search.query_revision,
            matches: search.matches,
            forward: false,
            ..Default::default()
        };
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
        self.quick_select = None;
        if let Some(generation) = self.copy_mode.generation {
            if self.copy_search.cursor != 0 {
                self.search_worker.cancel_and_wait();
                unsafe {
                    ffi::ghostty_egui_host_snapshot_cursor_free(self.copy_search.cursor as *mut _)
                };
            }
            unsafe { ffi::ghostty_egui_host_snapshot_end(self.host, generation) };
            self.copy_mode = CopyMode::default();
            self.copy_search = CopySearch::default();
            self.prompt_boundaries = None;
            self.wake.context.request_repaint();
        }
    }

    fn handle_copy_keys(&mut self, ui: &egui::Ui) -> bool {
        let was_active = self.copy_mode.generation.is_some();
        let events = ui.input(|input| input.events.clone());
        let mut handled = false;
        let mut skip_trigger_text = false;
        for event in events {
            if self.copy_mode.generation.is_none() {
                let command_direction = copy_command_shortcut(&event);
                if !self.search.active
                    && (is_copy_mode_shortcut(&event) || command_direction.is_some())
                {
                    self.open_copy_mode();
                    handled = true;
                    if let Some(forward) = command_direction {
                        self.navigate_copy_command(forward, 1);
                    }
                }
                continue;
            }
            if let Some(forward) = copy_command_shortcut(&event) {
                handled = true;
                let count = self.copy_mode.take_count();
                self.copy_mode.pending_g = false;
                self.copy_mode.pending_z = false;
                self.copy_mode.pending_jump = None;
                self.copy_mode.pending_yank = None;
                self.copy_mode.pending_visual_object = None;
                if !self.navigate_copy_command(forward, count) {
                    break;
                }
                continue;
            }
            if let egui::Event::Text(text) = &event {
                if let Some(pending) = self.copy_mode.pending_yank
                    && let Some((forward, till)) = pending.jump
                {
                    if skip_trigger_text {
                        skip_trigger_text = false;
                        continue;
                    }
                    let mut graphemes = text.graphemes(true);
                    if let Some(target) = graphemes.next()
                        && graphemes.next().is_none()
                    {
                        self.copy_mode.pending_yank = None;
                        let jump = Jump {
                            target: target.to_owned(),
                            forward,
                            till,
                        };
                        self.copy_mode.last_jump = Some(jump.clone());
                        let generation = self.copy_mode.generation.expect("copy mode is active");
                        let count = pending.count.saturating_mul(self.copy_mode.take_count());
                        if let Some(cell) = self.copy_jump_target(generation, &jump, count)
                            && let Some((range, linewise)) = self
                                .copy_mode
                                .yank_motion_range(Motion::CharacterJump.result(cell), |row| {
                                    self.read_snapshot_row(generation, row)
                                })
                            && self.yank_range(ui.ctx(), generation, range, linewise, true)
                        {
                            self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                        }
                    }
                    continue;
                }
                if let Some((forward, till)) = self.copy_mode.pending_jump {
                    if skip_trigger_text {
                        skip_trigger_text = false;
                        continue;
                    }
                    let mut graphemes = text.graphemes(true);
                    if let Some(target) = graphemes.next()
                        && graphemes.next().is_none()
                    {
                        self.copy_mode.pending_jump = None;
                        let jump = Jump {
                            target: target.to_owned(),
                            forward,
                            till,
                        };
                        self.copy_mode.last_jump = Some(jump.clone());
                        let count = self.copy_mode.take_count();
                        if !self.perform_copy_jump(&jump, false, count) {
                            break;
                        }
                    }
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
            // egui 0.36 emits a separate Key event when Shift, Ctrl, Alt, or
            // Super is pressed. It changes the modifiers on the next key but
            // is never itself a copy-mode command or an operator target.
            if is_copy_modifier_key(key) {
                continue;
            }
            if key == egui::Key::Escape {
                if self.copy_mode.pending_z {
                    self.copy_mode.pending_z = false;
                    self.copy_mode.pending_count = None;
                    continue;
                }
                if self.copy_mode.pending_visual_object.take().is_some() {
                    self.copy_mode.pending_count = None;
                    continue;
                }
                if self.copy_mode.pending_yank.take().is_some() {
                    self.copy_mode.pending_count = None;
                    continue;
                }
                if self.copy_mode.visual_anchor.take().is_some() {
                    self.copy_mode.pending_count = None;
                    self.copy_mode.pending_g = false;
                    self.copy_mode.pending_jump = None;
                    let generation = self.copy_mode.generation.expect("copy mode is active");
                    self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                    continue;
                }
                self.close_copy_mode();
                break;
            }
            if self.copy_mode.pending_z {
                self.copy_mode.pending_z = false;
                if let Some(position) = copy_viewport_position_key(key, modifiers)
                    && !self.position_copy_viewport(position)
                {
                    break;
                }
                continue;
            }
            if let Some((object, prefix_count)) = self.copy_mode.pending_visual_object {
                if modifiers == egui::Modifiers::NONE
                    && let Some(digit) = copy_digit(key)
                    && self.copy_mode.add_count_digit(digit)
                {
                    continue;
                }
                self.copy_mode.pending_visual_object = None;
                let count = prefix_count.saturating_mul(self.copy_mode.take_count());
                let generation = self.copy_mode.generation.expect("copy mode is active");
                if let Some(kind) = copy_text_object_key(key, modifiers)
                    && let Some((range, _, trim)) =
                        self.copy_text_object_range(generation, count, object, kind)
                    && !self.select_copy_visual_object(generation, range, trim)
                {
                    break;
                }
                continue;
            }
            if self.copy_mode.pending_jump.is_some() {
                skip_trigger_text = false;
                continue;
            }
            if let Some(mut pending) = self.copy_mode.pending_yank {
                if pending.jump.is_some() {
                    continue;
                }
                if modifiers == egui::Modifiers::NONE
                    && let Some(digit) = copy_digit(key)
                    && self.copy_mode.add_count_digit(digit)
                {
                    continue;
                }
                if pending.object.is_none() && modifiers == egui::Modifiers::NONE {
                    if key == egui::Key::I || key == egui::Key::A {
                        pending.object = Some(if key == egui::Key::I {
                            WordObject::Inner
                        } else {
                            WordObject::Around
                        });
                        self.copy_mode.pending_yank = Some(pending);
                        continue;
                    }
                }
                if pending.object.is_none()
                    && let Some(jump) = copy_jump_prefix(key, modifiers)
                {
                    pending.jump = Some(jump);
                    self.copy_mode.pending_yank = Some(pending);
                    skip_trigger_text = true;
                    continue;
                }
                let pending_motion = if pending.object.is_none() {
                    copy_motion_with_prefix(&mut self.copy_mode, key, modifiers)
                } else {
                    None
                };
                if self.copy_mode.pending_g {
                    self.copy_mode.pending_yank = Some(pending);
                    continue;
                }
                self.copy_mode.pending_yank = None;
                let count = pending.count.saturating_mul(self.copy_mode.take_count());
                let generation = self.copy_mode.generation.expect("copy mode is active");
                if pending.object.is_none()
                    && modifiers == egui::Modifiers::NONE
                    && matches!(key, egui::Key::Semicolon | egui::Key::Comma)
                {
                    if let Some(mut jump) = self.copy_mode.last_jump.clone() {
                        if key == egui::Key::Comma {
                            jump.forward = !jump.forward;
                        }
                        if let Some(cell) = self.copy_jump_target(generation, &jump, count)
                            && let Some((range, linewise)) = self
                                .copy_mode
                                .yank_motion_range(Motion::CharacterJump.result(cell), |row| {
                                    self.read_snapshot_row(generation, row)
                                })
                            && self.yank_range(ui.ctx(), generation, range, linewise, true)
                        {
                            self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                        }
                    }
                    continue;
                }
                let (range, linewise, trim) = match (pending.object, key, modifiers) {
                    (None, egui::Key::Y, egui::Modifiers::NONE) => (
                        self.copy_mode
                            .yank_lines_range(count, |row| self.read_snapshot_row(generation, row)),
                        true,
                        true,
                    ),
                    (None, egui::Key::Num4, egui::Modifiers::SHIFT) => (
                        self.copy_mode.yank_to_row_end_range(count, |row| {
                            self.read_snapshot_row(generation, row)
                        }),
                        false,
                        true,
                    ),
                    (Some(object), key, modifiers) => copy_text_object_key(key, modifiers)
                        .and_then(|kind| {
                            self.copy_text_object_range(generation, count, object, kind)
                        })
                        .map_or((None, false, true), |(range, linewise, trim)| {
                            (Some(range), linewise, trim)
                        }),
                    _ => {
                        let target = pending_motion.and_then(|motion| {
                            self.copy_motion_target(
                                generation,
                                motion,
                                count,
                                pending.count > 1 || count > 1,
                            )
                            .and_then(|target| {
                                self.copy_mode.yank_motion_range(target, |row| {
                                    self.read_snapshot_row(generation, row)
                                })
                            })
                        });
                        match target {
                            Some((range, linewise)) => (Some(range), linewise, true),
                            None => (None, false, true),
                        }
                    }
                };
                if let Some(range) = range {
                    if self.yank_range(ui.ctx(), generation, range, linewise, trim) {
                        self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                    }
                }
                continue;
            }
            if !self.copy_mode.pending_g
                && key == egui::Key::O
                && (modifiers == egui::Modifiers::NONE || modifiers == egui::Modifiers::SHIFT)
            {
                self.copy_mode.pending_count = None;
                let swapped = if modifiers.shift {
                    self.copy_mode.swap_visual_horizontal_endpoint()
                } else {
                    self.copy_mode.swap_visual_endpoint()
                };
                if swapped {
                    let generation = self.copy_mode.generation.expect("copy mode is active");
                    if !self.apply_copy_target(
                        generation,
                        Motion::CharacterJump,
                        self.copy_mode.cursor,
                        false,
                    ) {
                        break;
                    }
                }
                continue;
            }
            if !self.copy_mode.pending_g
                && let Some(down) = copy_scroll_direction(key, modifiers)
            {
                let count = self.copy_mode.take_count();
                if !self.scroll_copy_viewport(down, count) {
                    break;
                }
                continue;
            }
            if modifiers == egui::Modifiers::NONE && key == egui::Key::Z {
                self.copy_mode.pending_z = true;
                self.copy_mode.pending_g = false;
                self.copy_mode.pending_count = None;
                continue;
            }
            if modifiers == egui::Modifiers::NONE
                && matches!(key, egui::Key::I | egui::Key::A)
                && self.copy_mode.begin_visual_object(if key == egui::Key::I {
                    WordObject::Inner
                } else {
                    WordObject::Around
                })
            {
                continue;
            }
            if key == egui::Key::Q
                && modifiers.ctrl
                && !modifiers.shift
                && !modifiers.alt
                && !modifiers.mac_cmd
            {
                self.copy_mode.pending_count = None;
                self.copy_mode.pending_g = false;
                self.copy_mode.toggle_visual(VisualMode::Blockwise);
                let generation = self.copy_mode.generation.expect("copy mode is active");
                self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                continue;
            }
            if key == egui::Key::V
                && (modifiers == egui::Modifiers::NONE || modifiers == egui::Modifiers::SHIFT)
            {
                self.copy_mode.pending_count = None;
                self.copy_mode.pending_g = false;
                let mode = if modifiers.shift {
                    VisualMode::Linewise
                } else {
                    VisualMode::Characterwise
                };
                self.copy_mode.toggle_visual(mode);
                let generation = self.copy_mode.generation.expect("copy mode is active");
                self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                continue;
            }
            if modifiers == egui::Modifiers::NONE && key == egui::Key::Y {
                self.copy_mode.pending_g = false;
                if self.copy_mode.visual_anchor.is_some() {
                    self.copy_mode.pending_count = None;
                    if self.yank_visual(ui.ctx()) {
                        self.copy_mode.visual_anchor = None;
                        let generation = self.copy_mode.generation.expect("copy mode is active");
                        self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
                    }
                } else {
                    self.copy_mode.pending_yank = Some(YankPending {
                        count: self.copy_mode.take_count(),
                        object: None,
                        jump: None,
                    });
                }
                continue;
            }
            if modifiers == egui::Modifiers::NONE
                && let Some(digit) = copy_digit(key)
                && self.copy_mode.add_count_digit(digit)
            {
                continue;
            }
            if let Some((forward, till)) = copy_jump_prefix(key, modifiers) {
                self.copy_mode.pending_g = false;
                self.copy_mode.pending_jump = Some((forward, till));
                skip_trigger_text = true;
                continue;
            }
            if modifiers == egui::Modifiers::NONE
                && matches!(key, egui::Key::Semicolon | egui::Key::Comma)
            {
                self.copy_mode.pending_g = false;
                let count = self.copy_mode.take_count();
                if let Some(mut jump) = self.copy_mode.last_jump.clone() {
                    if key == egui::Key::Comma {
                        jump.forward = !jump.forward;
                    }
                    if !self.perform_copy_jump(&jump, true, count) {
                        break;
                    }
                }
                continue;
            }
            if modifiers.shift
                && !modifiers.ctrl
                && !modifiers.alt
                && !modifiers.mac_cmd
                && matches!(key, egui::Key::Num8 | egui::Key::Num3)
            {
                self.copy_mode.pending_g = false;
                let count = self.copy_mode.take_count();
                self.search_copy_word(key == egui::Key::Num8, count);
                continue;
            }
            if !modifiers.ctrl && !modifiers.alt && !modifiers.mac_cmd && key == egui::Key::N {
                self.copy_mode.pending_g = false;
                let count = self.copy_mode.take_count();
                self.navigate_copy_search(modifiers.shift, count);
                continue;
            }
            let explicit_count = self.copy_mode.pending_count.is_some();
            let motion = copy_motion_with_prefix(&mut self.copy_mode, key, modifiers);
            let Some(motion) = motion else {
                if !self.copy_mode.pending_g {
                    self.copy_mode.pending_count = None;
                }
                continue;
            };
            let count = self.copy_mode.take_count();
            if !self.perform_copy_motion(motion, count, explicit_count) {
                break;
            }
        }
        was_active || self.copy_mode.generation.is_some() || handled
    }

    fn scroll_copy_viewport(&mut self, down: bool, count: u32) -> bool {
        let generation = self.copy_mode.generation.expect("copy mode is active");
        let mut view = SnapshotView::default();
        if !unsafe { ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view) } {
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
        if !unsafe { ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view) } {
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
            ffi::ghostty_egui_host_snapshot_resolve_cell(
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
            if !unsafe { ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view) } {
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
        let mut result = SnapshotTextRow::default();
        if !unsafe {
            ffi::ghostty_egui_host_snapshot_text_row(self.host, generation, row, &mut result)
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
        unsafe { ffi::ghostty_egui_host_snapshot_text_row_free(&mut result) };
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
        self.search.query_revision = self.search_worker.submit(
            snapshot_generation,
            self.search.query.clone(),
            self.search.regex,
            self.search.cursor,
        );
        self.search.waiting = true;
    }

    fn apply_search_result(&mut self) {
        if self.quick_select.is_some() {
            return;
        }
        let Some(result) = self.search_worker.take_latest_result() else {
            return;
        };
        if self.copy_mode.generation == Some(result.snapshot_generation)
            && self.copy_search.waiting
            && self.copy_search.query_revision == result.query_revision
        {
            self.copy_search.waiting = false;
            self.copy_search.matches = result.matches;
            self.copy_search.matches.reverse();
            let mut target = self.copy_mode.cursor;
            for (forward, count) in std::mem::take(&mut self.copy_search.pending_moves) {
                if let Some(next) =
                    next_match_cell(&self.copy_search.matches, target, forward, count)
                {
                    target = next;
                }
            }
            self.apply_copy_target(
                result.snapshot_generation,
                Motion::CharacterJump,
                target,
                false,
            );
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
        if self.copy_search.cursor == 0 {
            let cursor =
                unsafe { ffi::ghostty_egui_host_snapshot_cursor_new(self.host, generation) };
            if cursor.is_null() {
                return;
            }
            self.copy_search.cursor = cursor as usize;
        }
        self.copy_search.forward = forward;
        self.copy_search.matches.clear();
        self.copy_search.pending_moves.clear();
        self.copy_search.pending_moves.push((forward, count));
        self.copy_search.waiting = true;
        let pattern = word_search_pattern(&word);
        self.copy_search.query_revision =
            self.search_worker
                .submit(generation, pattern, true, self.copy_search.cursor);
        self.publish_copy_decoration(generation, self.copy_mode.cursor, false);
    }

    fn navigate_copy_search(&mut self, reverse: bool, count: u32) {
        let Some(generation) = self.copy_mode.generation else {
            return;
        };
        if self.copy_search.query_revision == 0 {
            return;
        }
        let forward = self.copy_search.forward != reverse;
        if self.copy_search.waiting {
            self.copy_search.pending_moves.push((forward, count));
        } else if let Some(target) = next_match_cell(
            &self.copy_search.matches,
            self.copy_mode.cursor,
            forward,
            count,
        ) {
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
        let selection = self.copy_visual_range(generation, cell);
        let selection_ranges = self.copy_decoration_ranges(
            generation,
            selection,
            self.copy_mode.visual_mode == VisualMode::Blockwise,
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
            ranges: self.copy_search.matches.as_ptr(),
            range_count: self.copy_search.matches.len(),
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
            ffi::ghostty_egui_host_snapshot_apply_decorations(
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
        if !unsafe { ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view) }
            || view.viewport_rows == 0
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

    fn copy_visual_range(&self, generation: u64, cell: SnapshotCell) -> Option<SnapshotCellRange> {
        match self.copy_mode.visual_mode {
            VisualMode::Characterwise => self.copy_mode.visual_range_to(cell),
            VisualMode::Blockwise => self.copy_mode.visual_rectangle_to(cell),
            VisualMode::Linewise => self
                .copy_mode
                .linewise_visual_range_to(cell, |row| self.read_snapshot_row(generation, row)),
        }
    }

    fn copy_text_object_range(
        &mut self,
        generation: u64,
        count: u32,
        object: WordObject,
        kind: CopyTextObject,
    ) -> Option<(SnapshotCellRange, bool, bool)> {
        let range = match kind {
            CopyTextObject::Word(big) => {
                self.copy_mode.word_object_range(count, object, big, |row| {
                    self.read_snapshot_row(generation, row)
                })
            }
            CopyTextObject::Quote(quote) => {
                self.copy_mode.quote_object_range(object, quote, |row| {
                    self.read_snapshot_row(generation, row)
                })
            }
            CopyTextObject::Delimiter(open, close) => {
                self.copy_mode
                    .delimiter_object_range(object, open, close, |row| {
                        self.read_snapshot_row(generation, row)
                    })
            }
            CopyTextObject::Command => {
                if !self.ensure_prompt_boundaries(generation) {
                    return None;
                }
                self.copy_mode.command_object_range(
                    object,
                    count,
                    &self.prompt_boundaries.as_ref()?.1,
                )
            }
        }?;
        let linewise = matches!(kind, CopyTextObject::Command);
        let trim =
            linewise || (matches!(kind, CopyTextObject::Word(_)) && object == WordObject::Inner);
        Some((range, linewise, trim))
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
        let Some(range) = self.copy_visual_range(generation, self.copy_mode.cursor) else {
            return false;
        };
        self.yank_selection(
            ctx,
            generation,
            range,
            self.copy_mode.visual_mode == VisualMode::Linewise,
            self.copy_mode.visual_trim && self.copy_mode.visual_mode != VisualMode::Blockwise,
            self.copy_mode.visual_mode == VisualMode::Blockwise,
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
        let mut result = ffi::SnapshotSelectionText::default();
        if !unsafe {
            ffi::ghostty_egui_host_snapshot_selection_text(
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
        unsafe { ffi::ghostty_egui_host_snapshot_selection_text_free(&mut result) };
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
            ffi::ghostty_egui_host_snapshot_apply_decorations(
                self.host,
                snapshot_generation,
                layers.as_ptr(),
                layers.len(),
                &viewport,
            );
        }
    }

    fn show_search_overlay(&mut self, ctx: &egui::Context, status_height: f32) {
        if !self.search.active {
            return;
        }

        let mut query_changed = false;
        let mut mode_changed = false;
        let mut navigate = None;
        let mut close = false;
        let mut accept = false;
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
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -status_height])
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

                        if response.has_focus() || response.lost_focus() {
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
                                } else {
                                    None
                                }
                            });
                            if search_key.is_some() {
                                navigate = search_key;
                            }
                            if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                                accept = true;
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
            self.search.accept_requested = false;
            self.queue_search();
        }
        if let Some(previous) = navigate {
            self.navigate_search(previous);
        }
        if close {
            self.close_search();
        } else if accept {
            self.accept_search();
        }
    }
}

/// Consume the app shortcut and its paired Text event before either copy
/// mode, the search field, or Ghostty sees them. Repeats do not reopen search.
fn consume_search_shortcut(events: &mut Vec<egui::Event>) -> (bool, bool) {
    let mut incoming = std::mem::take(events).into_iter().peekable();
    let mut open = false;
    let mut consumed = false;
    while let Some(event) = incoming.next() {
        if let egui::Event::Key {
            key: egui::Key::F,
            pressed,
            repeat,
            modifiers,
            ..
        } = &event
            && !modifiers.alt
            && ((modifiers.ctrl && modifiers.shift && !modifiers.mac_cmd)
                || (cfg!(target_os = "macos")
                    && modifiers.mac_cmd
                    && !modifiers.ctrl
                    && !modifiers.shift))
        {
            consumed = true;
            open |= *pressed && !*repeat;
            if *pressed && matches!(incoming.peek(), Some(egui::Event::Text(_))) {
                incoming.next();
            }
        } else {
            events.push(event);
        }
    }
    (open, consumed)
}

fn is_copy_modifier_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

fn copy_viewport_position_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Option<ViewportPosition> {
    if modifiers != egui::Modifiers::NONE {
        return None;
    }
    match key {
        egui::Key::T => Some(ViewportPosition::Top),
        egui::Key::Z => Some(ViewportPosition::Center),
        egui::Key::B => Some(ViewportPosition::Bottom),
        _ => None,
    }
}

fn copy_command_shortcut(event: &egui::Event) -> Option<bool> {
    match event {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } if modifiers.ctrl && modifiers.shift && !modifiers.alt && !modifiers.mac_cmd => match key
        {
            egui::Key::ArrowUp => Some(false),
            egui::Key::ArrowDown => Some(true),
            _ => None,
        },
        _ => None,
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

#[derive(Clone, Copy)]
enum CopyTextObject {
    Word(bool),
    Quote(char),
    Delimiter(char, char),
    Command,
}

fn copy_text_object_key(key: egui::Key, modifiers: egui::Modifiers) -> Option<CopyTextObject> {
    use egui::Key;
    if modifiers.ctrl || modifiers.alt || modifiers.mac_cmd {
        return None;
    }
    match key {
        Key::W => Some(CopyTextObject::Word(modifiers.shift)),
        Key::C if !modifiers.shift => Some(CopyTextObject::Command),
        Key::Quote => Some(CopyTextObject::Quote(if modifiers.shift {
            '"'
        } else {
            '\''
        })),
        Key::Backtick => Some(CopyTextObject::Quote('`')),
        Key::Num9 if modifiers.shift => Some(CopyTextObject::Delimiter('(', ')')),
        Key::Num0 if modifiers.shift => Some(CopyTextObject::Delimiter('(', ')')),
        Key::OpenBracket | Key::CloseBracket => Some(CopyTextObject::Delimiter('[', ']')),
        Key::OpenCurlyBracket | Key::CloseCurlyBracket => Some(CopyTextObject::Delimiter('{', '}')),
        Key::Comma | Key::Period if modifiers.shift => Some(CopyTextObject::Delimiter('<', '>')),
        _ => None,
    }
}

fn copy_scroll_direction(key: egui::Key, modifiers: egui::Modifiers) -> Option<bool> {
    // egui-winit also sets `command` for Ctrl on Windows and Linux.
    if !modifiers.ctrl || modifiers.shift || modifiers.alt || modifiers.mac_cmd {
        return None;
    }
    match key {
        egui::Key::E => Some(true),
        egui::Key::Y => Some(false),
        _ => None,
    }
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
            Key::Num6 => Some(Motion::FirstNonblank),
            Key::Num4 => Some(Motion::LastNonblank),
            Key::W => Some(Motion::ForwardBigWord),
            Key::B => Some(Motion::BackwardBigWord),
            Key::E => Some(Motion::ForwardBigWordEnd),
            Key::G => Some(Motion::ScrollbackBottom),
            Key::Num5 => Some(Motion::MatchingBracket),
            Key::Minus => Some(Motion::LineNonblank),
            Key::Equals | Key::Plus => Some(Motion::NextLineNonblank),
            _ => None,
        };
    }
    match key {
        Key::H | Key::ArrowLeft => Some(Motion::Left),
        Key::J | Key::ArrowDown => Some(Motion::Down),
        Key::K | Key::ArrowUp => Some(Motion::Up),
        Key::L | Key::ArrowRight => Some(Motion::Right),
        Key::W => Some(Motion::ForwardWord),
        Key::B => Some(Motion::BackwardWord),
        Key::E => Some(Motion::ForwardWordEnd),
        Key::Num0 => Some(Motion::RowStart),
        Key::Minus => Some(Motion::PreviousLineNonblank),
        Key::Plus => Some(Motion::NextLineNonblank),
        Key::PageUp => Some(Motion::PageUp),
        Key::PageDown => Some(Motion::PageDown),
        _ => None,
    }
}

fn copy_digit(key: egui::Key) -> Option<u32> {
    use egui::Key;
    Some(match key {
        Key::Num0 => 0,
        Key::Num1 => 1,
        Key::Num2 => 2,
        Key::Num3 => 3,
        Key::Num4 => 4,
        Key::Num5 => 5,
        Key::Num6 => 6,
        Key::Num7 => 7,
        Key::Num8 => 8,
        Key::Num9 => 9,
        _ => return None,
    })
}

fn copy_motion_with_prefix(
    mode: &mut CopyMode,
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Option<Motion> {
    if mode.pending_g {
        mode.pending_g = false;
        if key == egui::Key::G && modifiers == egui::Modifiers::NONE {
            return Some(Motion::ScrollbackTop);
        }
        if key == egui::Key::E && modifiers == egui::Modifiers::NONE {
            return Some(Motion::PreviousWordEnd);
        }
        if key == egui::Key::E
            && modifiers.shift
            && !modifiers.ctrl
            && !modifiers.alt
            && !modifiers.mac_cmd
        {
            return Some(Motion::PreviousBigWordEnd);
        }
    } else if key == egui::Key::G && modifiers == egui::Modifiers::NONE {
        mode.pending_g = true;
        return None;
    }
    copy_motion(key, modifiers)
}

fn copy_jump_prefix(key: egui::Key, modifiers: egui::Modifiers) -> Option<(bool, bool)> {
    if modifiers != egui::Modifiers::NONE
        && modifiers
            != (egui::Modifiers {
                shift: true,
                ..Default::default()
            })
    {
        return None;
    }
    match (key, modifiers.shift) {
        (egui::Key::F, false) => Some((true, false)),
        (egui::Key::F, true) => Some((false, false)),
        (egui::Key::T, false) => Some((true, true)),
        (egui::Key::T, true) => Some((false, true)),
        _ => None,
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
        CopyTextObject, consume_search_shortcut, copy_command_shortcut, copy_digit,
        copy_jump_prefix, copy_motion, copy_motion_with_prefix, copy_text_object_key,
        is_copy_mode_shortcut, is_copy_modifier_key,
    };

    #[test]
    fn search_shortcut_consumes_key_and_paired_text_only() {
        let key = |modifiers, pressed, repeat| egui::Event::Key {
            key: egui::Key::F,
            physical_key: Some(egui::Key::F),
            pressed,
            repeat,
            modifiers,
        };
        let modifiers = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        let mut events = vec![
            key(modifiers, true, false),
            egui::Event::Text("F".into()),
            egui::Event::Paste("query".into()),
        ];
        assert_eq!(consume_search_shortcut(&mut events), (true, true));
        assert_eq!(events, vec![egui::Event::Paste("query".into())]);

        for (pressed, repeat) in [(true, true), (false, false)] {
            let mut events = vec![key(modifiers, pressed, repeat)];
            assert_eq!(consume_search_shortcut(&mut events), (false, true));
            assert!(events.is_empty());
        }
        for modifiers in [
            egui::Modifiers::NONE,
            egui::Modifiers::CTRL,
            egui::Modifiers::SHIFT,
            egui::Modifiers {
                alt: true,
                ..modifiers
            },
        ] {
            let original = vec![key(modifiers, true, false), egui::Event::Text("f".into())];
            let mut events = original.clone();
            assert_eq!(consume_search_shortcut(&mut events), (false, false));
            assert_eq!(events, original);
        }
    }

    #[test]
    fn command_shortcuts_require_ctrl_shift_arrows_and_a_key_press() {
        let event = |key, modifiers, pressed| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        };
        let modifiers = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            copy_command_shortcut(&event(egui::Key::ArrowUp, modifiers, true)),
            Some(false)
        );
        assert_eq!(
            copy_command_shortcut(&event(egui::Key::ArrowDown, modifiers, true)),
            Some(true)
        );
        assert_eq!(
            copy_command_shortcut(&event(egui::Key::ArrowUp, modifiers, false)),
            None
        );
        assert_eq!(
            copy_command_shortcut(&event(egui::Key::ArrowUp, egui::Modifiers::CTRL, true)),
            None
        );
        assert_eq!(
            copy_command_shortcut(&event(egui::Key::ArrowUp, egui::Modifiers::SHIFT, true)),
            None
        );
        assert_eq!(
            copy_command_shortcut(&event(
                egui::Key::ArrowUp,
                egui::Modifiers {
                    alt: true,
                    ..modifiers
                },
                true
            )),
            None
        );
    }

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

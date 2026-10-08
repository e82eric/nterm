use crate::actions::AppAction;
use crate::app::UserEvent;
use crate::graphics as glow;
use std::ffi::c_char;

pub(crate) mod floating;
mod pane;
use pane::TerminalPane;

pub(crate) struct TerminalView {
    main: TerminalPane,
    messages: crate::messages::Messages,

    command_label: String,
    reload_source: crate::config::ReloadSource,
    picker_bindings: crate::picker::config::Bindings,
    floating_padding: Option<crate::config::FloatingPadding>,
    floating: floating::Panes<FloatingTerminal>,
    new_window_requested: bool,
}

struct FloatingTerminal {
    pane: TerminalPane,
}

impl std::ops::Deref for FloatingTerminal {
    type Target = TerminalPane;

    fn deref(&self) -> &Self::Target {
        &self.pane
    }
}

impl std::ops::DerefMut for FloatingTerminal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.pane
    }
}

impl FloatingTerminal {
    fn destroy(&mut self, gl: &glow::Context) {
        self.pane.destroy(gl);
    }

    fn acquire_frame(&mut self) -> bool {
        self.pane.acquire_frame()
    }
}

impl TerminalView {
    pub(crate) fn apply_theme(&mut self, name: &str) -> Result<(), String> {
        self.main.apply_theme(name)
    }
    fn active_pane_ref(&self) -> &TerminalPane {
        if self.floating.visible().is_some() {
            &self.floating.visible().expect("visible floating pane").pane
        } else {
            &self.main
        }
    }

    pub(crate) fn new(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        window_id: egui_winit::winit::window::WindowId,
        argc: usize,
        argv: *mut *mut c_char,
        command_label: String,
        picker_bindings: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        mut issues: Vec<String>,
        reload_source: crate::config::ReloadSource,
    ) -> Result<Self, crate::native_error::FatalError> {
        let main = TerminalPane::create(egui_ctx, gl, proxy, window_id, argc, argv)?;
        issues.extend(main.take_config_issues());
        Ok(Self {
            messages: crate::messages::Messages::new(issues),

            main,
            command_label,
            reload_source,
            picker_bindings,
            floating_padding,
            floating: Default::default(),
            new_window_requested: false,
        })
    }

    pub(crate) fn new_window(
        &self,
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        window_id: egui_winit::winit::window::WindowId,
    ) -> Result<Self, crate::native_error::FatalError> {
        let main = self.main.create_window(egui_ctx, gl, window_id)?;
        Ok(Self {
            messages: crate::messages::Messages::new(main.take_config_issues()),
            main,
            command_label: self.command_label.clone(),
            reload_source: self.reload_source.clone(),
            picker_bindings: self.picker_bindings.clone(),
            floating_padding: self.floating_padding,
            floating: Default::default(),
            new_window_requested: false,
        })
    }

    pub(crate) fn take_new_window_request(&mut self) -> bool {
        std::mem::take(&mut self.new_window_requested)
    }

    pub(crate) fn window_error(&mut self, error: String) {
        self.main
            .show_error("Could not open terminal window", error);
    }

    pub(crate) fn tick(&mut self) {
        self.main.tick();
    }

    pub(crate) fn acquire_frames(&mut self) -> bool {
        let mut acquired = self.main.acquire_frame();
        for floating in &mut self.floating.entries {
            acquired |= floating.acquire_frame();
        }
        acquired
    }

    fn pane_mut(&mut self, id: Option<u64>) -> Option<&mut TerminalPane> {
        match id {
            None => Some(&mut self.main),
            Some(id) => self
                .floating
                .entries
                .iter_mut()
                .find(|p| p.id == id)
                .map(|p| &mut p.value.pane),
        }
    }
    fn run_picker_callback(&mut self, origin: Option<u64>, gl: &glow::Context) {
        let Some(pane) = self.pane_mut(origin) else {
            return;
        };
        let Some((action, generation, from_copy)) = pane.pending_callback.take() else {
            return;
        };
        let selection = pane.selected_text();
        if let crate::picker::Action::Custom(callback, text, filesystem, range, value, params) =
            action
        {
            let result =
                callback.with_floating(&mut |request| self.floating_request(request, gl), || {
                    callback.call_with_selection(
                        &text, &value, filesystem, generation, range, &params, selection,
                    )
                });
            if let Some(pane) = self.pane_mut(origin) {
                pane.finish_picker_callback(result, generation, from_copy);
            }
        }
    }
    fn floating_request(
        &mut self,
        request: floating::Request,
        gl: &glow::Context,
    ) -> Result<serde_json::Value, String> {
        use floating::{CreateOptions, Request};
        let target_id = |panes: &floating::Panes<FloatingTerminal>,
                         target: Option<floating::Target>|
         -> Result<Option<u64>, String> {
            match target {
                Some(target) => panes
                    .resolve(&target)
                    .map(Some)
                    .ok_or_else(|| "floating pane does not exist".into()),
                None => Ok(panes.last_id()),
            }
        };
        let result = match request {
            Request::Create(options) => {
                self.floating.validate_create(&options)?;
                let pane = self
                    .main
                    .create_floating(gl, &options)
                    .map_err(|e| e.to_string())?;
                let id = self
                    .floating
                    .insert(options.name, FloatingTerminal { pane });
                serde_json::to_value(self.floating.description(id)).unwrap()
            }
            Request::Toggle => {
                if self.floating.visible_id().is_some() {
                    self.floating.hide();
                } else if let Some(id) = self.floating.last_id() {
                    self.floating.restore(id);
                } else {
                    return self.floating_request(Request::Create(CreateOptions::default()), gl);
                }
                serde_json::Value::Null
            }
            Request::Restore(target) => {
                let id = target_id(&self.floating, Some(target))?.unwrap();
                self.floating.restore(id);
                serde_json::to_value(self.floating.description(id)).unwrap()
            }
            Request::Get(target) => {
                let id = match target {
                    Some(target) => self.floating.resolve(&target),
                    None => self.floating.last_id(),
                };
                serde_json::to_value(id.and_then(|id| self.floating.description(id))).unwrap()
            }
            Request::List => serde_json::to_value(self.floating.descriptions()).unwrap(),
            Request::Next | Request::Previous => {
                self.floating.cycle(matches!(request, Request::Next));
                serde_json::Value::Null
            }
            Request::Close(target) => {
                if let Some(id) = target_id(&self.floating, target)? {
                    if let Some(mut pane) = self.floating.remove(id) {
                        pane.destroy(gl);
                    }
                }
                serde_json::Value::Null
            }
        };
        self.main.request_repaint();
        Ok(result)
    }
    fn execute_action(
        &mut self,
        action: AppAction,
        floating_target: bool,
        ctx: &egui::Context,
        gl: &glow::Context,
    ) {
        match action {
            AppAction::ReloadConfig => match self.reload_source.reload() {
                Ok(config) => {
                    if config.theme.is_some()
                        || config.font_family.is_some()
                        || config.font_size.is_some()
                    {
                        if let Err(error) = self.main.apply_settings(&config) {
                            self.main
                                .show_error("Could not reload configuration", error);
                            return;
                        }
                    }
                    if config.font_family.is_some() || config.font_size.is_some() {
                        self.main.refresh_picker_font(ctx);
                        for floating in &mut self.floating.entries {
                            floating.refresh_picker_font(ctx);
                        }
                    }
                    self.picker_bindings = config.bindings;
                    self.floating_padding = config.padding;
                    crate::colors::COLORS.reload(ctx, config.colors);
                    self.main.show_notice("", "Configuration reloaded".into());
                }
                Err(error) => self
                    .main
                    .show_error("Could not reload configuration", error),
            },
            AppAction::OpenThemePicker => {
                let entry = self
                    .picker_bindings
                    .named("builtin.themes")
                    .expect("builtin themes picker");
                let has_floating = !self.floating.is_empty();
                let pane = if self.floating.visible().is_some() {
                    &mut self
                        .floating
                        .visible_mut()
                        .expect("floating action target")
                        .pane
                } else {
                    &mut self.main
                };
                pane.open_picker(ctx, entry, None, &self.picker_bindings, has_floating);
            }
            AppAction::ApplyTheme(name) => match self.apply_theme(&name) {
                Ok(()) => self.main.show_notice("Theme applied", name),
                Err(error) => self.main.show_error("Could not apply theme", error),
            },
            AppAction::ShowMessages => self.messages.open(),
            AppAction::ShowErrorRecord(record) => self.messages.open_record(*record),
            AppAction::OpenErrorLog => {
                if let Some(floating) = self.floating.visible_mut() {
                    floating.open_error_log(ctx);
                } else {
                    self.main.open_error_log(ctx);
                }
            }
            AppAction::NewWindow => {
                self.new_window_requested = true;
            }
            AppAction::RestoreFloatingPane(id) => {
                if self.floating.description(id).is_some() {
                    self.floating.restore(id);
                } else {
                    self.main.show_notice(
                        "Floating pane closed",
                        format!("Pane {id} is no longer available"),
                    );
                }
            }
            AppAction::CreateFloatingPane
            | AppAction::ToggleFloatingPane
            | AppAction::NextFloatingPane
            | AppAction::PreviousFloatingPane
            | AppAction::CloseFloatingPane => {
                let request = match action {
                    AppAction::CreateFloatingPane => floating::Request::Create(Default::default()),
                    AppAction::ToggleFloatingPane => floating::Request::Toggle,
                    AppAction::NextFloatingPane => floating::Request::Next,
                    AppAction::PreviousFloatingPane => floating::Request::Previous,
                    _ => floating::Request::Close(None),
                };
                if let Err(error) = self.floating_request(request, gl) {
                    self.main.show_error("Floating pane action failed", error);
                }
            }
            _ => {
                let has_floating = !self.floating.is_empty();
                let lua_action = if let AppAction::Lua(name) = &action {
                    self.picker_bindings.lua_action(name)
                } else {
                    None
                };
                let entry = if let AppAction::OpenPicker(name) = &action {
                    self.picker_bindings.named(name)
                } else {
                    None
                };
                let origin = if floating_target {
                    self.floating.visible_id()
                } else {
                    None
                };
                if let Some(lua_action) = lua_action {
                    let selection = self
                        .pane_mut(origin)
                        .map(|p| p.selected_text())
                        .unwrap_or_default();
                    let callback = lua_action.callback.clone();
                    let result = callback
                        .with_floating(&mut |request| self.floating_request(request, gl), || {
                            callback.call_action(selection)
                        });
                    if let Some(pane) = self.pane_mut(origin) {
                        pane.finish_lua_action(result, ctx);
                    }
                } else {
                    let bindings = self.picker_bindings.clone();
                    if let Some(pane) = self.pane_mut(origin) {
                        if let Some(entry) = entry {
                            pane.open_picker(ctx, entry, None, &bindings, has_floating);
                        } else {
                            pane.execute_action(&action, ctx);
                        }
                    }
                }
            }
        }
        ctx.request_repaint();
    }

    fn floating_rect(
        content: egui::Rect,
        padding: Option<crate::config::FloatingPadding>,
    ) -> egui::Rect {
        if let Some(padding) = padding {
            return egui::Rect::from_min_max(
                egui::pos2(
                    content.left() + content.width() * padding.left / 100.0,
                    content.top() + content.height() * padding.top / 100.0,
                ),
                egui::pos2(
                    content.right() - content.width() * padding.right / 100.0,
                    content.bottom() - content.height() * padding.bottom / 100.0,
                ),
            );
        }
        let size = egui::vec2(
            (content.width() * 0.8).min(1000.0),
            (content.height() * 0.7).min(700.0),
        );
        egui::Rect::from_center_size(content.center(), size)
    }

    fn show_floating(&self, ui: &egui::Ui, content: egui::Rect, rect: egui::Rect) {
        let Some(floating) = self.floating.visible() else {
            return;
        };
        let layer_id = egui::LayerId::new(egui::Order::Middle, egui::Id::new("floating-terminal"));
        egui::Area::new(layer_id.id)
            .order(layer_id.order)
            .fixed_pos(content.min)
            .interactable(false)
            .show(ui.ctx(), |floating_ui| {
                floating_ui.set_min_size(content.size());
                let painter = floating_ui.painter();
                painter.rect_filled(
                    content,
                    0.0,
                    crate::colors::palette()
                        .background
                        .gamma_multiply(90.0 / 255.0),
                );
                painter.rect_filled(rect.expand(3.0), 3.0, crate::colors::palette().border);
                painter.rect_filled(rect, 0.0, crate::colors::palette().background);
                painter.add(egui::PaintCallback {
                    rect,
                    callback: floating.paint_callback(),
                });
            });
        ui.ctx().move_to_top(layer_id);
    }
    pub(crate) fn update(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        let entries: Vec<_> = self
            .floating
            .entries
            .iter()
            .map(|pane| {
                crate::actions::floating_pane_entry(
                    pane.id,
                    pane.name.as_deref(),
                    &pane.title(),
                    self.floating.visible_id() == Some(pane.id),
                )
            })
            .collect();
        self.main.floating_palette_entries = entries.clone();
        for pane in &mut self.floating.entries {
            pane.floating_palette_entries = entries.clone();
        }
        self.messages.show(ui.ctx());
        let messages_open = self.messages.is_open();
        let context = self.active_pane_ref().action_context();
        let (shortcut, shortcut_consumed) = ui.input_mut(|input| {
            self.picker_bindings
                .consume_when(&mut input.events, |action, mods| {
                    !messages_open && context.allows(action, mods)
                })
        });
        let pane_shortcut = match shortcut {
            Some(action) if action.is_global() => {
                self.execute_action(action, false, ui.ctx(), gl);
                None
            }
            action => action,
        };
        let exited: Vec<_> = self
            .floating
            .entries
            .iter()
            .filter(|p| p.should_close())
            .map(|p| p.id)
            .collect();
        for id in exited {
            if let Some(mut pane) = self.floating.remove(id) {
                pane.destroy(gl);
            }
        }
        self.main.update_modes(ui.ctx());
        for floating in &mut self.floating.entries {
            floating.update_modes(ui.ctx());
        }

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
        let floating_active = self.floating.visible().is_some();
        let window_focused = ui.input(|input| input.viewport().focused.unwrap_or(false));
        let resized = self.main.resize(rect, scale);
        self.main
            .set_active(ui.ctx(), !floating_active, window_focused);
        let floating_rect = Self::floating_rect(rect, self.floating_padding);
        let visible_id = self.floating.visible_id();
        for floating in &mut self.floating.entries {
            let visible = Some(floating.id) == visible_id;
            if visible {
                floating.resize(floating_rect, scale);
            }
            floating.set_active(ui.ctx(), visible, window_focused);
        }
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: self.main.paint_callback(),
        });

        // Input is handled by the active pane before unused events reach Ghostty.
        if floating_active {
            self.main.show_background_overlays(ui.ctx(), rect);
        }
        if resized {
            self.main.refresh_decorations();
        }
        // Snapshot-based actions must observe the resized pane, just like
        // pane-local input handlers do.
        if let Some(action) = pane_shortcut {
            self.execute_action(action, floating_active, ui.ctx(), gl);
        }
        // Lua actions can change the active pane synchronously.
        let visible_id = self.floating.visible_id();
        let floating_active = visible_id.is_some();
        self.main
            .set_active(ui.ctx(), !floating_active, window_focused);
        for pane in &mut self.floating.entries {
            let visible = Some(pane.id) == visible_id;
            if visible {
                pane.resize(floating_rect, scale);
            }
            pane.set_active(ui.ctx(), visible, window_focused);
        }
        let active_bounds = if floating_active { floating_rect } else { rect };
        let has_floating = !self.floating.is_empty();
        #[cfg(windows)]
        {
            self.main.handle_pipe_requests(
                ui.ctx(),
                &self.picker_bindings,
                has_floating,
                !floating_active && !messages_open,
            );
            for floating in &mut self.floating.entries {
                let interactive = Some(floating.id) == visible_id && !messages_open;
                floating.pane.handle_pipe_requests(
                    ui.ctx(),
                    &self.picker_bindings,
                    has_floating,
                    interactive,
                );
            }
        }
        let active_pane = if floating_active {
            &mut self
                .floating
                .visible_mut()
                .expect("visible floating pane")
                .pane
        } else {
            &mut self.main
        };
        let input = if messages_open {
            pane::PaneInput {
                keyboard_blocked: true,
                suppress_forward: true,
                action: None,
            }
        } else {
            active_pane.handle_input(ui, active_bounds, &mut self.picker_bindings)
        };
        active_pane.forward_input(
            ui,
            active_bounds,
            input.keyboard_blocked || shortcut_consumed || messages_open,
            input.suppress_forward || shortcut_consumed || messages_open,
        );
        self.show_mode_status(ui, status_rect);

        self.show_floating(ui, rect, floating_rect);
        if floating_active {
            if let Some(floating) = self.floating.visible_mut() {
                floating.show_search_overlay(ui.ctx(), floating_rect);
            }
        } else {
            self.main.show_search_overlay(ui.ctx(), rect);
        }

        self.run_picker_callback(visible_id, gl);
        if let Some(action) = input.action {
            self.execute_action(action, floating_active, ui.ctx(), gl);
        }
        self.main.drain_clipboard(ui.ctx());
        for floating in &self.floating.entries {
            floating.drain_clipboard(ui.ctx());
        }

        if self.should_close() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.main.show_toasts(ui.ctx(), rect);
        if let Some(floating) = self.floating.visible_mut() {
            floating.show_toasts(ui.ctx(), floating_rect);
        }
    }

    pub(crate) fn should_close(&self) -> bool {
        self.main.should_close()
    }

    fn show_mode_status(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        let warning_right = rect.right() - 10.0;
        // Paint the background before the clickable status indicator.
        ui.painter()
            .rect_filled(rect, 0.0, crate::colors::palette().panel_background);
        let mut right_edge = self.messages.status(ui, rect).min(warning_right);
        let status = self.active_pane_ref().status(&self.command_label);
        let foreground = crate::colors::palette().text;
        let painter = ui.painter().with_clip_rect(rect);
        let separator = status.separator;
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
            status.label,
            font.clone(),
            status.accent,
        );
        if !self.floating.is_empty() {
            let color = if self.floating.visible_id().is_some() {
                crate::colors::palette().warning
            } else {
                crate::colors::palette().muted_text
            };
            let text = painter.layout_no_wrap(
                format!(
                    "Floating Panes: {}{}",
                    self.floating.entries.len(),
                    self.floating
                        .visible()
                        .map(|p| format!(
                            " [{}]",
                            p.name.clone().unwrap_or_else(|| p.id.to_string())
                        ))
                        .unwrap_or_default()
                ),
                egui::FontId::monospace(13.0),
                color,
            );
            let left = right_edge - text.size().x;
            painter.galley(
                egui::pos2(left, rect.center().y + 1.0 - text.size().y / 2.0),
                text,
                color,
            );
            right_edge = left - 12.0;
        }
        if let Some(connected) = status.pipe_status {
            let palette = crate::colors::palette();
            let color = if connected {
                palette.accent
            } else {
                palette.muted_text
            };
            let left = right_edge - 26.0;
            if left > label_rect.right() + 8.0 {
                let icon = egui::Rect::from_center_size(
                    egui::pos2(left + 13.0, rect.center().y + 1.0),
                    egui::vec2(26.0, 18.0),
                );
                let point = |x, y| icon.left_top() + egui::vec2(x, y);
                let stroke = egui::Stroke::new(1.5, color);
                // Two opposing arrows indicate bidirectional picker IPC.
                painter.line_segment([point(2.0, 6.0), point(24.0, 6.0)], stroke);
                painter.add(egui::Shape::line(
                    vec![point(20.0, 3.0), point(24.0, 6.0), point(20.0, 9.0)],
                    stroke,
                ));
                painter.line_segment([point(2.0, 12.0), point(24.0, 12.0)], stroke);
                painter.add(egui::Shape::line(
                    vec![point(6.0, 9.0), point(2.0, 12.0), point(6.0, 15.0)],
                    stroke,
                ));
                ui.interact(icon, ui.id().with("pipe_status"), egui::Sense::hover())
                    .on_hover_text(if connected {
                        "Picker pipe: client connected"
                    } else {
                        "Picker pipe: no client connected"
                    });
                right_edge = left - 12.0;
            }
        }
        if let Some(detail) = status.detail {
            let text = painter.layout_no_wrap(detail, font, foreground);
            let left = right_edge - text.size().x;
            if left > label_rect.right() + 8.0 {
                painter.galley(
                    egui::pos2(left, rect.center().y + 1.0 - text.size().y / 2.0),
                    text,
                    foreground,
                );
            }
        }
    }
    pub(crate) fn poll_retired(&self, gl: &glow::Context) {
        self.main.poll_retired(gl);
        for floating in &self.floating.entries {
            floating.poll_retired(gl);
        }
    }

    pub(crate) fn retired_pending(&self) -> bool {
        self.main.retired_pending()
            || self
                .floating
                .entries
                .iter()
                .any(|floating| floating.retired_pending())
    }

    pub(crate) fn destroy(&mut self, gl: &glow::Context) {
        for mut floating in self.floating.entries.drain(..) {
            floating.destroy(gl);
        }
        self.main.destroy(gl);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn floating_padding_positions_all_four_edges() {
        let content = egui::Rect::from_min_size(egui::pos2(20.0, 40.0), egui::vec2(1000.0, 600.0));
        let rect = super::TerminalView::floating_rect(
            content,
            Some(crate::config::FloatingPadding {
                top: 10.0,
                bottom: 20.0,
                left: 5.0,
                right: 15.0,
            }),
        );
        assert_eq!(rect.min, egui::pos2(70.0, 100.0));
        assert_eq!(rect.max, egui::pos2(870.0, 520.0));
    }
}

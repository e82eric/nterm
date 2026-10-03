use crate::actions::AppAction;
use crate::app::UserEvent;
use crate::graphics as glow;
use std::ffi::c_char;

mod pane;
use pane::TerminalPane;

pub(crate) struct TerminalView {
    main: TerminalPane,
    messages: crate::messages::Messages,

    command_label: String,
    reload_source: crate::config::ReloadSource,
    picker_bindings: crate::picker::config::Bindings,
    floating_padding: Option<crate::config::FloatingPadding>,
    floating: Option<FloatingTerminal>,
    new_window_requested: bool,
}

struct FloatingTerminal {
    pane: TerminalPane,
    visible: bool,
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
    fn new(
        parent: &TerminalView,
        gl: &glow::Context,
    ) -> Result<Option<Self>, crate::native_error::FatalError> {
        Ok(parent.main.create_floating(gl)?.map(|pane| Self {
            pane,
            visible: true,
        }))
    }

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
        if self.floating.as_ref().is_some_and(|pane| pane.visible) {
            &self.floating.as_ref().expect("visible floating pane").pane
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
            floating: None,
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
            floating: None,
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
        if let Some(floating) = &mut self.floating {
            acquired |= floating.acquire_frame();
        }
        acquired
    }

    fn close_floating(&mut self, gl: &glow::Context) {
        if let Some(mut floating) = self.floating.take() {
            floating.destroy(gl);
            self.main.request_repaint();
        }
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
                        if let Some(floating) = &mut self.floating {
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
                let has_floating = self.floating.is_some();
                let pane = if self.floating.as_ref().is_some_and(|pane| pane.visible) {
                    &mut self.floating.as_mut().expect("floating action target").pane
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
                if let Some(floating) = self.floating.as_mut().filter(|pane| pane.visible) {
                    floating.open_error_log(ctx);
                } else {
                    self.main.open_error_log(ctx);
                }
            }
            AppAction::NewWindow => {
                self.new_window_requested = true;
            }
            AppAction::ToggleFloatingPane => {
                if let Some(floating) = self.floating.as_mut() {
                    floating.visible = !floating.visible;
                } else {
                    match FloatingTerminal::new(self, gl) {
                        Ok(floating) => self.floating = floating,
                        Err(error) => self
                            .main
                            .show_error("Could not open floating terminal", error.to_string()),
                    }
                }
            }
            AppAction::CloseFloatingPane => self.close_floating(gl),
            _ => {
                let has_floating = self.floating.is_some();
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
                let pane = if floating_target {
                    let Some(floating) = &mut self.floating else {
                        return;
                    };
                    &mut floating.pane
                } else {
                    &mut self.main
                };
                if let Some(lua_action) = lua_action {
                    pane.execute_lua_action(&lua_action, ctx);
                } else if let Some(entry) = entry {
                    pane.open_picker(ctx, entry, None, &self.picker_bindings, has_floating);
                } else {
                    pane.execute_action(&action, ctx);
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
        let Some(floating) = self.floating.as_ref().filter(|pane| pane.visible) else {
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
        if self
            .floating
            .as_ref()
            .is_some_and(|floating| floating.should_close())
        {
            self.close_floating(gl);
        }
        self.main.update_modes(ui.ctx());
        if let Some(floating) = &mut self.floating {
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
        let floating_active = self.floating.as_ref().is_some_and(|pane| pane.visible);
        let window_focused = ui.input(|input| input.viewport().focused.unwrap_or(false));
        let resized = self.main.resize(rect, scale);
        self.main
            .set_active(ui.ctx(), !floating_active, window_focused);
        let floating_rect = Self::floating_rect(rect, self.floating_padding);
        if let Some(floating) = &mut self.floating {
            let visible = floating.visible;
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
        self.main.refresh_decorations(false, resized);
        // Snapshot-based actions must observe the resized pane, just like
        // pane-local input handlers do.
        if let Some(action) = pane_shortcut {
            self.execute_action(action, floating_active, ui.ctx(), gl);
        }
        let active_bounds = if floating_active { floating_rect } else { rect };
        let has_floating = self.floating.is_some();
        let active_pane = if floating_active {
            &mut self.floating.as_mut().expect("visible floating pane").pane
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
            active_pane.handle_input(ui, active_bounds, &mut self.picker_bindings, has_floating)
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
            if let Some(floating) = &mut self.floating {
                floating.show_search_overlay(ui.ctx(), floating_rect);
            }
        } else {
            self.main.show_search_overlay(ui.ctx(), rect);
        }

        if let Some(action) = input.action {
            self.execute_action(action, floating_active, ui.ctx(), gl);
        }
        self.main.drain_clipboard(ui.ctx());
        if let Some(floating) = &self.floating {
            floating.drain_clipboard(ui.ctx());
        }

        if self.should_close() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.main.show_toasts(ui.ctx(), rect);
        if let Some(floating) = self.floating.as_mut().filter(|pane| pane.visible) {
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
        if let Some(floating) = &self.floating {
            let color = if floating.visible {
                crate::colors::palette().warning
            } else {
                crate::colors::palette().muted_text
            };
            let text = painter.layout_no_wrap(
                "Floating Panes: 1".to_owned(),
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
        if let Some(floating) = &self.floating {
            floating.poll_retired(gl);
        }
    }

    pub(crate) fn retired_pending(&self) -> bool {
        self.main.retired_pending()
            || self
                .floating
                .as_ref()
                .is_some_and(|floating| floating.retired_pending())
    }

    pub(crate) fn destroy(&mut self, gl: &glow::Context) {
        self.close_floating(gl);
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

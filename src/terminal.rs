use crate::actions::AppAction;
use crate::app::UserEvent;
use std::ffi::c_char;

mod pane;
use pane::TerminalPane;

pub(crate) struct TerminalView {
    main: TerminalPane,
    command_label: String,
    picker_bindings: crate::picker::config::Bindings,
    floating_padding: Option<crate::config::FloatingPadding>,
    floating: Option<FloatingTerminal>,
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
    fn new(parent: &TerminalView, gl: &glow::Context) -> Option<Self> {
        Some(Self {
            pane: parent.main.create_floating(gl)?,
            visible: true,
        })
    }

    fn destroy(&mut self, gl: &glow::Context) {
        self.pane.destroy(gl);
    }

    fn acquire_frame(&mut self) -> bool {
        self.pane.acquire_frame()
    }
}

impl TerminalView {
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
        argc: usize,
        argv: *mut *mut c_char,
        command_label: String,
        picker_bindings: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
    ) -> Self {
        let main = TerminalPane::create(egui_ctx, gl, proxy, argc, argv);
        Self {
            main,
            command_label,
            picker_bindings,
            floating_padding,
            floating: None,
        }
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
            AppAction::NewWindow => {
                if let Err(error) = crate::app::open_new_window() {
                    eprintln!("could not open a new terminal window: {error}");
                }
            }
            AppAction::ToggleFloatingPane => {
                if let Some(floating) = self.floating.as_mut() {
                    floating.visible = !floating.visible;
                } else {
                    self.floating = FloatingTerminal::new(self, gl);
                }
            }
            AppAction::CloseFloatingPane => self.close_floating(gl),
            _ => {
                let has_floating = self.floating.is_some();
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
                if let Some(entry) = entry {
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
                painter.rect_filled(content, 0.0, egui::Color32::from_black_alpha(90));
                painter.rect_filled(
                    rect.expand(3.0),
                    3.0,
                    egui::Color32::from_rgb(0x66, 0x5c, 0x54),
                );
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(40, 40, 40));
                painter.add(egui::PaintCallback {
                    rect,
                    callback: floating.paint_callback(),
                });
            });
        ui.ctx().move_to_top(layer_id);
    }
    pub(crate) fn update(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        let context = self.active_pane_ref().action_context();
        let (shortcut, shortcut_consumed) = ui.input_mut(|input| {
            self.picker_bindings
                .consume_when(&mut input.events, |action, mods| {
                    context.allows(action, mods)
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
        let input =
            active_pane.handle_input(ui, active_bounds, &mut self.picker_bindings, has_floating);
        active_pane.forward_input(
            ui,
            active_bounds,
            input.keyboard_blocked || shortcut_consumed,
            input.suppress_forward || shortcut_consumed,
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
    }

    pub(crate) fn should_close(&self) -> bool {
        self.main.should_close()
    }

    fn show_mode_status(&self, ui: &egui::Ui, rect: egui::Rect) {
        let status = self.active_pane_ref().status(&self.command_label);
        let background = egui::Color32::from_rgb(0x1d, 0x20, 0x21);
        let foreground = egui::Color32::from_rgb(0xeb, 0xdb, 0xb2);
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, background);
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
        let mut right_edge = rect.right() - 10.0;
        if let Some(floating) = &self.floating {
            let color = if floating.visible {
                egui::Color32::from_rgb(250, 189, 47)
            } else {
                egui::Color32::from_rgb(124, 111, 100)
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

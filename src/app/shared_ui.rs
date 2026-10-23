use super::{GlutinWindowContext, UserEvent};
use crate::native_error::{FatalError, FatalResult};
use crate::presentation::UiChanges;
use egui::Plugin as _;
use egui_winit::winit;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

pub(super) fn viewport_id(id: winit::window::WindowId) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(id)
}

/// One UI context and GPU painter for every native window. The hidden anchor
/// retains the WGL configuration/DC when the original visible window closes.
pub(super) struct SharedUi {
    pub graphics: Rc<GlutinWindowContext>,
    pub gl: Arc<glow::Context>,
    pub ctx: egui::Context,
    painter: egui_glow::Painter,
}

impl SharedUi {
    pub fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) -> Result<Self, FatalError> {
        let graphics = Rc::new(unsafe { GlutinWindowContext::new(event_loop) }?);
        let gl = Arc::new(unsafe {
            glow::Context::from_loader_function(|name| {
                graphics.get_proc_address(
                    &std::ffi::CString::new(name).expect("OpenGL symbol contains NUL"),
                )
            })
        });
        let painter = egui_glow::Painter::new(Arc::clone(&gl), "", None, true)
            .fatal("create the shared egui painter")?;
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        crate::colors::COLORS.set(&ctx, crate::colors::palette());
        ctx.global_style_mut(|style| style.animation_time = 0.0);
        // Async workers hold a Context clone, which doesn't bind them to a
        // viewport. Wake all windows; per-window change tracking avoids swaps
        // for windows whose contents haven't changed.
        ctx.set_request_repaint_callback(move |info| {
            if let Some(deadline) = Instant::now().checked_add(info.delay) {
                let _ = proxy.send_event(UserEvent::Repaint(deadline));
            }
        });
        Ok(Self {
            graphics,
            gl,
            ctx,
            painter,
        })
    }

    pub fn max_texture_side(&self) -> usize {
        self.painter.max_texture_side()
    }

    /// The root is logical, with no visible window. Only rerun it when windows
    /// open/close, so egui retains precisely the live native child viewports.
    pub fn sync_viewports(
        &mut self,
        ids: impl IntoIterator<Item = egui::ViewportId>,
    ) -> Result<(), FatalError> {
        self.graphics.make_current()?;
        let ids: Vec<_> = ids.into_iter().collect();
        let output = register_viewports(&self.ctx, &ids);
        self.finish_output(output, None)?;
        Ok(())
    }

    /// Upload shared texture changes even when presentation is paced. Otherwise
    /// a second window could draw glyphs before their atlas updates are uploaded.
    pub fn finish_output(
        &mut self,
        mut output: egui::FullOutput,
        dimensions: Option<[u32; 2]>,
    ) -> Result<(), FatalError> {
        for (id, deltas) in output.textures_delta.set.drain() {
            for delta in deltas {
                self.painter.set_texture(id, &delta);
            }
        }
        if let Some(dimensions) = dimensions {
            let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
            self.painter
                .paint_primitives(dimensions, output.pixels_per_point, &primitives);
        }
        for id in output.textures_delta.free.drain() {
            self.painter.free_texture(id);
        }
        Ok(())
    }
}

impl Drop for SharedUi {
    fn drop(&mut self) {
        if let Err(error) = self.graphics.make_current() {
            crate::native_error::show(&error.to_string());
            std::process::exit(1);
        }
        self.painter.destroy();
    }
}

fn register_viewports(ctx: &egui::Context, ids: &[egui::ViewportId]) -> egui::FullOutput {
    ctx.run_ui(egui::RawInput::default(), |ui| {
        for id in ids {
            ui.ctx()
                .show_viewport_deferred(*id, egui::ViewportBuilder::default(), |_, _| {});
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(viewport_id: egui::ViewportId) -> egui::RawInput {
        let mut input = egui::RawInput {
            viewport_id,
            ..Default::default()
        };
        input.viewports.entry(viewport_id).or_default().parent = Some(egui::ViewportId::ROOT);
        input
    }

    #[test]
    fn closing_original_viewport_retains_survivor_and_widget_ids() {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let first = egui::ViewportId::from_hash_of("first");
        let second = egui::ViewportId::from_hash_of("second");
        register_viewports(&ctx, &[first, second])
            .textures_delta
            .clear();
        let mut first_widget = None;
        let mut output = ctx.run_ui(input(first), |ui| {
            first_widget = Some(ui.make_persistent_id("search"));
        });
        output.textures_delta.clear();
        let mut second_widget = None;
        let mut output = ctx.run_ui(input(second), |ui| {
            second_widget = Some(ui.make_persistent_id("search"));
        });
        output.textures_delta.clear();
        assert_ne!(first_widget, second_widget);
        let mut output = register_viewports(&ctx, &[second]);
        assert!(!output.viewport_output.contains_key(&first));
        assert!(output.viewport_output.contains_key(&second));
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(second), |ui| {
            assert_eq!(ui.make_persistent_id("search"), second_widget.unwrap());
        });
        output.textures_delta.clear();
    }

    #[test]
    fn shared_context_keeps_focus_and_dpi_independent_per_viewport() {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let first = egui::ViewportId::from_hash_of("normal dpi");
        let second = egui::ViewportId::from_hash_of("high dpi");
        register_viewports(&ctx, &[first, second])
            .textures_delta
            .clear();
        let mut text = String::new();
        let mut focused = None;
        let mut output = ctx.run_ui(input(first), |ui| {
            assert_eq!(ui.ctx().pixels_per_point(), 1.0);
            let response = ui.text_edit_singleline(&mut text);
            response.request_focus();
            focused = Some(response.id);
        });
        output.textures_delta.clear();
        let mut high_dpi = input(second);
        high_dpi
            .viewports
            .get_mut(&second)
            .unwrap()
            .native_pixels_per_point = Some(2.0);
        let mut output = ctx.run_ui(high_dpi, |ui| {
            assert_eq!(ui.ctx().pixels_per_point(), 2.0);
            assert_eq!(ui.ctx().memory(|memory| memory.focused()), None);
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(first), |ui| {
            assert_eq!(ui.ctx().pixels_per_point(), 1.0);
            assert_eq!(ui.text_edit_singleline(&mut text).id, focused.unwrap());
            assert_eq!(ui.ctx().memory(|memory| memory.focused()), focused);
        });
        output.textures_delta.clear();
    }
}

/// Input, focus, DPI and visual-change tracking stay independent per window.
pub(super) struct WindowUi {
    pub egui_ctx: egui::Context,
    pub viewport_id: egui::ViewportId,
    state: egui_winit::State,
    info: egui::ViewportInfo,
    changes: UiChanges,
}

impl WindowUi {
    pub fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        window: &winit::window::Window,
        shared: &SharedUi,
    ) -> Self {
        let viewport_id = viewport_id(window.id());
        // Direct keyboard input is used by this host; disable native IME once.
        window.set_ime_allowed(false);
        Self {
            egui_ctx: shared.ctx.clone(),
            viewport_id,
            state: egui_winit::State::new(
                shared.ctx.clone(),
                viewport_id,
                event_loop,
                Some(window.scale_factor() as f32),
                event_loop.system_theme(),
                Some(shared.max_texture_side()),
            ),
            info: egui::ViewportInfo::default(),
            changes: UiChanges::default(),
        }
    }

    pub fn on_window_event(
        &mut self,
        window: &winit::window::Window,
        event: &winit::event::WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    pub fn run(
        &mut self,
        window: &winit::window::Window,
        ui: impl FnMut(&mut egui::Ui),
    ) -> (egui::FullOutput, bool) {
        egui_winit::update_viewport_info(&mut self.info, &self.egui_ctx, window, false);
        let mut input = self.state.take_egui_input(window);
        let info = input.viewports.entry(self.viewport_id).or_default();
        info.parent = Some(egui::ViewportId::ROOT);
        // Keep State's transient focus/events, adding the native geometry.
        info.inner_rect = self.info.inner_rect;
        info.outer_rect = self.info.outer_rect;
        // Panes read window focus from here to drive native terminal focus.
        info.focused = self.info.focused;
        let mut output = self.egui_ctx.run_ui(input, ui);
        self.changes.output_hook(&self.egui_ctx, &mut output);
        let changed = std::mem::take(&mut self.changes.changed);
        if let Some(viewport) = output.viewport_output.get_mut(&self.viewport_id) {
            viewport.commands.retain(|command| {
                !matches!(
                    command,
                    egui::ViewportCommand::IMEAllowed(_)
                        | egui::ViewportCommand::IMERect(_)
                        | egui::ViewportCommand::IMEPurpose(_)
                )
            });
            egui_winit::process_viewport_commands(
                &self.egui_ctx,
                &mut self.info,
                std::mem::take(&mut viewport.commands),
                window,
                &mut Vec::new(),
            );
        }
        let mut platform_output = std::mem::take(&mut output.platform_output);
        // Avoid composition interruption and cursor-area calls during typing.
        platform_output.ime = None;
        self.state.handle_platform_output(window, platform_output);
        (output, changed)
    }
}

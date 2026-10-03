use super::{GlutinWindowContext, UserEvent};
use crate::native_error::FatalError;
use crate::presentation::UiChanges;
use egui::Plugin as _;
use egui_wgpu::wgpu;
use egui_winit::winit;
use std::{rc::Rc, sync::Arc, time::Instant};

pub(super) fn viewport_id(id: winit::window::WindowId) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(id)
}
pub(super) struct SharedUi {
    pub graphics: Rc<GlutinWindowContext>,
    pub gl: Arc<crate::graphics::Context>,
    pub ctx: egui::Context,
}
impl SharedUi {
    pub fn new(
        _: &winit::event_loop::ActiveEventLoop,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) -> Result<Self, FatalError> {
        let graphics = Rc::new(GlutinWindowContext::new()?);
        let ctx = graphics.gpu.borrow().context.clone();
        crate::colors::COLORS.set(&ctx, crate::colors::palette());
        ctx.global_style_mut(|s| s.animation_time = 0.0);
        ctx.set_request_repaint_callback(move |info| {
            if let Some(deadline) = Instant::now().checked_add(info.delay) {
                let _ = proxy.send_event(UserEvent::Repaint(deadline));
            }
        });
        Ok(Self {
            gl: graphics.gl.clone(),
            graphics,
            ctx,
        })
    }
    pub fn max_texture_side(&self) -> usize {
        self.gl.device().limits().max_texture_dimension_2d as usize
    }
    pub fn sync_viewports(
        &mut self,
        ids: impl IntoIterator<Item = egui::ViewportId>,
    ) -> Result<(), FatalError> {
        let output = register_viewports(&self.ctx, &ids.into_iter().collect::<Vec<_>>());
        self.finish_output(output, None)
    }
    pub fn finish_output(
        &mut self,
        mut output: egui::FullOutput,
        dimensions: Option<[u32; 2]>,
    ) -> Result<(), FatalError> {
        let mut gpu = self.graphics.gpu.borrow_mut();
        let submission = gpu.submission.clone();
        let _submission = submission.lock().unwrap_or_else(|e| e.into_inner());
        let Some(size) = dimensions.filter(|s| s[0] > 0 && s[1] > 0) else {
            for (id, deltas) in output.textures_delta.set.drain() {
                for delta in deltas {
                    let device = gpu.device.clone();
                    let queue = gpu.queue.clone();
                    gpu.egui.update_texture(&device, &queue, id, &delta);
                }
            }
            for id in output.textures_delta.free.drain() {
                gpu.egui.free_texture(&id);
            }
            return Ok(());
        };
        let (surface, configuration) = self
            .graphics
            .current_surface
            .borrow()
            .clone()
            .ok_or_else(|| FatalError::new("present Vulkan frame", "No window surface bound"))?;
        let mut config = configuration.borrow_mut();
        if config.width != size[0] || config.height != size[1] {
            config.width = size[0];
            config.height = size[1];
            surface.configure(&gpu.device, &config);
        }
        let mut acquired = surface.get_current_texture();
        if matches!(acquired, wgpu::CurrentSurfaceTexture::Outdated) {
            surface.configure(&gpu.device, &config);
            acquired = surface.get_current_texture();
        }
        let frame = match acquired {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                for (id, deltas) in output.textures_delta.set.drain() {
                    for delta in deltas {
                        let device = gpu.device.clone();
                        let queue = gpu.queue.clone();
                        gpu.egui.update_texture(&device, &queue, id, &delta);
                    }
                }
                for id in output.textures_delta.free.drain() {
                    gpu.egui.free_texture(&id);
                }
                self.ctx
                    .request_repaint_after(std::time::Duration::from_millis(50));
                return Ok(());
            }
            other => {
                output.textures_delta.clear();
                return Err(FatalError::new(
                    "acquire Vulkan presentation image",
                    format!("{other:?}"),
                ));
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let c = crate::colors::palette().background;
        let clear = wgpu::Color {
            r: f64::from(c.r()) / 255.0,
            g: f64::from(c.g()) / 255.0,
            b: f64::from(c.b()) / 255.0,
            a: 1.0,
        };
        let mut commands = gpu.encode_ui(&mut encoder, &view, output, size, Some(clear));
        commands.push(encoder.finish());
        gpu.queue.submit(commands);
        gpu.queue.present(frame);
        Ok(())
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
        let mut output = self.egui_ctx.run_ui(input, ui);
        self.changes.output_hook(&self.egui_ctx, &mut output);
        let changed = std::mem::take(&mut self.changes.changed);
        if let Some(viewport) = output.viewport_output.get_mut(&self.viewport_id) {
            egui_winit::process_viewport_commands(
                &self.egui_ctx,
                &mut self.info,
                std::mem::take(&mut viewport.commands),
                window,
                &mut Vec::new(),
            );
        }
        self.state
            .handle_platform_output(window, std::mem::take(&mut output.platform_output));
        (output, changed)
    }
}

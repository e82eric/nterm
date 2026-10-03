//! Shared native Vulkan device and egui renderer.
//!
//! This module deliberately enables only Vulkan. It must not silently fall
//! back to OpenGL when measuring the new renderer's memory footprint.
use egui_wgpu::wgpu;

pub struct SharedGpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub egui: egui_wgpu::Renderer,
    pub context: egui::Context,
    pub format: wgpu::TextureFormat,
    /// Surface reconfiguration must not race native worker submissions.
    pub submission: std::sync::Arc<std::sync::Mutex<()>>,
}

impl SharedGpu {
    /// Use a compatible surface during application startup. Offscreen tests
    /// can pass None. Subsequent windows share this device and queue.
    pub async fn new(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        format: wgpu::TextureFormat,
    ) -> Result<Self, String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: surface,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(|error| format!("Select Vulkan adapter: {error}"))?;
        if adapter.get_info().backend != wgpu::Backend::Vulkan {
            return Err("Selected adapter is not Vulkan".into());
        }
        if let Some(surface) = surface {
            if !surface.get_capabilities(&adapter).formats.contains(&format) {
                return Err(format!("Vulkan surface does not support {format:?}"));
            }
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Ghostty shared Vulkan device"),
                ..Default::default()
            })
            .await
            .map_err(|error| format!("Create Vulkan device: {error}"))?;
        let egui = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        let context = egui::Context::default();
        context.set_embed_viewports(false);
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            egui,
            context,
            format,
            submission: Default::default(),
        })
    }

    pub fn instance() -> wgpu::Instance {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        wgpu::Instance::new(descriptor)
    }

    /// Encode egui after terminal rendering into the same command stream.
    /// The caller submits returned callback buffers before this encoder.
    pub fn encode_ui(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        mut output: egui::FullOutput,
        size: [u32; 2],
        clear: Option<wgpu::Color>,
    ) -> Vec<wgpu::CommandBuffer> {
        for (id, deltas) in output.textures_delta.set.drain() {
            for delta in deltas {
                self.egui
                    .update_texture(&self.device, &self.queue, id, &delta);
            }
        }
        let jobs = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point: output.pixels_per_point,
        };
        let callbacks =
            self.egui
                .update_buffers(&self.device, &self.queue, encoder, &jobs, &screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Ghostty egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui.render(&mut pass, &jobs, &screen);
        }
        for id in output.textures_delta.free.drain() {
            self.egui.free_texture(&id);
        }
        callbacks
    }
}

pub mod bridge;
pub mod terminal;

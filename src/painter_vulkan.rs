use crate::{
    ghostty_ffi::{GlFrame, ghostty_egui_host_release_frame},
    graphics::Context,
};
use egui_wgpu::wgpu;
use ghostty_egui::gpu::bridge::Texture;
use std::sync::Arc;

struct OwnedFrame {
    handle: usize,
    texture: Texture,
}
impl Drop for OwnedFrame {
    fn drop(&mut self) {
        unsafe { ghostty_egui_host_release_frame(self.handle as *mut _) };
    }
}
pub(crate) struct TerminalPainter {
    context: Context,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    current: Option<OwnedFrame>,
}
impl TerminalPainter {
    pub(crate) fn new(context: &Context) -> Result<Self, crate::native_error::FatalError> {
        let shader = context.device().create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ghostty Vulkan composition"), source: wgpu::ShaderSource::Wgsl(r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
struct Vertex { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@builtin(vertex_index) i: u32) -> Vertex {
    let positions = array<vec2<f32>, 3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    var out: Vertex; let p = positions[i]; out.pos=vec4(p,0.0,1.0); out.uv=vec2((p.x+1.0)*0.5,(1.0-p.y)*0.5); return out;
}
@fragment fn fs(in: Vertex) -> @location(0) vec4<f32> { return textureSample(image,image_sampler,in.uv); }
"#.into()),
        });
        let pipeline = context
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Ghostty Vulkan compositor"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::TextureFormat::Bgra8Unorm.into())],
                }),
                multiview_mask: None,
                cache: None,
            });
        let sampler = context.device().create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            context: context.clone(),
            pipeline,
            sampler,
            current: None,
        })
    }
    pub(crate) fn replace_frame(&mut self, frame: GlFrame) {
        self.current = Some(OwnedFrame {
            handle: frame.handle as usize,
            texture: unsafe { self.context.texture(frame.texture) },
        });
    }
    pub(crate) fn poll_retired(&mut self, _: &Context) {}
    pub(crate) fn has_retired_frames(&self) -> bool {
        false
    }
    pub(crate) fn destroy(&mut self, _: &Context) {
        self.current = None;
    }
}
pub(crate) type PaintCallback = Arc<dyn std::any::Any + Send + Sync>;
struct Callback {
    painter: Arc<egui::mutex::Mutex<TerminalPainter>>,
}
impl egui_wgpu::CallbackTrait for Callback {
    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _: &egui_wgpu::CallbackResources,
    ) {
        let painter = self.painter.lock();
        let Some(frame) = &painter.current else {
            return;
        };
        let group = painter
            .context
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Ghostty Vulkan composition binding"),
                layout: &painter.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&frame.texture.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&painter.sampler),
                    },
                ],
            });
        pass.set_pipeline(&painter.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
}
pub(crate) fn callback(painter: Arc<egui::mutex::Mutex<TerminalPainter>>) -> PaintCallback {
    egui_wgpu::Callback::new_paint_callback(egui::Rect::NOTHING, Callback { painter }).callback
}

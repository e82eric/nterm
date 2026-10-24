use crate::ghostty_ffi::GpuFrame;
use crate::graphics::Context;
use egui_wgpu::wgpu;
use std::sync::Arc;

/// The pipeline that draws terminal frames into egui's pass, shared by every
/// pane through egui-wgpu's callback resources.
pub(crate) struct Compositor {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Per-draw values, as immediate data. Must match `Pane` in the shader and
/// fit in `crate::gpu::IMMEDIATE_SIZE`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PaneImmediates {
    pane_size: [f32; 2],
    texture_scale: [f32; 2],
    background: [f32; 4],
    radius: f32,
    _padding: [f32; 3],
}

const SHADER: &str = r#"
struct Pane {
    pane_size: vec2<f32>,
    texture_scale: vec2<f32>,
    background: vec4<f32>,
    radius: f32,
}
var<immediate> pane: Pane;
@group(0) @binding(0) var terminal_texture: texture_2d<f32>;
@group(0) @binding(1) var terminal_sampler: sampler;
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@builtin(vertex_index) i: u32) -> Vertex {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0))[i];
    var out: Vertex;
    out.position = vec4(p, 0.0, 1.0);
    out.uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}
@fragment fn fs(in: Vertex) -> @location(0) vec4<f32> {
    let half = pane.pane_size * 0.5;
    let q = abs(in.uv * pane.pane_size - half) - (half - vec2(pane.radius));
    // Distance in pixels outside the rounded rectangle; pixels straddling
    // the edge are partly covered, so corners are antialiased.
    let distance = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - pane.radius;
    let coverage = clamp(0.5 - distance, 0.0, 1.0);
    if (coverage == 0.0) { discard; }
    let p = in.uv * pane.texture_scale;
    // Sample unconditionally: derivatives need uniform control flow.
    let c = textureSampleLevel(terminal_texture, terminal_sampler, p, 0.0);
    // Colors are premultiplied, so coverage scales every channel.
    return select(c, pane.background, p.x >= 1.0 || p.y >= 1.0) * coverage;
}
"#;

impl Compositor {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ghostty terminal composition"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Ghostty terminal composition"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Ghostty terminal composition"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: crate::gpu::IMMEDIATE_SIZE,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Ghostty terminal composition"),
            layout: Some(&pipeline_layout),
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
                // Frames cover what egui drew beneath, except at partly
                // covered corner pixels, which blend with it.
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        // Frames keep their pixel size while a pane resizes, so sampling is 1:1.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Ghostty terminal composition"),
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
        }
    }
}

/// A frame taken from Ghostty. Dropping it returns its texture to Ghostty's
/// pool, so drop it only after the egui frame that last sampled it has been
/// submitted.
struct OwnedFrame {
    context: Context,
    handle: usize,
    view: wgpu::TextureView,
    size: [u32; 2],
}

impl Drop for OwnedFrame {
    fn drop(&mut self) {
        unsafe { self.context.release(self.handle) };
    }
}

pub(crate) struct TerminalPainter {
    context: Context,
    radius: f32,
    current: Option<OwnedFrame>,
    /// Bindings for `current`, created when first drawn.
    group: Option<wgpu::BindGroup>,
}

impl TerminalPainter {
    pub(crate) fn set_corner_radius(&mut self, radius: f32) {
        self.radius = radius;
    }

    pub(crate) fn new(context: &Context) -> Self {
        Self {
            context: context.clone(),
            radius: 0.0,
            current: None,
            group: None,
        }
    }

    /// Show `frame` from now on. The frame it replaces was last sampled by an
    /// egui frame that has already been submitted, so it is released here.
    pub(crate) fn replace_frame(&mut self, frame: GpuFrame) {
        let exported = unsafe { self.context.exported(frame.texture) };
        self.group = None;
        self.current = Some(OwnedFrame {
            context: self.context.clone(),
            handle: frame.texture,
            view: exported.view,
            size: exported.size,
        });
    }

    pub(crate) fn destroy(&mut self) {
        self.group = None;
        self.current = None;
    }

    fn paint(
        &mut self,
        info: &egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        compositor: &Compositor,
    ) {
        let Some(frame) = &self.current else {
            return;
        };
        let viewport = info.viewport_in_pixels();
        let (width, height) = (viewport.width_px as f32, viewport.height_px as f32);
        let group = self.group.get_or_insert_with(|| {
            self.context
                .device()
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Ghostty terminal frame"),
                    layout: &compositor.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&frame.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&compositor.sampler),
                        },
                    ],
                })
        });
        let background = crate::colors::palette().background;
        let immediates = PaneImmediates {
            pane_size: [width, height],
            texture_scale: frame_texture_scale(frame.size, [width, height]),
            background: [
                f32::from(background.r()) / 255.0,
                f32::from(background.g()) / 255.0,
                f32::from(background.b()) / 255.0,
                1.0,
            ],
            radius: self.radius.min(width.min(height) / 2.0),
            _padding: [0.0; 3],
        };
        pass.set_pipeline(&compositor.pipeline);
        pass.set_bind_group(0, &*group, &[]);
        pass.set_immediates(0, as_bytes(&immediates));
        pass.draw(0..3, 0..1);
    }
}

fn as_bytes(value: &PaneImmediates) -> &[u8] {
    // PaneImmediates is plain f32 data without padding bytes.
    unsafe {
        std::slice::from_raw_parts(
            (value as *const PaneImmediates).cast::<u8>(),
            std::mem::size_of::<PaneImmediates>(),
        )
    }
}

/// Renderer frames arrive asynchronously. Sample the previous frame at its
/// original pixel size instead of stretching it during resize.
fn frame_texture_scale(frame: [u32; 2], viewport: [f32; 2]) -> [f32; 2] {
    [
        viewport[0].max(0.0) / frame[0].max(1) as f32,
        viewport[1].max(0.0) / frame[1].max(1) as f32,
    ]
}

pub(crate) type PaintCallback = Arc<dyn std::any::Any + Send + Sync>;

struct Callback {
    painter: Arc<egui::mutex::Mutex<TerminalPainter>>,
}

impl egui_wgpu::CallbackTrait for Callback {
    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(compositor) = resources.get::<Compositor>() {
            self.painter.lock().paint(&info, pass, compositor);
        }
    }
}

/// The paint callback that draws `painter`'s current frame. The caller sets
/// the rectangle when adding it to a painter.
pub(crate) fn callback(painter: Arc<egui::mutex::Mutex<TerminalPainter>>) -> PaintCallback {
    egui_wgpu::Callback::new_paint_callback(egui::Rect::NOTHING, Callback { painter }).callback
}

#[cfg(test)]
mod tests {
    #[test]
    fn resizing_keeps_old_frame_pixels_at_the_same_size() {
        for viewport in [[400.0, 300.0], [800.0, 600.0], [1200.0, 900.0]] {
            let scale = super::frame_texture_scale([800, 600], viewport);
            // A texel at x=80/y=60 stays at that pixel position in every pane.
            assert!((0.1 / scale[0] * viewport[0] - 80.0).abs() < 0.001);
            assert!((0.1 / scale[1] * viewport[1] - 60.0).abs() < 0.001);
        }
    }

    #[test]
    fn immediates_fit_the_device_limit() {
        assert!(std::mem::size_of::<super::PaneImmediates>() as u32 <= crate::gpu::IMMEDIATE_SIZE);
        assert_eq!(std::mem::size_of::<super::PaneImmediates>() % 4, 0);
    }
}

//! Native record layouts and an offscreen pixel-test harness. The application
//! uses bridge.rs to receive draw calls from Zig; both paths use terminal.wgsl.
use super::SharedGpu;
use egui_wgpu::wgpu;
use wgpu::util::DeviceExt;

/// Matches src/renderer/opengl/shaders.zig's std140 Uniforms layout.
#[repr(C, align(16))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Globals {
    pub projection: [f32; 16],
    pub screen: [f32; 2],
    pub cell: [f32; 2],
    pub grid: u32,
    pub alignment_padding: [u32; 3],
    pub padding: [f32; 4],
    pub extend: u32,
    pub contrast: f32,
    pub cursor: u32,
    pub cursor_color: u32,
    pub background: u32,
    pub flags: u32,
    pub trailing_padding: [u32; 2],
}

/// Glyph position, size, packed bearings/grid/color/atlas/flags, in native order.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Glyph(pub [u32; 8]);

pub struct TerminalRenderer {
    background: wgpu::RenderPipeline,
    cells: wgpu::RenderPipeline,
    text: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

/// GPU-owned resources for a frame. They stay alive through command submission;
/// there is no GL fence or cross-device texture sharing.
pub struct PreparedFrame {
    group: wgpu::BindGroup,
    glyph_count: u32,
}

impl TerminalRenderer {
    pub fn new(gpu: &SharedGpu) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ghostty terminal WGSL"),
            source: wgpu::ShaderSource::Wgsl(include_str!("terminal.wgsl").into()),
        });
        let buffer_entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Ghostty terminal bindings"),
            entries: &[
                buffer_entry(0, wgpu::BufferBindingType::Uniform),
                buffer_entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                buffer_entry(2, wgpu::BufferBindingType::Storage { read_only: true }),
                texture_entry(3),
                texture_entry(4),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Ghostty terminal layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |name, vertex, fragment, topology, blending| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(name),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: gpu.format,
                        blend: if blending {
                            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING)
                        } else {
                            None
                        },
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            background: pipeline(
                "Ghostty background",
                "fullscreen",
                "background",
                wgpu::PrimitiveTopology::TriangleList,
                false,
            ),
            cells: pipeline(
                "Ghostty cells",
                "fullscreen",
                "cells",
                wgpu::PrimitiveTopology::TriangleList,
                true,
            ),
            text: pipeline(
                "Ghostty glyphs",
                "text_vertex",
                "text_fragment",
                wgpu::PrimitiveTopology::TriangleStrip,
                true,
            ),
            layout,
        }
    }

    pub fn prepare(
        &self,
        gpu: &SharedGpu,
        globals: &Globals,
        backgrounds: &[u32],
        glyphs: &[Glyph],
        grayscale: &wgpu::TextureView,
        color: &wgpu::TextureView,
    ) -> Result<PreparedFrame, String> {
        let columns = (globals.grid & 65535) as usize;
        let rows = (globals.grid >> 16) as usize;
        if columns == 0 || rows == 0 || backgrounds.len() != columns * rows {
            return Err("Background cell count does not match nonempty grid".into());
        }
        if !globals.cell.iter().all(|v| v.is_finite() && *v > 0.0) {
            return Err("Cell dimensions must be finite and positive".into());
        }
        for glyph in glyphs {
            let position = glyph.0[5];
            if (position & 65535) as usize >= columns || (position >> 16) as usize >= rows {
                return Err("Glyph position is outside the grid".into());
            }
        }
        let glyph_count = u32::try_from(glyphs.len()).map_err(|_| "Too many glyphs")?;
        let buffer = |label, contents: &[u8], usage| {
            gpu.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents,
                    usage,
                })
        };
        let uniforms = buffer(
            "Ghostty uniforms",
            bytemuck::bytes_of(globals),
            wgpu::BufferUsages::UNIFORM,
        );
        let cells = buffer(
            "Ghostty background cells",
            bytemuck::cast_slice(backgrounds),
            wgpu::BufferUsages::STORAGE,
        );
        // A storage binding must have enough bytes for at least one record.
        let empty = [Glyph([0; 8])];
        let glyph_data = if glyphs.is_empty() {
            &empty[..]
        } else {
            glyphs
        };
        let glyph_buffer = buffer(
            "Ghostty glyph cells",
            bytemuck::cast_slice(glyph_data),
            wgpu::BufferUsages::STORAGE,
        );
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Ghostty terminal frame"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: cells.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: glyph_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(grayscale),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(color),
                },
            ],
        });
        Ok(PreparedFrame { group, glyph_count })
    }

    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        frame: &PreparedFrame,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Ghostty terminal"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_bind_group(0, &frame.group, &[]);
        pass.set_pipeline(&self.background);
        pass.draw(0..3, 0..1);
        pass.set_pipeline(&self.cells);
        pass.draw(0..3, 0..1);
        if frame.glyph_count > 0 {
            pass.set_pipeline(&self.text);
            pass.draw(0..4, 0..frame.glyph_count);
        }
    }
}

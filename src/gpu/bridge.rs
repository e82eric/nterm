//! Synchronous native renderer dispatch. Opaque handles own wgpu references;
//! submitted commands retain their resources independently of the handles.
use super::SharedGpu;
use egui_wgpu::wgpu;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};
use wgpu::util::DeviceExt;

#[repr(C)]
pub struct Driver {
    version: u32,
    context: *mut c_void,
    call: unsafe extern "C" fn(*mut c_void, u32, *const [usize; 12]) -> usize,
}
pub struct Context {
    core: Arc<Core>,
    driver: Box<Driver>,
}
// Driver points to the immutable Arc-owned Core; dispatch uses thread-owned
// frame encoders and wgpu's thread-safe device/queue.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}
impl Clone for Context {
    fn clone(&self) -> Self {
        let core = self.core.clone();
        let driver = Box::new(Driver {
            version: 1,
            context: Arc::as_ptr(&core) as *mut c_void,
            call: dispatch,
        });
        Self { core, driver }
    }
}

struct Core {
    submission: Arc<Mutex<()>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    shader: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    dummy: wgpu::Buffer,
    gray: Texture,
    color: Texture,
    pipelines: Mutex<std::collections::HashMap<(usize, wgpu::TextureFormat), wgpu::RenderPipeline>>,
    export_pool: Mutex<Vec<Texture>>,
}
#[derive(Clone)]
pub struct Texture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    source_format: usize,
}
struct Frame {
    encoder: Option<wgpu::CommandEncoder>,
    uniforms: Option<wgpu::Buffer>,
    failed: bool,
}
enum Resource {
    Buffer(wgpu::Buffer),
    Texture(Texture),
    Exported(Texture),
    Pipeline {
        kind: usize,
        custom: Option<wgpu::ShaderModule>,
        cache: Mutex<
            std::collections::HashMap<
                wgpu::TextureFormat,
                (wgpu::BindGroupLayout, wgpu::RenderPipeline),
            >,
        >,
    },
    Frame(Mutex<Frame>),
}

impl Context {
    pub fn new(gpu: &SharedGpu) -> Self {
        let d = &gpu.device;
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ghostty native Vulkan shaders"),
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
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Ghostty native Vulkan layout"),
            entries: &[
                buffer_entry(0, wgpu::BufferBindingType::Uniform),
                buffer_entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                buffer_entry(2, wgpu::BufferBindingType::Storage { read_only: true }),
                texture_entry(3),
                texture_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Ghostty native pipelines"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let core = Arc::new(Core {
            submission: gpu.submission.clone(),
            device: d.clone(),
            queue: gpu.queue.clone(),
            shader,
            layout,
            pipeline_layout,
            sampler: d.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            dummy: d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Ghostty empty binding"),
                contents: &[0; 256],
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::UNIFORM,
            }),
            gray: make_texture(d, 1, 1, 0, false),
            color: make_texture(d, 1, 1, 1, false),
            pipelines: Mutex::new(Default::default()),
            export_pool: Mutex::new(Vec::new()),
        });
        let driver = Box::new(Driver {
            version: 1,
            context: Arc::as_ptr(&core) as *mut c_void,
            call: dispatch,
        });
        Self { core, driver }
    }
    pub fn driver(&self) -> *mut c_void {
        (&*self.driver as *const Driver) as *mut c_void
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.core.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.core.queue
    }
    /// Clone a frame's GPU texture while the native frame lease is held.
    pub unsafe fn texture(&self, handle: usize) -> Texture {
        unsafe { as_texture(handle).clone() }
    }
}

fn make_texture(
    d: &wgpu::Device,
    width: u32,
    height: u32,
    source_format: usize,
    srgb: bool,
) -> Texture {
    let format = if source_format == 0 {
        wgpu::TextureFormat::R8Unorm
    } else if srgb {
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    };
    let texture = d.create_texture(&wgpu::TextureDescriptor {
        label: Some("Ghostty Vulkan texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | if source_format == 0 {
                wgpu::TextureUsages::empty()
            } else {
                wgpu::TextureUsages::RENDER_ATTACHMENT
            },
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    Texture {
        texture,
        view,
        format,
        source_format,
    }
}
unsafe fn resource(handle: usize) -> &'static Resource {
    assert_ne!(handle, 0);
    unsafe { &*(handle as *const Resource) }
}
unsafe fn as_buffer(handle: usize) -> &'static wgpu::Buffer {
    match unsafe { resource(handle) } {
        Resource::Buffer(b) => b,
        _ => panic!("Expected GPU buffer"),
    }
}
unsafe fn as_texture(handle: usize) -> &'static Texture {
    match unsafe { resource(handle) } {
        Resource::Texture(t) | Resource::Exported(t) => t,
        _ => panic!("Expected GPU texture"),
    }
}
fn boxed(r: Resource) -> usize {
    Box::into_raw(Box::new(r)) as usize
}

unsafe extern "C" fn dispatch(ctx: *mut c_void, op: u32, args: *const [usize; 12]) -> usize {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        execute(&*(ctx as *const Core), op, &*args)
    }))
    .unwrap_or_else(|_| {
        eprintln!("Ghostty Vulkan dispatch failed (operation {op})");
        0
    })
}
unsafe fn execute(core: &Core, op: u32, a: &[usize; 12]) -> usize {
    let _submission = core.submission.lock().unwrap_or_else(|e| e.into_inner());
    match op {
        0 => {
            let resource = unsafe { *Box::from_raw(a[0] as *mut Resource) };
            if let Resource::Exported(texture) = resource {
                let mut pool = core.export_pool.lock().unwrap();
                if pool.len() < 6 {
                    pool.push(texture);
                }
            }
            1
        }
        1 => boxed(Resource::Buffer(core.device.create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("Ghostty native buffer"),
                size: a[0].max(256).next_multiple_of(16) as u64,
                usage: wgpu::BufferUsages::COPY_DST
                    | if a[1] != 0 {
                        wgpu::BufferUsages::UNIFORM
                    } else {
                        wgpu::BufferUsages::STORAGE
                    },
                mapped_at_creation: false,
            },
        ))),
        2 => {
            core.queue
                .write_buffer(unsafe { as_buffer(a[0]) }, a[1] as u64, unsafe {
                    std::slice::from_raw_parts(a[2] as *const u8, a[3])
                });
            1
        }
        3 => {
            let t = make_texture(&core.device, a[0] as u32, a[1] as u32, a[2], a[3] != 0);
            if a[4] != 0 {
                unsafe {
                    upload(core, &t, 0, 0, a[0], a[1], a[4], a[5]);
                }
            }
            boxed(Resource::Texture(t))
        }
        4 => {
            unsafe {
                upload(core, as_texture(a[0]), a[1], a[2], a[3], a[4], a[5], a[6]);
            }
            1
        }
        5 => boxed(Resource::Frame(Mutex::new(Frame {
            encoder: Some(core.device.create_command_encoder(&Default::default())),
            uniforms: None,
            failed: false,
        }))),
        6 => {
            let Resource::Frame(frame) = (unsafe { resource(a[0]) }) else {
                panic!("Expected frame")
            };
            let mut frame = frame.lock().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                draw(core, &mut frame, a)
            }));
            if result.is_err() {
                frame.failed = true;
                return 0;
            }
            1
        }
        7 => {
            let Resource::Frame(frame) = (unsafe { *Box::from_raw(a[0] as *mut Resource) }) else {
                panic!("Expected frame")
            };
            let mut frame = frame.into_inner().unwrap();
            if frame.failed {
                return 0;
            }
            let source = unsafe { as_texture(a[1]) };
            let output = {
                let mut pool = core.export_pool.lock().unwrap();
                pool.iter()
                    .position(|t| t.texture.size() == source.texture.size())
                    .map(|index| pool.swap_remove(index))
                    .unwrap_or_else(|| {
                        make_texture(
                            &core.device,
                            source.texture.width(),
                            source.texture.height(),
                            1,
                            false,
                        )
                    })
            };
            let mut encoder = frame.encoder.take().unwrap();
            encoder.copy_texture_to_texture(
                source.texture.as_image_copy(),
                output.texture.as_image_copy(),
                source.texture.size(),
            );
            core.queue.submit([encoder.finish()]);
            // The native renderer reuses its CPU/GPU upload slots after this
            // callback. Completion keeps those uploads ordered across workers.
            core.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_secs(10)),
                })
                .unwrap();
            boxed(Resource::Exported(output))
        }
        8 => {
            let custom = if a[0] == 5 {
                let bytes = unsafe { std::slice::from_raw_parts(a[1] as *const u8, a[2]) };
                assert_eq!(bytes.len() % 4, 0);
                let words = bytes
                    .chunks_exact(4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                    .collect::<Vec<_>>();
                Some(
                    core.device
                        .create_shader_module(wgpu::ShaderModuleDescriptor {
                            label: Some("Ghostty custom SPIR-V shader"),
                            source: wgpu::ShaderSource::SpirV(words.into()),
                        }),
                )
            } else {
                None
            };
            boxed(Resource::Pipeline {
                kind: a[0],
                custom,
                cache: Mutex::new(Default::default()),
            })
        }
        _ => panic!("Unknown native Vulkan operation"),
    }
}
unsafe fn upload(
    core: &Core,
    t: &Texture,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    ptr: usize,
    len: usize,
) {
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    let stride = w * if t.source_format == 0 { 1 } else { 4 };
    assert_eq!(len, stride * h);
    let converted;
    let bytes = if t.source_format == 2 {
        converted = bytes
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect::<Vec<_>>();
        &converted[..]
    } else {
        bytes
    };
    let mut dest = t.texture.as_image_copy();
    dest.origin = wgpu::Origin3d {
        x: x as u32,
        y: y as u32,
        z: 0,
    };
    core.queue.write_texture(
        dest,
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride as u32),
            rows_per_image: Some(h as u32),
        },
        wgpu::Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
    );
}

unsafe fn draw(core: &Core, frame: &mut Frame, a: &[usize; 12]) {
    let target = unsafe { as_texture(a[1]) };
    let Resource::Pipeline {
        kind,
        custom,
        cache,
    } = (unsafe { resource(a[2]) })
    else {
        panic!("Expected pipeline")
    };
    if a[3] != 0 {
        frame.uniforms = Some(unsafe { as_buffer(a[3]).clone() });
    }
    let uniforms = frame.uniforms.as_ref().expect("Frame has no uniforms");
    let data = if a[4] == 0 {
        &core.dummy
    } else {
        unsafe { as_buffer(a[4]) }
    };
    let cells = if a[5] == 0 {
        &core.dummy
    } else {
        unsafe { as_buffer(a[5]) }
    };
    let tex0 = if a[6] == 0 {
        &core.gray
    } else {
        unsafe { as_texture(a[6]) }
    };
    let tex1 = if a[7] == 0 {
        &core.color
    } else {
        unsafe { as_texture(a[7]) }
    };
    let (group, pipeline) = if let Some(custom) = custom {
        let mut cache = cache.lock().unwrap();
        let (layout, cached_pipeline) = cache.entry(target.format).or_insert_with(|| {
            let layout = core
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Ghostty custom bindings"),
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
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });
            let pipeline_layout =
                core.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("Ghostty custom layout"),
                        bind_group_layouts: &[Some(&layout)],
                        immediate_size: 0,
                    });
            let p = pipeline(
                core,
                &pipeline_layout,
                custom,
                "main",
                target.format,
                "fullscreen",
                false,
                false,
            );
            (layout, p)
        });
        let group = core.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex0.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&core.sampler),
                },
            ],
        });
        (group, cached_pipeline.clone())
    } else {
        let group = core.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &core.layout,
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
                    resource: data.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&tex0.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&tex1.view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&core.sampler),
                },
            ],
        });
        let mut cache = core.pipelines.lock().unwrap();
        let p = cache.entry((*kind, target.format)).or_insert_with(|| {
            let (v, f) = match kind {
                0 => ("fullscreen", "background"),
                1 => ("fullscreen", "cells"),
                2 => ("text_vertex", "text_fragment"),
                3 => ("image_vertex", "image_fragment"),
                4 => ("fullscreen", "background_image"),
                _ => panic!("Invalid pipeline"),
            };
            pipeline(
                core,
                &core.pipeline_layout,
                &core.shader,
                f,
                target.format,
                v,
                *kind != 0 && *kind != 4,
                *kind == 2 || *kind == 3,
            )
        });
        (group, p.clone())
    };
    let clear = if a[10] == 0 {
        None
    } else {
        let c = unsafe { &*(a[10] as *const [f32; 4]) };
        Some(wgpu::Color {
            r: c[0] as f64,
            g: c[1] as f64,
            b: c[2] as f64,
            a: c[3] as f64,
        })
    };
    let mut pass = frame
        .encoder
        .as_mut()
        .unwrap()
        .begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Ghostty native Vulkan pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.draw(0..a[8] as u32, 0..a[9] as u32);
}
fn pipeline(
    core: &Core,
    layout: &wgpu::PipelineLayout,
    fragment: &wgpu::ShaderModule,
    f: &str,
    format: wgpu::TextureFormat,
    v: &str,
    blend: bool,
    strip: bool,
) -> wgpu::RenderPipeline {
    core.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Ghostty native Vulkan pipeline"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &core.shader,
                entry_point: Some(v),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: if strip {
                    wgpu::PrimitiveTopology::TriangleStrip
                } else {
                    wgpu::PrimitiveTopology::TriangleList
                },
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: fragment,
                entry_point: Some(f),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: if blend {
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
}

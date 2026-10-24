//! Executes the Zig renderer's GPU operations (src/renderer/Wgpu.zig) on the
//! shared wgpu device. Handles passed to Zig are boxed `Resource`s; Zig's
//! renderer threads call `dispatch` concurrently, one thread per surface.
//!
//! Frames are rendered into textures pooled per frame target. A finished
//! frame is handed to egui as that texture, without a copy; the texture
//! returns to the pool when egui releases the frame. The queue orders every
//! submission, so neither side waits on the GPU: a pooled texture rendered
//! again after release is written by a submission that follows egui's read.
use super::SharedGpu;
use egui_wgpu::wgpu;
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use wgpu::util::DeviceExt as _;

/// Must match `driver_version` in src/renderer/Wgpu.zig.
const VERSION: u32 = 2;

/// Operations. Must match `Op` in src/renderer/Wgpu.zig.
mod op {
    pub const RELEASE: u32 = 0;
    pub const CREATE_BUFFER: u32 = 1;
    pub const WRITE_BUFFER: u32 = 2;
    pub const CREATE_TEXTURE: u32 = 3;
    pub const WRITE_TEXTURE: u32 = 4;
    pub const BEGIN_FRAME: u32 = 5;
    pub const DRAW: u32 = 6;
    pub const SUBMIT_FRAME: u32 = 7;
    pub const CREATE_PIPELINE: u32 = 8;
    pub const END_PASS: u32 = 9;
}

/// Zig's ImageTextureFormat.
const FORMAT_GRAY: usize = 0;
const FORMAT_BGRA: usize = 2;

/// Pipeline kind of custom ShaderToy shaders; 0-4 are terminal.wgsl's.
const CUSTOM_PIPELINE: usize = 5;

/// Textures kept per frame target: one the host shows, one waiting to be
/// taken, and one being rendered. More are created if needed, then dropped.
const POOLED_TARGETS: usize = 3;

#[repr(C)]
pub(crate) struct Driver {
    version: u32,
    context: *mut c_void,
    call: unsafe extern "C" fn(*mut c_void, u32, *const [usize; 12]) -> usize,
}

/// The host side of the dispatch table. Clones share one table, whose
/// address stays valid while any clone lives.
#[derive(Clone)]
pub(crate) struct Context {
    core: Arc<Core>,
}

struct Core {
    driver: Driver,
    submission: Arc<Mutex<()>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    shader: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    dummy: wgpu::Buffer,
    dummy_texture: Texture,
    pipelines: Mutex<HashMap<(usize, wgpu::TextureFormat), wgpu::RenderPipeline>>,
    /// Bind groups for terminal pipelines, by the ids of their resources.
    /// Entries are removed when any of their resources is released.
    bind_groups: Mutex<HashMap<[u64; 5], wgpu::BindGroup>>,
    next_id: AtomicU64,
}

// The table points into the Arc-owned Core. wgpu's device and queue are
// thread-safe, and per-frame encoders belong to one renderer thread.
unsafe impl Send for Core {}
unsafe impl Sync for Core {}

#[derive(Clone)]
pub(crate) struct Texture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    source_format: usize,
}

/// A texture of a frame target's pool.
struct Pooled {
    /// The view rendered to, in the target's format.
    render: wgpu::TextureView,
    /// The view the host samples: the stored bytes, without sRGB decoding,
    /// which is what the compositor writes to the window.
    sample: wgpu::TextureView,
}

struct Target {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    /// A texture is free when the pool holds its only reference.
    pool: Mutex<Vec<Arc<Pooled>>>,
}

struct Step {
    pipeline: wgpu::RenderPipeline,
    group: wgpu::BindGroup,
    vertices: u32,
    instances: u32,
}

/// Steps recorded for the current render pass, encoded together when the
/// pass ends.
struct Pass {
    target: usize,
    view: wgpu::TextureView,
    clear: Option<wgpu::Color>,
    steps: Vec<Step>,
}

struct Frame {
    encoder: wgpu::CommandEncoder,
    /// The target's texture this frame renders into.
    output: Arc<Pooled>,
    target: usize,
    uniforms: Option<wgpu::Buffer>,
    pass: Option<Pass>,
    failed: bool,
}

enum Resource {
    Buffer {
        id: u64,
        buffer: wgpu::Buffer,
    },
    Texture {
        id: u64,
        texture: Texture,
    },
    Target(Target),
    /// A finished frame, owned by the host until released.
    Exported {
        frame: Arc<Pooled>,
        size: [u32; 2],
    },
    Pipeline {
        kind: usize,
        custom: Option<wgpu::ShaderModule>,
        cache: Mutex<HashMap<wgpu::TextureFormat, (wgpu::BindGroupLayout, wgpu::RenderPipeline)>>,
    },
    Frame(Mutex<Frame>),
}

/// A finished frame, borrowed from its resource for composition.
pub(crate) struct ExportedFrame {
    pub view: wgpu::TextureView,
    pub size: [u32; 2],
}

impl Context {
    pub(crate) fn new(gpu: &SharedGpu) -> Self {
        let d = &gpu.device;
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ghostty terminal shaders"),
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
            label: Some("Ghostty terminal bindings"),
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
            label: Some("Ghostty terminal pipelines"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let core = Arc::new_cyclic(|core: &std::sync::Weak<Core>| Core {
            driver: Driver {
                version: VERSION,
                context: core.as_ptr() as *mut c_void,
                call: dispatch,
            },
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
            dummy_texture: make_texture(d, 1, 1, FORMAT_GRAY, false),
            pipelines: Mutex::new(HashMap::new()),
            bind_groups: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        });
        Self { core }
    }

    /// The dispatch table handed to Ghostty.
    pub(crate) fn driver(&self) -> *mut c_void {
        (&self.core.driver as *const Driver).cast_mut().cast()
    }

    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.core.device
    }

    /// The texture of a frame taken from Ghostty.
    ///
    /// # Safety
    /// `handle` must be an exported frame that has not been released.
    pub(crate) unsafe fn exported(&self, handle: usize) -> ExportedFrame {
        match unsafe { resource(handle) } {
            Resource::Exported { frame, size } => ExportedFrame {
                view: frame.sample.clone(),
                size: *size,
            },
            _ => panic!("Expected an exported frame"),
        }
    }

    /// Release a frame taken from Ghostty. Release it only after submitting
    /// every read of it: its texture may then be rendered again.
    ///
    /// # Safety
    /// `handle` must be an exported frame that has not been released.
    pub(crate) unsafe fn release(&self, handle: usize) {
        unsafe { release(&self.core, handle) };
    }
}

fn make_texture(
    d: &wgpu::Device,
    width: u32,
    height: u32,
    source_format: usize,
    srgb: bool,
) -> Texture {
    let format = match (source_format, srgb) {
        (FORMAT_GRAY, _) => wgpu::TextureFormat::R8Unorm,
        (FORMAT_BGRA, true) => wgpu::TextureFormat::Bgra8UnormSrgb,
        (FORMAT_BGRA, false) => wgpu::TextureFormat::Bgra8Unorm,
        (_, true) => wgpu::TextureFormat::Rgba8UnormSrgb,
        (_, false) => wgpu::TextureFormat::Rgba8Unorm,
    };
    let texture = d.create_texture(&wgpu::TextureDescriptor {
        label: Some("Ghostty texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        // Color textures are also custom shader passes' targets.
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | if source_format == FORMAT_GRAY {
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

impl Target {
    fn new(width: u32, height: u32, srgb: bool) -> Self {
        Self {
            width,
            height,
            format: if srgb {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            pool: Mutex::new(Vec::new()),
        }
    }

    /// A texture no exported frame holds, created if every one is in use.
    fn acquire(&self, device: &wgpu::Device) -> Arc<Pooled> {
        let mut pool = self.pool.lock().unwrap();
        if let Some(free) = pool.iter().find(|t| Arc::strong_count(t) == 1) {
            return free.clone();
        }
        let sample_format = self.format.remove_srgb_suffix();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Ghostty frame"),
            size: wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[sample_format],
        });
        let pooled = Arc::new(Pooled {
            render: texture.create_view(&Default::default()),
            sample: texture.create_view(&wgpu::TextureViewDescriptor {
                format: Some(sample_format),
                ..Default::default()
            }),
        });
        pool.push(pooled.clone());
        // Drop surplus free textures once the host catches up.
        while pool.len() > POOLED_TARGETS {
            let Some(index) = pool
                .iter()
                .position(|t| Arc::strong_count(t) == 1 && !Arc::ptr_eq(t, &pooled))
            else {
                break;
            };
            pool.swap_remove(index);
        }
        pooled
    }
}

unsafe fn resource(handle: usize) -> &'static Resource {
    assert_ne!(handle, 0);
    unsafe { &*(handle as *const Resource) }
}

unsafe fn as_buffer(handle: usize) -> (u64, &'static wgpu::Buffer) {
    match unsafe { resource(handle) } {
        Resource::Buffer { id, buffer } => (*id, buffer),
        _ => panic!("Expected GPU buffer"),
    }
}

unsafe fn as_texture(handle: usize) -> (u64, &'static Texture) {
    match unsafe { resource(handle) } {
        Resource::Texture { id, texture } => (*id, texture),
        _ => panic!("Expected GPU texture"),
    }
}

fn boxed(r: Resource) -> usize {
    Box::into_raw(Box::new(r)) as usize
}

unsafe fn release(core: &Core, handle: usize) {
    let resource = unsafe { Box::from_raw(handle as *mut Resource) };
    if let Resource::Buffer { id, .. } | Resource::Texture { id, .. } = *resource {
        core.bind_groups
            .lock()
            .unwrap()
            .retain(|ids, _| !ids.contains(&id));
    }
}

fn color(ptr: usize) -> Option<wgpu::Color> {
    if ptr == 0 {
        return None;
    }
    let c = unsafe { &*(ptr as *const [f32; 4]) };
    Some(wgpu::Color {
        r: c[0] as f64,
        g: c[1] as f64,
        b: c[2] as f64,
        a: c[3] as f64,
    })
}

unsafe extern "C" fn dispatch(ctx: *mut c_void, op: u32, args: *const [usize; 12]) -> usize {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        execute(&*(ctx as *const Core), op, &*args)
    }))
    .unwrap_or_else(|_| {
        crate::error_log::error(
            "graphics",
            "Terminal render operation failed",
            &format!("operation {op}"),
        );
        0
    })
}

unsafe fn execute(core: &Core, op: u32, a: &[usize; 12]) -> usize {
    match op {
        op::RELEASE => {
            unsafe { release(core, a[0]) };
            1
        }
        op::CREATE_BUFFER => boxed(Resource::Buffer {
            id: core.next_id.fetch_add(1, Ordering::Relaxed),
            buffer: core.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Ghostty buffer"),
                size: a[0].max(256).next_multiple_of(16) as u64,
                usage: wgpu::BufferUsages::COPY_DST
                    | if a[1] != 0 {
                        wgpu::BufferUsages::UNIFORM
                    } else {
                        wgpu::BufferUsages::STORAGE
                    },
                mapped_at_creation: false,
            }),
        }),
        op::WRITE_BUFFER => {
            // Staged by the queue: Zig may reuse its memory immediately.
            core.queue
                .write_buffer(unsafe { as_buffer(a[0]).1 }, a[1] as u64, unsafe {
                    std::slice::from_raw_parts(a[2] as *const u8, a[3])
                });
            1
        }
        op::CREATE_TEXTURE => {
            let (width, height) = (a[0] as u32, a[1] as u32);
            if a[6] != 0 {
                return boxed(Resource::Target(Target::new(width, height, a[3] != 0)));
            }
            let texture = make_texture(&core.device, width, height, a[2], a[3] != 0);
            if a[4] != 0 {
                unsafe { upload(core, &texture, 0, 0, a[0], a[1], a[4], a[5]) };
            }
            boxed(Resource::Texture {
                id: core.next_id.fetch_add(1, Ordering::Relaxed),
                texture,
            })
        }
        op::WRITE_TEXTURE => {
            unsafe { upload(core, as_texture(a[0]).1, a[1], a[2], a[3], a[4], a[5], a[6]) };
            1
        }
        op::BEGIN_FRAME => {
            let Resource::Target(target) = (unsafe { resource(a[0]) }) else {
                panic!("Expected frame target")
            };
            boxed(Resource::Frame(Mutex::new(Frame {
                encoder: core.device.create_command_encoder(&Default::default()),
                output: target.acquire(&core.device),
                target: a[0],
                uniforms: None,
                pass: None,
                failed: false,
            })))
        }
        op::DRAW | op::END_PASS => {
            let Resource::Frame(frame) = (unsafe { resource(a[0]) }) else {
                panic!("Expected frame")
            };
            let mut frame = frame.lock().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                if op == op::DRAW {
                    draw(core, &mut frame, a);
                } else {
                    if frame.pass.is_none() {
                        // A pass without steps still clears its target.
                        if let Some(clear) = color(a[2]) {
                            let view = target_view(&frame, a[1]);
                            frame.pass = Some(Pass {
                                target: a[1],
                                view,
                                clear: Some(clear),
                                steps: Vec::new(),
                            });
                        }
                    }
                    end_pass(&mut frame);
                }
            }));
            if result.is_err() {
                frame.failed = true;
                return 0;
            }
            1
        }
        op::SUBMIT_FRAME => {
            let Resource::Frame(frame) = (unsafe { *Box::from_raw(a[0] as *mut Resource) }) else {
                panic!("Expected frame")
            };
            let mut frame = frame.into_inner().unwrap();
            end_pass(&mut frame);
            if frame.failed {
                return 0;
            }
            let commands = frame.encoder.finish();
            {
                let _submission = core.submission.lock().unwrap_or_else(|e| e.into_inner());
                core.queue.submit([commands]);
            }
            let Resource::Target(target) = (unsafe { resource(frame.target) }) else {
                panic!("Expected frame target")
            };
            boxed(Resource::Exported {
                frame: frame.output,
                size: [target.width, target.height],
            })
        }
        op::CREATE_PIPELINE => {
            let custom = (a[0] == CUSTOM_PIPELINE).then(|| {
                let bytes = unsafe { std::slice::from_raw_parts(a[1] as *const u8, a[2]) };
                assert_eq!(bytes.len() % 4, 0);
                let words = bytes
                    .chunks_exact(4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                    .collect::<Vec<_>>();
                core.device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("Ghostty custom shader"),
                        source: wgpu::ShaderSource::SpirV(words.into()),
                    })
            });
            boxed(Resource::Pipeline {
                kind: a[0],
                custom,
                cache: Mutex::new(HashMap::new()),
            })
        }
        _ => panic!("Unknown terminal render operation {op}"),
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
    let stride = w * if t.source_format == FORMAT_GRAY { 1 } else { 4 };
    assert_eq!(len, stride * h);
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

/// The view a pass renders to: the frame's pooled texture for its target,
/// or a custom shader pass's texture.
unsafe fn target_view(frame: &Frame, handle: usize) -> wgpu::TextureView {
    match unsafe { resource(handle) } {
        Resource::Target(_) => {
            assert_eq!(handle, frame.target, "Rendering to another frame's target");
            frame.output.render.clone()
        }
        Resource::Texture { texture, .. } => texture.view.clone(),
        _ => panic!("Expected render target"),
    }
}

unsafe fn target_format(handle: usize) -> wgpu::TextureFormat {
    match unsafe { resource(handle) } {
        Resource::Target(target) => target.format,
        Resource::Texture { texture, .. } => texture.format,
        _ => panic!("Expected render target"),
    }
}

/// Encode the recorded pass, if any, as one wgpu render pass.
fn end_pass(frame: &mut Frame) {
    let Some(pass) = frame.pass.take() else {
        return;
    };
    let mut encoded = frame
        .encoder
        .begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Ghostty terminal pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &pass.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: pass.clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
    for step in &pass.steps {
        encoded.set_pipeline(&step.pipeline);
        encoded.set_bind_group(0, &step.group, &[]);
        encoded.draw(0..step.vertices, 0..step.instances);
    }
}

unsafe fn draw(core: &Core, frame: &mut Frame, a: &[usize; 12]) {
    let target = a[1];
    let format = unsafe { target_format(target) };
    let Resource::Pipeline {
        kind,
        custom,
        cache,
    } = (unsafe { resource(a[2]) })
    else {
        panic!("Expected pipeline")
    };
    if a[3] != 0 {
        frame.uniforms = Some(unsafe { as_buffer(a[3]).1.clone() });
    }
    let uniforms_id = if a[3] != 0 {
        unsafe { as_buffer(a[3]).0 }
    } else {
        0
    };
    let uniforms = frame.uniforms.clone().expect("Frame has no uniforms");
    let (data_id, data) = if a[4] == 0 {
        (0, &core.dummy)
    } else {
        unsafe { as_buffer(a[4]) }
    };
    let (cells_id, cells) = if a[5] == 0 {
        (0, &core.dummy)
    } else {
        unsafe { as_buffer(a[5]) }
    };
    let (tex0_id, tex0) = if a[6] == 0 {
        (0, &core.dummy_texture)
    } else {
        unsafe { as_texture(a[6]) }
    };
    let (tex1_id, tex1) = if a[7] == 0 {
        (0, &core.dummy_texture)
    } else {
        unsafe { as_texture(a[7]) }
    };

    let (group, pipeline) = if let Some(custom) = custom {
        let mut cache = cache.lock().unwrap();
        let (layout, pipeline) = cache
            .entry(format)
            .or_insert_with(|| custom_pipeline(core, custom, format));
        // Custom shaders run once per frame; their bindings are not cached.
        let group = core.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Ghostty custom shader bindings"),
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
        (group, pipeline.clone())
    } else {
        // A step that reuses the frame's uniforms has no id of its own; key
        // it by the buffer it reuses, which is the last one bound.
        let key = [uniforms_id, cells_id, data_id, tex0_id, tex1_id];
        let cacheable = uniforms_id != 0;
        let cached = cacheable
            .then(|| core.bind_groups.lock().unwrap().get(&key).cloned())
            .flatten();
        let group = cached.unwrap_or_else(|| {
            let group = core.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Ghostty terminal bindings"),
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
            if cacheable {
                core.bind_groups.lock().unwrap().insert(key, group.clone());
            }
            group
        });
        let pipeline = core
            .pipelines
            .lock()
            .unwrap()
            .entry((*kind, format))
            .or_insert_with(|| terminal_pipeline(core, *kind, format))
            .clone();
        (group, pipeline)
    };

    // Steps for another target, or that clear, begin a new pass.
    let clear = color(a[10]);
    if frame
        .pass
        .as_ref()
        .is_some_and(|pass| pass.target != target || clear.is_some())
    {
        end_pass(frame);
    }
    if frame.pass.is_none() {
        let view = unsafe { target_view(frame, target) };
        frame.pass = Some(Pass {
            target,
            view,
            clear,
            steps: Vec::new(),
        });
    }
    frame.pass.as_mut().unwrap().steps.push(Step {
        pipeline,
        group,
        vertices: a[8] as u32,
        instances: a[9] as u32,
    });
}

fn terminal_pipeline(
    core: &Core,
    kind: usize,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let (vertex, fragment) = match kind {
        0 => ("fullscreen", "background"),
        1 => ("fullscreen", "cells"),
        2 => ("text_vertex", "text_fragment"),
        3 => ("image_vertex", "image_fragment"),
        4 => ("fullscreen", "background_image"),
        _ => panic!("Invalid pipeline kind {kind}"),
    };
    let blend = kind != 0 && kind != 4;
    let strip = kind == 2 || kind == 3;
    pipeline(
        core,
        &core.pipeline_layout,
        &core.shader,
        vertex,
        &core.shader,
        fragment,
        format,
        blend,
        strip,
    )
}

fn custom_pipeline(
    core: &Core,
    module: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> (wgpu::BindGroupLayout, wgpu::RenderPipeline) {
    let layout = core
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Ghostty custom shader bindings"),
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
    let pipeline_layout = core
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Ghostty custom shader layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
    let pipeline = pipeline(
        core,
        &pipeline_layout,
        &core.shader,
        "fullscreen",
        module,
        "main",
        format,
        false,
        false,
    );
    (layout, pipeline)
}

#[allow(clippy::too_many_arguments)]
fn pipeline(
    core: &Core,
    layout: &wgpu::PipelineLayout,
    vertex_module: &wgpu::ShaderModule,
    vertex: &str,
    fragment_module: &wgpu::ShaderModule,
    fragment: &str,
    format: wgpu::TextureFormat,
    blend: bool,
    strip: bool,
) -> wgpu::RenderPipeline {
    core.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Ghostty terminal pipeline"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: vertex_module,
                entry_point: Some(vertex),
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
                module: fragment_module,
                entry_point: Some(fragment),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: blend.then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
}

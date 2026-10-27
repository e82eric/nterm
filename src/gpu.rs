//! The process-wide wgpu device. egui and every terminal renderer use the
//! same device and queue, so terminal frames are ordinary textures that egui
//! samples directly: there is no cross-device or cross-API sharing.
pub(crate) mod bridge;

use egui_wgpu::wgpu;

/// Bytes of immediate data the terminal compositor uses (see painter.rs).
pub(crate) const IMMEDIATE_SIZE: u32 = 48;

pub(crate) struct SharedGpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Serializes queue submissions with surface configuration and
    /// presentation. Resource creation and writes need no lock.
    pub submission: std::sync::Arc<std::sync::Mutex<()>>,
}

impl SharedGpu {
    /// Vulkan, unless WGPU_BACKEND overrides it. wgpu's DX12 swapchain
    /// stretches the previous image while a window resizes.
    fn backends() -> wgpu::Backends {
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::VULKAN)
    }

    pub fn new() -> Result<Self, String> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = Self::backends();
        descriptor.flags = wgpu::InstanceFlags::from_build_config().with_env();
        let instance = wgpu::Instance::new(descriptor);
        let adapter = Self::adapter(&instance)?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            crate::error_log::warning(
                "graphics",
                "Rendering in software",
                &format!(
                    "No hardware {:?} adapter is available; using {}.",
                    info.backend, info.name
                ),
            );
        }
        if !adapter.features().contains(wgpu::Features::IMMEDIATES)
            || adapter.limits().max_immediate_size < IMMEDIATE_SIZE
        {
            return Err(format!(
                "{} does not support immediate data, which the terminal compositor needs",
                info.name
            ));
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Ghostty shared device"),
            required_features: wgpu::Features::IMMEDIATES,
            required_limits: wgpu::Limits {
                // Large windows need the adapter's full texture size.
                max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d,
                max_immediate_size: IMMEDIATE_SIZE,
                // On DX12 this sizes the shader-visible descriptor heap, which
                // is resident: the default million descriptors take 32 MiB.
                // Terminals and egui use a few hundred.
                max_non_sampler_bindings: 65_536,
                ..wgpu::Limits::default()
            },
            // The default reserves 64 MiB upload and 128 MiB device blocks,
            // far beyond a terminal's needs; upload blocks are resident.
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|error| format!("create the graphics device: {error}"))?;
        device.on_uncaptured_error(std::sync::Arc::new(|error| {
            crate::error_log::error("graphics", "Graphics device error", &error.to_string());
        }));
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            submission: Default::default(),
        })
    }

    /// Prefer a hardware adapter, falling back to a software one (WARP on
    /// Windows) where there is none, such as in a virtual machine.
    fn adapter(instance: &wgpu::Instance) -> Result<wgpu::Adapter, String> {
        let request = |fallback| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference:
                    wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower),
                force_fallback_adapter: fallback,
                ..Default::default()
            }))
        };
        request(false)
            .or_else(|_| request(true))
            .map_err(|error| format!("select a graphics adapter: {error}"))
    }
}

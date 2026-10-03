use crate::native_error::{FatalError, FatalResult};
use egui_wgpu::wgpu;
use egui_winit::winit;
use ghostty_egui::gpu::SharedGpu;
use std::{cell::RefCell, rc::Rc, sync::Arc};

pub(super) struct GlutinWindowContext {
    pub gpu: RefCell<SharedGpu>,
    pub gl: Arc<crate::graphics::Context>,
    pub current_surface: RefCell<
        Option<(
            Arc<wgpu::Surface<'static>>,
            Rc<RefCell<wgpu::SurfaceConfiguration>>,
        )>,
    >,
}
impl GlutinWindowContext {
    pub(super) fn new() -> Result<Self, FatalError> {
        let gpu = pollster::block_on(SharedGpu::new(
            SharedGpu::instance(),
            None,
            wgpu::TextureFormat::Bgra8Unorm,
        ))
        .fatal("create the shared Vulkan device")?;
        let gl = Arc::new(crate::graphics::Context::new(&gpu));
        Ok(Self {
            gpu: RefCell::new(gpu),
            gl,
            current_surface: RefCell::new(None),
        })
    }
}

pub(super) struct GlutinWindowSurface {
    window: Arc<winit::window::Window>,
    surface: Arc<wgpu::Surface<'static>>,
    graphics: Rc<GlutinWindowContext>,
    configuration: Rc<RefCell<wgpu::SurfaceConfiguration>>,
}
impl GlutinWindowSurface {
    pub(super) unsafe fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        graphics: Rc<GlutinWindowContext>,
    ) -> Result<Self, FatalError> {
        let window = Arc::new(
            event_loop
                .create_window(
                    winit::window::WindowAttributes::default()
                        .with_title("Ghostty — Vulkan")
                        .with_inner_size(winit::dpi::LogicalSize::new(1000.0, 700.0))
                        .with_decorations(false)
                        .with_visible(false),
                )
                .fatal("create the Vulkan window")?,
        );
        let gpu = graphics.gpu.borrow();
        let surface = Arc::new(
            gpu.instance
                .create_surface(window.clone())
                .fatal("create the Vulkan window surface")?,
        );
        let mut config = surface
            .get_default_config(&gpu.adapter, 1000, 700)
            .ok_or_else(|| {
                FatalError::new(
                    "create Vulkan presentation",
                    "Selected device cannot present to this window",
                )
            })?;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        if !capabilities.formats.contains(&gpu.format) {
            return Err(FatalError::new(
                "configure Vulkan presentation",
                "Window does not support the shared UI pixel format",
            ));
        }
        config.format = gpu.format;
        config.present_mode = wgpu::PresentMode::AutoNoVsync;
        config.desired_maximum_frame_latency = 2;
        {
            let _submission = gpu.submission.lock().unwrap_or_else(|e| e.into_inner());
            surface.configure(&gpu.device, &config);
        }
        drop(gpu);
        let result = Self {
            window,
            surface,
            graphics,
            configuration: Rc::new(RefCell::new(config)),
        };
        result.make_current()?;
        #[cfg(windows)]
        {
            use winit::raw_window_handle::HasWindowHandle;
            if let Ok(handle) = result.window.window_handle() {
                if let winit::raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() {
                    super::prefer_rounded_corners(handle.hwnd.get() as *mut _);
                }
            }
        }
        Ok(result)
    }
    pub(super) fn window(&self) -> &winit::window::Window {
        &self.window
    }
    pub(super) fn make_current(&self) -> Result<(), FatalError> {
        use winit::raw_window_handle::HasWindowHandle;
        let raw = self
            .window
            .window_handle()
            .fatal("get the terminal window handle")?;
        let window = match raw.as_raw() {
            winit::raw_window_handle::RawWindowHandle::Win32(h) => h.hwnd.get() as *mut _,
            _ => std::ptr::null_mut(),
        };
        crate::graphics::set_native(window, self.graphics.gl.driver());
        *self.graphics.current_surface.borrow_mut() =
            Some((self.surface.clone(), self.configuration.clone()));
        Ok(())
    }
    pub(super) fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        let (width, height) = (size.width, size.height);
        if width == 0 || height == 0 {
            return;
        }
        let mut config = self.configuration.borrow_mut();
        config.width = width;
        config.height = height;
        let gpu = self.graphics.gpu.borrow();
        let _submission = gpu.submission.lock().unwrap_or_else(|e| e.into_inner());
        self.surface.configure(&gpu.device, &config);
    }
    pub(super) fn swap_buffers(&self) -> Result<(), FatalError> {
        Ok(())
    }
}
impl Drop for GlutinWindowSurface {
    fn drop(&mut self) {
        if self
            .graphics
            .current_surface
            .borrow()
            .as_ref()
            .is_some_and(|(s, _)| Arc::ptr_eq(s, &self.surface))
        {
            self.graphics.current_surface.borrow_mut().take();
        }
    }
}

use crate::terminal::TerminalView;
use egui_winit::winit;
use glow::HasContext as _;
use std::ffi::{CString, c_char, c_void};
use std::num::NonZeroU32;
use std::sync::Arc;

pub(crate) fn run(args: Vec<CString>) {
    let event_loop = winit::event_loop::EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("create event loop");
    let proxy = event_loop.create_proxy();
    let mut app = NativeApp::new(proxy, args);
    event_loop.run_app(&mut app).expect("run event loop");
}

struct ShellApp {
    terminal: TerminalView,
    // Ghostty borrows argc/argv as process-global state, so retain both the
    // strings and pointer array until after the embedded app is destroyed.
    _args: Vec<CString>,
    _argv: Vec<*mut c_char>,
}

impl ShellApp {
    fn new(egui_ctx: &egui::Context, gl: &glow::Context, args: Vec<CString>) -> Self {
        let mut argv: Vec<*mut c_char> = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
        Self {
            terminal: TerminalView::new(egui_ctx, gl, argv.len(), argv.as_mut_ptr()),
            _args: args,
            _argv: argv,
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        self.terminal.update(ui);
    }

    fn destroy(&mut self, gl: &glow::Context) {
        self.terminal.destroy(gl);
    }
}

#[derive(Debug)]
enum UserEvent {
    Repaint(std::time::Duration),
}

struct NativeApp {
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    args: Option<Vec<CString>>,
    gl_window: Option<GlutinWindowContext>,
    gl: Option<Arc<glow::Context>>,
    egui: Option<egui_glow::EguiGlow>,
    shell: Option<ShellApp>,
}

impl NativeApp {
    fn new(proxy: winit::event_loop::EventLoopProxy<UserEvent>, args: Vec<CString>) -> Self {
        Self {
            proxy,
            args: Some(args),
            gl_window: None,
            gl: None,
            egui: None,
            shell: None,
        }
    }

    fn redraw(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let gl_window = self.gl_window.as_ref().expect("OpenGL window");
        let gl = self.gl.as_ref().expect("OpenGL context");
        let egui = self.egui.as_mut().expect("egui renderer");
        let shell = self.shell.as_mut().expect("terminal shell");

        egui.run(gl_window.window(), |ctx| shell.ui(ctx));
        if shell.terminal.should_close() {
            event_loop.exit();
            return;
        }

        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        egui.paint(gl_window.window());
        gl_window.swap_buffers().expect("swap egui buffers");
    }
}

impl winit::application::ApplicationHandler<UserEvent> for NativeApp {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.gl_window.is_some() {
            return;
        }

        let gl_window = unsafe { GlutinWindowContext::new(event_loop) };
        let gl = Arc::new(unsafe {
            glow::Context::from_loader_function(|name| {
                let name = CString::new(name).expect("OpenGL symbol contains NUL");
                gl_window.get_proc_address(&name)
            })
        });
        let egui = egui_glow::EguiGlow::new(event_loop, Arc::clone(&gl), None, None, true);
        let proxy = self.proxy.clone();
        egui.egui_ctx.set_request_repaint_callback(move |info| {
            let _ = proxy.send_event(UserEvent::Repaint(info.delay));
        });

        let shell = ShellApp::new(
            &egui.egui_ctx,
            &gl,
            self.args.take().expect("application resumed twice"),
        );
        gl_window.window().set_visible(true);
        gl_window.window().request_redraw();
        self.gl_window = Some(gl_window);
        self.gl = Some(gl);
        self.egui = Some(egui);
        self.shell = Some(shell);
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        use winit::event::WindowEvent;
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            event_loop.exit();
            return;
        }
        if matches!(event, WindowEvent::RedrawRequested) {
            self.redraw(event_loop);
            return;
        }
        if let WindowEvent::Resized(size) = event {
            self.gl_window.as_ref().expect("OpenGL window").resize(size);
        }

        let response = self.egui.as_mut().expect("egui renderer").on_window_event(
            self.gl_window.as_ref().expect("OpenGL window").window(),
            &event,
        );
        if response.repaint {
            self.gl_window
                .as_ref()
                .expect("OpenGL window")
                .window()
                .request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, event: UserEvent) {
        let UserEvent::Repaint(delay) = event;
        if delay.is_zero() {
            if let Some(window) = self.gl_window.as_ref() {
                window.window().request_redraw();
            }
        } else if let Some(deadline) = std::time::Instant::now().checked_add(delay) {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
        }
    }

    fn new_events(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        cause: winit::event::StartCause,
    ) {
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. })
            && let Some(window) = self.gl_window.as_ref()
        {
            window.window().request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        if let (Some(shell), Some(gl)) = (self.shell.as_mut(), self.gl.as_ref()) {
            shell.destroy(gl);
        }
        if let Some(egui) = self.egui.as_mut() {
            egui.destroy();
        }
    }
}

struct GlutinWindowContext {
    window: winit::window::Window,
    context: glutin::context::PossiblyCurrentContext,
    display: glutin::display::Display,
    surface: glutin::surface::Surface<glutin::surface::WindowSurface>,
}

impl GlutinWindowContext {
    unsafe fn new(event_loop: &winit::event_loop::ActiveEventLoop) -> Self {
        use glutin::context::NotCurrentGlContext as _;
        use glutin::display::{GetGlDisplay as _, GlDisplay as _};
        use glutin::prelude::GlSurface as _;
        use winit::raw_window_handle::HasWindowHandle as _;

        let window_attributes = winit::window::WindowAttributes::default()
            .with_title("Ghostty — egui OpenGL sample")
            .with_inner_size(winit::dpi::LogicalSize::new(1000.0, 700.0))
            .with_visible(false);
        let template = glutin::config::ConfigTemplateBuilder::new()
            .prefer_hardware_accelerated(None)
            .with_depth_size(0)
            .with_stencil_size(0)
            .with_transparency(false);
        let display_builder = glutin_winit::DisplayBuilder::new()
            .with_preference(gl_api_preference())
            .with_window_attributes(Some(window_attributes.clone()));
        let (window, config) = display_builder
            .build(event_loop, template, |mut configs| {
                configs.next().expect("no compatible OpenGL configuration")
            })
            .expect("create OpenGL display");
        let display = config.display();
        let raw_window = window
            .as_ref()
            .map(|window| window.window_handle().expect("window handle").as_raw());
        let attributes = glutin::context::ContextAttributesBuilder::new()
            .with_context_api(glutin::context::ContextApi::OpenGl(Some(
                glutin::context::Version::new(4, 3),
            )))
            .build(raw_window);
        let not_current = unsafe {
            display
                .create_context(&config, &attributes)
                .expect("create OpenGL 4.3 context")
        };
        let window = window.unwrap_or_else(|| {
            glutin_winit::finalize_window(event_loop, window_attributes, &config)
                .expect("create window")
        });
        let size = window.inner_size();
        let surface_attributes =
            glutin::surface::SurfaceAttributesBuilder::<glutin::surface::WindowSurface>::new()
                .build(
                    window.window_handle().expect("window handle").as_raw(),
                    NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
                    NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
                );
        let surface = unsafe {
            display
                .create_window_surface(&config, &surface_attributes)
                .expect("create OpenGL window surface")
        };
        let context = not_current
            .make_current(&surface)
            .expect("make OpenGL current");
        let _ = surface.set_swap_interval(&context, glutin::surface::SwapInterval::DontWait);

        Self {
            window,
            context,
            display,
            surface,
        }
    }

    fn window(&self) -> &winit::window::Window {
        &self.window
    }

    fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        use glutin::surface::GlSurface as _;
        self.surface.resize(
            &self.context,
            NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
        );
    }

    fn swap_buffers(&self) -> glutin::error::Result<()> {
        use glutin::surface::GlSurface as _;
        self.surface.swap_buffers(&self.context)
    }

    fn get_proc_address(&self, name: &std::ffi::CStr) -> *const c_void {
        use glutin::display::GlDisplay as _;
        self.display.get_proc_address(name)
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn gl_api_preference() -> glutin_winit::ApiPreference {
    // Ghostty's Unix OpenGL backend uses EGL. Prefer it on X11 as well as
    // Wayland so the two renderer contexts can share texture objects.
    glutin_winit::ApiPreference::PreferEgl
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn gl_api_preference() -> glutin_winit::ApiPreference {
    glutin_winit::ApiPreference::FallbackEgl
}

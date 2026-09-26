use crate::frame_trace::{FrameSample, FrameTrace};
use crate::presentation::{Presentation, UiChanges};
use crate::terminal::TerminalView;
use egui_winit::winit;
use glow::HasContext as _;
use std::ffi::{CString, c_char, c_void};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

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
    fn new(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        args: Vec<CString>,
    ) -> Self {
        let mut argv: Vec<*mut c_char> = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
        Self {
            terminal: TerminalView::new(egui_ctx, gl, proxy, argv.len(), argv.as_mut_ptr()),
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
pub(crate) enum UserEvent {
    Repaint(Instant),
    GhosttyWake,
    FrameReady(Instant),
}

struct NativeApp {
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    args: Option<Vec<CString>>,
    gl_window: Option<GlutinWindowContext>,
    gl: Option<Arc<glow::Context>>,
    egui: Option<egui_glow::EguiGlow>,
    shell: Option<ShellApp>,
    trace: FrameTrace,
    presentation: Presentation,
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
            trace: FrameTrace::new(),
            presentation: Presentation::default(),
        }
    }

    fn redraw(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let started = Instant::now();
        let gl_window = self.gl_window.as_ref().expect("OpenGL window");
        let gl = self.gl.as_ref().expect("OpenGL context");
        let egui = self.egui.as_mut().expect("egui renderer");
        let shell = self.shell.as_mut().expect("terminal shell");

        egui.run(gl_window.window(), |ctx| shell.ui(ctx));
        let ran_ui = Instant::now();
        let retired_before_paint = shell.terminal.retired_pending();
        if shell.terminal.should_close() {
            event_loop.exit();
            return;
        }

        self.presentation.ui_dirty |= egui
            .egui_ctx
            .with_plugin(|changes: &mut UiChanges| std::mem::take(&mut changes.changed))
            .expect("UI change tracking registered");
        let now = Instant::now();
        let paced = self
            .presentation
            .present_deadline(now)
            .is_some_and(|due| due > now);
        let mut new_frame = false;
        if self.presentation.ready(now) {
            new_frame = shell.terminal.acquire_frame();
            // A queued notification may refer to a frame already acquired.
            self.presentation.frame_pending = new_frame;
        }
        let generation = shell.terminal.frame_generation();
        let presented = !paced && self.presentation.ready(now);
        if presented {
            unsafe {
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
            egui.paint(gl_window.window());
        } else {
            shell.terminal.poll_retired(gl);
        }
        let painted = Instant::now();
        let paint_trace = shell.terminal.paint_trace();
        self.presentation
            .cleanup_pending(shell.terminal.retired_pending(), Instant::now());
        if presented {
            let present_started = Instant::now();
            gl_window.swap_buffers().expect("swap egui buffers");
            self.presentation.presented(present_started);
        }
        let swapped = Instant::now();
        self.trace.record(
            &egui.egui_ctx,
            FrameSample {
                started,
                ran_ui,
                painted,
                swapped,
                generation,
                new_frame,
                presented,
                paced,
                retired_before_paint,
                paint_trace,
            },
        );
    }
}

impl winit::application::ApplicationHandler<UserEvent> for NativeApp {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.gl_window.is_some() {
            return;
        }

        let gl_window = unsafe { GlutinWindowContext::new(event_loop) };
        if let Some(refresh_millihertz) = gl_window
            .window()
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .filter(|rate| *rate > 0)
        {
            self.presentation.interval = std::time::Duration::from_nanos(
                1_000_000_000_000_u64 / u64::from(refresh_millihertz),
            );
        }
        let gl = Arc::new(unsafe {
            glow::Context::from_loader_function(|name| {
                let name = CString::new(name).expect("OpenGL symbol contains NUL");
                gl_window.get_proc_address(&name)
            })
        });
        let egui = egui_glow::EguiGlow::new(event_loop, Arc::clone(&gl), None, None, true);
        egui.egui_ctx.add_plugin(UiChanges::default());
        let proxy = self.proxy.clone();
        egui.egui_ctx
            .global_style_mut(|style| style.animation_time = 0.0);
        egui.egui_ctx.set_request_repaint_callback(move |info| {
            if let Some(deadline) = Instant::now().checked_add(info.delay) {
                let _ = proxy.send_event(UserEvent::Repaint(deadline));
            }
        });

        let shell = ShellApp::new(
            &egui.egui_ctx,
            &gl,
            self.proxy.clone(),
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
            self.presentation.force = true;
        }
        if matches!(
            event,
            WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(false)
        ) {
            self.presentation.force = true;
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) {
            self.trace.input_events += 1;
            self.trace.pending_input.get_or_insert_with(Instant::now);
        }

        let response = self.egui.as_mut().expect("egui renderer").on_window_event(
            self.gl_window.as_ref().expect("OpenGL window").window(),
            &event,
        );
        if response.repaint {
            self.trace.window_repaints += 1;
            self.gl_window
                .as_ref()
                .expect("OpenGL window")
                .window()
                .request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::GhosttyWake => {
                if let Some(shell) = self.shell.as_mut() {
                    let started = Instant::now();
                    shell.terminal.tick();
                    self.trace.tick_events += 1;
                    self.trace.tick_time += started.elapsed();
                }
            }
            UserEvent::FrameReady(ready) => {
                self.trace.pending_frame_ready = Some(ready);
                self.trace.ready_events += 1;
                self.presentation.frame_pending = true;
                self.trace.frame_event_deferred |= !self.presentation.ready(Instant::now());
            }
            UserEvent::Repaint(deadline) => {
                self.trace.repaint_events += 1;
                self.presentation.repaint_at(deadline);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let now = Instant::now();
        if self
            .presentation
            .cleanup_deadline
            .is_some_and(|due| due <= now)
        {
            self.presentation.cleanup_deadline = None;
            if let (Some(shell), Some(gl)) = (&self.shell, &self.gl) {
                // The UI context remains current on this thread. Cleanup does
                // not run egui, paint, or swap buffers.
                shell.terminal.poll_retired(gl);
                self.presentation
                    .cleanup_pending(shell.terminal.retired_pending(), now);
            }
        }
        let repaint_due = self
            .presentation
            .repaint_deadline
            .is_some_and(|due| due <= now);
        if repaint_due {
            self.presentation.repaint_deadline = None;
            self.trace.timer_events += 1;
        }
        if (repaint_due || self.presentation.ready(now))
            && let Some(window) = &self.gl_window
        {
            window.window().request_redraw();
        }
        event_loop.set_control_flow(match self.presentation.next_deadline(now) {
            Some(deadline) => winit::event_loop::ControlFlow::WaitUntil(deadline),
            None => winit::event_loop::ControlFlow::Wait,
        });
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
            .with_decorations(false)
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
        if let Err(err) =
            surface.set_swap_interval(&context, glutin::surface::SwapInterval::DontWait)
        {
            eprintln!("egui OpenGL swap interval could not be disabled: {err}");
        }

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

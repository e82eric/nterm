use crate::native_error::{FatalError, FatalResult};
use crate::presentation::{Presentation, UiChanges};
use crate::terminal::TerminalView;
use egui_winit::winit;
use glow::HasContext as _;
use std::ffi::{CString, c_char, c_void};
use std::num::NonZeroU32;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

pub(crate) fn run(
    args: Vec<CString>,
    pickers: crate::picker::config::Bindings,
    floating_padding: Option<crate::config::FloatingPadding>,
    issues: Vec<String>,
) -> Result<(), FatalError> {
    let event_loop = winit::event_loop::EventLoop::<UserEvent>::with_user_event()
        .build()
        .fatal("create the application event loop")?;
    let proxy = event_loop.create_proxy();
    let mut app = NativeApp::new(proxy, args, pickers, floating_padding, issues);
    let result = event_loop
        .run_app(&mut app)
        .fatal("run the application event loop");
    match app.fatal_error.take() {
        Some(error) => Err(error),
        None => result,
    }
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
        pickers: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        issues: Vec<String>,
    ) -> Result<Self, FatalError> {
        let command_label = command_label(&args);
        let mut argv: Vec<*mut c_char> = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
        Ok(Self {
            terminal: TerminalView::new(
                egui_ctx,
                gl,
                proxy,
                argv.len(),
                argv.as_mut_ptr(),
                command_label,
                pickers,
                floating_padding,
                issues,
            )?,
            _args: args,
            _argv: argv,
        })
    }

    fn ui(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        self.terminal.update(ui, gl);
    }

    fn destroy(&mut self, gl: &glow::Context) {
        self.terminal.destroy(gl);
    }
}

fn command_label(args: &[CString]) -> String {
    let mut command = None;
    let mut args = args.iter().skip(1).map(|arg| arg.to_string_lossy());
    while let Some(arg) = args.next() {
        if arg == "-e" || arg == "--" {
            command = args.next().map(|arg| arg.into_owned());
            break;
        }
        if arg == "--command" {
            command = args.next().map(|arg| arg.into_owned());
        } else if let Some(value) = arg.strip_prefix("--command=") {
            command = Some(value.to_owned());
        }
    }
    let Some(command) = command else {
        return "TERMINAL".into();
    };
    let command = command.trim();
    let executable = if let Some(quoted) = command.strip_prefix('"') {
        quoted.split('"').next().unwrap_or(quoted)
    } else if let Some(end) = command.to_ascii_lowercase().find(".exe") {
        &command[..end + 4]
    } else {
        command.split_whitespace().next().unwrap_or(command)
    };
    let executable = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
    if executable.is_empty() {
        "TERMINAL".into()
    } else {
        executable.to_ascii_uppercase()
    }
}

pub(crate) fn open_new_window() -> std::io::Result<()> {
    Command::new(std::env::current_exe()?)
        .args(std::env::args_os().skip(1))
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod command_label_tests {
    use super::command_label;
    use std::ffi::CString;

    #[test]
    fn explicit_command_overrides_lua_command() {
        let args = [
            "ghostty",
            "--command=cmd.exe",
            "--command=C:\\Program Files\\PowerShell\\7\\pwsh.exe",
        ]
        .map(|arg| CString::new(arg).unwrap());
        assert_eq!(command_label(&args), "PWSH.EXE");
    }

    #[test]
    fn defaults_to_terminal_without_a_command() {
        let args = [CString::new("ghostty").unwrap()];
        assert_eq!(command_label(&args), "TERMINAL");
    }
}

#[derive(Debug)]
pub(crate) enum UserEvent {
    Repaint(Instant),
    GhosttyWake,
    FrameReady,
}

struct NativeApp {
    fatal_error: Option<FatalError>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    args: Option<Vec<CString>>,
    pickers: Option<crate::picker::config::Bindings>,
    floating_padding: Option<crate::config::FloatingPadding>,
    issues: Vec<String>,
    gl_window: Option<GlutinWindowContext>,
    gl: Option<Arc<glow::Context>>,
    egui: Option<egui_glow::EguiGlow>,
    shell: Option<ShellApp>,
    presentation: Presentation,
}

impl NativeApp {
    fn new(
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        args: Vec<CString>,
        pickers: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        issues: Vec<String>,
    ) -> Self {
        Self {
            fatal_error: None,
            proxy,
            args: Some(args),
            pickers: Some(pickers),
            floating_padding,
            issues,
            gl_window: None,
            gl: None,
            egui: None,
            shell: None,
            presentation: Presentation::default(),
        }
    }

    fn redraw(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.fatal_error.is_some() {
            return;
        }
        let gl_window = self.gl_window.as_ref().expect("OpenGL window");
        let gl = self.gl.as_ref().expect("OpenGL context");
        let egui = self.egui.as_mut().expect("egui renderer");
        let shell = self.shell.as_mut().expect("terminal shell");

        egui.run(gl_window.window(), |ctx| shell.ui(ctx, gl));
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
        if self.presentation.ready(now) {
            let new_frame = shell.terminal.acquire_frames();
            // A queued notification may refer to a frame already acquired.
            self.presentation.frame_pending = new_frame;
        }
        let presented = !paced && self.presentation.ready(now);
        if presented {
            unsafe {
                let background = crate::colors::palette().background;
                gl.clear_color(
                    f32::from(background.r()) / 255.0,
                    f32::from(background.g()) / 255.0,
                    f32::from(background.b()) / 255.0,
                    1.0,
                );
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
            egui.paint(gl_window.window());
        } else {
            shell.terminal.poll_retired(gl);
        }
        self.presentation
            .cleanup_pending(shell.terminal.retired_pending(), Instant::now());
        if presented {
            let present_started = Instant::now();
            if let Err(error) = gl_window
                .swap_buffers()
                .fatal("present the application frame (swap OpenGL buffers)")
            {
                self.fatal_error = Some(error);
                event_loop.exit();
                return;
            }
            self.presentation.presented(present_started);
        }
    }
}

impl winit::application::ApplicationHandler<UserEvent> for NativeApp {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.gl_window.is_some() {
            return;
        }

        if self.fatal_error.is_some() {
            return;
        }
        let gl_window = match unsafe { GlutinWindowContext::new(event_loop) } {
            Ok(window) => window,
            Err(error) => {
                self.fatal_error = Some(error);
                event_loop.exit();
                return;
            }
        };
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
        let mut egui = egui_glow::EguiGlow::new(event_loop, Arc::clone(&gl), None, None, true);
        crate::colors::COLORS.set(&egui.egui_ctx, crate::colors::palette());
        egui.egui_ctx.add_plugin(UiChanges::default());
        let proxy = self.proxy.clone();
        egui.egui_ctx
            .global_style_mut(|style| style.animation_time = 0.0);
        egui.egui_ctx.set_request_repaint_callback(move |info| {
            if let Some(deadline) = Instant::now().checked_add(info.delay) {
                let _ = proxy.send_event(UserEvent::Repaint(deadline));
            }
        });

        let shell = match ShellApp::new(
            &egui.egui_ctx,
            &gl,
            self.proxy.clone(),
            self.args.take().expect("application resumed twice"),
            self.pickers.take().expect("application resumed twice"),
            self.floating_padding,
            std::mem::take(&mut self.issues),
        ) {
            Ok(shell) => shell,
            Err(error) => {
                egui.destroy();
                self.fatal_error = Some(error);
                event_loop.exit();
                return;
            }
        };
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
        if self.fatal_error.is_some() {
            return;
        }
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

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, event: UserEvent) {
        if self.fatal_error.is_some() {
            return;
        }
        match event {
            UserEvent::GhosttyWake => {
                if let Some(shell) = self.shell.as_mut() {
                    shell.terminal.tick();
                }
            }
            UserEvent::FrameReady => {
                self.presentation.frame_pending = true;
            }
            UserEvent::Repaint(deadline) => {
                self.presentation.repaint_at(deadline);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.fatal_error.is_some() {
            return;
        }
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

/// Cosmetic opt-in only: unsupported DWM attributes must not prevent startup.
#[cfg(windows)]
fn prefer_rounded_corners(hwnd: *mut std::ffi::c_void) {
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut std::ffi::c_void,
            attribute: u32,
            value: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;
    const E_INVALIDARG: i32 = 0x80070057u32 as i32;
    // DWM clips the entire client area, including our custom border. Leave
    // maximized/snapped-window corner policy to Windows.
    let preference = DWMWCP_ROUND;
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&preference as *const u32).cast(),
            std::mem::size_of_val(&preference) as u32,
        )
    };
    // Windows 10 does not recognize this attribute; retain its normal corners.
    if result < 0 && result != E_INVALIDARG {
        crate::error_log::warning(
            "window.corners",
            "Rounded window corners unavailable",
            &format!(
                "DwmSetWindowAttribute failed with HRESULT 0x{:08X}",
                result as u32
            ),
        );
    }
}

impl GlutinWindowContext {
    unsafe fn new(event_loop: &winit::event_loop::ActiveEventLoop) -> Result<Self, FatalError> {
        use glutin::context::NotCurrentGlContext as _;
        use glutin::display::GlDisplay as _;
        use glutin::prelude::GlSurface as _;
        use winit::raw_window_handle::{HasDisplayHandle as _, HasWindowHandle as _};

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
        // WGL needs a native window to enumerate its full configuration set.
        // Enumerate directly: DisplayBuilder's picker cannot return an error
        // when the driver supplies no compatible configurations.
        #[cfg(windows)]
        let window = Some(
            event_loop
                .create_window(window_attributes.clone())
                .fatal("create the OpenGL window")?,
        );
        #[cfg(not(windows))]
        let window: Option<winit::window::Window> = None;
        let raw_window = window
            .as_ref()
            .map(|window| {
                window
                    .window_handle()
                    .fatal("get the window handle")
                    .map(|handle| handle.as_raw())
            })
            .transpose()?;
        #[cfg(windows)]
        if let Some(winit::raw_window_handle::RawWindowHandle::Win32(handle)) = raw_window {
            prefer_rounded_corners(handle.hwnd.get() as *mut std::ffi::c_void);
        }
        let raw_display = event_loop
            .display_handle()
            .fatal("get the display handle")?
            .as_raw();
        #[cfg(windows)]
        let preference = glutin::display::DisplayApiPreference::WglThenEgl(raw_window);
        #[cfg(target_os = "macos")]
        let preference = glutin::display::DisplayApiPreference::Cgl;
        #[cfg(not(any(windows, target_os = "macos")))]
        let preference = glutin::display::DisplayApiPreference::EglThenGlx(Box::new(
            winit::platform::x11::register_xlib_error_hook,
        ));
        let display = unsafe { glutin::display::Display::new(raw_display, preference) }
            .fatal("create the OpenGL display")?;
        #[cfg(windows)]
        let template = if let Some(handle) = raw_window {
            template.compatible_with_native_window(handle)
        } else {
            template
        };
        let config = unsafe { display.find_configs(template.build()) }
            .fatal("enumerate OpenGL configurations")?
            .next()
            .ok_or_else(|| {
                FatalError::new(
                    "select an OpenGL configuration",
                    "The graphics driver returned no compatible OpenGL configuration.",
                )
            })?;
        let attributes = glutin::context::ContextAttributesBuilder::new()
            .with_context_api(glutin::context::ContextApi::OpenGl(Some(
                glutin::context::Version::new(4, 3),
            )))
            .build(raw_window);
        let not_current = unsafe {
            display
                .create_context(&config, &attributes)
                .fatal("create the OpenGL 4.3 context")?
        };
        let window = match window {
            Some(window) => window,
            None => glutin_winit::finalize_window(event_loop, window_attributes, &config)
                .fatal("create the OpenGL window")?,
        };
        let size = window.inner_size();
        let surface_attributes =
            glutin::surface::SurfaceAttributesBuilder::<glutin::surface::WindowSurface>::new()
                .build(
                    window
                        .window_handle()
                        .fatal("get the window handle")?
                        .as_raw(),
                    NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
                    NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
                );
        let surface = unsafe {
            display
                .create_window_surface(&config, &surface_attributes)
                .fatal("create the OpenGL window surface")?
        };
        let context = not_current
            .make_current(&surface)
            .fatal("make the OpenGL context current")?;
        if let Err(err) =
            surface.set_swap_interval(&context, glutin::surface::SwapInterval::DontWait)
        {
            crate::error_log::warning(
                "graphics",
                "Swap interval could not be disabled",
                &err.to_string(),
            );
        }

        Ok(Self {
            window,
            context,
            display,
            surface,
        })
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

use crate::native_error::{FatalError, FatalResult};
use crate::presentation::Presentation;
use std::cell::RefCell;
use std::rc::Rc;

mod shared_ui;
use crate::graphics as glow;
use crate::terminal::TerminalView;
use egui_winit::winit;

use glow::HasContext as _;
use shared_ui::{SharedUi, WindowUi};

use std::ffi::c_void;
use std::ffi::{CString, c_char};

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

pub(crate) fn run(
    args: Vec<CString>,
    pickers: crate::picker::config::Bindings,
    floating_padding: Option<crate::config::FloatingPadding>,
    issues: Vec<String>,
    reload_source: crate::config::ReloadSource,
) -> Result<(), FatalError> {
    let event_loop = winit::event_loop::EventLoop::<UserEvent>::with_user_event()
        .build()
        .fatal("create the application event loop")?;
    let proxy = event_loop.create_proxy();
    let _ = APP_PROXY.set(proxy.clone());
    let mut app = NativeApp::new(
        proxy,
        args,
        pickers,
        floating_padding,
        issues,
        reload_source,
    );
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
    _startup: std::rc::Rc<StartupArgs>,
}

struct StartupArgs {
    _args: Vec<CString>,
    _argv: Vec<*mut c_char>,
}

impl ShellApp {
    fn new(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        window_id: winit::window::WindowId,
        args: Vec<CString>,
        pickers: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        issues: Vec<String>,
        reload_source: crate::config::ReloadSource,
    ) -> Result<Self, FatalError> {
        let command_label = command_label(&args);
        let mut argv: Vec<*mut c_char> = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
        Ok(Self {
            terminal: TerminalView::new(
                egui_ctx,
                gl,
                proxy,
                window_id,
                argv.len(),
                argv.as_mut_ptr(),
                command_label,
                pickers,
                floating_padding,
                issues,
                reload_source,
            )?,
            _startup: std::rc::Rc::new(StartupArgs {
                _args: args,
                _argv: argv,
            }),
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

static APP_PROXY: std::sync::OnceLock<winit::event_loop::EventLoopProxy<UserEvent>> =
    std::sync::OnceLock::new();
static APP_WAKE_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// The shared native app outlives any particular pane's callback userdata.
#[unsafe(no_mangle)]
extern "C" fn ghostty_egui_app_wakeup() {
    crate::native_error::ffi_boundary("wake the shared terminal app", || {
        if let Some(proxy) = APP_PROXY.get() {
            if !APP_WAKE_PENDING.swap(true, std::sync::atomic::Ordering::AcqRel)
                && proxy.send_event(UserEvent::GhosttyWake).is_err()
            {
                APP_WAKE_PENDING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
    });
}

#[derive(Debug)]
pub(crate) enum UserEvent {
    Repaint(Instant),
    GhosttyWake,
    FrameReady(winit::window::WindowId),
    PickerWake { window_id: winit::window::WindowId },
}

struct NativeApp {
    fatal_error: Option<FatalError>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    startup: Option<(
        Vec<CString>,
        crate::picker::config::Bindings,
        Option<crate::config::FloatingPadding>,
        Vec<String>,
        crate::config::ReloadSource,
    )>,
    windows: std::collections::HashMap<winit::window::WindowId, NativeWindow>,
    shared_ui: Option<Rc<RefCell<SharedUi>>>,
}

struct NativeWindow {
    // Release pane resources with the shared context bound to this surface.
    gl_window: GlutinWindowSurface,
    shared_ui: Rc<RefCell<SharedUi>>,
    gl: Arc<glow::Context>,
    egui: WindowUi,
    shell: Option<ShellApp>,
    presentation: Presentation,
}

impl NativeWindow {
    fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        shared_ui: Rc<RefCell<SharedUi>>,
    ) -> Result<Self, FatalError> {
        let graphics = shared_ui.borrow().graphics.clone();
        let gl_window = unsafe { GlutinWindowSurface::new(event_loop, graphics) }?;
        let mut presentation = Presentation::default();
        if let Some(rate) = gl_window
            .window()
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .filter(|rate| *rate > 0)
        {
            presentation.interval =
                std::time::Duration::from_nanos(1_000_000_000_000 / u64::from(rate));
        }
        let gl = shared_ui.borrow().gl.clone();
        let egui = WindowUi::new(event_loop, gl_window.window(), &shared_ui.borrow());
        Ok(Self {
            gl_window,
            shared_ui,
            gl,
            egui,
            shell: None,
            presentation,
        })
    }

    fn show(&self) {
        self.gl_window.window().set_visible(true);
        self.gl_window.window().request_redraw();
    }

    fn redraw(&mut self) -> Result<(), FatalError> {
        self.gl_window.make_current()?;
        let shell = self.shell.as_mut().expect("terminal shell");
        let (output, changed) = self
            .egui
            .run(self.gl_window.window(), |ui| shell.ui(ui, &self.gl));
        self.presentation.ui_dirty |= changed;
        let now = Instant::now();
        let paced = self
            .presentation
            .present_deadline(now)
            .is_some_and(|due| due > now);
        if self.presentation.ready(now) {
            self.presentation.frame_pending = shell.terminal.acquire_frames();
        }
        let presented = !paced && self.presentation.ready(now);
        if presented {
            unsafe {
                let background = crate::colors::palette().background;
                self.gl.clear_color(
                    f32::from(background.r()) / 255.0,
                    f32::from(background.g()) / 255.0,
                    f32::from(background.b()) / 255.0,
                    1.0,
                );
                self.gl.clear(glow::COLOR_BUFFER_BIT);
            }
            self.shared_ui
                .borrow_mut()
                .finish_output(output, Some(self.gl_window.window().inner_size().into()))?;
        } else {
            self.shared_ui.borrow_mut().finish_output(output, None)?;
            shell.terminal.poll_retired(&self.gl);
        }
        self.presentation
            .cleanup_pending(shell.terminal.retired_pending(), Instant::now());
        if presented {
            let started = Instant::now();
            self.gl_window
                .swap_buffers()
                .fatal("present the application frame (swap OpenGL buffers)")?;
            self.presentation.presented(started);
        }
        Ok(())
    }

    fn before_wait(&mut self, now: Instant) -> Result<Option<Instant>, FatalError> {
        if self
            .presentation
            .cleanup_deadline
            .is_some_and(|due| due <= now)
        {
            self.gl_window.make_current()?;
            self.presentation.cleanup_deadline = None;
            if let Some(shell) = &self.shell {
                shell.terminal.poll_retired(&self.gl);
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
        if repaint_due || self.presentation.ready(now) {
            self.gl_window.window().request_redraw();
        }
        Ok(self.presentation.next_deadline(now))
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        if let Err(error) = self.gl_window.make_current() {
            crate::error_log::error(
                "window.close",
                "Could not activate graphics context",
                &error.to_string(),
            );
            // Native renderer callbacks still reference the panes. Do not drop
            // their userdata unless we can safely release the native resources.
            crate::native_error::show(&error.to_string());
            std::process::exit(1);
        }
        if let Some(shell) = &mut self.shell {
            shell.destroy(&self.gl);
        }
    }
}

impl NativeApp {
    fn new(
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        args: Vec<CString>,
        pickers: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        issues: Vec<String>,
        reload_source: crate::config::ReloadSource,
    ) -> Self {
        Self {
            fatal_error: None,
            proxy,
            startup: Some((args, pickers, floating_padding, issues, reload_source)),
            windows: std::collections::HashMap::new(),
            shared_ui: None,
        }
    }

    fn open_window(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        parent_id: winit::window::WindowId,
    ) -> Result<(), FatalError> {
        let Some(parent) = self.windows.get(&parent_id) else {
            return Ok(());
        };
        let mut window = NativeWindow::new(
            event_loop,
            self.shared_ui.as_ref().expect("shared renderer").clone(),
        )?;
        let parent_shell = parent.shell.as_ref().expect("parent terminal");
        let terminal = parent_shell.terminal.new_window(
            &window.egui.egui_ctx,
            &window.gl,
            window.gl_window.window().id(),
        )?;
        window.shell = Some(ShellApp {
            terminal,
            _startup: parent_shell._startup.clone(),
        });
        window.show();
        self.windows.insert(window.gl_window.window().id(), window);
        self.sync_viewports()?;
        Ok(())
    }

    fn sync_viewports(&mut self) -> Result<(), FatalError> {
        if let Some(shared) = &self.shared_ui {
            shared
                .borrow_mut()
                .sync_viewports(self.windows.values().map(|window| window.egui.viewport_id))?;
        }
        Ok(())
    }

    fn fail(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, error: FatalError) {
        self.fatal_error = Some(error);
        event_loop.exit();
    }

    fn close_window(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
    ) {
        self.windows.remove(&id);
        if self.windows.is_empty() {
            self.shared_ui = None;
            event_loop.exit();
        } else if let Err(error) = self.sync_viewports() {
            self.fail(event_loop, error);
        }
    }
}

impl winit::application::ApplicationHandler<UserEvent> for NativeApp {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.fatal_error.is_some() {
            return;
        }
        let Some((args, pickers, padding, issues, reload)) = self.startup.take() else {
            return;
        };
        let result = (|| {
            self.shared_ui = Some(Rc::new(RefCell::new(SharedUi::new(
                event_loop,
                self.proxy.clone(),
            )?)));
            let mut window = NativeWindow::new(
                event_loop,
                self.shared_ui.as_ref().expect("shared renderer").clone(),
            )?;
            window.shell = Some(ShellApp::new(
                &window.egui.egui_ctx,
                &window.gl,
                self.proxy.clone(),
                window.gl_window.window().id(),
                args,
                pickers,
                padding,
                issues,
                reload,
            )?);
            window.show();
            self.windows.insert(window.gl_window.window().id(), window);
            self.sync_viewports()?;
            Ok::<_, FatalError>(())
        })();
        if let Err(error) = result {
            self.fail(event_loop, error);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        use winit::event::WindowEvent;
        if self.fatal_error.is_some() {
            return;
        }
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            self.close_window(event_loop, id);
            return;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        if matches!(event, WindowEvent::RedrawRequested) {
            if let Err(error) = window.redraw() {
                self.fail(event_loop, error);
                return;
            }
            let terminal = &mut window.shell.as_mut().expect("terminal shell").terminal;
            if terminal.should_close() {
                self.close_window(event_loop, id);
                return;
            }
            if terminal.take_new_window_request() {
                if let Err(error) = self.open_window(event_loop, id) {
                    if let Some(parent) = self.windows.get_mut(&id) {
                        parent
                            .shell
                            .as_mut()
                            .expect("parent terminal")
                            .terminal
                            .window_error(error.to_string());
                        parent.gl_window.window().request_redraw();
                    }
                }
            }
            return;
        }
        if let WindowEvent::Resized(size) = event {
            if let Err(error) = window.gl_window.make_current() {
                self.fail(event_loop, error);
                return;
            }
            window.gl_window.resize(size);
            window.presentation.force = true;
        }
        if matches!(
            event,
            WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(false)
        ) {
            window.presentation.force = true;
        }
        if window
            .egui
            .on_window_event(window.gl_window.window(), &event)
            .repaint
        {
            window.gl_window.window().request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, event: UserEvent) {
        if self.fatal_error.is_some() {
            return;
        }
        match event {
            UserEvent::PickerWake { window_id } => {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    // Service queued work immediately; only GPU presentation is paced.
                    window.gl_window.window().request_redraw();
                }
            }
            UserEvent::GhosttyWake => {
                APP_WAKE_PENDING.store(false, std::sync::atomic::Ordering::Release);
                // All windows/panes share one app; a single tick drains its mailbox.
                if let Some(window) = self.windows.values_mut().next() {
                    window
                        .shell
                        .as_mut()
                        .expect("terminal shell")
                        .terminal
                        .tick();
                }
            }
            UserEvent::FrameReady(id) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.presentation.frame_pending = true;
                }
            }
            UserEvent::Repaint(deadline) => {
                for window in self.windows.values_mut() {
                    window.presentation.repaint_at(deadline);
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.fatal_error.is_some() {
            return;
        }
        let now = Instant::now();
        let mut deadline = None;
        for window in self.windows.values_mut() {
            match window.before_wait(now) {
                Ok(Some(due)) => {
                    deadline = Some(deadline.map_or(due, |old: Instant| old.min(due)));
                }
                Ok(None) => {}
                Err(error) => {
                    self.fail(event_loop, error);
                    return;
                }
            }
        }
        event_loop.set_control_flow(match deadline {
            Some(due) => winit::event_loop::ControlFlow::WaitUntil(due),
            None => winit::event_loop::ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        self.windows.clear();
        self.shared_ui = None;
    }
}

struct GlutinWindowContext {
    // Release the DC/surface before the hidden native window.
    surface: glutin::surface::Surface<glutin::surface::WindowSurface>,
    context: glutin::context::PossiblyCurrentContext,
    config: glutin::config::Config,
    display: glutin::display::Display,
    _window: winit::window::Window,
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
        #[cfg(not(windows))]
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
            _window: window,
            context,
            display,
            config,
            surface,
        })
    }

    fn make_current(&self) -> Result<(), FatalError> {
        use glutin::context::PossiblyCurrentGlContext as _;
        self.context
            .make_current(&self.surface)
            .fatal("activate the window OpenGL context")
    }

    fn get_proc_address(&self, name: &std::ffi::CStr) -> *const c_void {
        use glutin::display::GlDisplay as _;
        self.display.get_proc_address(name)
    }
}

/// Every window has a drawable surface, all using the same UI GL context.

struct GlutinWindowSurface {
    surface: glutin::surface::Surface<glutin::surface::WindowSurface>,
    window: winit::window::Window,
    graphics: Rc<GlutinWindowContext>,
}

impl GlutinWindowSurface {
    unsafe fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        graphics: Rc<GlutinWindowContext>,
    ) -> Result<Self, FatalError> {
        use glutin::display::GlDisplay as _;
        use glutin::surface::GlSurface as _;
        use winit::raw_window_handle::HasWindowHandle as _;
        let attributes = winit::window::WindowAttributes::default()
            .with_title("Ghostty shared graphics anchor")
            .with_inner_size(winit::dpi::LogicalSize::new(1000.0, 700.0))
            .with_decorations(false)
            .with_visible(false);
        let window = glutin_winit::finalize_window(event_loop, attributes, &graphics.config)
            .fatal("create the terminal window")?;
        let handle = window
            .window_handle()
            .fatal("get the terminal window handle")?
            .as_raw();
        #[cfg(windows)]
        if let winit::raw_window_handle::RawWindowHandle::Win32(handle) = handle {
            prefer_rounded_corners(handle.hwnd.get() as *mut c_void);
        }
        let size = window.inner_size();
        let attributes =
            glutin::surface::SurfaceAttributesBuilder::<glutin::surface::WindowSurface>::new()
                .build(
                    handle,
                    NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
                    NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
                );
        let surface = unsafe {
            graphics
                .display
                .create_window_surface(&graphics.config, &attributes)
        }
        .fatal("create the terminal window surface")?;
        let result = Self {
            surface,
            window,
            graphics,
        };
        result.make_current()?;
        if let Err(error) = result.surface.set_swap_interval(
            &result.graphics.context,
            glutin::surface::SwapInterval::DontWait,
        ) {
            crate::error_log::warning(
                "graphics",
                "Swap interval could not be disabled",
                &error.to_string(),
            );
        }
        Ok(result)
    }

    fn make_current(&self) -> Result<(), FatalError> {
        use glutin::context::PossiblyCurrentGlContext as _;
        self.graphics
            .context
            .make_current(&self.surface)
            .fatal("activate the shared OpenGL context for this window")
    }

    fn window(&self) -> &winit::window::Window {
        &self.window
    }

    fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        use glutin::surface::GlSurface as _;
        self.surface.resize(
            &self.graphics.context,
            NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
        );
    }

    fn swap_buffers(&self) -> glutin::error::Result<()> {
        use glutin::surface::GlSurface as _;
        self.surface.swap_buffers(&self.graphics.context)
    }
}

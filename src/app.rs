use crate::gpu::SharedGpu;
use crate::native_error::{FatalError, FatalResult};
use crate::presentation::Presentation;
use std::cell::RefCell;
use std::rc::Rc;

mod shared_ui;
use crate::graphics as glow;
use crate::terminal::TerminalView;
use egui_wgpu::wgpu;
use egui_winit::winit;

use shared_ui::{SharedUi, WindowUi};

use std::ffi::c_void;
use std::ffi::{CString, c_char};

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
        window: crate::graphics::PaneWindow,
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
                window,
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

    fn destroy(&mut self) {
        self.terminal.destroy();
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
extern "C" fn nterm_app_wakeup() {
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
    surface: WindowSurface,
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
        let gpu = shared_ui.borrow().gpu.clone();
        let surface = WindowSurface::new(event_loop, gpu)?;
        let mut presentation = Presentation::default();
        if let Some(rate) = surface
            .window()
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .filter(|rate| *rate > 0)
        {
            presentation.interval =
                std::time::Duration::from_nanos(1_000_000_000_000 / u64::from(rate));
        }
        let gl = shared_ui.borrow().gl.clone();
        let egui = WindowUi::new(event_loop, surface.window(), &shared_ui.borrow());
        Ok(Self {
            surface,
            shared_ui,
            gl,
            egui,
            shell: None,
            presentation,
        })
    }

    /// Runs the Lua startup callback, then shows the window for its first frame.
    fn start(&mut self) -> Result<(), FatalError> {
        let shell = self.shell.as_mut().expect("terminal shell");
        shell.terminal.run_startup(&self.egui.egui_ctx, &self.gl);
        self.surface.window().set_visible(true);
        self.surface.window().request_redraw();
        Ok(())
    }

    fn redraw(&mut self) -> Result<(), FatalError> {
        let shell = self.shell.as_mut().expect("terminal shell");
        // The previous frame's paint callbacks have run or been dropped.
        shell.terminal.before_frame();
        let (output, changed) = self
            .egui
            .run(self.surface.window(), |ui| shell.ui(ui, &self.gl));
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
        let started = Instant::now();
        if presented {
            self.shared_ui
                .borrow_mut()
                .finish_output(output, Some(&self.surface))?;
        } else {
            self.shared_ui.borrow_mut().finish_output(output, None)?;
        }
        if presented {
            self.presentation.presented(started);
        }
        Ok(())
    }

    fn before_wait(&mut self, now: Instant) -> Result<Option<Instant>, FatalError> {
        let repaint_due = self
            .presentation
            .repaint_deadline
            .is_some_and(|due| due <= now);
        if repaint_due {
            self.presentation.repaint_deadline = None;
        }
        if repaint_due || self.presentation.ready(now) {
            self.surface.window().request_redraw();
        }
        Ok(self.presentation.next_deadline(now))
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        if let Some(shell) = &mut self.shell {
            shell.destroy();
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
            window.surface.pane_window(),
        )?;
        window.shell = Some(ShellApp {
            terminal,
            _startup: parent_shell._startup.clone(),
        });
        window.start()?;
        self.windows.insert(window.surface.window().id(), window);
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
                window.surface.pane_window(),
                args,
                pickers,
                padding,
                issues,
                reload,
            )?);
            window.start()?;
            self.windows.insert(window.surface.window().id(), window);
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
                        parent.surface.window().request_redraw();
                    }
                }
            }
            return;
        }
        if let WindowEvent::Resized(size) = event {
            window.surface.resize(size);
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
            .on_window_event(window.surface.window(), &event)
            .repaint
        {
            window.surface.window().request_redraw();
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
                    window.surface.window().request_redraw();
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

/// A native window and its wgpu swapchain on the shared device.
pub(super) struct WindowSurface {
    // The surface presents to the window, so it is dropped first.
    surface: wgpu::Surface<'static>,
    config: RefCell<wgpu::SurfaceConfiguration>,
    window: Arc<winit::window::Window>,
    gpu: Rc<SharedGpu>,
}

impl WindowSurface {
    fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        gpu: Rc<SharedGpu>,
    ) -> Result<Self, FatalError> {
        let attributes = winit::window::WindowAttributes::default()
            .with_title("nterm")
            .with_inner_size(winit::dpi::LogicalSize::new(1000.0, 700.0))
            .with_decorations(false)
            .with_visible(false);
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .fatal("create the terminal window")?,
        );
        let native = native_window(&window);
        #[cfg(windows)]
        if !native.is_null() {
            prefer_rounded_corners(native);
        }
        let surface = gpu
            .instance
            .create_surface(window.clone())
            .fatal("create the window surface")?;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        if !capabilities.formats.contains(&shared_ui::SURFACE_FORMAT) {
            return Err(FatalError::new(
                "configure window presentation",
                format!(
                    "The graphics adapter cannot present {:?} to this window",
                    shared_ui::SURFACE_FORMAT
                ),
            ));
        }
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: shared_ui::SURFACE_FORMAT,
            width: size.width.max(1),
            height: size.height.max(1),
            // Presentation is paced by the app; mailbox replaces a queued
            // image instead of blocking, so several windows never wait on
            // each other's vertical blank.
            present_mode: if capabilities
                .present_modes
                .contains(&wgpu::PresentMode::Mailbox)
            {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: Vec::new(),
        };
        let result = Self {
            surface,
            config: RefCell::new(config),
            window,
            gpu,
        };
        result.configure();
        Ok(result)
    }

    fn window(&self) -> &winit::window::Window {
        &self.window
    }

    /// This window, for creating its first terminal.
    fn pane_window(&self) -> crate::graphics::PaneWindow {
        crate::graphics::PaneWindow {
            id: self.window.id(),
            native: native_window(&self.window),
        }
    }

    pub(super) fn size(&self) -> [u32; 2] {
        let config = self.config.borrow();
        [config.width, config.height]
    }

    fn configure(&self) {
        // Reconfiguring replaces the swapchain; keep terminal submissions out.
        let _submission = self
            .gpu
            .submission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.surface
            .configure(&self.gpu.device, &self.config.borrow());
    }

    fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        {
            let mut config = self.config.borrow_mut();
            if config.width == size.width && config.height == size.height {
                return;
            }
            config.width = size.width;
            config.height = size.height;
        }
        self.configure();
    }

    /// The next image to present, or None when the window cannot show one
    /// now (occluded, or the request timed out).
    pub(super) fn acquire(&self) -> Result<Option<wgpu::SurfaceTexture>, FatalError> {
        let size = self.window.inner_size();
        self.resize(size);
        for attempt in 0..2 {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => return Ok(Some(frame)),
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    return Ok(None);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
                    if attempt == 0 =>
                {
                    self.configure();
                }
                other => {
                    return Err(FatalError::new(
                        "acquire the window image",
                        format!("{other:?}"),
                    ));
                }
            }
        }
        Ok(None)
    }
}

/// The window's native handle: an HWND on Windows, null elsewhere.
fn native_window(window: &winit::window::Window) -> *mut c_void {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    match window.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get() as *mut c_void,
        _ => std::ptr::null_mut(),
    }
}

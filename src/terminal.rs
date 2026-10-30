use crate::actions::AppAction;
use crate::app::UserEvent;
use crate::graphics as glow;
use std::ffi::c_char;

pub(crate) mod floating;
mod layout;
mod pane;
pub(crate) mod workspace;
pub(crate) mod workspace_ui;
use pane::TerminalPane;

pub(crate) struct TerminalView {
    /// Tiled terminals across all workspaces, by tiled ID. Which workspace
    /// each belongs to is in `Workspace::panes`.
    tiled: Vec<(u64, TerminalPane)>,
    closed_panes: Vec<TerminalPane>,
    workspaces: Vec<workspace::Workspace>,
    current: u64,
    previous: u64,
    next_workspace: u64,
    next_pane: u64,
    messages: crate::messages::Messages,
    /// The window's own notices and errors, shown over the workspace.
    toasts: crate::toast::Toasts,
    context: egui::Context,

    command_label: String,
    reload_source: crate::config::ReloadSource,
    /// Shared so per-frame borrows can clone it cheaply; copy-on-write edits.
    picker_bindings: std::rc::Rc<crate::picker::config::Bindings>,
    floating_padding: Option<crate::config::FloatingPadding>,
    floating: floating::Panes<FloatingTerminal>,
    /// Work for the app to do after this window's frame; see `post_requests`.
    requests: Vec<WindowRequest>,
    /// Whether a `UserEvent::WindowRequests` is on its way to the app.
    requests_posted: bool,
    proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
    window_id: egui_winit::winit::window::WindowId,
    /// Set by the status bar's close button; the app then closes this window.
    close_requested: bool,
}

struct FloatingTerminal {
    pane: TerminalPane,
}

/// A terminal taken out of one window on its way to a new window.
pub(crate) struct MovedPane(TerminalPane);

/// Something a window's frame asks of the app. A frame cannot do these
/// itself: they need the event loop or other windows, which the app holds.
pub(crate) enum WindowRequest {
    /// Open another window with a new terminal.
    NewWindow,
    /// Open a new window whose first pane is this one, moved out of the
    /// requesting window.
    MovePaneToNewWindow(MovedPane),
}

/// Window state that a pane's pickers show: the command palette's float and
/// workspace entries, and the messages picker's configuration issues. Built by
/// the window when a picker opens, not kept on each pane.
#[derive(Default)]
struct PaletteContext {
    entries: Vec<crate::actions::Entry>,
    config_issues: Vec<String>,
}

impl std::ops::Deref for FloatingTerminal {
    type Target = TerminalPane;

    fn deref(&self) -> &Self::Target {
        &self.pane
    }
}

impl std::ops::DerefMut for FloatingTerminal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.pane
    }
}

impl FloatingTerminal {
    fn destroy(&mut self) {
        self.pane.destroy();
    }

    fn acquire_frame(&mut self) -> bool {
        self.pane.acquire_frame()
    }
}

/// A terminal in this window. Tiled and floating panes have separate ID
/// spaces, so a reference says which kind it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneRef {
    Tiled(u64),
    Floating(u64),
}

impl TerminalView {
    pub(crate) fn apply_theme(&mut self, name: &str) -> Result<(), String> {
        self.app_pane()
            .ok_or("no terminal is open")?
            .apply_theme(name)
    }
    /// Any terminal in this window. Every terminal shares one native app, so
    /// app-wide work (ticks, settings, and new terminals) can go through any
    /// of them. None only once the window's last terminal has closed.
    fn app_pane(&self) -> Option<&TerminalPane> {
        self.tiled
            .first()
            .map(|(_, p)| p)
            .or_else(|| self.floating.entries.first().map(|e| &e.value.pane))
    }
    /// The visible float, else the focused tiled pane. Only called while the
    /// window has a tiled pane.
    fn active_pane_ref(&self) -> &TerminalPane {
        if let Some(float) = self.visible_float() {
            &float.pane
        } else {
            self.focused_id()
                .and_then(|id| self.tiled_ref(id))
                .or_else(|| self.tiled.first().map(|(_, p)| p))
                .expect("an open window has a tiled pane")
        }
    }
    fn show_error(&mut self, title: &str, error: String) {
        crate::error_log::error("terminal", title, &error);
        self.toasts.push_error(title.into(), error, 10000);
    }
    fn show_notice(&mut self, title: &str, message: String) {
        self.toasts.push(title.into(), message, 3000);
    }

    pub(crate) fn new(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        window: crate::graphics::PaneWindow,
        argc: usize,
        argv: *mut *mut c_char,
        command_label: String,
        picker_bindings: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        mut issues: Vec<String>,
        reload_source: crate::config::ReloadSource,
    ) -> Result<Self, crate::native_error::FatalError> {
        let window_id = window.id;
        let pane = TerminalPane::create(egui_ctx, gl, proxy.clone(), window, argc, argv)?;
        issues.extend(pane.take_config_issues());
        #[cfg(any(windows, unix))]
        pane.set_pipe_policy(&picker_bindings.pipe_policy);
        Ok(Self::with_pane(
            pane,
            egui_ctx,
            proxy,
            window_id,
            issues,
            command_label,
            reload_source,
            std::rc::Rc::new(picker_bindings),
            floating_padding,
        ))
    }

    pub(crate) fn new_window(
        &self,
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        window: crate::graphics::PaneWindow,
    ) -> Result<Self, crate::native_error::FatalError> {
        let window_id = window.id;
        let pane = self
            .app_pane()
            .ok_or_else(|| {
                crate::native_error::FatalError::new("open a window", "no terminal is open")
            })?
            .create_window(egui_ctx, gl, window)?;
        #[cfg(any(windows, unix))]
        pane.set_pipe_policy(&self.picker_bindings.pipe_policy);
        Ok(self.sibling(pane, window_id))
    }

    /// A view for a new window whose first pane is one moved out of this
    /// window, with its process and UI state intact.
    pub(crate) fn adopt_window(
        &self,
        mut pane: MovedPane,
        window: crate::graphics::PaneWindow,
    ) -> Self {
        pane.0.move_to_window(window.id);
        self.sibling(pane.0, window.id)
    }

    /// A view for another window around `pane`, sharing this window's settings.
    fn sibling(&self, pane: TerminalPane, window_id: egui_winit::winit::window::WindowId) -> Self {
        let issues = pane.take_config_issues();
        Self::with_pane(
            pane,
            &self.context,
            self.proxy.clone(),
            window_id,
            issues,
            self.command_label.clone(),
            self.reload_source.clone(),
            self.picker_bindings.clone(),
            self.floating_padding,
        )
    }

    /// A one-workspace view whose only pane is `pane`, tiled pane 0.
    fn with_pane(
        pane: TerminalPane,
        context: &egui::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        window_id: egui_winit::winit::window::WindowId,
        issues: Vec<String>,
        command_label: String,
        reload_source: crate::config::ReloadSource,
        picker_bindings: std::rc::Rc<crate::picker::config::Bindings>,
        floating_padding: Option<crate::config::FloatingPadding>,
    ) -> Self {
        Self {
            messages: crate::messages::Messages::new(issues),
            toasts: Default::default(),
            context: context.clone(),
            tiled: vec![(0, pane)],
            closed_panes: Vec::new(),
            workspaces: vec![{
                let mut w = workspace::Workspace::new(1, 1);
                w.layout = picker_bindings.default_layout;
                w.panes.push(0);
                w.focused = Some(0);
                w
            }],
            current: 1,
            previous: 1,
            next_workspace: 2,
            next_pane: 1,
            command_label,
            reload_source,
            close_requested: false,
            picker_bindings,
            floating_padding,
            floating: Default::default(),
            requests: Vec::new(),
            requests_posted: false,
            proxy,
            window_id,
        }
    }

    /// Creates a tiled or floating pane's terminal in the window's app, with
    /// the configured pipe policy applied.
    fn create_pane(
        &self,
        gl: &glow::Context,
        options: &pane::SpawnOptions,
    ) -> Result<TerminalPane, String> {
        let pane = self
            .app_pane()
            .ok_or("no terminal is open")?
            .create_child(gl, options)
            .map_err(|e| e.to_string())?;
        #[cfg(any(windows, unix))]
        pane.set_pipe_policy(&self.picker_bindings.pipe_policy);
        Ok(pane)
    }

    /// Requests made since the last call, in order, for the app to carry out.
    pub(crate) fn take_requests(&mut self) -> Vec<WindowRequest> {
        self.requests_posted = false;
        std::mem::take(&mut self.requests)
    }

    /// Asks the app, by a posted event, to handle this window's requests, and
    /// to close the window once it has no panes left. Called after a frame or
    /// a Lua callback; the app handles the event after the current one, so
    /// never in the middle of a frame.
    fn post_requests(&mut self) {
        if self.requests_posted || (self.requests.is_empty() && !self.should_close()) {
            return;
        }
        self.requests_posted = self
            .proxy
            .send_event(UserEvent::WindowRequests(self.window_id))
            .is_ok();
    }

    pub(crate) fn window_error(&mut self, error: String) {
        self.show_error("Could not open terminal window", error);
    }

    /// Drains the shared native app's mailbox. False if this window has no
    /// terminal left to do it through.
    pub(crate) fn tick(&mut self) -> bool {
        self.app_pane().map(|pane| pane.tick()).is_some()
    }

    pub(crate) fn acquire_frames(&mut self) -> bool {
        let mut acquired = false;
        for (_, pane) in &mut self.tiled {
            acquired |= pane.acquire_frame();
        }
        for floating in &mut self.floating.entries {
            acquired |= floating.acquire_frame();
        }
        acquired
    }

    /// The terminal that gets input: the visible float, else the focused
    /// tiled pane.
    fn active_pane(&self) -> Option<PaneRef> {
        self.floats()
            .visible()
            .map(PaneRef::Floating)
            .or_else(|| self.focused_id().map(PaneRef::Tiled))
    }
    fn pane_mut(&mut self, pane: PaneRef) -> Option<&mut TerminalPane> {
        match pane {
            PaneRef::Tiled(id) => self.tiled_mut(id),
            PaneRef::Floating(id) => self
                .floating
                .entries
                .iter_mut()
                .find(|p| p.id == id)
                .map(|p| &mut p.value.pane),
        }
    }
    fn run_picker_callback(&mut self, origin: PaneRef, gl: &glow::Context, ctx: &egui::Context) {
        let Some(pane) = self.pane_mut(origin) else {
            return;
        };
        let Some((action, generation, from_copy)) = pane.pending_callback.take() else {
            return;
        };
        let selection = pane.selected_text();
        if let crate::picker::Action::Custom(callback, text, filesystem, range, value, params) =
            action
        {
            let result = callback.with_host(&mut |request| self.host_request(request, gl), || {
                callback.call_with_selection(
                    &text, &value, filesystem, generation, range, &params, selection,
                )
            });
            let bindings = self.picker_bindings.clone();
            let palette = self.palette_context();
            if let Some(pane) = self.pane_mut(origin) {
                pane.finish_picker_callback(
                    result, generation, from_copy, ctx, &bindings, &palette,
                );
            }
        }
    }
    fn floating_request(
        &mut self,
        request: floating::Request,
        gl: &glow::Context,
    ) -> Result<serde_json::Value, String> {
        use floating::{CreateOptions, Request};
        // Targets resolve among the current workspace's floats; no target
        // means its most recently focused float.
        let target_id =
            |view: &Self, target: Option<floating::Target>| -> Result<Option<u64>, String> {
                match target {
                    Some(target) => view
                        .floating
                        .resolve(&target, view.floats())
                        .map(Some)
                        .ok_or_else(|| "floating pane does not exist".into()),
                    None => Ok(view.floats().last()),
                }
            };
        let describe = |view: &Self, workspace: u64, id: Option<u64>| {
            let floats = &view.workspaces.iter().find(|w| w.id == workspace)?.floats;
            view.floating
                .description(id?, workspace, floats, workspace == view.current)
        };
        let result = match request {
            Request::Create(options) => {
                self.floating.validate_create(&options, self.floats())?;
                let pane = self.create_pane(gl, &options.spawn())?;
                let id = self
                    .floating
                    .insert(options.name, FloatingTerminal { pane });
                self.floats_mut().add(id);
                serde_json::to_value(describe(self, self.current, Some(id))).unwrap()
            }
            Request::Toggle => {
                if self.floats().visible().is_some() {
                    self.floats_mut().hide();
                } else if let Some(id) = self.floats().last() {
                    self.floats_mut().show(id);
                } else {
                    return self.floating_request(Request::Create(CreateOptions::default()), gl);
                }
                serde_json::Value::Null
            }
            Request::Restore(target) => {
                let id = target_id(self, Some(target))?.unwrap();
                self.floats_mut().show(id);
                serde_json::to_value(describe(self, self.current, Some(id))).unwrap()
            }
            Request::Get { target, workspace } => {
                let workspace = workspace.unwrap_or(self.current);
                let floats = &self
                    .workspaces
                    .iter()
                    .find(|w| w.id == workspace)
                    .ok_or("workspace does not exist")?
                    .floats;
                let id = match target {
                    Some(target) => self.floating.resolve(&target, floats),
                    None => floats.last(),
                };
                serde_json::to_value(describe(self, workspace, id)).unwrap()
            }
            Request::List { workspace } => {
                let workspace = workspace.unwrap_or(self.current);
                let w = self
                    .workspaces
                    .iter()
                    .find(|w| w.id == workspace)
                    .ok_or("workspace does not exist")?;
                serde_json::to_value(self.floating.descriptions(
                    workspace,
                    &w.floats,
                    workspace == self.current,
                ))
                .unwrap()
            }
            Request::Close(target) => {
                if let Some(id) = target_id(self, target)? {
                    self.close_floats([id]);
                }
                serde_json::Value::Null
            }
            Request::Float(pane) => {
                let (workspace, id) = self.float_tiled(pane)?;
                serde_json::to_value(describe(self, workspace, Some(id))).unwrap()
            }
            Request::Tile(target) => {
                let id = target_id(self, target)?.ok_or("floating pane does not exist")?;
                let pane = self.tile_float(id)?;
                serde_json::json!({"id": pane, "workspace": self.current})
            }
        };
        self.context.request_repaint();
        Ok(result)
    }
    fn execute_action(
        &mut self,
        action: AppAction,
        floating_target: bool,
        ctx: &egui::Context,
        gl: &glow::Context,
    ) {
        match action {
            AppAction::Workspace(action) => self.workspace_action(action, ctx, gl),
            AppAction::SelectWorkspaceId(id) => {
                if let Err(error) = self.select_workspace(id) {
                    self.show_error("Workspace action failed", error);
                }
            }
            AppAction::MovePaneToWorkspaceId { pane, workspace } => {
                if let Err(error) = self.move_pane_to_workspace(pane, workspace, gl) {
                    self.show_error("Workspace action failed", error);
                }
            }
            AppAction::ReloadConfig => match self.reload_source.reload() {
                Ok(config) => {
                    if config.theme.is_some()
                        || config.font_family.is_some()
                        || config.font_size.is_some()
                    {
                        let applied = match self.app_pane() {
                            Some(pane) => pane.apply_settings(&config),
                            None => Err("no terminal is open".into()),
                        };
                        if let Err(error) = applied {
                            self.show_error("Could not reload configuration", error);
                            return;
                        }
                    }
                    if config.font_family.is_some() || config.font_size.is_some() {
                        for (_, pane) in &mut self.tiled {
                            pane.refresh_picker_font(ctx);
                        }
                        for floating in &mut self.floating.entries {
                            floating.refresh_picker_font(ctx);
                        }
                    }
                    self.picker_bindings = std::rc::Rc::new(config.bindings);
                    #[cfg(any(windows, unix))]
                    {
                        let policy = &self.picker_bindings.pipe_policy;
                        for (_, pane) in &self.tiled {
                            pane.set_pipe_policy(policy);
                        }
                        for floating in &self.floating.entries {
                            floating.set_pipe_policy(policy);
                        }
                    }
                    self.floating_padding = config.padding;
                    crate::colors::COLORS.reload(ctx, config.colors);
                    self.show_notice("", "Configuration reloaded".into());
                }
                Err(error) => self.show_error("Could not reload configuration", error),
            },
            AppAction::OpenThemePicker => {
                let entry = self
                    .picker_bindings
                    .named("builtin.themes")
                    .expect("builtin themes picker");
                let palette = self.palette_context();
                let bindings = self.picker_bindings.clone();
                let pane = if self.floats().visible().is_some() {
                    &mut self
                        .visible_float_mut()
                        .expect("floating action target")
                        .pane
                } else {
                    let Some(id) = self.focused_id() else {
                        return;
                    };
                    let Some(pane) = self.tiled_mut(id) else {
                        return;
                    };
                    pane
                };
                pane.open_picker(ctx, entry, None, &bindings, &palette);
            }
            AppAction::ApplyTheme(name) => match self.apply_theme(&name) {
                Ok(()) => self.show_notice("Theme applied", name),
                Err(error) => self.show_error("Could not apply theme", error),
            },
            AppAction::ShowMessages | AppAction::OpenErrorLog => self.show_messages(ctx),
            AppAction::NewWindow => {
                self.requests.push(WindowRequest::NewWindow);
            }
            AppAction::RestoreFloatingPane(id) => {
                if self.floats().contains(id) {
                    self.floats_mut().show(id);
                } else {
                    self.show_notice(
                        "Floating pane closed",
                        format!("Pane {id} is no longer available"),
                    );
                }
            }
            AppAction::CreateFloatingPane | AppAction::ToggleFloatingPane => {
                let request = if action == AppAction::CreateFloatingPane {
                    floating::Request::Create(Default::default())
                } else {
                    floating::Request::Toggle
                };
                if let Err(error) = self.floating_request(request, gl) {
                    self.show_error("Floating pane action failed", error);
                }
            }
            _ => {
                let lua_action = if let AppAction::Lua(name) = &action {
                    self.picker_bindings.lua_action(name)
                } else {
                    None
                };
                let entry = if let AppAction::OpenPicker(name) = &action {
                    self.picker_bindings.named(name)
                } else {
                    None
                };
                let quick_select = if let AppAction::OpenQuickSelect(name) = &action {
                    Some(self.picker_bindings.quick_select(name.as_deref()))
                } else {
                    None
                };
                let origin = if floating_target {
                    self.active_pane()
                } else {
                    self.focused_id().map(PaneRef::Tiled)
                };
                if let Some(lua_action) = lua_action {
                    let selection = origin
                        .and_then(|o| self.pane_mut(o))
                        .map(|p| p.selected_text())
                        .unwrap_or_default();
                    let callback = lua_action.callback.clone();
                    let result = callback
                        .with_host(&mut |request| self.host_request(request, gl), || {
                            callback.call_action(selection)
                        });
                    if let Some(pane) = origin.and_then(|o| self.pane_mut(o)) {
                        pane.finish_lua_action(result, ctx);
                    }
                } else {
                    let bindings = self.picker_bindings.clone();
                    let palette = if entry.is_some() {
                        self.palette_context()
                    } else {
                        PaletteContext::default()
                    };
                    if let Some(pane) = origin.and_then(|o| self.pane_mut(o)) {
                        if let Some(entry) = entry {
                            pane.open_picker(ctx, entry, None, &bindings, &palette);
                        } else if let Some(definition) = quick_select {
                            pane.toggle_quick_select(definition);
                        } else {
                            pane.execute_action(&action, ctx);
                        }
                    }
                }
            }
        }
        ctx.request_repaint();
    }

    fn floating_rect(
        content: egui::Rect,
        padding: Option<crate::config::FloatingPadding>,
    ) -> egui::Rect {
        if let Some(padding) = padding {
            return egui::Rect::from_min_max(
                egui::pos2(
                    content.left() + content.width() * padding.left / 100.0,
                    content.top() + content.height() * padding.top / 100.0,
                ),
                egui::pos2(
                    content.right() - content.width() * padding.right / 100.0,
                    content.bottom() - content.height() * padding.bottom / 100.0,
                ),
            );
        }
        let size = egui::vec2(
            (content.width() * 0.8).min(1000.0),
            (content.height() * 0.7).min(700.0),
        );
        egui::Rect::from_center_size(content.center(), size)
    }

    fn show_floating(
        &self,
        ui: &egui::Ui,
        content: egui::Rect,
        rect: egui::Rect,
        style: &workspace::Appearance,
    ) {
        let Some(floating) = self.visible_float() else {
            return;
        };
        let layer_id = egui::LayerId::new(egui::Order::Middle, egui::Id::new("floating-terminal"));
        egui::Area::new(layer_id.id)
            .order(layer_id.order)
            .fade_in(false)
            .fixed_pos(content.min)
            .interactable(false)
            .show(ui.ctx(), |floating_ui| {
                floating_ui.set_min_size(content.size());
                let painter = floating_ui.painter();
                painter.rect_filled(
                    content,
                    0.0,
                    crate::colors::palette()
                        .background
                        .gamma_multiply(90.0 / 255.0),
                );
                style.paint_frame(painter, rect, true);
                painter.rect_filled(
                    rect,
                    style.corner_radius,
                    crate::colors::palette().background,
                );
                painter.add(egui::PaintCallback {
                    rect: style.content_rect(rect),
                    callback: floating.paint_callback(),
                });
            });
        ui.ctx().move_to_top(layer_id);
    }
    pub(crate) fn update(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        self.update_workspaces(ui, gl);
        self.post_requests();
    }

    pub(crate) fn should_close(&self) -> bool {
        self.close_requested || self.workspaces.iter().all(|w| w.panes.is_empty())
    }

    /// Opens the messages picker in the visible float, or else the focused pane.
    fn show_messages(&mut self, ctx: &egui::Context) {
        let issues = self.messages.config_issues().to_vec();
        if issues.is_empty() && crate::error_log::snapshot().is_empty() {
            self.show_notice("Messages", "No messages.".into());
            return;
        }
        if let Some(floating) = self.visible_float_mut() {
            floating.open_messages(ctx, &issues);
        } else if let Some(p) = self.focused_id().and_then(|id| self.tiled_mut(id)) {
            p.open_messages(ctx, &issues);
        }
    }

    fn show_mode_status(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        // Paint the background before the clickable status indicator.
        ui.painter()
            .rect_filled(rect, 0.0, crate::colors::palette().panel_background);
        // Empty bar space moves the window; registered first so the bar's own
        // controls, registered later, take their clicks. Not focusable: Tab
        // would focus it and egui would then keep keys from the terminal.
        let drag = ui.interact(
            rect,
            ui.id().with("status-drag"),
            egui::Sense::CLICK | egui::Sense::DRAG,
        );
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        let (messages_left, messages_clicked) = self.messages.status(ui, rect);
        if messages_clicked {
            self.show_messages(ui.ctx());
        }
        let mut right_edge = messages_left;
        let status = self.active_pane_ref().status(&self.command_label);
        let foreground = crate::colors::palette().text;
        let painter = ui.painter().with_clip_rect(rect);
        let separator = status.separator;
        painter.line_segment(
            [
                rect.left_top() + egui::vec2(0.0, 0.75),
                rect.right_top() + egui::vec2(0.0, 0.75),
            ],
            egui::Stroke::new(1.5, separator),
        );
        let font = egui::FontId::monospace(13.0);
        let palette = crate::colors::palette();
        // The label is a chip: filled with the accent while a mode such as
        // COPY or SEARCH is active, otherwise a subtle tint.
        let mode_active = status.accent != palette.muted_text;
        let (chip_fill, chip_text) = if mode_active {
            (status.accent, palette.background)
        } else {
            (egui::Color32::from_white_alpha(14), palette.text)
        };
        let chip_height = (rect.height() - 8.0).clamp(8.0, 22.0);
        let chip_top = rect.center().y + 1.0 - chip_height / 2.0;
        // A menu button at the bar's left end.
        let menu = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 8.0, chip_top),
            egui::vec2(chip_height, chip_height),
        );
        // CLICK without FOCUSABLE keeps Tab with the terminal.
        let menu_response = ui.interact(menu, ui.id().with("bar-menu"), egui::Sense::CLICK);
        let menu_open =
            egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&menu_response));
        let menu_color = if menu_response.hovered() || menu_open {
            painter.rect_filled(menu, 4.0, egui::Color32::from_white_alpha(32));
            palette.text
        } else {
            palette.muted_text
        };
        let stroke = egui::Stroke::new(1.5, menu_color);
        for dy in [-4.0, 0.0, 4.0] {
            let y = menu.center().y + dy;
            painter.line_segment(
                [
                    egui::pos2(menu.center().x - 5.0, y),
                    egui::pos2(menu.center().x + 5.0, y),
                ],
                stroke,
            );
        }
        let mut close = false;
        egui::Popup::menu(&menu_response).show(|ui| {
            if ui.button("Close window").clicked() {
                close = true;
                ui.close();
            }
        });
        let label = painter.layout_no_wrap(status.label.to_owned(), font.clone(), chip_text);
        let label_rect = egui::Rect::from_min_size(
            egui::pos2(menu.right() + 6.0, chip_top),
            egui::vec2(label.size().x + 16.0, chip_height),
        );
        painter.rect_filled(label_rect, 4.0, chip_fill);
        painter.galley(label_rect.center() - label.size() / 2.0, label, chip_text);
        // The floating panes chip has a fixed width, even at 0 or while a float
        // is shown, so the controls to its left keep their place. A visible
        // float fills it; its name is in the tooltip.
        {
            let count = self.floats().ids().len();
            let visible = self
                .visible_float()
                .map(|p| p.name.clone().unwrap_or_else(|| p.id.to_string()));
            let (fill, content) = match (&visible, count) {
                (Some(_), _) => (palette.warning, palette.background),
                (None, 0) => (egui::Color32::from_white_alpha(14), palette.muted_text),
                (None, _) => (egui::Color32::from_white_alpha(14), palette.text),
            };
            const ICON: f32 = 11.0;
            // Room for two digits keeps the width constant up to 99 floats.
            let digits = painter
                .layout_no_wrap("99".to_owned(), font.clone(), content)
                .size()
                .x;
            let chip = egui::Rect::from_min_max(
                egui::pos2(
                    right_edge - (8.0 + ICON + 6.0 + digits + 8.0),
                    label_rect.top(),
                ),
                egui::pos2(right_edge, label_rect.bottom()),
            );
            painter.rect_filled(chip, 4.0, fill);
            // Two overlapping windows: a pane floating over another.
            let stroke = egui::Stroke::new(1.0, content);
            let icon = egui::Rect::from_min_size(
                egui::pos2(chip.left() + 8.0, chip.center().y - ICON / 2.0),
                egui::vec2(ICON, ICON),
            );
            let back = egui::Rect::from_min_size(icon.min, egui::vec2(7.0, 7.0));
            let front = egui::Rect::from_min_size(icon.min + egui::vec2(4.0, 4.0), back.size());
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.left_top(), back.left_bottom()], stroke);
            painter.line_segment(
                [back.right_top(), egui::pos2(back.right(), front.top())],
                stroke,
            );
            painter.line_segment(
                [back.left_bottom(), egui::pos2(front.left(), back.bottom())],
                stroke,
            );
            painter.rect_stroke(front, 0.0, stroke, egui::StrokeKind::Inside);
            let text = painter.layout_no_wrap(count.to_string(), font.clone(), content);
            painter.galley(
                egui::pos2(
                    chip.right() - 8.0 - text.size().x,
                    chip.center().y - text.size().y / 2.0,
                ),
                text,
                content,
            );
            ui.interact(chip, ui.id().with("floating_status"), egui::Sense::hover())
                .on_hover_text(match visible {
                    Some(name) => format!("Floating panes: {count}\nShowing: {name}"),
                    None => format!("Floating panes: {count}"),
                });
            right_edge = chip.left() - 12.0;
        }
        if let Some(connected) = status.pipe_status {
            let palette = crate::colors::palette();
            let color = if connected {
                palette.accent
            } else {
                palette.muted_text
            };
            let left = right_edge - 26.0;
            if left > label_rect.right() + 8.0 {
                let icon = egui::Rect::from_center_size(
                    egui::pos2(left + 13.0, rect.center().y + 1.0),
                    egui::vec2(26.0, 18.0),
                );
                let point = |x, y| icon.left_top() + egui::vec2(x, y);
                let stroke = egui::Stroke::new(1.5, color);
                // Two opposing arrows indicate bidirectional picker IPC.
                painter.line_segment([point(2.0, 6.0), point(24.0, 6.0)], stroke);
                painter.add(egui::Shape::line(
                    vec![point(20.0, 3.0), point(24.0, 6.0), point(20.0, 9.0)],
                    stroke,
                ));
                painter.line_segment([point(2.0, 12.0), point(24.0, 12.0)], stroke);
                painter.add(egui::Shape::line(
                    vec![point(6.0, 9.0), point(2.0, 12.0), point(6.0, 15.0)],
                    stroke,
                ));
                ui.interact(icon, ui.id().with("pipe_status"), egui::Sense::hover())
                    .on_hover_text(if connected {
                        "Picker pipe: client connected"
                    } else {
                        "Picker pipe: no client connected"
                    });
                right_edge = left - 12.0;
            }
        }
        // Reserve workspace controls immediately to the left of the pipe icon.
        let workspace_width =
            self.workspace_bar_width(ui, (right_edge - label_rect.right() - 24.0).max(0.0));
        let workspace_rect = egui::Rect::from_min_max(
            egui::pos2(right_edge - workspace_width, rect.top()),
            egui::pos2(right_edge, rect.bottom()),
        );
        right_edge = workspace_rect.left() - 12.0;
        // While searching, the query field takes the detail text's place.
        let searching = self.active_pane_ref().search_active();
        if let Some(detail) = status.detail.filter(|_| !searching) {
            let text = painter.layout_no_wrap(detail, font.clone(), foreground);
            let left = right_edge - text.size().x;
            if left > label_rect.right() + 8.0 {
                painter.galley(
                    egui::pos2(left, rect.center().y + 1.0 - text.size().y / 2.0),
                    text,
                    foreground,
                );
            }
        }
        if searching {
            let search_rect = egui::Rect::from_min_max(
                egui::pos2(label_rect.right() + 8.0, rect.top()),
                egui::pos2(right_edge, rect.bottom()),
            );
            if let Some(p) = self.active_pane().and_then(|p| self.pane_mut(p)) {
                p.show_search_bar(ui.ctx(), search_rect);
            }
        }
        self.workspace_bar(ui, workspace_rect);
        // Applied once `status` no longer borrows the active pane.
        self.close_requested |= close;
    }

    pub(crate) fn destroy(&mut self) {
        for mut floating in self.floating.entries.drain(..) {
            floating.destroy();
        }
        for mut pane in self.closed_panes.drain(..) {
            pane.destroy();
        }
        for request in self.requests.drain(..) {
            if let WindowRequest::MovePaneToNewWindow(mut pane) = request {
                pane.0.destroy();
            }
        }
        for (_, mut pane) in self.tiled.drain(..) {
            pane.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn floating_padding_positions_all_four_edges() {
        let content = egui::Rect::from_min_size(egui::pos2(20.0, 40.0), egui::vec2(1000.0, 600.0));
        let rect = super::TerminalView::floating_rect(
            content,
            Some(crate::config::FloatingPadding {
                top: 10.0,
                bottom: 20.0,
                left: 5.0,
                right: 15.0,
            }),
        );
        assert_eq!(rect.min, egui::pos2(70.0, 100.0));
        assert_eq!(rect.max, egui::pos2(870.0, 520.0));
    }
}

use crate::actions::AppAction;
use crate::app::UserEvent;
use crate::graphics as glow;
use ghostty_app::GhosttyApp;

mod empty;
pub(crate) mod floating;
pub(crate) mod ghostty_app;
mod layout;
mod miniature;
mod pane;
pub(crate) use pane::close_from_session as close_pane_from_session;
pub(crate) use pane::wake_from_session as wake_pane_from_session;
mod pane_resize;
mod resize_keys;
mod restore;
pub(crate) mod workspace;
pub(crate) mod workspace_ui;
use pane::TerminalPane;

/// A pane made for a client of a pane's pipe, until its command is seen to
/// start or to have failed to.
#[cfg(any(windows, unix))]
struct Starting {
    pane: u64,
    /// What was asked to run, for saying what could not be.
    command: Option<String>,
    session: std::sync::Arc<crate::picker::pipe::Session>,
    since: std::time::Instant,
}

pub(crate) struct TerminalView {
    /// Tiled terminals across all workspaces, by tiled ID. Which workspace
    /// each belongs to is in `Workspace::panes`.
    tiled: Vec<(u64, TerminalPane)>,
    closed_panes: Vec<TerminalPane>,
    /// Panes asked for over a pane's pipe whose commands have yet to be seen
    /// to start, and so whose askers have yet to be answered.
    #[cfg(any(windows, unix))]
    pipe_panes_starting: Vec<Starting>,
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
    floating: floating::Panes<workspace::Group>,
    /// Work for the app to do after this window's frame; see `post_requests`.
    requests: Vec<WindowRequest>,
    /// Whether a `UserEvent::WindowRequests` is on its way to the app.
    requests_posted: bool,
    proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
    window: crate::graphics::PaneWindow,
    /// The ghostty application, which every window shares.
    app: std::rc::Rc<GhosttyApp>,
    /// Set by the status bar's close button; the app then closes this window.
    close_requested: bool,
    /// A workspace was closed by its command this frame: a window that
    /// leaves empty stays, to ask what to show next.
    stay_open: bool,
    /// The waiting session whose row the picker has selected, and the
    /// panes that look at it.
    peek: Option<miniature::Peek>,
    pane_drag: Option<pane_resize::Drag>,
    /// The `key_tables` entry whose keys are active, if any.
    key_table: Option<String>,
    /// The resize or move key pressed this frame was held with the key
    /// table's `nudge` modifier, for a small step.
    nudge_step: bool,
    /// Why the window has no terminal, while it has none and is to stay.
    empty: Option<empty::Empty>,
    /// The picker of workspaces and sessions, while it is open. It is the
    /// window's and not a pane's: it is about no pane, and a window with no
    /// terminal shows it too.
    window_picker: Option<Box<crate::picker::Picker>>,
    /// What the rows `window_picker` was last given are of; see
    /// `workspace_picker_rows`.
    window_picker_rows: String,
    /// The folders the window's picker lists after its rows, and the search
    /// for them that a daemon does, once one has been asked.
    window_folders: Option<(std::sync::Arc<crate::picker::FolderRows>, Option<std::sync::Arc<crate::session::Folders>>)>,
    /// What that picker was last told to say, and when: new rows take it
    /// away, and it is said again while it would still have been showing.
    picker_said: Option<(String, std::time::Instant)>,
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

/// A terminal in this window: a tiled pane by its id, or the pane a float
/// has focus on by the float's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneRef {
    Tiled(u64),
    Floating(u64),
}

impl TerminalView {
    pub(crate) fn apply_theme(&mut self, name: &str) -> Result<(), String> {
        self.app.apply_theme(name)?;
        self.context.request_repaint();
        Ok(())
    }
    /// The visible float, else the focused tiled pane. Only called while the
    /// window has a tiled pane.
    fn active_pane_ref(&self) -> &TerminalPane {
        self.active_pane_opt().expect("an open window has a tiled pane")
    }
    /// The visible float, else the focused tiled pane, if the window has
    /// a terminal at all.
    fn active_pane_opt(&self) -> Option<&TerminalPane> {
        if let Some(pane) = self.visible_float_pane() {
            Some(pane)
        } else {
            self.focused_id()
                .and_then(|id| self.tiled_ref(id))
                .or_else(|| self.tiled.first().map(|(_, p)| p))
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
        app: std::rc::Rc<GhosttyApp>,
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        window: crate::graphics::PaneWindow,
        command_label: String,
        picker_bindings: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        mut issues: Vec<String>,
        reload_source: crate::config::ReloadSource,
    ) -> Result<Self, crate::native_error::FatalError> {
        issues.extend(app.take_config_issues());
        // With shells kept in the session daemon the window starts with no
        // terminal: what it shows first depends on what the daemon holds,
        // and the daemon is reached while the window is already up.
        let pane = if crate::session::status(&crate::session::home()) == crate::session::Status::Off {
            let pane = TerminalPane::create(
                &app,
                egui_ctx,
                gl,
                proxy.clone(),
                window,
                &pane::SpawnOptions::default(),
            )?;
            #[cfg(any(windows, unix))]
            pane.set_pipe_policy(&picker_bindings.pipe_policy);
            Some(pane)
        } else {
            crate::session::connect(&crate::session::home());
            None
        };
        let mut view = Self::with_pane(
            app,
            pane,
            egui_ctx,
            proxy,
            window,
            issues,
            command_label,
            reload_source,
            std::rc::Rc::new(picker_bindings),
            floating_padding,
        );
        if view.tiled.is_empty() {
            view.empty = Some(empty::Empty::Connecting);
        }
        Ok(view)
    }

    pub(crate) fn new_window(
        &self,
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        window: crate::graphics::PaneWindow,
    ) -> Result<Self, crate::native_error::FatalError> {
        let pane = TerminalPane::create(
            &self.app,
            egui_ctx,
            gl,
            self.proxy.clone(),
            window,
            &pane::SpawnOptions::default(),
        )?;
        #[cfg(any(windows, unix))]
        pane.set_pipe_policy(&self.picker_bindings.pipe_policy);
        Ok(self.sibling(Some(pane), window))
    }

    /// A view for a new window whose first pane is one moved out of this
    /// window, with its process and UI state intact.
    pub(crate) fn adopt_window(
        &self,
        mut pane: MovedPane,
        window: crate::graphics::PaneWindow,
    ) -> Self {
        pane.0.move_to_window(window.id);
        self.sibling(Some(pane.0), window)
    }

    /// A view for another window around `pane`, sharing this window's settings.
    fn sibling(&self, pane: Option<TerminalPane>, window: crate::graphics::PaneWindow) -> Self {
        Self::with_pane(
            self.app.clone(),
            pane,
            &self.context,
            self.proxy.clone(),
            window,
            Vec::new(),
            self.command_label.clone(),
            self.reload_source.clone(),
            self.picker_bindings.clone(),
            self.floating_padding,
        )
    }

    /// A one-workspace view whose only pane is `pane`, tiled pane 0, or which
    /// has none yet.
    fn with_pane(
        app: std::rc::Rc<GhosttyApp>,
        pane: Option<TerminalPane>,
        context: &egui::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        window: crate::graphics::PaneWindow,
        issues: Vec<String>,
        command_label: String,
        reload_source: crate::config::ReloadSource,
        picker_bindings: std::rc::Rc<crate::picker::config::Bindings>,
        floating_padding: Option<crate::config::FloatingPadding>,
    ) -> Self {
        let first = pane.is_some();
        // A pane moved here brings its place; a window with none starts in
        // the place it was started for.
        let place = pane.as_ref().map_or_else(crate::session::home, |pane| pane.place());
        Self {
            messages: crate::messages::Messages::new(issues),
            toasts: Default::default(),
            context: context.clone(),
            tiled: pane.into_iter().map(|pane| (0, pane)).collect(),
            closed_panes: Vec::new(),
            #[cfg(any(windows, unix))]
            pipe_panes_starting: Vec::new(),
            workspaces: vec![{
                let mut w = workspace::Workspace::new(1, 1);
                w.layout = picker_bindings.default_layout;
                w.place = place;
                if first {
                    w.panes.push(0);
                    w.focused = Some(0);
                }
                w
            }],
            current: 1,
            previous: 1,
            next_workspace: 2,
            next_pane: 1,
            command_label,
            reload_source,
            close_requested: false,
            stay_open: false,
            peek: None,
            pane_drag: None,
            key_table: None,
            nudge_step: false,
            empty: None,
            window_picker: None,
            window_picker_rows: String::new(),
            window_folders: None,
            picker_said: None,
            picker_bindings,
            floating_padding,
            floating: Default::default(),
            requests: Vec::new(),
            requests_posted: false,
            proxy,
            window,
            app,
        }
    }

    /// Creates a tiled or floating pane's terminal in the window's app, with
    /// the configured pipe policy applied.
    fn create_pane(
        &self,
        gl: &glow::Context,
        options: &pane::SpawnOptions,
    ) -> Result<TerminalPane, String> {
        // One that names no place is the current workspace's.
        let mut options = options.clone();
        options.place.get_or_insert_with(|| self.current_place());
        let pane = TerminalPane::create(
            &self.app,
            &self.context,
            gl,
            self.proxy.clone(),
            self.window,
            &options,
        )
        .map_err(|e| e.to_string())?;
        #[cfg(any(windows, unix))]
        pane.set_pipe_policy(&self.picker_bindings.pipe_policy);
        Ok(pane)
    }

    /// Where the current workspace's shells run, which is where a new pane
    /// of it is made.
    pub(super) fn current_place(&self) -> crate::session::Place {
        self.place_of(self.current)
    }

    /// Where a workspace's shells run.
    pub(super) fn place_of(&self, workspace: u64) -> crate::session::Place {
        let found = self.workspaces.iter().find(|w| w.id == workspace);
        found.map_or_else(crate::session::home, |w| w.place.clone())
    }

    /// `spawn` for a new shell, which starts where the active pane's shell
    /// is unless it names a directory. Ghostty hands a new terminal the
    /// focused one's directory itself, but only once it has seen the shell
    /// report it: a pane taken from the session daemon has not until its
    /// next prompt, and the daemon would start the shell in its own
    /// directory.
    fn beside_active(&self, mut spawn: pane::SpawnOptions) -> pane::SpawnOptions {
        // Only on this machine, and from a pane of it: another machine's
        // directory is not one of this one's.
        let here = crate::session::Place::Here;
        let local = self.current_place() == here && spawn.place.as_ref().is_none_or(|p| *p == here);
        if spawn.cwd.is_none() && local {
            let active = self
                .visible_float_pane()
                .or_else(|| self.focused_id().and_then(|id| self.tiled_ref(id)));
            spawn.cwd = active
                .and_then(|pane| pane.pwd())
                .filter(|pwd| std::path::Path::new(pwd).is_dir());
        }
        spawn
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
            .send_event(UserEvent::WindowRequests(self.window.id))
            .is_ok();
    }

    pub(crate) fn window_error(&mut self, error: String) {
        self.show_error("Could not open terminal window", error);
    }

    /// Shows failures of the ghostty application's own work, which belong
    /// to no terminal, as (operation, cause).
    pub(crate) fn show_app_issues(&mut self, issues: Vec<(String, String)>) {
        for (operation, cause) in issues {
            self.toasts.push_error(
                "Terminal operation failed".into(),
                format!("{operation}: {cause}"),
                10000,
            );
        }
    }

    pub(crate) fn acquire_frames(&mut self) -> bool {
        let mut acquired = false;
        for (_, pane) in &mut self.tiled {
            acquired |= pane.acquire_frame();
        }
        acquired |= self.acquire_peek_frames();
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
    /// The id of the terminal a reference is to.
    fn pane_id(&self, pane: PaneRef) -> Option<u64> {
        match pane {
            PaneRef::Tiled(id) => Some(id),
            PaneRef::Floating(id) => self.float_pane_id(id),
        }
    }
    fn pane_mut(&mut self, pane: PaneRef) -> Option<&mut TerminalPane> {
        let id = self.pane_id(pane)?;
        self.tiled_mut(id)
    }
    fn run_picker_callback(&mut self, origin: PaneRef, gl: &glow::Context, ctx: &egui::Context) {
        let Some(pane) = self.pane_mut(origin) else {
            return;
        };
        let Some(pane::PendingCallback {
            action,
            generation,
            from_copy,
        }) = pane.pending_callback.take()
        else {
            return;
        };
        let selection = pane.selected_text();
        let result =
            action
                .callback
                .with_host(&mut |request| self.host_request(request, gl), || {
                    action.callback.call_with_selection(
                        &action.text,
                        &action.value,
                        action.filesystem,
                        generation,
                        action.range,
                        &action.params,
                        selection,
                    )
                });
        let bindings = self.picker_bindings.clone();
        let palette = self.palette_context();
        if let Some(pane) = self.pane_mut(origin) {
            pane.finish_picker_callback(
                result,
                generation,
                from_copy,
                action.accepted,
                ctx,
                &bindings,
                &palette,
            );
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
            |view: &Self, target: Option<u64>| -> Result<Option<u64>, String> {
                match target {
                    Some(target) => view
                        .floating
                        .resolve(target, view.floats())
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
                self.floating.validate_create(&options)?;
                let pane = self.create_pane(gl, &self.beside_active(options.spawn()))?;
                let id = self.add_float(pane);
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
                    Some(target) => self.floating.resolve(target, floats),
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
            AppAction::EnterKeyTable(name) => self.key_table = Some(name),
            AppAction::ExitKeyTable => self.key_table = None,
            AppAction::SelectWorkspaceId(id) => {
                if let Err(error) = self.select_workspace(id) {
                    self.show_error("Workspace action failed", error);
                }
            }
            AppAction::RenameWorkspace { id, name } => self.rename_workspace(id, &name),
            AppAction::CreateWorkspaceIn(cwd) => {
                if let Err(error) = self.create_workspace_in(cwd, gl) {
                    self.show_error("Workspace action failed", error);
                }
            }
            AppAction::RenameWorkspacePrompt(id) => self.rename_workspace_prompt(id, ctx),
            AppAction::DetachWorkspaceId(id) => {
                if let Err(error) = self.detach_workspace(id) {
                    self.show_error("Workspace action failed", error);
                }
            }
            AppAction::EndWorkspaceId(id) => self.end_workspace(id),
            AppAction::EndSession(held) => self.end_session(&held),
            AppAction::AttachSession(held) => {
                if let Err(error) = self.attach_session(&held, gl) {
                    self.show_error("Session could not be shown", error);
                }
            }
            // The row of the picker a window without a terminal asks with,
            // which acts on it itself.
            AppAction::NewSession => {}
            AppAction::NewSessionOn(place) => {
                if let Err(error) = self.create_workspace_on(place, gl) {
                    self.show_error("Session could not be started", error);
                }
            }
            // Its row gives way to what the place holds, so the picker is
            // shown again to choose from that.
            AppAction::Connect(place) => {
                crate::session::connect(&place);
                self.open_workspace_picker(ctx);
            }
            AppAction::ReloadConfig => match self.reload_source.reload() {
                Ok(config) => {
                    if config.theme.is_some()
                        || config.font_family.is_some()
                        || config.font_size.is_some()
                    {
                        if let Err(error) = self.app.apply_settings(&config) {
                            self.show_error("Could not reload configuration", error);
                            return;
                        }
                    }
                    if config.font_family.is_some() || config.font_size.is_some() {
                        for (_, pane) in &mut self.tiled {
                            pane.refresh_picker_font(ctx);
                        }
                    }
                    self.picker_bindings = std::rc::Rc::new(config.bindings);
                    #[cfg(any(windows, unix))]
                    {
                        let policy = &self.picker_bindings.pipe_policy;
                        for (_, pane) in &self.tiled {
                            pane.set_pipe_policy(policy);
                        }
                    }
                    self.floating_padding = config.padding;
                    // The reloaded tables may not include the active one.
                    self.key_table = None;
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
                let Some(pane) = self.active_pane().and_then(|pane| self.pane_mut(pane)) else {
                    return;
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

    /// Where the current workspace's floating pane goes: the size the mouse
    /// gave it, or else the configured padding or default size.
    fn floating_rect_for(&self, content: egui::Rect) -> egui::Rect {
        let w = self.workspace();
        Self::floating_rect(content, w.floats.size().or(self.floating_padding))
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

    /// Shows where the visible pane sits among panes the layout hides:
    /// pagination dots set into the right of its top border, the current one
    /// in the accent colour. Past `MAX_DOTS` panes it shows "N / M" instead.
    pub(super) fn paint_pane_dots(
        painter: &egui::Painter,
        rect: egui::Rect,
        position: usize,
        count: usize,
    ) {
        const MAX_DOTS: usize = 9;
        const RADIUS: f32 = 3.0;
        const SPACING: f32 = 11.0;
        let palette = crate::colors::palette();
        let y = rect.top();
        let galley = (count > MAX_DOTS).then(|| {
            painter.layout_no_wrap(
                format!("{position} / {count}"),
                egui::FontId::monospace(11.0),
                palette.accent,
            )
        });
        let width = galley
            .as_ref()
            .map_or(SPACING * (count - 1) as f32 + RADIUS * 2.0, |galley| {
                galley.size().x
            });
        // Cut the border behind the indicator so it sits in the line, clear
        // of the rounded corner.
        let size = egui::vec2(width + 14.0, RADIUS * 2.0 + 4.0);
        let gap =
            egui::Rect::from_center_size(egui::pos2(rect.right() - 18.0 - size.x / 2.0, y), size);
        painter.rect_filled(gap, 0.0, palette.background);
        if let Some(galley) = galley {
            let at = gap.center() - galley.size() / 2.0;
            painter.galley(at, galley, palette.accent);
            return;
        }
        let first = gap.center().x - SPACING * (count - 1) as f32 / 2.0;
        for index in 0..count {
            let center = egui::pos2(first + SPACING * index as f32, y);
            if index + 1 == position {
                painter.circle_filled(center, RADIUS, palette.accent);
            } else {
                painter.circle_filled(center, RADIUS - 0.5, palette.muted_text);
            }
        }
    }

    /// Draws the shown float over the tiled panes: its frame at `rect`, and
    /// each of its panes at its place in `panes`. `focus` is the pane with
    /// focus, which is framed when there are several.
    #[allow(clippy::too_many_arguments)]
    fn show_floating(
        &self,
        ui: &egui::Ui,
        content: egui::Rect,
        rect: egui::Rect,
        style: &workspace::Appearance,
        overlay: &resize_keys::KeyOverlay,
        panes: &[(u64, egui::Rect)],
        focus: Option<u64>,
    ) {
        let Some(floating) = self.visible_float() else {
            return;
        };
        // Also when its layout shows one of them at a time.
        let several = floating.panes.len() > 1;
        let hides_panes = match floating.layout {
            workspace::Layout::Monocle => floating.panes.len() > 1,
            workspace::Layout::Deck => floating.panes.len() > 2,
            _ => false,
        };
        let inside = Self::float_group_style(style);
        let layer_id = egui::LayerId::new(egui::Order::Middle, egui::Id::new("floating-terminal"));
        egui::Area::new(layer_id.id)
            .order(layer_id.order)
            .fade_in(false)
            .fixed_pos(content.min)
            .interactable(false)
            .show(ui.ctx(), |floating_ui| {
                floating_ui.set_min_size(content.size());
                let painter = floating_ui.painter();
                let palette = crate::colors::palette();
                // Resize and move modes darken what is behind the float further.
                let dim = if overlay.is_none() {
                    90.0 / 255.0
                } else {
                    resize_keys::DIM
                };
                painter.rect_filled(content, 0.0, palette.background.gamma_multiply(dim));
                // The overlay replaces the focus highlight while shown. With
                // several panes the highlight is on the one with focus.
                style.paint_frame(painter, rect, overlay.is_none() && !several);
                painter.rect_filled(rect, style.corner_radius, palette.background);
                for (id, pane_rect) in panes {
                    if pane_rect.width() < 1.0 || pane_rect.height() < 1.0 {
                        continue;
                    }
                    if several {
                        let highlight = focus == Some(*id) && overlay.is_none();
                        inside.paint_frame(painter, *pane_rect, highlight);
                    }
                    if let Some(terminal) = self.tiled_ref(*id) {
                        painter.add(egui::PaintCallback {
                            rect: style.content_rect(*pane_rect),
                            callback: terminal.paint_callback(),
                        });
                    }
                    // Where the pane sits among those the layout hides.
                    if hides_panes && focus == Some(*id) {
                        let (number, count) = (floating.pane_number(), floating.panes.len());
                        Self::paint_pane_dots(painter, *pane_rect, number, count);
                    }
                }
                let ids = self.floats().ids();
                if ids.len() > 1 {
                    let position = ids.iter().position(|id| *id == floating.id).unwrap_or(0) + 1;
                    Self::paint_pane_dots(painter, rect, position, ids.len());
                }
                // The overlay, drawn last, over the terminal: on the float
                // when the keys act on it, or else on the pane they act on.
                let marked = panes.iter().find(|(id, _)| several && focus == Some(*id));
                let marked = marked.map_or(rect, |(_, pane_rect)| *pane_rect);
                overlay.paint(painter, marked, style.corner_radius, style.border_width);
            });
        ui.ctx().move_to_top(layer_id);
    }
    pub(crate) fn update(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        self.update_workspaces(ui, gl);
        // A pane closed in the frame may have been the last.
        self.stay_for_sessions();
        self.post_requests();
    }

    pub(crate) fn should_close(&self) -> bool {
        self.close_requested || (self.empty.is_none() && !self.has_panes())
    }

    /// Opens the messages picker in the visible float, or else the focused pane.
    fn show_messages(&mut self, ctx: &egui::Context) {
        let issues = self.messages.config_issues().to_vec();
        if issues.is_empty() && crate::error_log::snapshot().is_empty() {
            self.show_notice("Messages", "No messages.".into());
            return;
        }
        if let Some(pane) = self.active_pane().and_then(|pane| self.pane_mut(pane)) {
            pane.open_messages(ctx, &issues);
        }
    }

    /// The status bar. While a key table is active its name fills the mode
    /// chip, `hints` follow it, and the focused terminal's size is shown.
    fn show_mode_status(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        hints: &[resize_keys::KeyHint],
    ) {
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
        // A workspace on another machine says which, where one of this
        // machine only says what it runs.
        let elsewhere = self
            .current_place()
            .name()
            .map(|name| format!("{} \u{b7} {}", self.command_label, name.to_uppercase()));
        let mut status = match self.active_pane_opt() {
            Some(pane) => pane.status(&self.command_label),
            None => self.empty_status(),
        };
        if let Some(elsewhere) = &elsewhere
            && std::ptr::eq(status.label, self.command_label.as_str())
        {
            status.label = elsewhere;
        }
        if let Some(picker) = &self.window_picker {
            status.label = picker.label();
        }
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
        // An active key table swallows other keys, so it takes the chip.
        let table = self.key_table.as_ref().map(|name| name.to_uppercase());
        let (chip_fill, chip_text) = if table.is_some() {
            (palette.warning, palette.background)
        } else if mode_active {
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
        let label = painter.layout_no_wrap(
            table.clone().unwrap_or_else(|| status.label.to_owned()),
            font.clone(),
            chip_text,
        );
        let label_rect = egui::Rect::from_min_size(
            egui::pos2(menu.right() + 6.0, chip_top),
            egui::vec2(label.size().x + 16.0, chip_height),
        );
        painter.rect_filled(label_rect, 4.0, chip_fill);
        painter.galley(label_rect.center() - label.size() / 2.0, label, chip_text);
        // The floating panes chip has a fixed width, even at 0 or while a float
        // is shown, so the controls to its left keep their place. A visible
        // float fills it; its number is in the tooltip.
        {
            let count = self.floats().ids().len();
            let visible = self
                .visible_float()
                .map(|p| p.id.to_string());
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
        // A key table shows the focused terminal's size instead, and its
        // hints fill the space up to it.
        let searching =
            self.active_pane_opt().is_some_and(|pane| pane.search_active()) && table.is_none();
        let detail = if table.is_some() {
            let (columns, rows) = self.active_pane_opt().map_or((0, 0), |pane| pane.grid_size());
            Some(format!("{columns}×{rows}"))
        } else {
            status.detail.filter(|_| !searching)
        };
        let mut hints_right = right_edge;
        if let Some(detail) = detail {
            let text = painter.layout_no_wrap(detail, font.clone(), foreground);
            let left = right_edge - text.size().x;
            if left > label_rect.right() + 8.0 {
                painter.galley(
                    egui::pos2(left, rect.center().y + 1.0 - text.size().y / 2.0),
                    text,
                    foreground,
                );
                hints_right = left - 16.0;
            }
        }
        if table.is_some() {
            resize_keys::paint_hints(&painter, hints, label_rect, hints_right);
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

    /// The window is closing. Panes that were closed lose their shells as
    /// usual; the ones still open keep theirs when the session daemon runs
    /// them.
    pub(crate) fn destroy(&mut self) {
        self.save_sessions();
        self.drop_peek();
        self.floating.entries.clear();
        for mut pane in self.closed_panes.drain(..) {
            pane.destroy();
        }
        for request in self.requests.drain(..) {
            if let WindowRequest::MovePaneToNewWindow(mut pane) = request {
                pane.0.keep_shell();
                pane.0.destroy();
            }
        }
        for (_, mut pane) in self.tiled.drain(..) {
            pane.keep_shell();
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

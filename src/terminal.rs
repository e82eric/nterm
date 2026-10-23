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
    main: TerminalPane,
    /// Tiled terminals other than pane 0 (`main`), across all workspaces, by
    /// tiled ID. Which workspace each belongs to is in `Workspace::panes`.
    tiled: Vec<(u64, TerminalPane)>,
    closed_panes: Vec<TerminalPane>,
    close_main_pending: bool,
    workspaces: Vec<workspace::Workspace>,
    current: u64,
    previous: u64,
    next_workspace: u64,
    next_pane: u64,
    messages: crate::messages::Messages,

    command_label: String,
    reload_source: crate::config::ReloadSource,
    /// Shared so per-frame borrows can clone it cheaply; copy-on-write edits.
    picker_bindings: std::rc::Rc<crate::picker::config::Bindings>,
    floating_padding: Option<crate::config::FloatingPadding>,
    floating: floating::Panes<FloatingTerminal>,
    new_window_requested: bool,
}

struct FloatingTerminal {
    pane: TerminalPane,
}

/// Window state that a pane's command palette shows: its float and workspace
/// entries. Built by the window when a picker opens, not kept on each pane.
#[derive(Default)]
struct PaletteContext {
    has_floating: bool,
    entries: Vec<crate::actions::Entry>,
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
    fn destroy(&mut self, gl: &glow::Context) {
        self.pane.destroy(gl);
    }

    fn acquire_frame(&mut self) -> bool {
        self.pane.acquire_frame()
    }
}

/// A terminal in this window. Tiled and floating panes have separate ID
/// spaces, so a reference says which kind it is; tiled 0 is `main`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneRef {
    Tiled(u64),
    Floating(u64),
}

impl TerminalView {
    pub(crate) fn apply_theme(&mut self, name: &str) -> Result<(), String> {
        self.main.apply_theme(name)
    }
    fn active_pane_ref(&self) -> &TerminalPane {
        if let Some(float) = self.visible_float() {
            &float.pane
        } else {
            self.tiled_ref(self.focused_id().unwrap_or(0))
                .unwrap_or(&self.main)
        }
    }

    pub(crate) fn new(
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<UserEvent>,
        window_id: egui_winit::winit::window::WindowId,
        argc: usize,
        argv: *mut *mut c_char,
        command_label: String,
        picker_bindings: crate::picker::config::Bindings,
        floating_padding: Option<crate::config::FloatingPadding>,
        mut issues: Vec<String>,
        reload_source: crate::config::ReloadSource,
    ) -> Result<Self, crate::native_error::FatalError> {
        let main = TerminalPane::create(egui_ctx, gl, proxy, window_id, argc, argv)?;
        issues.extend(main.take_config_issues());
        #[cfg(any(windows, unix))]
        main.set_pipe_policy(&picker_bindings.pipe_policy);
        Ok(Self {
            messages: crate::messages::Messages::new(issues),

            main,
            closed_panes: Vec::new(),
            close_main_pending: false,
            tiled: Vec::new(),
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
            picker_bindings: std::rc::Rc::new(picker_bindings),
            floating_padding,
            floating: Default::default(),
            new_window_requested: false,
        })
    }

    pub(crate) fn new_window(
        &self,
        egui_ctx: &egui::Context,
        gl: &glow::Context,
        window_id: egui_winit::winit::window::WindowId,
    ) -> Result<Self, crate::native_error::FatalError> {
        let main = self.main.create_window(egui_ctx, gl, window_id)?;
        #[cfg(any(windows, unix))]
        main.set_pipe_policy(&self.picker_bindings.pipe_policy);
        Ok(Self {
            messages: crate::messages::Messages::new(main.take_config_issues()),
            main,
            closed_panes: Vec::new(),
            close_main_pending: false,
            tiled: Vec::new(),
            workspaces: vec![{
                let mut w = workspace::Workspace::new(1, 1);
                w.layout = self.picker_bindings.default_layout;
                w.panes.push(0);
                w.focused = Some(0);
                w
            }],
            current: 1,
            previous: 1,
            next_workspace: 2,
            next_pane: 1,
            command_label: self.command_label.clone(),
            reload_source: self.reload_source.clone(),
            picker_bindings: self.picker_bindings.clone(),
            floating_padding: self.floating_padding,
            floating: Default::default(),
            new_window_requested: false,
        })
    }

    /// Creates a tiled or floating pane's terminal as a child of the window's
    /// app, with the configured pipe policy applied.
    fn create_pane(
        &self,
        gl: &glow::Context,
        options: &pane::SpawnOptions,
    ) -> Result<TerminalPane, String> {
        let pane = self
            .main
            .create_child(gl, options)
            .map_err(|e| e.to_string())?;
        #[cfg(any(windows, unix))]
        pane.set_pipe_policy(&self.picker_bindings.pipe_policy);
        Ok(pane)
    }

    pub(crate) fn take_new_window_request(&mut self) -> bool {
        std::mem::take(&mut self.new_window_requested)
    }

    pub(crate) fn window_error(&mut self, error: String) {
        self.main
            .show_error("Could not open terminal window", error);
    }

    pub(crate) fn tick(&mut self) {
        self.main.tick();
    }

    pub(crate) fn acquire_frames(&mut self) -> bool {
        let mut acquired = self.main.acquire_frame();
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
            Request::Next | Request::Previous => {
                self.floats_mut().cycle(matches!(request, Request::Next));
                serde_json::Value::Null
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
        self.main.request_repaint();
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
                    self.main.show_error("Workspace action failed", error);
                }
            }
            AppAction::MovePaneToWorkspaceId { pane, workspace } => {
                if let Err(error) =
                    self.workspace_request(workspace::Request::MovePane(pane, workspace), gl)
                {
                    self.main.show_error("Workspace action failed", error);
                }
            }
            AppAction::ReloadConfig => match self.reload_source.reload() {
                Ok(config) => {
                    if config.theme.is_some()
                        || config.font_family.is_some()
                        || config.font_size.is_some()
                    {
                        if let Err(error) = self.main.apply_settings(&config) {
                            self.main
                                .show_error("Could not reload configuration", error);
                            return;
                        }
                    }
                    if config.font_family.is_some() || config.font_size.is_some() {
                        self.main.refresh_picker_font(ctx);
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
                        self.main.set_pipe_policy(policy);
                        for (_, pane) in &self.tiled {
                            pane.set_pipe_policy(policy);
                        }
                        for floating in &self.floating.entries {
                            floating.set_pipe_policy(policy);
                        }
                    }
                    self.floating_padding = config.padding;
                    crate::colors::COLORS.reload(ctx, config.colors);
                    self.main.show_notice("", "Configuration reloaded".into());
                }
                Err(error) => self
                    .main
                    .show_error("Could not reload configuration", error),
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
                Ok(()) => self.main.show_notice("Theme applied", name),
                Err(error) => self.main.show_error("Could not apply theme", error),
            },
            AppAction::ShowMessages => self.messages.open(),
            AppAction::ShowErrorRecord(record) => self.messages.open_record(*record),
            AppAction::OpenErrorLog => {
                if let Some(floating) = self.visible_float_mut() {
                    floating.open_error_log(ctx);
                } else {
                    if let Some(id) = self.focused_id() {
                        if let Some(p) = self.tiled_mut(id) {
                            p.open_error_log(ctx);
                        }
                    }
                }
            }
            AppAction::NewWindow => {
                self.new_window_requested = true;
            }
            AppAction::RestoreFloatingPane(id) => {
                if self.floats().contains(id) {
                    self.floats_mut().show(id);
                } else {
                    self.main.show_notice(
                        "Floating pane closed",
                        format!("Pane {id} is no longer available"),
                    );
                }
            }
            AppAction::CreateFloatingPane
            | AppAction::ToggleFloatingPane
            | AppAction::NextFloatingPane
            | AppAction::PreviousFloatingPane
            | AppAction::CloseFloatingPane => {
                let request = match action {
                    AppAction::CreateFloatingPane => floating::Request::Create(Default::default()),
                    AppAction::ToggleFloatingPane => floating::Request::Toggle,
                    AppAction::NextFloatingPane => floating::Request::Next,
                    AppAction::PreviousFloatingPane => floating::Request::Previous,
                    _ => floating::Request::Close(None),
                };
                if let Err(error) = self.floating_request(request, gl) {
                    self.main.show_error("Floating pane action failed", error);
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
    }

    pub(crate) fn should_close(&self) -> bool {
        self.workspaces.iter().all(|w| w.panes.is_empty())
    }

    fn show_mode_status(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let warning_right = rect.right() - 10.0;
        // Paint the background before the clickable status indicator.
        ui.painter()
            .rect_filled(rect, 0.0, crate::colors::palette().panel_background);
        let mut right_edge = self.messages.status(ui, rect).min(warning_right);
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
        let label = painter.layout_no_wrap(status.label.to_owned(), font.clone(), chip_text);
        let label_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.left() + 8.0,
                rect.center().y + 1.0 - (rect.height() - 8.0).clamp(8.0, 22.0) / 2.0,
            ),
            egui::vec2(
                label.size().x + 16.0,
                (rect.height() - 8.0).clamp(8.0, 22.0),
            ),
        );
        painter.rect_filled(label_rect, 4.0, chip_fill);
        painter.galley(label_rect.center() - label.size() / 2.0, label, chip_text);
        if self.has_floating() {
            let color = if self.floats().visible().is_some() {
                crate::colors::palette().warning
            } else {
                crate::colors::palette().muted_text
            };
            let text = painter.layout_no_wrap(
                format!(
                    "Floating Panes: {}{}",
                    self.floats().ids().len(),
                    self.visible_float()
                        .map(|p| format!(
                            " [{}]",
                            p.name.clone().unwrap_or_else(|| p.id.to_string())
                        ))
                        .unwrap_or_default()
                ),
                egui::FontId::monospace(13.0),
                color,
            );
            let left = right_edge - text.size().x;
            painter.galley(
                egui::pos2(left, rect.center().y + 1.0 - text.size().y / 2.0),
                text,
                color,
            );
            right_edge = left - 12.0;
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
        if let Some(detail) = status.detail {
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
        self.workspace_bar(ui, workspace_rect);
    }
    pub(crate) fn poll_retired(&self, gl: &glow::Context) {
        self.main.poll_retired(gl);
        for (_, pane) in &self.tiled {
            pane.poll_retired(gl);
        }
        for floating in &self.floating.entries {
            floating.poll_retired(gl);
        }
    }

    pub(crate) fn retired_pending(&self) -> bool {
        self.main.retired_pending()
            || self.tiled.iter().any(|(_, p)| p.retired_pending())
            || self
                .floating
                .entries
                .iter()
                .any(|floating| floating.retired_pending())
    }

    pub(crate) fn destroy(&mut self, gl: &glow::Context) {
        for mut floating in self.floating.entries.drain(..) {
            floating.destroy(gl);
        }
        for mut pane in self.closed_panes.drain(..) {
            pane.destroy(gl);
        }
        for (_, mut pane) in self.tiled.drain(..) {
            pane.destroy(gl);
        }
        self.main.destroy(gl);
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

use super::{
    PaneRef, TerminalView, floating,
    workspace::{self, Layout, Request, Workspace},
};
use crate::{actions::AppAction, graphics as glow};
const WORKSPACE_FONT_SIZE: f32 = 13.0;
const WORKSPACE_CELL: f32 = 22.0;
const WORKSPACE_GAP: f32 = 3.0;
pub(crate) enum HostRequest {
    Floating(super::floating::Request),
    Workspace(Request),
}
pub(crate) fn workspace_picker_options(
    workspaces: &[Workspace],
    current: u64,
    pane: Option<PaneRef>,
) -> crate::picker::config::Options {
    use crate::picker::config::*;
    Options {
        scope: Scope::Workspace,
        position: Position::Bottom,
        sort: Sort::SourceOrder,
        source: Source::Workspaces(
            workspaces
                .iter()
                .map(|w| crate::actions::Entry {
                    id: format!("workspace.{}", w.id),
                    label: format!(
                        "Workspace {}{}",
                        w.index,
                        if w.id == current { " [current]" } else { "" }
                    ),
                    shortcut: Some(format!("{} / {} panes", w.layout.tag(), w.panes.len())),
                    action: match pane {
                        Some(pane) => AppAction::MovePaneToWorkspaceId {
                            pane,
                            workspace: w.id,
                        },
                        None => AppAction::SelectWorkspaceId(w.id),
                    },
                })
                .collect(),
        ),
        ..Options::default()
    }
}
/// A workspace action. Config, Lua and the command palette name actions as
/// strings; `parse` converts them once and they stay typed from then on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceAction {
    CreateWorkspace,
    RemoveWorkspace,
    WorkspacePicker,
    MovePaneToWorkspacePicker,
    /// Moves the focused pane, or the visible float, into a new workspace.
    MovePaneToNewWorkspace,
    /// Moves the focused pane, or the visible float, into a new window.
    MovePaneToNewWindow,
    NextWorkspace,
    PreviousWorkspace,
    LastWorkspace,
    /// The 1-based workspace index shown in the bar.
    SelectWorkspace(usize),
    /// Moves the focused pane to the 1-based workspace index.
    MoveToWorkspace(usize),
    CreatePane,
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveToMain,
    /// Floats the focused tiled pane, or tiles the visible float.
    FloatOrTilePane,
    SetTile,
    SetTileReversed,
    SetMonocle,
    SetDeck,
    SetGrid,
    SetHorizontalGrid,
    ResetLayout,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
}
impl WorkspaceAction {
    /// The actions without an index argument, in command palette order.
    pub(crate) const ALL: &[Self] = &[
        Self::CreateWorkspace,
        Self::RemoveWorkspace,
        Self::WorkspacePicker,
        Self::MovePaneToWorkspacePicker,
        Self::MovePaneToNewWorkspace,
        Self::MovePaneToNewWindow,
        Self::NextWorkspace,
        Self::PreviousWorkspace,
        Self::LastWorkspace,
        Self::CreatePane,
        Self::ClosePane,
        Self::FocusLeft,
        Self::FocusRight,
        Self::FocusUp,
        Self::FocusDown,
        Self::MoveLeft,
        Self::MoveRight,
        Self::MoveUp,
        Self::MoveDown,
        Self::MoveToMain,
        Self::FloatOrTilePane,
        Self::SetTile,
        Self::SetTileReversed,
        Self::SetMonocle,
        Self::SetDeck,
        Self::SetGrid,
        Self::SetHorizontalGrid,
        Self::ResetLayout,
        Self::ResizeLeft,
        Self::ResizeRight,
        Self::ResizeUp,
        Self::ResizeDown,
    ];
    /// Parses a config name, including `SelectWorkspace:N` and
    /// `MoveToWorkspace:N` with N >= 1, and the old `SetHorizontalDeck` name.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        if name == "SetHorizontalDeck" {
            return Some(Self::SetHorizontalGrid);
        }
        if let Some(action) = Self::ALL.iter().find(|a| a.name() == name) {
            return Some(*action);
        }
        let index = |prefix| {
            name.strip_prefix(prefix)?
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
        };
        index("SelectWorkspace:")
            .map(Self::SelectWorkspace)
            .or_else(|| index("MoveToWorkspace:").map(Self::MoveToWorkspace))
    }
    /// The config name. Spelled out rather than derived from the variant so
    /// renaming a variant cannot silently break existing configs.
    pub(crate) fn name(self) -> std::borrow::Cow<'static, str> {
        use WorkspaceAction::*;
        match self {
            SelectWorkspace(index) => format!("SelectWorkspace:{index}").into(),
            MoveToWorkspace(index) => format!("MoveToWorkspace:{index}").into(),
            CreateWorkspace => "CreateWorkspace".into(),
            RemoveWorkspace => "RemoveWorkspace".into(),
            WorkspacePicker => "WorkspacePicker".into(),
            MovePaneToWorkspacePicker => "MovePaneToWorkspacePicker".into(),
            MovePaneToNewWorkspace => "MovePaneToNewWorkspace".into(),
            MovePaneToNewWindow => "MovePaneToNewWindow".into(),
            NextWorkspace => "NextWorkspace".into(),
            PreviousWorkspace => "PreviousWorkspace".into(),
            LastWorkspace => "LastWorkspace".into(),
            CreatePane => "CreatePane".into(),
            ClosePane => "ClosePane".into(),
            FocusLeft => "FocusLeft".into(),
            FocusRight => "FocusRight".into(),
            FocusUp => "FocusUp".into(),
            FocusDown => "FocusDown".into(),
            MoveLeft => "MoveLeft".into(),
            MoveRight => "MoveRight".into(),
            MoveUp => "MoveUp".into(),
            MoveDown => "MoveDown".into(),
            MoveToMain => "MoveToMain".into(),
            FloatOrTilePane => "FloatOrTilePane".into(),
            SetTile => "SetTile".into(),
            SetTileReversed => "SetTileReversed".into(),
            SetMonocle => "SetMonocle".into(),
            SetDeck => "SetDeck".into(),
            SetGrid => "SetGrid".into(),
            SetHorizontalGrid => "SetHorizontalGrid".into(),
            ResetLayout => "ResetLayout".into(),
            ResizeLeft => "ResizeLeft".into(),
            ResizeRight => "ResizeRight".into(),
            ResizeUp => "ResizeUp".into(),
            ResizeDown => "ResizeDown".into(),
        }
    }
    /// The name in sentence case, matching the other palette entries, e.g.
    /// "Focus left" or "Select workspace 3".
    pub(crate) fn label(self) -> String {
        let mut label = String::new();
        for (i, c) in self.name().chars().enumerate() {
            if c == ':' {
                label.push(' ');
            } else if i > 0 && c.is_uppercase() {
                label.push(' ');
                label.extend(c.to_lowercase());
            } else {
                label.push(c);
            }
        }
        label
    }
    /// Whether holding the key runs the action again on each auto-repeat.
    /// Navigation and resizing repeat; creating, closing and switching do
    /// not. Every variant is listed so a new action must choose.
    pub(crate) fn allows_key_repeat(self) -> bool {
        use WorkspaceAction::*;
        match self {
            FocusLeft | FocusRight | FocusUp | FocusDown | MoveLeft | MoveRight | MoveUp
            | MoveDown | ResizeLeft | ResizeRight | ResizeUp | ResizeDown => true,
            CreateWorkspace
            | RemoveWorkspace
            | WorkspacePicker
            | MovePaneToWorkspacePicker
            | MovePaneToNewWorkspace
            | MovePaneToNewWindow
            | NextWorkspace
            | PreviousWorkspace
            | LastWorkspace
            | SelectWorkspace(_)
            | MoveToWorkspace(_)
            | CreatePane
            | ClosePane
            | MoveToMain
            | FloatOrTilePane
            | SetTile
            | SetTileReversed
            | SetMonocle
            | SetDeck
            | SetGrid
            | SetHorizontalGrid
            | ResetLayout => false,
        }
    }
}
impl std::fmt::Display for WorkspaceAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}
pub(crate) fn default_bindings() -> Vec<crate::picker::config::KeyBinding> {
    use crate::config::keybindings::{BindingTrigger, KeyBinding, parse_key};
    use crate::picker::config::BindingTarget;
    use WorkspaceAction::*;
    let mut keys = Vec::new();
    let mut add = |key: &str, shift: bool, action: WorkspaceAction| {
        keys.push(KeyBinding {
            trigger: BindingTrigger::Key(parse_key(key).unwrap()),
            mods: egui::Modifiers {
                ctrl: true,
                alt: true,
                shift,
                ..Default::default()
            },
            description: action.label(),
            action: BindingTarget::Action(AppAction::Workspace(action)),
        })
    };
    for (key, action) in [
        ("h", FocusLeft),
        ("j", FocusDown),
        ("k", FocusUp),
        ("l", FocusRight),
        ("Enter", MoveToMain),
        ("f", FloatOrTilePane),
        ("m", SetMonocle),
        ("y", SetDeck),
        ("u", SetGrid),
        ("t", SetTile),
        ("n", ResetLayout),
        ("w", WorkspacePicker),
        ("c", CreatePane),
    ] {
        add(key, false, action);
    }
    for (key, action) in [
        ("h", MoveLeft),
        ("j", MoveDown),
        ("k", MoveUp),
        ("l", MoveRight),
        ("w", CreateWorkspace),
        ("c", ClosePane),
        ("y", SetHorizontalGrid),
        ("Left", ResizeLeft),
        ("Right", ResizeRight),
        ("Up", ResizeUp),
        ("Down", ResizeDown),
    ] {
        add(key, true, action);
    }
    for index in 1..=10 {
        let key = (index % 10).to_string();
        add(&key, false, SelectWorkspace(index));
        add(&key, true, MoveToWorkspace(index));
    }
    keys
}
pub(crate) fn install_lua_helpers(lua: &mlua::Lua, nterm: &mlua::Table) -> mlua::Result<()> {
    nterm.set(
        "workspace_keys",
        lua.create_function(|lua, mods: String| {
            crate::config::keybindings::parse_modifiers(&mods)
                .map_err(|e| mlua::Error::external(format!("unknown modifier {e}")))?;
            let list = lua.create_table()?;
            for (i, binding) in default_bindings().into_iter().enumerate() {
                let row = lua.create_table()?;
                if let crate::config::keybindings::BindingTrigger::Key(key) = binding.trigger {
                    row.set("key", key.name())?;
                }
                row.set(
                    "mods",
                    if binding.mods.shift {
                        format!("{mods}|SHIFT")
                    } else {
                        mods.clone()
                    },
                )?;
                if let crate::picker::config::BindingTarget::Action(AppAction::Workspace(action)) =
                    binding.action
                {
                    row.set("action", action.to_string())?;
                }
                list.set(i + 1, row)?;
            }
            Ok(list)
        })?,
    )
}
impl TerminalView {
    fn open_workspace_picker(&mut self, ctx: &egui::Context, moving: bool) {
        let pane = if moving { self.active_pane() } else { None };
        if moving && pane.is_none() {
            return;
        }
        let options = workspace_picker_options(&self.workspaces, self.current, pane);
        let bindings = self.picker_bindings.clone();
        let palette = self.palette_context();
        if let Some(pane) = self.active_pane().and_then(|p| self.pane_mut(p)) {
            pane.open_picker(
                ctx,
                crate::picker::config::PickerEntry::Static(options),
                None,
                &bindings,
                &palette,
            );
        }
    }
    fn workspace(&self) -> &Workspace {
        self.workspaces
            .iter()
            .find(|w| w.id == self.current)
            .expect("current workspace")
    }
    fn workspace_mut(&mut self) -> &mut Workspace {
        self.workspaces
            .iter_mut()
            .find(|w| w.id == self.current)
            .expect("current workspace")
    }
    pub(super) fn focused_id(&self) -> Option<u64> {
        self.workspace().focused
    }
    pub(super) fn tiled_ref(&self, id: u64) -> Option<&super::TerminalPane> {
        self.tiled.iter().find(|(i, _)| *i == id).map(|(_, p)| p)
    }
    pub(super) fn tiled_mut(&mut self, id: u64) -> Option<&mut super::TerminalPane> {
        self.tiled
            .iter_mut()
            .find(|(i, _)| *i == id)
            .map(|(_, p)| p)
    }
    pub(super) fn select_workspace(&mut self, id: u64) -> Result<(), String> {
        if !self.workspaces.iter().any(|w| w.id == id) {
            return Err("workspace does not exist".into());
        }
        if self.current != id {
            let panes = self.workspace().panes.clone();
            for pane_id in panes {
                if let Some(pane) = self.tiled_mut(pane_id) {
                    pane.cancel_workspace_picker();
                }
            }
            for pane in &mut self.floating.entries {
                pane.cancel_workspace_picker();
            }
            self.previous = self.current;
            self.current = id;
        }
        Ok(())
    }
    /// The current workspace's floats.
    pub(super) fn floats(&self) -> &floating::Floats {
        &self.workspace().floats
    }
    pub(super) fn floats_mut(&mut self) -> &mut floating::Floats {
        &mut self.workspace_mut().floats
    }
    pub(super) fn has_floating(&self) -> bool {
        !self.floats().is_empty()
    }
    /// The current workspace's floats and every workspace, for a command
    /// palette about to open. Labels capture float titles at this moment.
    pub(super) fn palette_context(&self) -> super::PaletteContext {
        let floats = self.floats();
        let mut entries: Vec<_> = floats
            .ids()
            .iter()
            .filter_map(|id| self.floating.get(*id))
            .map(|p| {
                crate::actions::floating_pane_entry(
                    p.id,
                    p.name.as_deref(),
                    &p.title(),
                    floats.visible() == Some(p.id),
                )
            })
            .collect();
        entries.extend(self.workspaces.iter().map(|w| crate::actions::Entry {
            id: format!("workspace.{}", w.id),
            label: format!("Workspace {}", w.index),
            shortcut: None,
            action: AppAction::SelectWorkspaceId(w.id),
        }));
        super::PaletteContext {
            has_floating: self.has_floating(),
            entries,
            config_issues: self.messages.config_issues().to_vec(),
        }
    }
    pub(super) fn visible_float(&self) -> Option<&floating::Entry<super::FloatingTerminal>> {
        self.floating.get(self.floats().visible()?)
    }
    pub(super) fn visible_float_mut(
        &mut self,
    ) -> Option<&mut floating::Entry<super::FloatingTerminal>> {
        let id = self.floats().visible()?;
        self.floating.get_mut(id)
    }
    /// Closes floats in any workspace. Their terminals are released next
    /// frame, after this frame's paint callbacks.
    pub(super) fn close_floats(&mut self, ids: impl IntoIterator<Item = u64>) {
        for id in ids {
            for w in &mut self.workspaces {
                w.floats.remove(id);
            }
            if let Some(float) = self.floating.remove(id) {
                self.closed_panes.push(float.pane);
            }
        }
    }
    fn prune_workspaces(&mut self) {
        let orphaned =
            workspace::prune_empty(&mut self.workspaces, &mut self.current, &mut self.previous);
        self.close_floats(orphaned);
    }
    /// Moves a tiled pane's terminal into its workspace's floats and shows it.
    /// Returns the workspace and the new float ID.
    pub(super) fn float_tiled(&mut self, id: u64) -> Result<(u64, u64), String> {
        let w = self
            .workspaces
            .iter()
            .find(|w| w.panes.contains(&id))
            .ok_or("pane does not exist")?;
        if w.panes.len() == 1 {
            return Err("a workspace needs at least one tiled pane".into());
        }
        let workspace = w.id;
        let at = self
            .tiled
            .iter()
            .position(|(i, _)| *i == id)
            .ok_or("pane does not exist")?;
        let (_, pane) = self.tiled.remove(at);
        let float = self.floating.insert(None, super::FloatingTerminal { pane });
        let w = self
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
            .unwrap();
        w.remove(id);
        w.floats.add(float);
        Ok((workspace, float))
    }
    /// Moves a float's terminal into its workspace's tiled panes, after the
    /// focused pane, and focuses it. Returns the new tiled pane ID.
    pub(super) fn tile_float(&mut self, id: u64) -> Result<u64, String> {
        let workspace = self
            .workspaces
            .iter()
            .find(|w| w.floats.contains(id))
            .ok_or("floating pane does not exist")?
            .id;
        let float = self
            .floating
            .remove(id)
            .ok_or("floating pane does not exist")?;
        let pane_id = self.next_pane;
        self.next_pane += 1;
        self.tiled.push((pane_id, float.pane));
        let w = self
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
            .unwrap();
        w.floats.remove(id);
        w.floats.hide();
        let at = w
            .panes
            .iter()
            .position(|p| Some(*p) == w.focused)
            .map_or(w.panes.len(), |i| i + 1);
        w.panes.insert(at, pane_id);
        w.focus(pane_id);
        Ok(pane_id)
    }
    /// The lowest workspace index not in use.
    fn free_index(&self) -> usize {
        (1..)
            .find(|i| !self.workspaces.iter().any(|w| w.index == *i))
            .unwrap()
    }
    /// Adds a workspace without panes. The caller gives it a tiled pane
    /// before the next prune removes it.
    fn push_workspace(&mut self, index: usize, layout: Option<Layout>) -> &mut Workspace {
        let id = self.next_workspace;
        self.next_workspace += 1;
        let mut w = Workspace::new(id, index);
        w.set_layout(layout.unwrap_or(self.picker_bindings.default_layout));
        self.workspaces.push(w);
        self.workspaces.sort_by_key(|w| w.index);
        self.workspaces.iter_mut().find(|w| w.id == id).unwrap()
    }
    pub(super) fn move_pane_to_workspace(
        &mut self,
        pane: PaneRef,
        workspace: u64,
        gl: &glow::Context,
    ) -> Result<(), String> {
        let request = match pane {
            PaneRef::Tiled(id) => Request::MovePane(id, workspace),
            PaneRef::Floating(id) => Request::MoveFloat(id, workspace),
        };
        self.workspace_request(request, gl).map(|_| ())
    }
    /// Moves a pane into a new workspace at the lowest free index without
    /// selecting it. A float becomes the new workspace's tiled pane, because a
    /// workspace needs at least one.
    fn move_pane_to_new_workspace(
        &mut self,
        pane: PaneRef,
        gl: &glow::Context,
    ) -> Result<(), String> {
        // Check before creating so a failure leaves no empty workspace.
        let exists = match pane {
            PaneRef::Tiled(id) => self.workspaces.iter().any(|w| w.panes.contains(&id)),
            PaneRef::Floating(id) => self.workspaces.iter().any(|w| w.floats.contains(id)),
        };
        if !exists {
            return Err("pane does not exist".into());
        }
        let index = self.free_index();
        let workspace = self.push_workspace(index, None).id;
        self.move_pane_to_workspace(pane, workspace, gl)?;
        if let PaneRef::Floating(id) = pane {
            self.tile_float(id)?;
        }
        Ok(())
    }
    /// Takes a pane out of this window; the app opens a new window around it
    /// after this frame. Its process, copy mode, search, quick select, and
    /// pane pickers carry over; a workspace-scope picker is cancelled. A float
    /// becomes the new window's tiled pane.
    pub(super) fn move_pane_to_new_window(&mut self, pane: PaneRef) -> Result<(), String> {
        let mut pane = match pane {
            PaneRef::Tiled(id) => {
                let at = self
                    .tiled
                    .iter()
                    .position(|(i, _)| *i == id)
                    .ok_or("pane does not exist")?;
                for w in &mut self.workspaces {
                    w.remove(id);
                }
                self.prune_workspaces();
                self.tiled.remove(at).1
            }
            PaneRef::Floating(id) => {
                let float = self
                    .floating
                    .remove(id)
                    .ok_or("floating pane does not exist")?;
                for w in &mut self.workspaces {
                    w.floats.remove(id);
                }
                float.pane
            }
        };
        pane.cancel_workspace_picker();
        self.requests
            .push(super::WindowRequest::MovePaneToNewWindow(super::MovedPane(
                pane,
            )));
        self.context.request_repaint();
        Ok(())
    }
    /// Takes back a pane whose new window could not open, as a tiled pane
    /// after the focused one in the current workspace.
    pub(crate) fn return_moved_pane(&mut self, pane: super::MovedPane) {
        let id = self.next_pane;
        self.next_pane += 1;
        self.tiled.push((id, pane.0));
        let w = self.workspace_mut();
        let at = w
            .panes
            .iter()
            .position(|p| Some(*p) == w.focused)
            .map_or(w.panes.len(), |i| i + 1);
        w.panes.insert(at, id);
        w.focus(id);
    }
    fn close_tiled(&mut self, id: u64) -> Result<(), String> {
        if !self.workspaces.iter().any(|w| w.panes.contains(&id)) {
            return Err("pane does not exist".into());
        }
        for w in &mut self.workspaces {
            w.remove(id);
        }
        self.prune_workspaces();
        if let Some(i) = self.tiled.iter().position(|(i, _)| *i == id) {
            let (_, p) = self.tiled.remove(i);
            self.closed_panes.push(p);
        }
        self.context.request_repaint();
        Ok(())
    }
    pub(super) fn host_request(
        &mut self,
        request: HostRequest,
        gl: &glow::Context,
    ) -> Result<serde_json::Value, String> {
        match request {
            HostRequest::Floating(r) => self.floating_request(r, gl),
            HostRequest::Workspace(r) => self.workspace_request(r, gl),
        }
    }
    pub(super) fn workspace_request(
        &mut self,
        request: Request,
        gl: &glow::Context,
    ) -> Result<serde_json::Value, String> {
        use serde_json::{Value, json};
        match request {
            Request::Ensure(o) => {
                if let Some(w) = self.workspaces.iter().find(|w| o.index == Some(w.index)) {
                    return Ok(json!(w));
                }
                self.workspace_request(Request::Create(o), gl)
            }
            Request::Create(o) => {
                if o.index == Some(0) {
                    return Err("workspace index must be positive".into());
                }
                let index = o.index.unwrap_or_else(|| self.free_index());
                if self.workspaces.iter().any(|w| w.index == index) {
                    return Err("workspace index already exists".into());
                }
                // Indexes stay contiguous, so only the next one can be new.
                if index > self.workspaces.len() + 1 {
                    return Err(format!(
                        "workspace index {index} would leave a gap; the next index is {}",
                        self.workspaces.len() + 1
                    ));
                }
                // Spawn before publishing the workspace so failures leave the
                // workspace collection and focus untouched.
                let pane = self.create_pane(gl, &Default::default())?;
                let pane_id = self.next_pane;
                self.next_pane += 1;
                self.tiled.push((pane_id, pane));
                let w = self.push_workspace(index, o.layout);
                w.panes.push(pane_id);
                w.focus(pane_id);
                let result = json!(w);
                self.prune_workspaces();
                Ok(result)
            }
            Request::List => Ok(json!(self.workspaces)),
            Request::Get(id) => Ok(json!(
                self.workspaces
                    .iter()
                    .find(|w| w.id == id.unwrap_or(self.current))
            )),
            Request::Select(id) => {
                self.select_workspace(id)?;
                Ok(Value::Null)
            }
            Request::Remove(id) => {
                let w = self
                    .workspaces
                    .iter()
                    .find(|w| w.id == id)
                    .ok_or("workspace does not exist")?;
                if !w.panes.is_empty() {
                    return Err("move or close panes before removing a workspace".into());
                }
                if self.workspaces.len() == 1 {
                    return Err("keep at least one workspace".into());
                }
                let floats = w.floats.ids().to_vec();
                self.workspaces.retain(|w| w.id != id);
                workspace::renumber(&mut self.workspaces);
                if self.current == id {
                    self.current = self.workspaces[0].id;
                }
                if self.previous == id {
                    self.previous = self.current;
                }
                // Floats close with their workspace.
                self.close_floats(floats);
                Ok(Value::Null)
            }
            Request::SetLayout(id, layout) => {
                self.workspaces
                    .iter_mut()
                    .find(|w| w.id == id)
                    .ok_or("workspace does not exist")?
                    .set_layout(layout);
                Ok(Value::Null)
            }
            Request::CreatePane(options) => {
                let spawn = options.spawn();
                spawn.validate()?;
                let target = options.workspace.unwrap_or(self.current);
                if !self.workspaces.iter().any(|w| w.id == target) {
                    return Err("workspace does not exist".into());
                }
                let pane = self.create_pane(gl, &spawn)?;
                let id = self.next_pane;
                self.next_pane += 1;
                self.tiled.push((id, pane));
                let w = self.workspaces.iter_mut().find(|w| w.id == target).unwrap();
                w.panes.push(id);
                w.focus(id);
                Ok(json!({"id":id,"workspace":target}))
            }
            Request::MovePane(id, target) => {
                if !self.workspaces.iter().any(|w| w.id == target) {
                    return Err("workspace does not exist".into());
                }
                if !self.workspaces.iter().any(|w| w.panes.contains(&id)) {
                    return Err("pane does not exist".into());
                }
                for w in &mut self.workspaces {
                    w.remove(id);
                }
                let w = self.workspaces.iter_mut().find(|w| w.id == target).unwrap();
                w.panes.push(id);
                w.focus(id);
                self.prune_workspaces();
                Ok(Value::Null)
            }
            Request::MoveFloat(id, target) => {
                if !self.workspaces.iter().any(|w| w.id == target) {
                    return Err("workspace does not exist".into());
                }
                let source = self
                    .workspaces
                    .iter_mut()
                    .find(|w| w.floats.contains(id))
                    .ok_or("floating pane does not exist")?;
                // The source shows its next float, if any; the destination
                // shows the moved float when it is next selected.
                source.floats.remove(id);
                let w = self.workspaces.iter_mut().find(|w| w.id == target).unwrap();
                w.floats.add(id);
                Ok(Value::Null)
            }
            Request::ClosePane(id) => {
                self.close_tiled(id)?;
                Ok(Value::Null)
            }
            Request::MovePaneToNewWindow(id) => {
                self.move_pane_to_new_window(PaneRef::Tiled(id))?;
                Ok(Value::Null)
            }
            Request::MoveFloatToNewWindow(id) => {
                self.move_pane_to_new_window(PaneRef::Floating(id))?;
                Ok(Value::Null)
            }
            Request::FocusPane(id) => {
                let target = self
                    .workspaces
                    .iter()
                    .find(|w| w.panes.contains(&id))
                    .ok_or("pane does not exist")?
                    .id;
                self.select_workspace(target)?;
                self.workspace_mut().focus(id);
                Ok(Value::Null)
            }
            Request::ListPanes => Ok(json!(
                self.workspaces
                    .iter()
                    .flat_map(|w| w.panes.iter().map(
                        move |id| json!({"id":id,"workspace":w.id,"focused":w.focused==Some(*id)})
                    ))
                    .collect::<Vec<_>>()
            )),
            Request::Appearance(a) => {
                a.validate()?;
                std::rc::Rc::make_mut(&mut self.picker_bindings).workspace_appearance = a;
                Ok(Value::Null)
            }
            Request::SetAppearance(id, a) => {
                a.validate()?;
                self.workspaces
                    .iter_mut()
                    .find(|w| w.id == id)
                    .ok_or("workspace does not exist")?
                    .appearance = Some(a);
                Ok(Value::Null)
            }
            Request::MainRatio(id, ratio) => {
                if !ratio.is_finite() || !(0.1..=0.9).contains(&ratio) {
                    return Err("main ratio must be 0.1..0.9".into());
                }
                self.workspaces
                    .iter_mut()
                    .find(|w| w.id == id)
                    .ok_or("workspace does not exist")?
                    .main_ratio = ratio;
                Ok(Value::Null)
            }
        }
    }
    pub(super) fn workspace_action(
        &mut self,
        action: WorkspaceAction,
        ctx: &egui::Context,
        gl: &glow::Context,
    ) {
        use WorkspaceAction::*;
        let result = (|| -> Result<(), String> {
            match action {
                SelectWorkspace(index) | MoveToWorkspace(index) => {
                    let id = self
                        .workspaces
                        .iter()
                        .find(|w| w.index == index)
                        .map(|w| w.id)
                        .ok_or("workspace index does not exist")?;
                    if let MoveToWorkspace(_) = action {
                        if let Some(pane) = self.active_pane() {
                            self.move_pane_to_workspace(pane, id, gl)?;
                        }
                    } else {
                        self.select_workspace(id)?;
                    }
                }
                CreateWorkspace => {
                    let w = self.workspace_request(Request::Create(Default::default()), gl)?;
                    self.select_workspace(w["id"].as_u64().unwrap())?;
                }
                MovePaneToNewWorkspace => {
                    if let Some(pane) = self.active_pane() {
                        self.move_pane_to_new_workspace(pane, gl)?;
                    }
                }
                MovePaneToNewWindow => {
                    if let Some(pane) = self.active_pane() {
                        self.move_pane_to_new_window(pane)?;
                    }
                }
                WorkspacePicker | MovePaneToWorkspacePicker => {
                    self.open_workspace_picker(ctx, action == MovePaneToWorkspacePicker);
                }
                RemoveWorkspace => {
                    self.workspace_request(Request::Remove(self.current), gl)?;
                }
                NextWorkspace | PreviousWorkspace => {
                    let i = self
                        .workspaces
                        .iter()
                        .position(|w| w.id == self.current)
                        .unwrap();
                    let n = self.workspaces.len();
                    let i = if action == NextWorkspace {
                        (i + 1) % n
                    } else {
                        (i + n - 1) % n
                    };
                    self.select_workspace(self.workspaces[i].id)?;
                }
                LastWorkspace => {
                    self.select_workspace(self.previous)?;
                }
                // Creates in the layer that has focus: a float while one is shown.
                CreatePane => {
                    if self.floats().visible().is_some() {
                        self.floating_request(floating::Request::Create(Default::default()), gl)?;
                    } else {
                        self.workspace_request(Request::CreatePane(Default::default()), gl)?;
                    }
                }
                ClosePane => {
                    if let Some(id) = self.focused_id() {
                        self.close_tiled(id)?;
                    }
                }
                FloatOrTilePane => {
                    if let Some(id) = self.floats().visible() {
                        self.tile_float(id)?;
                    } else if let Some(id) = self.focused_id() {
                        self.float_tiled(id)?;
                    }
                }
                MoveToMain => {
                    let w = self.workspace_mut();
                    if let Some(i) = w.panes.iter().position(|p| Some(*p) == w.focused) {
                        let id = w.panes.remove(i);
                        w.panes.insert(0, id);
                    }
                }
                ResetLayout => {
                    let w = self.workspace_mut();
                    w.main_ratio = 0.5;
                    w.weights_mut().clear();
                }
                ResizeLeft | ResizeRight | ResizeUp | ResizeDown => {
                    self.workspace_mut().resize(
                        matches!(action, ResizeLeft | ResizeRight),
                        matches!(action, ResizeRight | ResizeDown),
                    );
                }
                SetTile => self.workspace_mut().set_layout(Layout::Tile),
                SetTileReversed => self.workspace_mut().set_layout(Layout::TileReversed),
                SetMonocle => self.workspace_mut().set_layout(Layout::Monocle),
                SetDeck => self.workspace_mut().set_layout(Layout::Deck),
                SetGrid => self.workspace_mut().set_layout(Layout::Grid),
                SetHorizontalGrid => self.workspace_mut().set_layout(Layout::HorizontalGrid),
                FocusLeft | FocusRight | FocusUp | FocusDown | MoveLeft | MoveRight | MoveUp
                | MoveDown => self.navigate(action, ctx),
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.show_error("Workspace action failed", error);
        }
        ctx.request_repaint();
    }
    fn navigate(&mut self, action: WorkspaceAction, ctx: &egui::Context) {
        use super::layout::Motion;
        use WorkspaceAction::*;
        let (motion, moving) = match action {
            FocusLeft => (Motion::Left, false),
            FocusRight => (Motion::Right, false),
            FocusUp => (Motion::Up, false),
            FocusDown => (Motion::Down, false),
            MoveLeft => (Motion::Left, true),
            MoveRight => (Motion::Right, true),
            MoveUp => (Motion::Up, true),
            MoveDown => (Motion::Down, true),
            _ => return,
        };
        // While a float is shown, focus keys cycle the workspace's floats.
        if !moving && self.floats().visible().is_some() {
            let next = matches!(motion, Motion::Down | Motion::Right);
            return self.floats_mut().cycle(next);
        }
        let rects = self.workspace().rectangles(
            ctx.content_rect(),
            &self.picker_bindings.workspace_appearance,
        );
        if self.workspace_mut().navigate(motion, moving, &rects) {
            self.floats_mut().hide();
        }
    }
    fn workspace_bar_detail(&self, font: &egui::FontId) -> egui::text::LayoutJob {
        let palette = crate::colors::palette();
        let w = self.workspace();
        let pane = w.pane_number();
        let mut job = egui::text::LayoutJob::default();
        let mut append = |text: &str, color: egui::Color32| {
            job.append(text, 0.0, egui::TextFormat::simple(font.clone(), color));
        };
        append(w.layout.tag(), palette.accent);
        append(" · ", palette.muted_text);
        append(&format!("{pane}/{}", w.panes.len()), palette.text);
        job
    }
    pub(super) fn workspace_bar_width(&self, ui: &egui::Ui, available: f32) -> f32 {
        let font = egui::FontId::monospace(WORKSPACE_FONT_SIZE);
        let detail = ui.painter().layout_job(self.workspace_bar_detail(&font));
        (self.workspaces.len() as f32 * (WORKSPACE_CELL + WORKSPACE_GAP)
            + WORKSPACE_GAP
            + 10.0
            + detail.size().x)
            .min(available.max(0.0) * 0.65)
    }
    pub(super) fn workspace_bar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if rect.width() < 32.0 || rect.height() < 8.0 {
            return;
        }
        let palette = crate::colors::palette();
        let painter = ui.painter().with_clip_rect(rect);
        let font = egui::FontId::monospace(WORKSPACE_FONT_SIZE);
        let inner = egui::Rect::from_center_size(
            rect.center() + egui::vec2(0.0, 1.0),
            egui::vec2(rect.width(), (rect.height() - 8.0).clamp(8.0, 22.0)),
        );
        // Status text sits on the right: layout tag and focused/total panes.
        let galley = painter.layout_job(self.workspace_bar_detail(&font));
        let mut right = inner.right();
        let detail_rect = egui::Rect::from_min_max(
            egui::pos2((right - galley.size().x).max(inner.left()), inner.top()),
            egui::pos2(right, inner.bottom()),
        );
        painter.galley(
            egui::pos2(
                detail_rect.left(),
                detail_rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            palette.text,
        );
        right = detail_rect.left() - 10.0;
        // Workspace pills fill the remaining space, hugging the status text.
        let strip = egui::Rect::from_min_max(
            inner.min,
            egui::pos2(right.max(inner.left()), inner.bottom()),
        );
        let pitch = WORKSPACE_CELL + WORKSPACE_GAP;
        let slots = ((strip.width() + WORKSPACE_GAP) / pitch).floor().max(1.0) as usize;
        let overflow = self.workspaces.len() > slots;
        let visible = if overflow {
            slots.saturating_sub(1).max(1)
        } else {
            self.workspaces.len()
        };
        let current = self
            .workspaces
            .iter()
            .position(|w| w.id == self.current)
            .unwrap_or(0);
        let start = if current >= visible {
            current + 1 - visible
        } else {
            0
        };
        let used = (visible + usize::from(overflow)) as f32 * pitch - WORKSPACE_GAP;
        let origin = (strip.right() - used).max(strip.left());
        let pill_fill = egui::Color32::from_white_alpha(14);
        let hover_fill = egui::Color32::from_white_alpha(32);
        let mut selected = None;
        for (slot, w) in self.workspaces.iter().skip(start).take(visible).enumerate() {
            let cell = egui::Rect::from_min_size(
                egui::pos2(origin + slot as f32 * pitch, strip.top()),
                egui::vec2(WORKSPACE_CELL, strip.height()),
            )
            .intersect(strip);
            // CLICK without FOCUSABLE: Tab must stay with the terminal, not
            // move egui keyboard focus onto the bar.
            let response = ui.interact(
                cell,
                ui.id().with(("workspace-bar", w.id)),
                egui::Sense::CLICK,
            );
            let active = w.id == self.current;
            let occupied = !w.panes.is_empty();
            // Active is filled with the accent, occupied gets a subtle fill,
            // and empty workspaces are only outlined.
            let (fill, color) = if active {
                (palette.accent, palette.background)
            } else if response.hovered() {
                (hover_fill, palette.text)
            } else if occupied {
                (pill_fill, palette.text)
            } else {
                (egui::Color32::TRANSPARENT, palette.muted_text)
            };
            painter.rect_filled(cell, 4.0, fill);
            if fill == egui::Color32::TRANSPARENT {
                painter.rect_stroke(
                    cell,
                    4.0,
                    egui::Stroke::new(1.0, pill_fill),
                    egui::StrokeKind::Inside,
                );
            }
            let label = if w.index == 10 {
                "0".to_owned()
            } else {
                w.index.to_string()
            };
            painter.text(
                cell.center(),
                egui::Align2::CENTER_CENTER,
                label,
                font.clone(),
                color,
            );
            if response.clicked() {
                selected = Some(w.id);
            }
            response.on_hover_text(format!(
                "{} · {} · {} panes",
                w.index,
                w.layout.tag(),
                w.panes.len()
            ));
        }
        if overflow && slots > 1 {
            let cell = egui::Rect::from_min_size(
                egui::pos2(origin + visible as f32 * pitch, strip.top()),
                egui::vec2(WORKSPACE_CELL, strip.height()),
            );
            let response = ui
                .interact(cell, ui.id().with("workspace-overflow"), egui::Sense::CLICK)
                .on_hover_text("All workspaces");
            if response.hovered() {
                painter.rect_filled(cell, 4.0, hover_fill);
            }
            painter.text(
                cell.center(),
                egui::Align2::CENTER_CENTER,
                format!("+{}", self.workspaces.len() - visible),
                egui::FontId::monospace(WORKSPACE_FONT_SIZE - 2.0),
                palette.muted_text,
            );
            if response.clicked() {
                self.open_workspace_picker(ui.ctx(), false);
            }
        }
        ui.interact(
            detail_rect,
            ui.id().with("workspace-layout-status"),
            egui::Sense::hover(),
        )
        .on_hover_text("Layout and focused pane / total panes");
        if let Some(id) = selected {
            let _ = self.select_workspace(id);
        }
    }
    /// Runs the Lua `startup` callback for a newly created window, before its
    /// first frame.
    pub(crate) fn run_startup(&mut self, ctx: &egui::Context, gl: &glow::Context) {
        if let Some(callback) = self.picker_bindings.startup.clone() {
            let result = callback.with_host(&mut |r| self.host_request(r, gl), || {
                callback.call_action(String::new())
            });
            // Its output (toasts, text) goes to the pane that has focus after it.
            if let Some(pane) = self.active_pane().and_then(|p| self.pane_mut(p)) {
                pane.finish_lua_action(result, ctx);
            }
            self.post_requests();
        }
    }
    /// Pane lifecycle work before building a frame: releases terminals closed
    /// during the previous frame, whose paint callbacks have run or been
    /// dropped by now, then closes panes whose shell exited. Those are released
    /// before the next frame, like any other close.
    pub(crate) fn before_frame(&mut self) {
        for mut p in self.closed_panes.drain(..) {
            p.destroy();
        }
        let exited: Vec<_> = self
            .workspaces
            .iter()
            .flat_map(|w| w.panes.iter())
            .filter(|id| self.tiled_ref(**id).is_some_and(|p| p.should_close()))
            .copied()
            .collect();
        for id in exited {
            let _ = self.close_tiled(id);
        }
        let exited: Vec<_> = self
            .floating
            .entries
            .iter()
            .filter(|p| p.should_close())
            .map(|p| p.id)
            .collect();
        self.close_floats(exited);
    }
    pub(super) fn update_workspaces(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        let ctx = ui.ctx().clone();
        if self.should_close() {
            return;
        }
        let context = self.active_pane_ref().action_context();
        let (shortcut, consumed) = ui.input_mut(|i| {
            self.picker_bindings
                .consume_when(&mut i.events, |a, m| context.allows(a, m))
        });
        let shortcut = match shortcut {
            Some(a) if a.is_global() => {
                self.execute_action(a, false, &ctx, gl);
                None
            }
            a => a,
        };
        // A global shortcut may have closed the window's last pane.
        if self.should_close() {
            return;
        }
        let available = ui.max_rect();
        // Workspaces share the existing status row; reserve only one bar.
        let status_height = 24.0_f32.min(available.height());
        let status = egui::Rect::from_min_max(
            egui::pos2(available.left(), available.bottom() - status_height),
            available.max,
        );
        let content = egui::Rect::from_min_max(available.min, status.right_top());
        for (_, pane) in &mut self.tiled {
            pane.workspace_bounds = Some(content);
        }
        for pane in &mut self.floating.entries {
            pane.workspace_bounds = Some(content);
        }
        let mut style = self
            .workspace()
            .appearance
            .as_ref()
            .unwrap_or(&self.picker_bindings.workspace_appearance)
            .clone();
        let floating_style = style.clone();
        if self.workspace().panes.len() == 1 {
            style.inner_gap = 0.0;
            style.outer_gap = 0.0;
            style.corner_radius = 0.0;
            style.shadow_size = 0.0;
        }
        let rects = self.workspace().rectangles(content, &style);
        let scale = ctx.pixels_per_point();
        let floating_active = self.floats().visible().is_some();
        let focused_window = ui.input(|i| i.viewport().focused.unwrap_or(false));
        if !floating_active && !self.active_pane_ref().workspace_picker_active() {
            if let Some(pos) = ui.input(|i| {
                if i.pointer.any_pressed() {
                    i.pointer.interact_pos()
                } else {
                    None
                }
            }) {
                if let Some((id, _)) = rects.iter().find(|(_, r)| r.contains(pos)) {
                    self.workspace_mut().focus(*id);
                }
            }
        }
        let focus = self.focused_id();
        // Activate the destination first: deactivating the old pane first can
        // briefly blur the shared native app and wake every terminal renderer.
        let target = self.active_pane();
        if let Some(pane) = target {
            if let Some(p) = self.pane_mut(pane) {
                p.set_active(&ctx, true, focused_window);
            }
        }
        for (id, p) in &mut self.tiled {
            if target != Some(PaneRef::Tiled(*id)) {
                p.set_active(&ctx, false, focused_window);
            }
        }
        for p in &mut self.floating.entries {
            if target != Some(PaneRef::Floating(p.id)) {
                p.set_active(&ctx, false, focused_window);
            }
        }
        for (id, r) in &rects {
            if r.width() < 1.0 || r.height() < 1.0 {
                continue;
            }
            if style.inner_gap > 0.0 || style.outer_gap > 0.0 {
                style.paint_frame(ui.painter(), *r, focus == Some(*id) && !floating_active);
            }
            let padding = style
                .padding
                .min((r.width().min(r.height()) - 1.0).max(0.0) / 2.0);
            let inner = r.shrink(padding);
            ui.painter()
                .rect_filled(*r, style.corner_radius, crate::colors::palette().background);
            if let Some(p) = self.tiled_mut(*id) {
                p.update_modes(&ctx);
                if p.resize(inner, scale) {
                    p.refresh_decorations();
                }
                p.set_corner_radius((style.corner_radius - padding).max(0.0) * scale);
                ui.painter().add(egui::PaintCallback {
                    rect: inner,
                    callback: p.paint_callback(),
                });
            }
        }
        for (id, r) in &rects {
            if floating_active || focus != Some(*id) {
                if let Some(p) = self.tiled_mut(*id) {
                    p.show_background_overlays(&ctx, *r);
                }
            }
        }
        let floating_rect = Self::floating_rect(content, self.floating_padding);
        let floating_inner = floating_style.content_rect(floating_rect);
        let visible = self.floats().visible();
        for p in &mut self.floating.entries {
            let active = Some(p.id) == visible;
            if active {
                p.resize(floating_inner, scale);
                let padding = (floating_rect.width() - floating_inner.width()) / 2.0;
                p.set_corner_radius((floating_style.corner_radius - padding).max(0.0) * scale);
                p.update_modes(&ctx);
            }
        }
        if let Some(a) = shortcut {
            self.execute_action(a, floating_active, &ctx, gl);
        }
        let origin = self.active_pane();
        let bounds = if self.floats().visible().is_some() {
            Some(floating_inner)
        } else {
            self.focused_id().and_then(|id| {
                rects.iter().find(|(i, _)| *i == id).map(|(_, r)| {
                    r.shrink(
                        style
                            .padding
                            .min((r.width().min(r.height()) - 1.0).max(0.0) / 2.0),
                    )
                })
            })
        };
        let bindings = self.picker_bindings.clone();
        #[cfg(any(windows, unix))]
        {
            let interactive = |pane| origin == Some(pane);
            for (id, p) in &mut self.tiled {
                p.handle_pipe_requests(&ctx, &bindings, interactive(PaneRef::Tiled(*id)));
            }
            for p in &mut self.floating.entries {
                let active = interactive(PaneRef::Floating(p.id));
                p.handle_pipe_requests(&ctx, &bindings, active);
            }
        }
        let mut action = None;
        // Escape dismisses the window's toasts along with the focused pane's.
        let window_toasts = !self.toasts.is_empty();
        let mut toasts_dismissed = false;
        if let (Some(origin), Some(bounds)) = (origin, bounds) {
            let bindings = self.picker_bindings.clone();
            if let Some(p) = self.pane_mut(origin) {
                toasts_dismissed = !consumed
                    && (p.has_toasts() || window_toasts)
                    && p.escape_dismisses_toasts()
                    && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                if toasts_dismissed {
                    p.dismiss_toasts();
                }
                let input = p.handle_input(ui, bounds, &bindings);
                p.forward_input(
                    ui,
                    bounds,
                    input.keyboard_blocked || consumed,
                    input.suppress_forward || consumed,
                );
                action = input.action;
                p.show_toasts(&ctx, bounds);
            }
            self.run_picker_callback(origin, gl, &ctx);
        }
        if toasts_dismissed {
            self.toasts.clear();
        }
        if let Some(action) = action {
            self.execute_action(action, self.floats().visible().is_some(), &ctx, gl);
        }
        self.show_mode_status(ui, status);
        self.show_floating(ui, content, floating_rect, &floating_style);
        for (_, p) in &self.tiled {
            p.drain_clipboard(&ctx);
        }
        for p in &self.floating.entries {
            p.drain_clipboard(&ctx);
        }
        self.toasts
            .show(&ctx, content, egui::Id::new("window-toasts"), None, 0.0);
    }
}
#[cfg(test)]
mod tests {
    use super::WorkspaceAction;

    #[test]
    fn workspace_action_names_round_trip() {
        for action in WorkspaceAction::ALL.iter().copied().chain([
            WorkspaceAction::SelectWorkspace(3),
            WorkspaceAction::MoveToWorkspace(10),
        ]) {
            assert_eq!(WorkspaceAction::parse(&action.name()), Some(action));
        }
        assert_eq!(
            WorkspaceAction::parse("SetHorizontalDeck"),
            Some(WorkspaceAction::SetHorizontalGrid)
        );
        for invalid in [
            "",
            "focusleft",
            "SelectWorkspace:0",
            "MoveToWorkspace:x",
            "Unknown",
        ] {
            assert_eq!(WorkspaceAction::parse(invalid), None, "{invalid}");
        }
        assert_eq!(WorkspaceAction::FocusLeft.label(), "Focus left");
        assert_eq!(
            WorkspaceAction::FloatOrTilePane.label(),
            "Float or tile pane"
        );
        assert_eq!(
            WorkspaceAction::SelectWorkspace(3).label(),
            "Select workspace 3"
        );
    }
}

use super::{
    PaneRef, TerminalView, floating, pane_resize,
    resize_keys::{self, KeyOverlay},
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
) -> crate::picker::config::Options {
    use crate::picker::config::*;
    Options {
        scope: Scope::Workspace,
        position: Position::Bottom,
        sort: Sort::Score,
        source: Source::Workspaces(
            workspaces
                .iter()
                .map(|w| crate::actions::Entry {
                    id: format!("workspace.{}", w.id),
                    label: w.index.to_string(),
                    shortcut: None,
                    // Name, state, layout and panes. What runs, and what to
                    // call one without a name, is for whoever has its panes
                    // to say.
                    cells: vec![
                        w.name.clone().unwrap_or_default(),
                        if w.id == current { "current" } else { "" }.to_owned(),
                        w.layout.tag().to_owned(),
                        w.panes.len().to_string(),
                    ],
                    action: AppAction::SelectWorkspaceId(w.id),
                })
                .collect(),
        ),
        ..Options::default()
    }
}
/// How much of the window's height the picker of workspaces takes. Most of
/// it is the preview, where the selected workspace is drawn small.
const PICKER_HEIGHT: f32 = 0.75;
/// How long a picker says what it was told to: `nfm_egui`'s notice.
const PICKER_NOTICE: std::time::Duration = std::time::Duration::from_secs(2);
/// A picker over rows that each run an action, placed like the workspace
/// picker. Its preview is room left empty, for the selected
/// workspace to be drawn in.
pub(super) fn entry_picker(entries: Vec<crate::actions::Entry>) -> crate::picker::config::Options {
    use crate::picker::config::*;
    Options {
        scope: Scope::Workspace,
        position: Position::Bottom,
        sort: Sort::Score,
        source: Source::Workspaces(entries),
        preview: Some(Preview::Host),
        preview_visible: true,
        height: Some(PICKER_HEIGHT),
        ..Options::default()
    }
}
/// A workspace action. Config, Lua and the command palette name actions as
/// strings; `parse` converts them once and they stay typed from then on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceAction {
    CreateWorkspace,
    RemoveWorkspace,
    /// Ask for a name for the workspace.
    RenameWorkspace,
    /// Take the workspace out of the window; its shells stay in the daemon.
    DetachWorkspace,
    /// End every shell in the workspace.
    EndWorkspace,
    WorkspacePicker,
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
    /// Closes the visible float, or else the focused tiled pane.
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    /// Moves the focused pane as far as it goes that way; with a float shown,
    /// moves the float to that edge of the workspace.
    MoveFarLeft,
    MoveFarRight,
    MoveFarUp,
    MoveFarDown,
    /// Centres the shown float in the workspace, keeping its size.
    CenterFloat,
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
    /// Moves a divider; with a float shown, its left or top edge.
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    /// Like `Resize*`, but with a float shown, moves its right or bottom edge.
    ResizeFarLeft,
    ResizeFarRight,
    ResizeFarUp,
    ResizeFarDown,
}
impl WorkspaceAction {
    /// The actions without an index argument, in command palette order.
    pub(crate) const ALL: &[Self] = &[
        Self::CreateWorkspace,
        Self::RemoveWorkspace,
        Self::RenameWorkspace,
        Self::DetachWorkspace,
        Self::EndWorkspace,
        Self::WorkspacePicker,
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
        Self::MoveFarLeft,
        Self::MoveFarRight,
        Self::MoveFarUp,
        Self::MoveFarDown,
        Self::CenterFloat,
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
        Self::ResizeFarLeft,
        Self::ResizeFarRight,
        Self::ResizeFarUp,
        Self::ResizeFarDown,
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
            RenameWorkspace => "RenameWorkspace".into(),
            DetachWorkspace => "DetachWorkspace".into(),
            EndWorkspace => "EndWorkspace".into(),
            WorkspacePicker => "WorkspacePicker".into(),
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
            MoveFarLeft => "MoveFarLeft".into(),
            MoveFarRight => "MoveFarRight".into(),
            MoveFarUp => "MoveFarUp".into(),
            MoveFarDown => "MoveFarDown".into(),
            CenterFloat => "CenterFloat".into(),
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
            ResizeFarLeft => "ResizeFarLeft".into(),
            ResizeFarRight => "ResizeFarRight".into(),
            ResizeFarUp => "ResizeFarUp".into(),
            ResizeFarDown => "ResizeFarDown".into(),
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
    /// The `Far` version of a `Resize*` or `Move*` action, which a key
    /// table's `far` modifier runs in its place.
    pub(crate) fn far(self) -> Option<Self> {
        use WorkspaceAction::*;
        Some(match self {
            ResizeLeft => ResizeFarLeft,
            ResizeRight => ResizeFarRight,
            ResizeUp => ResizeFarUp,
            ResizeDown => ResizeFarDown,
            MoveLeft => MoveFarLeft,
            MoveRight => MoveFarRight,
            MoveUp => MoveFarUp,
            MoveDown => MoveFarDown,
            _ => return None,
        })
    }
    /// Whether this is one of the `Resize*` or `ResizeFar*` actions.
    pub(crate) fn is_resize(self) -> bool {
        use WorkspaceAction::*;
        matches!(
            self,
            ResizeLeft
                | ResizeRight
                | ResizeUp
                | ResizeDown
                | ResizeFarLeft
                | ResizeFarRight
                | ResizeFarUp
                | ResizeFarDown
        )
    }
    /// Whether this is one of the `Move*` or `MoveFar*` actions.
    pub(crate) fn is_move(self) -> bool {
        use WorkspaceAction::*;
        matches!(
            self,
            MoveLeft
                | MoveRight
                | MoveUp
                | MoveDown
                | MoveFarLeft
                | MoveFarRight
                | MoveFarUp
                | MoveFarDown
        )
    }
    /// Whether holding the key runs the action again on each auto-repeat.
    /// Navigation and resizing repeat; creating, closing and switching do
    /// not. Every variant is listed so a new action must choose.
    pub(crate) fn allows_key_repeat(self) -> bool {
        use WorkspaceAction::*;
        match self {
            FocusLeft | FocusRight | FocusUp | FocusDown | MoveLeft | MoveRight | MoveUp
            | MoveDown | MoveFarLeft | MoveFarRight | MoveFarUp | MoveFarDown | ResizeLeft
            | ResizeRight | ResizeUp | ResizeDown | ResizeFarLeft | ResizeFarRight
            | ResizeFarUp | ResizeFarDown => true,
            CreateWorkspace
            | RemoveWorkspace
            | RenameWorkspace
            | DetachWorkspace
            | EndWorkspace
            | WorkspacePicker
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
            | ResetLayout
            | CenterFloat => false,
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
/// The default `key_tables`, `resize` and `move`. Each leaves on Enter or
/// Escape, and a config's table of the same name merges over it.
///
/// `resize`, entered with Ctrl+Shift+R: h/j/k/l or the arrows resize like
/// `Resize*`, which with a float shown moves its left and top edges. Its
/// `far` modifier is Shift, which makes them `ResizeFar*`, moving the right
/// and bottom edges. `=` resets sizes like `ResetLayout`, and `m` switches
/// to `move`.
///
/// `move`, entered with `m` from `resize`: the same keys move like `Move*`,
/// swapping the focused pane with its neighbour or moving the shown float;
/// with Shift, `MoveFar*` moves it as far as it goes. `c` centres a float
/// like `CenterFloat`, and `r` switches to `resize`.
///
/// In both, Ctrl is the `nudge` modifier, for a much smaller step, and Alt
/// with those keys moves focus like `Focus*`, to pick another pane, or with
/// a float shown, cycles the floats.
pub(crate) fn default_key_tables()
-> std::collections::BTreeMap<String, crate::picker::config::KeyTable> {
    use crate::config::keybindings::{BindingTrigger, KeyBinding, parse_key};
    use crate::picker::config::{BindingTarget, KeyTable};
    use WorkspaceAction::*;
    let binding = |key: &str, mods: egui::Modifiers, action: AppAction| KeyBinding {
        trigger: BindingTrigger::Key(parse_key(key).unwrap()),
        mods,
        description: String::new(),
        action: BindingTarget::Action(action),
    };
    let none = egui::Modifiers::NONE;
    let alt = egui::Modifiers::ALT;
    let enter = |name: &str| AppAction::EnterKeyTable(name.to_owned());
    let mut resize = vec![
        binding("Enter", none, AppAction::ExitKeyTable),
        binding("=", none, AppAction::Workspace(ResetLayout)),
        binding("m", none, enter("move")),
    ];
    let mut moving = vec![
        binding("Enter", none, AppAction::ExitKeyTable),
        binding("c", none, AppAction::Workspace(CenterFloat)),
        binding("r", none, enter("resize")),
    ];
    for (keys, focus, size, step) in [
        (["h", "Left"], FocusLeft, ResizeLeft, MoveLeft),
        (["j", "Down"], FocusDown, ResizeDown, MoveDown),
        (["k", "Up"], FocusUp, ResizeUp, MoveUp),
        (["l", "Right"], FocusRight, ResizeRight, MoveRight),
    ] {
        for key in keys {
            resize.push(binding(key, none, AppAction::Workspace(size)));
            moving.push(binding(key, none, AppAction::Workspace(step)));
            for table in [&mut resize, &mut moving] {
                table.push(binding(key, alt, AppAction::Workspace(focus)));
            }
        }
    }
    let table = |keys| KeyTable {
        keys,
        far: Some(egui::Modifiers::SHIFT),
        nudge: Some(egui::Modifiers::CTRL),
    };
    [
        ("resize".to_owned(), table(resize)),
        ("move".to_owned(), table(moving)),
    ]
    .into()
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
/// Why a pane stays in its workspace: its shell is where it is.
const OTHER_PLACE: &str = "A pane cannot move to a workspace on another machine.";

impl TerminalView {
    /// The rows of the picker of workspaces and sessions: this window's
    /// workspaces, the sessions the daemon holds that no window shows, and
    /// a new one. With them, what they are rows of: an open picker is given
    /// its rows again when that changes, and not for a waiting shell's new
    /// title or directory, which its row shows as they were. Rows given
    /// again take the selection back to the first.
    pub(super) fn workspace_picker_rows(&self) -> (Vec<crate::actions::Entry>, String) {
        let window = self.has_panes();
        let options = workspace_picker_options(&self.workspaces, self.current);
        let mut entries = match options.source {
            crate::picker::config::Source::Workspaces(entries) if window => entries,
            _ => Vec::new(),
        };
        // Each float after the tiled panes, as a session's row counts them.
        for (entry, workspace) in entries.iter_mut().zip(&self.workspaces) {
            let floats = workspace.floats.ids().iter().filter_map(|id| self.floating.get(*id));
            let floats = floats.map(|float| float.panes.len());
            entry.cells[3] = super::restore::counted(workspace.panes.len(), floats);
            // One whose shells are on another machine says which.
            if let Some(place) = workspace.place.name() {
                entry.cells[1] = format!("{} on {place}", entry.cells[1]).trim_start().to_owned();
            }
        }
        // So far all a workspace's row says is the user's doing. What its
        // shells call themselves and where they are is left out of what the
        // rows are of, as for a session: see `session_rows_key`.
        let mut of: String =
            entries.iter().map(|row| format!("{:?}{:?}", row.label, row.cells)).collect();
        for (entry, workspace) in entries.iter_mut().zip(&self.workspaces) {
            let panes = workspace.panes.iter().filter_map(|id| self.tiled_ref(*id));
            let titles: Vec<String> = panes.clone().map(|pane| pane.title()).collect();
            let places: Vec<String> = panes.filter_map(|pane| pane.pwd()).collect();
            let (name, running) =
                super::restore::called(workspace.name.clone(), &titles, places.first());
            entry.cells[0] = name;
            entry.cells.push(running);
        }
        // Under the names, as it has no number.
        let new = |id: &str, name: &str, action| crate::actions::Entry {
            id: id.into(),
            label: String::new(),
            shortcut: None,
            cells: vec![name.into()],
            action,
        };
        // What each place's daemon holds that nothing shows: this machine's,
        // then each remote's. A place that is not connected has one row,
        // which connects to it: its sessions take that row's place once it
        // is. Nothing is connected to until it is asked for.
        use crate::session::{Place, Status};
        let current = self.current_place();
        let mut starts = Vec::new();
        let remotes = crate::session::remotes().into_iter().map(|remote| Place::Remote(remote.name));
        for place in std::iter::once(Place::Here).chain(remotes) {
            let status = crate::session::status(&place);
            let name = place.name().unwrap_or("this machine").to_owned();
            of.push_str(&format!("{name}{status:?}"));
            let row = |state: &str| crate::actions::Entry {
                id: format!("place.{name}"),
                label: String::new(),
                shortcut: None,
                cells: vec![name.clone(), state.to_owned()],
                action: AppAction::Connect(place.clone()),
            };
            match status {
                Status::Connected => {
                    if let Some(listing) = crate::session::sessions(&place) {
                        entries.extend(super::restore::session_rows(&listing));
                        of.push_str(&super::restore::session_rows_key(&listing));
                    }
                }
                // Shells of this machine that are not kept in a daemon.
                Status::Off => {}
                Status::Idle => starts.push(row("not connected: Enter connects")),
                Status::Connecting => starts.push(row("connecting\u{2026}")),
                Status::Lost => starts.push(row("connection lost: Enter connects again")),
                Status::Unavailable(reason) => {
                    let reason = reason.lines().last().unwrap_or_default().trim();
                    starts.push(row(&format!("cannot be reached: {reason}")));
                }
            }
            // A new workspace is made where the current one is, by the rows
            // below. Any other place that can take one has a row of its own.
            let takes = matches!(crate::session::status(&place), Status::Connected | Status::Off);
            if takes && (place != current || !window) && (window || place != crate::session::home()) {
                let label = format!("New session on {name}");
                let action = AppAction::NewSessionOn(place.clone());
                starts.push(new(&format!("session.new.{name}"), &label, action));
            }
        }
        // With folders to start one in, the picker goes on to list each it
        // finds. Without, one row makes a new workspace where the window is.
        if !self.picker_bindings.new_workspace_roots.is_empty() {
            of.push_str("folders");
        } else if window {
            let create = AppAction::Workspace(WorkspaceAction::CreateWorkspace);
            // It is made where the current one is.
            let label = match current.name() {
                Some(place) => format!("New workspace on {place}"),
                None => "New workspace".into(),
            };
            entries.push(new("workspace.new", &label, create));
        } else {
            // A window with no terminal starts a shell once its picker has
            // gone, whatever was chosen: see `chosen`.
            entries.push(new("session.new", "New session", AppAction::NewSession));
        }
        entries.extend(starts);
        of.push_str(if window { "window" } else { "launch" });
        (entries, of)
    }

    /// Open the picker of workspaces and sessions. The window holds it,
    /// with or without a terminal: it is about no pane. Its rows are given
    /// to it, so it reads no terminal.
    pub(super) fn open_workspace_picker(&mut self, ctx: &egui::Context) {
        self.close_window_picker();
        let (rows, of) = self.workspace_picker_rows();
        self.window_picker_rows = of;
        self.window_picker = Some(Box::new(crate::picker::Picker::start_replacing(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            entry_picker(rows),
            &self.picker_bindings.prompt_patterns,
            None,
        )));
        // After those rows, the folders a new one can be started in, which
        // this machine's daemon finds: see `show_window_picker`.
        if !self.picker_bindings.new_workspace_roots.is_empty()
            && let Some(picker) = self.window_picker.as_mut()
        {
            self.window_folders = picker.list_folders(ctx).map(|rows| (rows, None));
        }
        // One opened again as a window empties says what emptied it.
        if let (Some(picker), Some((said, at))) = (self.window_picker.as_mut(), &self.picker_said)
            && at.elapsed() < PICKER_NOTICE
        {
            picker.notify(ctx, said.clone());
        }
        ctx.request_repaint();
    }

    pub(super) fn close_window_picker(&mut self) {
        self.window_folders = None;
        if let Some(picker) = self.window_picker.take() {
            picker.surrender_focus();
            self.context.request_repaint();
        }
    }

    /// A frame of the window's picker, across `content`. Returns what was
    /// chosen, or that it was left, once: the picker has then gone.
    pub(super) fn show_window_picker(
        &mut self,
        ctx: &egui::Context,
        content: egui::Rect,
    ) -> Option<crate::picker::Action> {
        self.window_picker.as_ref()?;
        // Text is sized to a terminal's cells, when there is a terminal.
        let typography = self
            .active_pane()
            .and_then(|pane| self.pane_mut(pane))
            .map(|pane| pane.picker_typography(ctx));
        let (rows, of) = self.workspace_picker_rows();
        let builtin = self.picker_bindings.builtin_pickers;
        let padding = self.picker_bindings.cursor_scroll_padding;
        let picker = self.window_picker.as_mut()?;
        if let Some(typography) = typography {
            picker.typography = typography;
        }
        picker.builtin_pickers = builtin;
        picker.set_cursor_scroll_padding(padding);
        // The folders it lists are found by this machine's daemon, for what
        // is typed. Asked for once there is a connection, and again over a
        // new one.
        if let Some((rows, search)) = &mut self.window_folders {
            if !search.as_ref().is_some_and(|search| search.live()) {
                let bindings = &self.picker_bindings;
                let roots = bindings.new_workspace_roots.iter().map(|root| root.to_string_lossy().into_owned());
                let to = rows.clone();
                let found = move |found: Vec<String>, done| to.set(&found, done);
                let here = crate::session::Place::Here;
                *search = crate::session::folders(&here, roots.collect(), bindings.new_workspace_depth, found);
            }
            if let Some(search) = search {
                search.typed(picker.typed());
            }
        }
        // The daemon says when the sessions change. Rows of what the picker
        // already has rows of are left alone.
        if of != self.window_picker_rows {
            self.window_picker_rows = of;
            picker.set_entries(ctx, rows);
            // New rows take away what the picker was saying.
            if let Some((said, at)) = &self.picker_said
                && at.elapsed() < PICKER_NOTICE
            {
                picker.notify(ctx, said.clone());
            }
        }
        let action = picker.show_scoped(ctx, content, None, true)?;
        // Its rows run no Lua.
        if matches!(action, crate::picker::Action::Custom(_)) {
            return None;
        }
        // Done to a row with the picker left open. Its rows change as the
        // window and the daemon do, and it says what was done.
        if let crate::picker::Action::Stay { action, said } = action {
            let done = match action {
                AppAction::DetachWorkspaceId(id) => self.detach_workspace(id),
                AppAction::EndWorkspaceId(id) => Ok(self.end_workspace(id)),
                AppAction::EndSession(held) => Ok(self.end_session(&held)),
                _ => Ok(()),
            };
            match done {
                Ok(()) => {
                    if let Some(picker) = self.window_picker.as_mut() {
                        picker.notify(ctx, said.clone());
                    }
                    self.picker_said = Some((said, std::time::Instant::now()));
                }
                Err(error) => self.show_error("Workspace action failed", error),
            }
            return None;
        }
        self.close_window_picker();
        Some(action)
    }
    /// Make a workspace whose first shell starts in `cwd`, and go to it.
    /// One started in a chosen directory is called after it.
    pub(super) fn create_workspace_in(
        &mut self,
        cwd: Option<String>,
        gl: &glow::Context,
    ) -> Result<(), String> {
        let name = cwd.as_deref().and_then(|cwd| workspace::named_after(cwd, &self.workspaces));
        // A folder chosen is this machine's, and so is the workspace.
        let options = workspace::CreateOptions::in_folder(cwd);
        let w = self.workspace_request(Request::Create(options), gl)?;
        let id = w["id"].as_u64().unwrap();
        if let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == id) {
            workspace.name = name;
        }
        self.select_workspace(id)
    }
    /// Make a workspace whose shells run in `place`, and go to it.
    pub(super) fn create_workspace_on(
        &mut self,
        place: crate::session::Place,
        gl: &glow::Context,
    ) -> Result<(), String> {
        let options = workspace::CreateOptions { place: Some(place), ..Default::default() };
        let w = self.workspace_request(Request::Create(options), gl)?;
        self.select_workspace(w["id"].as_u64().unwrap())
    }
    /// Ask what to call a workspace, starting from what it is called now.
    /// Nothing typed takes its name away.
    pub(super) fn rename_workspace_prompt(&mut self, id: u64, ctx: &egui::Context) {
        use crate::picker::config::{PickerEntry, PromptAction, PromptOptions, Scope};
        let Some(workspace) = self.workspaces.iter().find(|w| w.id == id) else {
            return;
        };
        let options = PromptOptions {
            scope: Scope::Workspace,
            position: Default::default(),
            action: PromptAction::App(std::rc::Rc::new(move |name| {
                AppAction::RenameWorkspace { id, name }
            })),
            params: workspace
                .name
                .iter()
                .map(|name| ("query".to_owned(), name.clone()))
                .collect(),
            prompt: Some(format!("Name for workspace {}:", workspace.index)),
            placeholder: Some("nothing, for no name".into()),
        };
        let bindings = self.picker_bindings.clone();
        let palette = self.palette_context();
        if let Some(pane) = self.active_pane().and_then(|p| self.pane_mut(p)) {
            pane.open_picker(ctx, PickerEntry::Prompt(options), None, &bindings, &palette);
        }
    }
    pub(super) fn rename_workspace(&mut self, id: u64, name: &str) {
        if let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == id) {
            workspace.name = workspace::named(name);
            self.context.request_repaint();
        }
    }
    pub(super) fn workspace(&self) -> &Workspace {
        self.workspaces
            .iter()
            .find(|w| w.id == self.current)
            .expect("current workspace")
    }
    pub(super) fn workspace_mut(&mut self) -> &mut Workspace {
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
            self.close_window_picker();
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
    /// The current workspace's floats and every workspace, for a command
    /// palette about to open. Labels capture float titles at this moment.
    pub(super) fn palette_context(&self) -> super::PaletteContext {
        let floats = self.floats();
        let mut entries: Vec<_> = floats
            .ids()
            .iter()
            .filter_map(|id| Some((self.floating.get(*id)?, self.float_pane(*id)?)))
            .map(|(p, pane)| {
                crate::actions::floating_pane_entry(
                    p.id,
                    &pane.title(),
                    floats.visible() == Some(p.id),
                )
            })
            .collect();
        entries.extend(self.workspaces.iter().map(|w| crate::actions::Entry {
            id: format!("workspace.{}", w.id),
            label: format!("Workspace {}", w.index),
            shortcut: None,
            cells: Vec::new(),
            action: AppAction::SelectWorkspaceId(w.id),
        }));
        super::PaletteContext {
            entries,
            config_issues: self.messages.config_issues().to_vec(),
        }
    }
    pub(super) fn visible_float(&self) -> Option<&floating::Entry<workspace::Group>> {
        self.floating.get(self.floats().visible()?)
    }
    /// The pane a float has focus on: the one that is the float wherever a
    /// float is treated as one terminal.
    pub(super) fn float_pane_id(&self, float: u64) -> Option<u64> {
        let group = &self.floating.get(float)?.value;
        group.focused.or_else(|| group.panes.first().copied())
    }
    pub(super) fn float_pane(&self, float: u64) -> Option<&super::TerminalPane> {
        self.tiled_ref(self.float_pane_id(float)?)
    }
    pub(super) fn visible_float_pane(&self) -> Option<&super::TerminalPane> {
        self.float_pane(self.floats().visible()?)
    }
    /// Keeps a terminal as a pane of this window and returns its new id.
    /// Which group shows it is for the caller to say.
    pub(super) fn add_terminal(&mut self, pane: super::TerminalPane) -> u64 {
        let id = self.next_pane;
        self.next_pane += 1;
        self.tiled.push((id, pane));
        id
    }
    /// Takes a terminal out of this window. Its group is for the caller to
    /// take it out of.
    pub(super) fn take_terminal(&mut self, id: u64) -> Option<super::TerminalPane> {
        let at = self.tiled.iter().position(|(i, _)| *i == id)?;
        Some(self.tiled.remove(at).1)
    }
    /// The group the pane keys act on: the shown float's, or else the
    /// workspace's tiled panes.
    pub(super) fn active_group(&self) -> &workspace::Group {
        match self.visible_float() {
            Some(float) => &float.value,
            None => &self.workspace().group,
        }
    }
    pub(super) fn active_group_mut(&mut self) -> &mut workspace::Group {
        match self.floats().visible() {
            Some(id) if self.floating.get(id).is_some() => {
                &mut self.floating.get_mut(id).expect("the float just found").value
            }
            _ => &mut self.workspace_mut().group,
        }
    }
    /// Whether a float is shown that is one pane. The move and resize keys
    /// then act on the float itself, as there is nothing in it to move.
    pub(super) fn float_is_whole(&self) -> bool {
        self.visible_float().is_some_and(|float| float.panes.len() <= 1)
    }
    /// How the panes of a float are laid out inside it. The float's frame is
    /// around them all, so they keep a gap from it and cast no shadows.
    pub(super) fn float_group_style(style: &workspace::Appearance) -> workspace::Appearance {
        let mut style = style.clone();
        style.outer_gap = style.inner_gap;
        style.shadow_size = 0.0;
        style
    }
    /// Where each pane of the shown float goes, in the float's `rect`. One
    /// pane is the whole float.
    pub(super) fn float_rects(
        &self,
        rect: egui::Rect,
        style: &workspace::Appearance,
    ) -> Vec<(u64, egui::Rect)> {
        match self.visible_float() {
            Some(float) => Self::group_rects(float, rect, style),
            None => Vec::new(),
        }
    }
    /// Where each pane of a float's `group` goes, in the float's `rect`.
    pub(super) fn group_rects(
        group: &workspace::Group,
        rect: egui::Rect,
        style: &workspace::Appearance,
    ) -> Vec<(u64, egui::Rect)> {
        if let [only] = group.panes[..] {
            return vec![(only, rect)];
        }
        group.rectangles(rect, &Self::float_group_style(style))
    }
    /// A new pane in the group pane `from` is in, a float's or a workspace's
    /// tiled panes, wherever that is shown or not. It starts where `from`
    /// is unless `spawn` says. Returns the new pane's id.
    pub(super) fn create_pane_beside(
        &mut self,
        from: u64,
        mut spawn: super::pane::SpawnOptions,
        focus: bool,
        gl: &glow::Context,
    ) -> Result<u64, String> {
        spawn.validate()?;
        // In the place of the workspace `from` is in, tiled or in a float.
        let owner = self.workspaces.iter().find(|w| {
            let in_float = |float: &u64| {
                self.floating.get(*float).is_some_and(|entry| entry.panes.contains(&from))
            };
            w.panes.contains(&from) || w.floats.ids().iter().any(in_float)
        });
        let place = owner.map_or_else(|| self.current_place(), |w| w.place.clone());
        let local = place == crate::session::Place::Here;
        spawn.place = Some(place);
        if spawn.cwd.is_none() && local {
            spawn.cwd = self
                .tiled_ref(from)
                .and_then(|pane| pane.pwd())
                .filter(|pwd| std::path::Path::new(pwd).is_dir());
        }
        let float = self
            .floating
            .entries
            .iter()
            .find(|float| float.panes.contains(&from))
            .map(|float| float.id);
        if float.is_none() && !self.workspaces.iter().any(|w| w.panes.contains(&from)) {
            return Err("the pane that asked has gone".into());
        }
        let pane = self.create_pane(gl, &spawn)?;
        let id = self.add_terminal(pane);
        let group = match float.and_then(|float| self.floating.get_mut(float)) {
            Some(float) => &mut float.value,
            None => {
                let workspace = self.workspaces.iter_mut().find(|w| w.panes.contains(&from));
                &mut workspace.expect("the workspace just found").group
            }
        };
        group.panes.push(id);
        if focus || group.focused.is_none() {
            group.focus(id);
        }
        Ok(id)
    }
    /// Answers those who asked for a pane over the pipe, once it is known
    /// whether its command started. A command that could not be started
    /// leaves a pane that never has a process: one in the session daemon
    /// closes, and one here stays to say that it is of no use. Either is an
    /// error to whoever asked, not a pane, and the pane goes. Called before
    /// the panes that are to close are closed, while they can still say.
    #[cfg(any(windows, unix))]
    fn answer_pipe_panes(&mut self) {
        /// How long a command has to start. Starting one takes far less, so
        /// one with no process by then is taken not to have started.
        const PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);
        let waiting = std::mem::take(&mut self.pipe_panes_starting);
        for starting in waiting {
            let id = starting.pane;
            let pane = self.tiled_ref(id);
            let started = pane.is_some_and(|pane| pane.shell_started());
            let gone = pane.is_none_or(|pane| pane.should_close());
            if started {
                starting.session.finish(
                    "accepted",
                    serde_json::json!({"selected":[{"id":id.to_string(),"text":"","value":id}]}),
                );
            } else if gone || starting.since.elapsed() >= PATIENCE {
                if !gone {
                    let float = self.floating.entries.iter().find(|float| float.panes.contains(&id));
                    match float.map(|float| float.id) {
                        Some(float) => self.close_float_pane(float, id),
                        None => drop(self.close_tiled(id)),
                    }
                }
                let what = starting.command.as_deref().unwrap_or("the shell");
                starting.session.finish(
                    "error",
                    serde_json::json!({
                        "code":"pane_failed",
                        "message":format!("could not start {what:?}; the error log may say why"),
                    }),
                );
            } else {
                self.pipe_panes_starting.push(starting);
            }
        }
        if !self.pipe_panes_starting.is_empty() {
            self.context.request_repaint_after(std::time::Duration::from_millis(20));
        }
    }
    #[cfg(not(any(windows, unix)))]
    fn answer_pipe_panes(&mut self) {}
    /// A new float around a pane this window already has. Which workspace
    /// it belongs to is for the caller to say.
    pub(super) fn float_around(&mut self, pane: u64) -> u64 {
        let mut group = workspace::Group::default();
        group.layout = self.picker_bindings.default_layout;
        group.panes.push(pane);
        group.focused = Some(pane);
        self.floating.insert(group)
    }
    /// A new float around a new terminal.
    pub(super) fn add_float(&mut self, pane: super::TerminalPane) -> u64 {
        let pane = self.add_terminal(pane);
        self.float_around(pane)
    }
    /// Closes floats in any workspace. Their terminals are released next
    /// frame, after this frame's paint callbacks.
    pub(super) fn close_floats(&mut self, ids: impl IntoIterator<Item = u64>) {
        for id in ids {
            for w in &mut self.workspaces {
                w.floats.remove(id);
            }
            for pane in self.floating.remove(id).map(|group| group.panes).unwrap_or_default() {
                if let Some(pane) = self.take_terminal(pane) {
                    self.closed_panes.push(pane);
                }
            }
        }
    }
    /// Closes one pane of a float, and the float with its last.
    pub(super) fn close_float_pane(&mut self, float: u64, pane: u64) {
        let Some(entry) = self.floating.get_mut(float) else {
            return;
        };
        if entry.panes.len() <= 1 {
            return self.close_floats([float]);
        }
        entry.value.remove(pane);
        if let Some(pane) = self.take_terminal(pane) {
            self.closed_panes.push(pane);
        }
    }
    /// Workspaces whose last tiled pane has gone go too, unless they have a
    /// float: its panes are then the workspace's tiled ones, laid out as
    /// they were, and the workspace stays. The float that is showing is the
    /// one taken, or else the first; any others stay floats.
    pub(super) fn prune_workspaces(&mut self) {
        for at in 0..self.workspaces.len() {
            let workspace = &self.workspaces[at];
            if !workspace.panes.is_empty() {
                continue;
            }
            let float = workspace.floats.visible().or(workspace.floats.ids().first().copied());
            let Some(float) = float else { continue };
            self.workspaces[at].floats.remove(float);
            if let Some(group) = self.floating.remove(float) {
                self.workspaces[at].group = group;
            }
        }
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
        let float = self.float_around(id);
        let w = self
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
            .unwrap();
        w.remove(id);
        w.floats.add(float);
        Ok((workspace, float))
    }
    /// Takes the pane a float has focus on out of the float, which goes when
    /// that was its last. The pane stays in the window, in no group.
    fn take_from_float(&mut self, float: u64) -> Option<u64> {
        let pane = self.float_pane_id(float)?;
        let group = &mut self.floating.get_mut(float)?.value;
        group.remove(pane);
        if group.panes.is_empty() {
            self.floating.remove(float);
            for w in &mut self.workspaces {
                w.floats.remove(float);
            }
        }
        Some(pane)
    }
    /// Moves the pane a float has focus on into its workspace's tiled panes,
    /// after the focused pane, and focuses it. Returns the pane's ID.
    pub(super) fn tile_float(&mut self, id: u64) -> Result<u64, String> {
        let workspace = self
            .workspaces
            .iter()
            .find(|w| w.floats.contains(id))
            .ok_or("floating pane does not exist")?
            .id;
        let pane_id = self
            .take_from_float(id)
            .ok_or("floating pane does not exist")?;
        let w = self
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
            .unwrap();
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
    pub(super) fn free_index(&self) -> usize {
        (1..)
            .find(|i| !self.workspaces.iter().any(|w| w.index == *i))
            .unwrap()
    }
    /// Adds a workspace without panes. The caller gives it a tiled pane
    /// before the next prune removes it.
    pub(super) fn push_workspace(
        &mut self,
        index: usize,
        layout: Option<Layout>,
    ) -> &mut Workspace {
        let id = self.next_workspace;
        self.next_workspace += 1;
        let mut w = Workspace::new(id, index);
        // Until its maker says otherwise.
        w.place = crate::session::home();
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
        let pane = match pane {
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
            PaneRef::Floating(id) => self
                .take_from_float(id)
                .and_then(|pane| self.take_terminal(pane))
                .ok_or("floating pane does not exist")?,
        };
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
    pub(super) fn close_tiled(&mut self, id: u64) -> Result<(), String> {
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
                let place = o.place.clone().unwrap_or_else(|| self.current_place());
                let spawn = super::pane::SpawnOptions {
                    cwd: o.cwd.clone(),
                    place: Some(place.clone()),
                    ..Default::default()
                };
                let pane = self.create_pane(gl, &self.beside_active(spawn))?;
                let pane_id = self.next_pane;
                self.next_pane += 1;
                self.tiled.push((pane_id, pane));
                let w = self.push_workspace(index, o.layout);
                w.place = place;
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
                let mut spawn = options.spawn();
                spawn.validate()?;
                let target = options.workspace.unwrap_or(self.current);
                if !self.workspaces.iter().any(|w| w.id == target) {
                    return Err("workspace does not exist".into());
                }
                spawn.place = Some(self.place_of(target));
                let pane = self.create_pane(gl, &self.beside_active(spawn))?;
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
                let Some(source) = self.workspaces.iter().find(|w| w.panes.contains(&id)) else {
                    return Err("pane does not exist".into());
                };
                if source.place != self.place_of(target) {
                    return Err(OTHER_PLACE.into());
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
                let place = self.place_of(target);
                let source = self
                    .workspaces
                    .iter_mut()
                    .find(|w| w.floats.contains(id))
                    .ok_or("floating pane does not exist")?;
                if source.place != place {
                    return Err(OTHER_PLACE.into());
                }
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
            Request::FocusDirection(direction) | Request::MovePaneDirection(direction) => {
                use super::workspace::Direction::*;
                use WorkspaceAction::*;
                let moving = matches!(request, Request::MovePaneDirection(_));
                let action = match (direction, moving) {
                    (Left, false) => FocusLeft,
                    (Right, false) => FocusRight,
                    (Up, false) => FocusUp,
                    (Down, false) => FocusDown,
                    (Left, true) => MoveLeft,
                    (Right, true) => MoveRight,
                    (Up, true) => MoveUp,
                    (Down, true) => MoveDown,
                };
                let ctx = self.context.clone();
                self.navigate(action, &ctx);
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
                    // Where the current one is. One in a folder is started
                    // from the picker of workspaces.
                    self.create_workspace_in(None, gl)?;
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
                WorkspacePicker => self.open_workspace_picker(ctx),
                RemoveWorkspace => {
                    self.workspace_request(Request::Remove(self.current), gl)?;
                }
                RenameWorkspace => self.rename_workspace_prompt(self.current, ctx),
                DetachWorkspace => self.detach_workspace(self.current)?,
                EndWorkspace => self.end_workspace(self.current),
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
                // Creates in the group that has focus: one more pane in the
                // float while one is shown.
                CreatePane => {
                    if self.floats().visible().is_some() {
                        let pane = self.create_pane(gl, &self.beside_active(Default::default()))?;
                        let id = self.add_terminal(pane);
                        let group = self.active_group_mut();
                        group.panes.push(id);
                        group.focus(id);
                    } else {
                        self.workspace_request(Request::CreatePane(Default::default()), gl)?;
                    }
                }
                ClosePane => {
                    if let Some(float) = self.floats().visible() {
                        if let Some(pane) = self.float_pane_id(float) {
                            self.close_float_pane(float, pane);
                        }
                    } else if let Some(id) = self.focused_id() {
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
                    let w = self.active_group_mut();
                    if let Some(i) = w.panes.iter().position(|p| Some(*p) == w.focused) {
                        let id = w.panes.remove(i);
                        w.panes.insert(0, id);
                    }
                }
                ResetLayout => {
                    let group = self.active_group_mut();
                    group.main_ratio = 0.5;
                    group.weights_mut().clear();
                    self.floats_mut().set_size(None);
                }
                ResizeLeft | ResizeRight | ResizeUp | ResizeDown | ResizeFarLeft
                | ResizeFarRight | ResizeFarUp | ResizeFarDown => {
                    let horizontal = matches!(
                        action,
                        ResizeLeft | ResizeRight | ResizeFarLeft | ResizeFarRight
                    );
                    let forward = matches!(
                        action,
                        ResizeRight | ResizeDown | ResizeFarRight | ResizeFarDown
                    );
                    let far = matches!(
                        action,
                        ResizeFarLeft | ResizeFarRight | ResizeFarUp | ResizeFarDown
                    );
                    // A shown float of one pane has only its own edges to
                    // move. A key table's nudge modifier makes a small step.
                    let nudge = std::mem::take(&mut self.nudge_step);
                    let whole = self.float_is_whole();
                    if !(whole && self.resize_float_with_keys(horizontal, far, forward, nudge)) {
                        self.active_group_mut()
                            .resize_by(horizontal, far, forward, nudge);
                    }
                }
                SetTile => self.active_group_mut().set_layout(Layout::Tile),
                SetTileReversed => self.active_group_mut().set_layout(Layout::TileReversed),
                SetMonocle => self.active_group_mut().set_layout(Layout::Monocle),
                SetDeck => self.active_group_mut().set_layout(Layout::Deck),
                SetGrid => self.active_group_mut().set_layout(Layout::Grid),
                SetHorizontalGrid => self.active_group_mut().set_layout(Layout::HorizontalGrid),
                CenterFloat => self.center_float(),
                FocusLeft | FocusRight | FocusUp | FocusDown | MoveLeft | MoveRight | MoveUp
                | MoveDown | MoveFarLeft | MoveFarRight | MoveFarUp | MoveFarDown => {
                    self.navigate(action, ctx)
                }
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
        let (motion, moving, far) = match action {
            FocusLeft => (Motion::Left, false, false),
            FocusRight => (Motion::Right, false, false),
            FocusUp => (Motion::Up, false, false),
            FocusDown => (Motion::Down, false, false),
            MoveLeft => (Motion::Left, true, false),
            MoveRight => (Motion::Right, true, false),
            MoveUp => (Motion::Up, true, false),
            MoveDown => (Motion::Down, true, false),
            MoveFarLeft => (Motion::Left, true, true),
            MoveFarRight => (Motion::Right, true, true),
            MoveFarUp => (Motion::Up, true, true),
            MoveFarDown => (Motion::Down, true, true),
            _ => return,
        };
        let bounds = ctx.content_rect();
        let style = self.picker_bindings.workspace_appearance.clone();
        // While a float is shown the keys act in it. Focus with nowhere to
        // go in it goes to the workspace's next float, and the move keys move
        // a float of one pane, as the resize keys resize it.
        if self.floats().visible().is_some() {
            if moving && self.float_is_whole() {
                let nudge = std::mem::take(&mut self.nudge_step);
                self.move_float_with_keys(motion, far, nudge);
                return;
            }
            let rect = self.floating_rect_for(bounds);
            if moving && far {
                let style = Self::float_group_style(&style);
                self.active_group_mut().move_far(motion, rect, &style);
                return;
            }
            let rects = self.float_rects(rect, &style);
            let changed = self.active_group_mut().navigate(motion, moving, &rects);
            if !changed && !moving {
                let next = matches!(motion, Motion::Down | Motion::Right);
                self.floats_mut().cycle(next);
            }
            return;
        }
        if far {
            self.workspace_mut().move_far(motion, bounds, &style);
            return;
        }
        let rects = self.workspace().rectangles(bounds, &style);
        self.workspace_mut().navigate(motion, moving, &rects);
    }
    fn workspace_bar_detail(&self, font: &egui::FontId) -> egui::text::LayoutJob {
        let palette = crate::colors::palette();
        // Of the group the pane keys act on: the shown float's, if any.
        let w = self.active_group();
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
                w.name.as_deref().map_or_else(
                    || w.index.to_string(),
                    |name| format!("{}: {name}", w.index)
                ),
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
                self.open_workspace_picker(ui.ctx());
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
    /// Pane lifecycle work before building a frame: releases terminals closed
    /// during the previous frame, whose paint callbacks have run or been
    /// dropped by now, then closes panes whose shell exited. Those are released
    /// before the next frame, like any other close.
    pub(crate) fn before_frame(&mut self) {
        for mut p in self.closed_panes.drain(..) {
            p.destroy();
        }
        self.save_sessions();
        self.answer_pipe_panes();
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
        let exited: Vec<(u64, u64)> = self
            .floating
            .entries
            .iter()
            .flat_map(|float| float.panes.iter().map(|pane| (float.id, *pane)))
            .filter(|(_, pane)| self.tiled_ref(*pane).is_some_and(|p| p.should_close()))
            .collect();
        for (float, pane) in exited {
            self.close_float_pane(float, pane);
        }
        self.note_daemon_loss();
        self.stay_for_sessions();
    }
    pub(super) fn update_workspaces(&mut self, ui: &mut egui::Ui, gl: &glow::Context) {
        let ctx = ui.ctx().clone();
        if self.should_close() {
            return;
        }
        if self.empty.is_some() {
            self.update_empty(ui, gl);
            return;
        }
        // Leaving the window leaves a key table, so returning to it does not
        // swallow typing unexpectedly.
        if ui.input(|i| i.viewport().focused == Some(false)) {
            self.key_table = None;
        }
        let (shortcut, consumed) = if let Some(table) = self.key_table.clone() {
            let input = ui.input_mut(|i| self.picker_bindings.consume_table(&table, &mut i.events));
            if input.exit {
                self.key_table = None;
            }
            self.nudge_step = input.nudge;
            (input.action, true)
        } else {
            self.nudge_step = false;
            let mut context = self.active_pane_ref().action_context();
            // The window's picker takes the keys a pane's would.
            context.picker |= self.window_picker.is_some();
            ui.input_mut(|i| {
                self.picker_bindings
                    .consume_when(&mut i.events, |a, m| context.allows(a, m))
            })
        };
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
        let scale = ctx.pixels_per_point();
        let floating_active = self.floats().visible().is_some();
        let focused_window = ui.input(|i| i.viewport().focused.unwrap_or(false));
        let picker_active = self.window_picker.is_some();
        // A shown float covers the tiled panes, so only its edges resize.
        let resizable = if floating_active {
            let rect = self.floating_rect_for(content);
            pane_resize::Resizable::Float {
                rect,
                inner: floating_style.content_rect(rect),
            }
        } else {
            pane_resize::Resizable::Panes
        };
        let resizing = self.resize_with_mouse(ui, content, &style, resizable, !picker_active);
        let rects = self.workspace().rectangles(content, &style);
        let floating_rect = self.floating_rect_for(content);
        // The shown float's panes, which are what a press can reach then.
        let float_rects = self.float_rects(floating_rect, &floating_style);
        if !picker_active && !resizing {
            if let Some(pos) = ui.input(|i| {
                if i.pointer.any_pressed() {
                    i.pointer.interact_pos()
                } else {
                    None
                }
            }) {
                let pressed = if floating_active { &float_rects } else { &rects };
                if let Some((id, _)) = pressed.iter().find(|(_, r)| r.contains(pos)) {
                    self.active_group_mut().focus(*id);
                }
            }
        }
        // A float of one pane is moved and resized whole; in one of several
        // the keys act on its panes.
        let float_whole = self.float_is_whole();
        let float_focus = self.visible_float().and_then(|float| float.focused);
        let float_focused_rect = float_rects
            .iter()
            .find(|(id, _)| float_focus == Some(*id))
            .copied();
        let focus = self.focused_id();
        // Activate the destination first: deactivating the old pane first can
        // briefly blur the shared native app and wake every terminal renderer.
        let target = self.active_pane();
        if let Some(pane) = target {
            if let Some(p) = self.pane_mut(pane) {
                p.set_active(&ctx, true, focused_window);
            }
        }
        let target_id = target.and_then(|pane| self.pane_id(pane));
        for (id, p) in &mut self.tiled {
            if target_id != Some(*id) {
                p.set_active(&ctx, false, focused_window);
            }
        }
        // A key table that resizes or moves marks what its keys act on: the
        // shown float, drawn with it, or else the focused pane. For resizing,
        // the edges its keys move, the table's `far` modifier picking between
        // two on the same axis; for moving, the sides it can move toward.
        let modifiers = self
            .key_table
            .as_deref()
            .map(|name| self.picker_bindings.key_table_modifiers(name))
            .unwrap_or_default();
        let held = ui.input(|i| i.modifiers);
        let far = modifiers
            .far
            .is_some_and(|far| crate::config::keybindings::holds(held, far));
        let focused_rect = rects.iter().find(|(id, _)| focus == Some(*id));
        let overlay = if self.key_table_resizes() {
            let edges = if float_whole {
                resize_keys::float_resize_edges(far)
            } else if floating_active {
                match float_focused_rect {
                    Some((id, r)) => {
                        let style = Self::float_group_style(&floating_style);
                        let dividers = self.active_group().dividers(floating_rect, &style);
                        resize_keys::tile_resize_edges(r, id, &dividers, far)
                    }
                    None => Vec::new(),
                }
            } else if let Some((id, r)) = focused_rect {
                let dividers = self.workspace().dividers(content, &style);
                resize_keys::tile_resize_edges(*r, *id, &dividers, far)
            } else {
                Vec::new()
            };
            KeyOverlay::Resize { edges, far }
        } else if self.key_table_moves() {
            let sides = if float_whole {
                resize_keys::float_sides(floating_rect, content)
            } else if floating_active {
                self.active_group().move_sides(&float_rects)
            } else {
                self.workspace().move_sides(&rects)
            };
            KeyOverlay::Move { sides }
        } else {
            KeyOverlay::None
        };
        // The resize and move overlay replaces the focus highlight, which is the same colour
        // in some themes and would compete with them.
        let overlaid = !overlay.is_none();
        for (id, r) in &rects {
            if r.width() < 1.0 || r.height() < 1.0 {
                continue;
            }
            if style.inner_gap > 0.0 || style.outer_gap > 0.0 {
                let highlight = focus == Some(*id) && !floating_active && !overlaid;
                style.paint_frame(ui.painter(), *r, highlight);
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
        if !overlay.is_none()
            && !floating_active
            && let Some((id, r)) = focused_rect
        {
            // Everything else recedes behind the pane the keys act on.
            let others = rects.iter().filter(|(i, _)| i != id).map(|(_, r)| *r);
            resize_keys::dim_others(ui.painter(), others, style.corner_radius);
            overlay.paint(ui.painter(), *r, style.corner_radius, style.border_width);
        }
        let key_hints = self.key_table.as_deref().map_or_else(Vec::new, |name| {
            let table = self.picker_bindings.key_table_keys(name);
            resize_keys::hints(&table, modifiers, overlay.edges(), far, float_whole)
        });
        // Layouts that hide panes show where the focused one sits among them.
        let w = self.workspace();
        let hides_panes = match w.layout {
            Layout::Monocle => w.panes.len() > 1,
            Layout::Deck => w.panes.len() > 2,
            _ => false,
        };
        if hides_panes && !floating_active {
            if let Some((_, r)) = focused_rect {
                Self::paint_pane_dots(ui.painter(), *r, w.pane_number(), w.panes.len());
            }
        }
        for (id, r) in &rects {
            if floating_active || focus != Some(*id) {
                if let Some(p) = self.tiled_mut(*id) {
                    p.show_background_overlays(&ctx, *r);
                }
            }
        }
        for (id, r) in &float_rects {
            let inner = floating_style.content_rect(*r);
            if let Some(p) = self.tiled_mut(*id) {
                p.update_modes(&ctx);
                if p.resize(inner, scale) {
                    p.refresh_decorations();
                }
                let padding = (r.width() - inner.width()) / 2.0;
                p.set_corner_radius((floating_style.corner_radius - padding).max(0.0) * scale);
            }
        }
        if let Some(a) = shortcut {
            self.execute_action(a, floating_active, &ctx, gl);
        }
        let origin = self.active_pane();
        let bounds = if self.floats().visible().is_some() {
            float_focused_rect.map(|(_, r)| floating_style.content_rect(r))
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
            let origin_id = origin.and_then(|pane| self.pane_id(pane));
            let mut asked = Vec::new();
            for (id, p) in &mut self.tiled {
                p.handle_pipe_requests(&ctx, &bindings, origin_id == Some(*id));
                asked.extend(p.pipe_panes.drain(..).map(|pane| (*id, pane)));
            }
            for (from, (spawn, focus, session)) in asked {
                let command = spawn.command.clone();
                match self.create_pane_beside(from, spawn, focus, gl) {
                    // Answered once the command is seen to start, or not to.
                    Ok(id) => self.pipe_panes_starting.push(super::Starting {
                        pane: id,
                        command,
                        session,
                        since: std::time::Instant::now(),
                    }),
                    Err(message) => session.finish(
                        "error",
                        serde_json::json!({"code":"pane_failed","message":message}),
                    ),
                }
            }
        }
        let mut action = None;
        // Escape dismisses the window's toasts along with the focused pane's.
        let window_toasts = !self.toasts.is_empty();
        let mut toasts_dismissed = false;
        let windowed = self.window_picker.is_some();
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
                let input = if windowed {
                    // The window's picker has the keys; what the pane shows
                    // stays beneath it.
                    p.show_background_overlays(&ctx, bounds);
                    super::pane::PaneInput {
                        keyboard_blocked: true,
                        suppress_forward: true,
                        action: None,
                    }
                } else {
                    p.handle_input(ui, bounds, &bindings)
                };
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
        if let Some(crate::picker::Action::Execute(action)) = self.show_window_picker(&ctx, content) {
            self.execute_action(action, self.floats().visible().is_some(), &ctx, gl);
        }
        // The selected workspace, small, in the picker's preview.
        self.show_miniature(&ctx, content, &style, gl);
        self.show_mode_status(ui, status, &key_hints);
        let float_overlay = if floating_active {
            overlay
        } else {
            KeyOverlay::None
        };
        self.show_floating(
            ui,
            content,
            floating_rect,
            &floating_style,
            &float_overlay,
            &float_rects,
            float_focus,
        );
        for (_, p) in &self.tiled {
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

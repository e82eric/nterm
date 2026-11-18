use crate::picker::config::{BindingTarget, Bindings, KeyBinding};
use crate::terminal::workspace_ui::WorkspaceAction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppAction {
    Workspace(WorkspaceAction),
    /// Picker results that refer to window-local IDs, not config indexes.
    SelectWorkspaceId(u64),
    /// Ask for a name for a workspace of this window.
    RenameWorkspacePrompt(u64),
    /// Take a workspace of this window out of it; its shells stay in the
    /// daemon.
    DetachWorkspaceId(u64),
    /// End every shell in a workspace of this window.
    EndWorkspaceId(u64),
    /// End every shell of a session waiting in the daemon, by its name.
    EndSession(crate::session::Held),
    /// Call a workspace of this window `name`; nothing takes its name away.
    RenameWorkspace { id: u64, name: String },
    /// Make a workspace whose first shell starts in a directory, and go to
    /// it. `None` is where the active pane is.
    CreateWorkspaceIn(Option<String>),
    /// Show a session waiting in the session daemon as a workspace. The
    /// name is the daemon's; empty for the shells no session describes.
    AttachSession(crate::session::Held),
    /// The launch picker's choice of a new session over the waiting ones.
    NewSession,
    /// A new workspace whose shells run in that place.
    NewSessionOn(crate::session::Place),
    /// Connect to a place, to see what its daemon holds.
    Connect(crate::session::Place),
    ReloadConfig,
    NewWindow,
    ShowMessages,
    OpenErrorLog,
    OpenThemePicker,
    ApplyTheme(String),
    RestoreFloatingPane(u64),
    CreateFloatingPane,
    ToggleFloatingPane,
    OpenPicker(String),
    OpenSearch,
    EnterCopyMode,
    ExitCopyMode,
    /// Open a quick select: the built-in one, or a `quick_selects` entry by name.
    OpenQuickSelect(Option<String>),
    PreviousCommand,
    NextCommand,
    /// Enter copy mode if needed and scroll one row, like Ctrl+Y / Ctrl+E.
    ScrollLineUp,
    ScrollLineDown,
    /// Make a `key_tables` entry the active keys until `ExitKeyTable` or
    /// Escape: its keys run their actions and other keys are ignored.
    EnterKeyTable(String),
    ExitKeyTable,
    Lua(String),
}

/// Something to do with a picker's selected row other than what choosing
/// it does. The picker lists them as its actions for the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowCommand {
    RenameWorkspace,
    DetachWorkspace,
    EndWorkspace,
}

impl RowCommand {
    /// What the picker of workspaces and sessions offers for a row.
    pub(crate) const WORKSPACE: [Self; 3] =
        [Self::RenameWorkspace, Self::DetachWorkspace, Self::EndWorkspace];

    /// Whether the picker stays open through the command, as it does
    /// through a copy: its rows are given again and it says what was done.
    /// Not one that asks something of its own over the picker.
    pub(crate) fn keeps_picker(self) -> bool {
        matches!(self, Self::DetachWorkspace | Self::EndWorkspace)
    }

    /// What the picker says once the command has been done to `row`.
    pub(crate) fn done(self, row: &Entry) -> String {
        let what = match self {
            Self::RenameWorkspace => "Renamed",
            Self::DetachWorkspace => "Detached",
            Self::EndWorkspace => "Closed",
        };
        format!("{what} {}", row.described())
    }

    /// The key that does the command to the selected row without its being
    /// chosen from the picker's list of them. Chords the picker and the
    /// field typed in have no use for: Ctrl+X never arrives as a key, being
    /// that field's cut.
    pub(crate) fn key(self) -> Option<(egui::Modifiers, egui::Key)> {
        match self {
            Self::RenameWorkspace => None,
            Self::DetachWorkspace => Some((egui::Modifiers::CTRL, egui::Key::D)),
            Self::EndWorkspace => Some((egui::Modifiers::CTRL, egui::Key::Q)),
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::RenameWorkspace => "Rename",
            Self::DetachWorkspace => "Detach (its shells keep running)",
            Self::EndWorkspace => "Close (ends its shells)",
        }
    }

    /// The action for a row that `row` is the action of. `None` for a row
    /// the command has nothing to do with: a waiting session is detached
    /// already and is named where it is shown, and a row that makes a new
    /// one is neither a workspace nor a session.
    pub(crate) fn of(self, row: &Entry) -> Option<AppAction> {
        match (self, &row.action) {
            (Self::RenameWorkspace, AppAction::SelectWorkspaceId(id)) => {
                Some(AppAction::RenameWorkspacePrompt(*id))
            }
            (Self::DetachWorkspace, AppAction::SelectWorkspaceId(id)) => {
                Some(AppAction::DetachWorkspaceId(*id))
            }
            (Self::EndWorkspace, AppAction::SelectWorkspaceId(id)) => {
                Some(AppAction::EndWorkspaceId(*id))
            }
            (Self::EndWorkspace, AppAction::AttachSession(id)) => {
                Some(AppAction::EndSession(id.clone()))
            }
            _ => None,
        }
    }
}

impl AppAction {
    /// Whether a held key runs this action again on each auto-repeat.
    pub(crate) fn allows_key_repeat(&self) -> bool {
        match self {
            Self::Workspace(action) => action.allows_key_repeat(),
            Self::ScrollLineUp | Self::ScrollLineDown => true,
            _ => false,
        }
    }
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        if let Some(action) = WorkspaceAction::parse(name) {
            return Some(Self::Workspace(action));
        }
        if let Some(table) = name.strip_prefix("EnterKeyTable:") {
            return Some(Self::EnterKeyTable(table.to_owned()));
        }
        Some(match name {
            "ExitKeyTable" => Self::ExitKeyTable,
            "ReloadConfig" => Self::ReloadConfig,
            "NewWindow" => Self::NewWindow,
            "ShowMessages" => Self::ShowMessages,
            "OpenErrorLog" => Self::OpenErrorLog,
            "OpenThemePicker" => Self::OpenThemePicker,
            "CreateFloatingPane" => Self::CreateFloatingPane,
            "ToggleFloatingPane" => Self::ToggleFloatingPane,
            "OpenSearch" => Self::OpenSearch,
            "EnterCopyMode" => Self::EnterCopyMode,
            "ExitCopyMode" => Self::ExitCopyMode,
            "OpenQuickSelect" => Self::OpenQuickSelect(None),
            "PreviousCommand" => Self::PreviousCommand,
            "NextCommand" => Self::NextCommand,
            "ScrollLineUp" => Self::ScrollLineUp,
            "ScrollLineDown" => Self::ScrollLineDown,
            _ => return None,
        })
    }

    pub(crate) fn is_global(&self) -> bool {
        matches!(
            self,
            Self::Workspace(_)
                | Self::SelectWorkspaceId(_)
                | Self::RenameWorkspace { .. }
                | Self::CreateWorkspaceIn(_)
                | Self::RenameWorkspacePrompt(_)
                | Self::DetachWorkspaceId(_)
                | Self::EndWorkspaceId(_)
                | Self::EndSession(_)
                | Self::AttachSession(_)
                | Self::NewSession
                | Self::NewSessionOn(_)
                | Self::Connect(_)
                | Self::NewWindow
                | Self::ReloadConfig
                | Self::ShowMessages
                | Self::OpenErrorLog
                | Self::OpenThemePicker
                | Self::ApplyTheme(_)
                | Self::RestoreFloatingPane(_)
                | Self::CreateFloatingPane
                | Self::ToggleFloatingPane
                | Self::EnterKeyTable(_)
                | Self::ExitKeyTable
        )
    }
}

#[derive(Default)]
pub(crate) struct ActionContext {
    pub(crate) copying: bool,
    pub(crate) picker: bool,
    pub(crate) search: bool,
    pub(crate) quick_select: bool,
    pub(crate) palette: bool,
}

impl ActionContext {
    pub(crate) fn allows(&self, action: &AppAction, mods: egui::Modifiers) -> bool {
        if self.palette {
            return false;
        }
        if (self.picker || self.search || self.quick_select) && !mods.ctrl && !mods.alt {
            return false;
        }
        if action.is_global() {
            return true;
        }
        if self.picker {
            return false;
        }
        if self.quick_select {
            // The same shortcut closes it again.
            return matches!(action, AppAction::OpenQuickSelect(_));
        }
        if self.search {
            return *action == AppAction::OpenSearch;
        }
        if *action == AppAction::ExitCopyMode {
            return self.copying;
        }
        true
    }
}

pub(crate) fn default_bindings() -> Vec<KeyBinding> {
    let keys = [
        ("n", "CTRL|SHIFT", "NewWindow"),
        ("e", "CTRL|SHIFT", "ToggleFloatingPane"),
        ("w", "CTRL|SHIFT", "ClosePane"),
        ("/", "CTRL|SHIFT", "OpenSearch"),
        ("m", "CTRL|SHIFT", "EnterCopyMode"),
        ("r", "CTRL|SHIFT", "EnterKeyTable:resize"),
        ("Space", "ALT", "OpenQuickSelect"),
        ("ArrowUp", "CTRL|SHIFT", "PreviousCommand"),
        ("ArrowDown", "CTRL|SHIFT", "NextCommand"),
        ("ArrowUp", "CTRL", "ScrollLineUp"),
        ("ArrowDown", "CTRL", "ScrollLineDown"),
    ]
    .into_iter()
    .map(|(key, mods, action)| KeyBinding {
        trigger: crate::config::keybindings::BindingTrigger::Key(
            crate::config::keybindings::parse_key(key).expect("known default key"),
        ),
        description: String::new(),
        mods: crate::config::keybindings::parse_modifiers(mods).expect("known default modifiers"),
        action: BindingTarget::Action(AppAction::from_name(action).expect("known default action")),
    })
    .collect::<Vec<_>>();
    let mut keys = keys;
    keys.extend(crate::terminal::workspace_ui::default_bindings());
    keys
}

pub(crate) fn shortcut_label(key: egui::Key, mods: egui::Modifiers) -> String {
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("Ctrl".to_owned());
    }
    if mods.shift {
        parts.push("Shift".to_owned());
    }
    if mods.alt {
        parts.push("Alt".to_owned());
    }
    parts.push(key.name().to_owned());
    parts.join("+")
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) shortcut: Option<String>,
    /// The columns after the first in a picker of workspaces and sessions,
    /// which has no shortcut column. A row with fewer leaves the rest empty.
    pub(crate) cells: Vec<String>,
    pub(crate) action: AppAction,
}

impl Entry {
    /// What a row of workspaces or sessions is of, for saying what was done
    /// to it: "workspace 2 (name)", or "session name".
    pub(crate) fn described(&self) -> String {
        let name = self.cells.first().map_or("", String::as_str);
        match &self.action {
            AppAction::SelectWorkspaceId(_) if name.is_empty() => {
                format!("workspace {}", self.label)
            }
            AppAction::SelectWorkspaceId(_) => format!("workspace {} ({name})", self.label),
            _ => format!("session {name}"),
        }
    }
}

pub(crate) fn floating_pane_entry(id: u64, title: &str, active: bool) -> Entry {
    let label = title;
    // Titles may contain controls; keep each palette item on one display line.
    let label: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(256)
        .collect();
    let detail = if label.trim().is_empty() {
        String::new()
    } else {
        format!(" — {}", label.trim())
    };
    Entry {
        id: format!("floating.restore.{id}"),
        label: format!(
            "Restore floating pane {id}{detail}{}",
            if active { " (active)" } else { "" }
        ),
        shortcut: None,
        cells: Vec::new(),
        action: AppAction::RestoreFloatingPane(id),
    }
}

pub(crate) fn catalog(bindings: &Bindings, copying: bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (id, label, action, available) in [
        (
            "config.reload",
            "Reload configuration",
            AppAction::ReloadConfig,
            true,
        ),
        ("window.new", "New window", AppAction::NewWindow, true),
        (
            "errors.open",
            "Error history",
            AppAction::OpenErrorLog,
            !bindings.has_error_log_picker(),
        ),
        (
            "themes.open",
            "Choose theme",
            AppAction::OpenThemePicker,
            !bindings.has_themes_picker(),
        ),
        (
            "messages.show",
            "Show messages",
            AppAction::ShowMessages,
            true,
        ),
        (
            "floating.toggle",
            "Toggle floating pane",
            AppAction::ToggleFloatingPane,
            true,
        ),
        (
            "floating.create",
            "Create floating pane",
            AppAction::CreateFloatingPane,
            true,
        ),
        (
            "search.open",
            "Search terminal",
            AppAction::OpenSearch,
            true,
        ),
        (
            "copy.enter",
            "Enter copy mode",
            AppAction::EnterCopyMode,
            !copying,
        ),
        (
            "copy.exit",
            "Exit copy mode",
            AppAction::ExitCopyMode,
            copying,
        ),
        (
            "quick_select.open",
            "Quick select",
            AppAction::OpenQuickSelect(None),
            true,
        ),
        (
            "command.previous",
            "Previous command",
            AppAction::PreviousCommand,
            true,
        ),
        ("command.next", "Next command", AppAction::NextCommand, true),
    ] {
        if available {
            entries.push(Entry {
                id: id.into(),
                label: label.into(),
                shortcut: bindings.shortcut_for(&action),
        cells: Vec::new(),
                action,
            });
        }
    }
    for workspace_action in WorkspaceAction::ALL {
        let action = AppAction::Workspace(*workspace_action);
        entries.push(Entry {
            id: format!("workspace.{workspace_action}"),
            label: workspace_action.label(),
            shortcut: bindings.shortcut_for(&action),
        cells: Vec::new(),
            action,
        });
    }
    entries.extend(bindings.palette_entries().map(|(name, shortcut)| Entry {
        id: format!("picker.{name}"),
        label: match name {
            "builtin.themes" => "Choose theme".into(),
            "builtin.lines" => "Pick scrollback line".into(),
            "builtin.words" => "Pick scrollback word".into(),
            _ => format!("Open picker: {name}"),
        },
        shortcut,
        cells: Vec::new(),
        action: AppAction::OpenPicker(name.to_owned()),
    }));
    entries.extend(bindings.key_table_names().map(|name| {
        let action = AppAction::EnterKeyTable(name.to_owned());
        Entry {
            id: format!("key_table.{name}"),
            label: format!("Enter key table: {name}"),
            shortcut: bindings.shortcut_for(&action),
        cells: Vec::new(),
            action,
        }
    }));
    entries.extend(
        bindings
            .quick_selects
            .named
            .iter()
            .map(|(name, definition)| {
                let action = AppAction::OpenQuickSelect(Some(name.clone()));
                Entry {
                    id: format!("quick_select.{name}"),
                    label: definition.label.clone(),
                    shortcut: bindings.shortcut_for(&action),
        cells: Vec::new(),
                    action,
                }
            }),
    );
    entries.extend(bindings.lua_actions().map(|(name, action)| Entry {
        id: format!("action.{name}"),
        label: action.label.clone(),
        shortcut: bindings.shortcut_for(&AppAction::Lua(name.to_owned())),
        cells: Vec::new(),
        action: AppAction::Lua(name.to_owned()),
    }));
    entries
}

pub(crate) struct LuaAction {
    pub(crate) label: String,
    pub(crate) callback: std::rc::Rc<crate::lua_callbacks::Callback>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_row_command_says_what_it_did_to_which_row() {
        let row = |label: &str, name: &str, action| Entry {
            id: String::new(),
            label: label.into(),
            shortcut: None,
            cells: vec![name.into()],
            action,
        };
        let workspace = row("2", "nterm", AppAction::SelectWorkspaceId(7));
        assert_eq!(RowCommand::EndWorkspace.done(&workspace), "Closed workspace 2 (nterm)");
        assert_eq!(RowCommand::DetachWorkspace.done(&workspace), "Detached workspace 2 (nterm)");
        let unnamed = row("3", "", AppAction::SelectWorkspaceId(8));
        assert_eq!(RowCommand::EndWorkspace.done(&unnamed), "Closed workspace 3");
        // A session has no number; its name is under the workspaces' names.
        let session = row("", "build", AppAction::AttachSession("a".into()));
        assert_eq!(RowCommand::EndWorkspace.done(&session), "Closed session build");
        // The picker stays through what changes its rows, not through a
        // command that asks something over it, and those have a key each.
        let keys: Vec<_> = RowCommand::WORKSPACE.iter().filter_map(|command| command.key()).collect();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
        assert!(RowCommand::WORKSPACE.iter().all(|command| command.key().is_some() == command.keeps_picker()));
        assert!(!RowCommand::RenameWorkspace.keeps_picker());
    }
    #[test]
    fn floating_entries_use_stable_ids_and_titles_and_mark_active() {
        let named = floating_pane_entry(7, "tools", true);
        assert_eq!(named.id, "floating.restore.7");
        assert_eq!(named.label, "Restore floating pane 7 — tools (active)");
        assert_eq!(named.action, AppAction::RestoreFloatingPane(7));
        assert_eq!(
            floating_pane_entry(8, "nvim README.md", false).label,
            "Restore floating pane 8 — nvim README.md"
        );
        assert_eq!(
            floating_pane_entry(9, "", false).label,
            "Restore floating pane 9"
        );
        assert!(
            !floating_pane_entry(10, "title\nnext\tline", false)
                .label
                .contains('\n')
        );
    }
    #[test]
    fn catalog_lists_unbound_pickers_skips_request_sources_and_checks_state() {
        let definitions = serde_json::from_value(serde_json::json!({
            "unbound": { "type": "Scrollback" },
            "bound": { "type": "Items", "items": [{ "text": "shown", "value": "value" }] },
            "completion": { "type": "Items" },
            "ripgrep": { "type": "Command", "args": ["rg", "{pattern}"] },
            "command": { "type": "Command", "args": ["echo", "hello"] }
        }))
        .unwrap();
        let keys = serde_json::from_value(serde_json::json!([
            { "key": "o", "mods": "CTRL|SHIFT", "picker": "bound" }
        ]))
        .unwrap();
        let bindings = Bindings::compile(Some(definitions), Some(keys)).unwrap();
        let entries = catalog(&bindings, false);
        assert!(entries.iter().any(|entry| entry.action
            == AppAction::OpenPicker("unbound".into())
            && entry.shortcut.is_none()));
        assert!(
            entries.iter().any(|entry| entry.id == "picker.bound"
                && entry.shortcut.as_deref() == Some("Ctrl+Shift+O"))
        );
        assert!(entries.iter().any(|entry| entry.id == "picker.command"));
        assert!(!entries.iter().any(|entry| matches!(
            entry.id.as_str(),
            "picker.completion" | "picker.ripgrep" | "copy.exit"
        )));
        let active = catalog(&bindings, true);
        assert!(
            active
                .iter()
                .any(|entry| entry.action == AppAction::ExitCopyMode)
        );
        assert!(
            !active
                .iter()
                .any(|entry| entry.action == AppAction::EnterCopyMode)
        );
    }
}

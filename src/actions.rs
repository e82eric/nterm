use crate::picker::config::{BindingTarget, Bindings, KeyBinding};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppAction {
    ReloadConfig,
    NewWindow,
    ShowMessages,
    OpenErrorLog,
    ShowErrorRecord(Box<crate::error_log::Record>),
    OpenThemePicker,
    ApplyTheme(String),
    RestoreFloatingPane(u64),
    CreateFloatingPane,
    NextFloatingPane,
    PreviousFloatingPane,
    ToggleFloatingPane,
    CloseFloatingPane,
    OpenPicker(String),
    OpenSearch,
    EnterCopyMode,
    ExitCopyMode,
    OpenQuickSelect,
    PreviousCommand,
    NextCommand,
    Lua(String),
}

impl AppAction {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "ReloadConfig" => Self::ReloadConfig,
            "NewWindow" => Self::NewWindow,
            "ShowMessages" => Self::ShowMessages,
            "OpenErrorLog" => Self::OpenErrorLog,
            "OpenThemePicker" => Self::OpenThemePicker,
            "CreateFloatingPane" => Self::CreateFloatingPane,
            "NextFloatingPane" => Self::NextFloatingPane,
            "PreviousFloatingPane" => Self::PreviousFloatingPane,
            "ToggleFloatingPane" => Self::ToggleFloatingPane,
            "CloseFloatingPane" => Self::CloseFloatingPane,
            "OpenSearch" => Self::OpenSearch,
            "EnterCopyMode" => Self::EnterCopyMode,
            "ExitCopyMode" => Self::ExitCopyMode,
            "OpenQuickSelect" => Self::OpenQuickSelect,
            "PreviousCommand" => Self::PreviousCommand,
            "NextCommand" => Self::NextCommand,
            _ => return None,
        })
    }

    pub(crate) fn is_global(&self) -> bool {
        matches!(
            self,
            Self::NewWindow
                | Self::ReloadConfig
                | Self::ShowMessages
                | Self::OpenErrorLog
                | Self::ShowErrorRecord(_)
                | Self::OpenThemePicker
                | Self::ApplyTheme(_)
                | Self::RestoreFloatingPane(_)
                | Self::CreateFloatingPane
                | Self::NextFloatingPane
                | Self::PreviousFloatingPane
                | Self::ToggleFloatingPane
                | Self::CloseFloatingPane
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
        if (self.picker || self.search || self.quick_select)
            && !mods.ctrl
            && !mods.alt
            && !mods.mac_cmd
        {
            return false;
        }
        if action.is_global() {
            return true;
        }
        if self.picker {
            return false;
        }
        if self.quick_select {
            return *action == AppAction::OpenQuickSelect;
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
    let mut keys = [
        ("n", "CTRL|SHIFT", "NewWindow"),
        ("e", "CTRL|SHIFT", "ToggleFloatingPane"),
        ("e", "CTRL|ALT|SHIFT", "CreateFloatingPane"),
        ("]", "CTRL|ALT", "NextFloatingPane"),
        ("[", "CTRL|ALT", "PreviousFloatingPane"),
        ("w", "CTRL|SHIFT", "CloseFloatingPane"),
        ("/", "CTRL|SHIFT", "OpenSearch"),
        ("m", "CTRL|SHIFT", "EnterCopyMode"),
        ("Space", "ALT", "OpenQuickSelect"),
        ("ArrowUp", "CTRL|SHIFT", "PreviousCommand"),
        ("ArrowDown", "CTRL|SHIFT", "NextCommand"),
    ]
    .into_iter()
    .map(|(key, mods, action)| KeyBinding {
        key: key.into(),
        mods: mods.into(),
        target: BindingTarget::Action(AppAction::from_name(action).expect("known default action")),
    })
    .collect::<Vec<_>>();
    if cfg!(target_os = "macos") {
        keys.push(KeyBinding {
            key: "f".into(),
            mods: "CMD".into(),
            target: BindingTarget::Action(AppAction::OpenSearch),
        });
    }
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
    if mods.mac_cmd {
        parts.push("Cmd".to_owned());
    }
    parts.push(key.name().to_owned());
    parts.join("+")
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) shortcut: Option<String>,
    pub(crate) action: AppAction,
}

pub(crate) fn floating_pane_entry(id: u64, name: Option<&str>, title: &str, active: bool) -> Entry {
    let label = name.filter(|name| !name.trim().is_empty()).unwrap_or(title);
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
        action: AppAction::RestoreFloatingPane(id),
    }
}

pub(crate) fn catalog(bindings: &Bindings, has_floating: bool, copying: bool) -> Vec<Entry> {
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
            "floating.next",
            "Next floating pane",
            AppAction::NextFloatingPane,
            has_floating,
        ),
        (
            "floating.previous",
            "Previous floating pane",
            AppAction::PreviousFloatingPane,
            has_floating,
        ),
        (
            "floating.close",
            "Close floating pane",
            AppAction::CloseFloatingPane,
            has_floating,
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
            AppAction::OpenQuickSelect,
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
                action,
            });
        }
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
        action: AppAction::OpenPicker(name.to_owned()),
    }));
    entries.extend(bindings.lua_actions().map(|(name, action)| Entry {
        id: format!("action.{name}"),
        label: action.label.clone(),
        shortcut: bindings.shortcut_for(&AppAction::Lua(name.to_owned())),
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
    fn floating_entries_use_stable_ids_name_then_title_and_mark_active() {
        let named = floating_pane_entry(7, Some("tools"), "PowerShell", true);
        assert_eq!(named.id, "floating.restore.7");
        assert_eq!(named.label, "Restore floating pane 7 — tools (active)");
        assert_eq!(named.action, AppAction::RestoreFloatingPane(7));
        assert_eq!(
            floating_pane_entry(8, None, "nvim README.md", false).label,
            "Restore floating pane 8 — nvim README.md"
        );
        assert_eq!(
            floating_pane_entry(9, None, "", false).label,
            "Restore floating pane 9"
        );
        assert!(
            !floating_pane_entry(10, None, "title\nnext\tline", false)
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
        let entries = catalog(&bindings, false, false);
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
            "picker.completion" | "picker.ripgrep" | "floating.close" | "copy.exit"
        )));
        let active = catalog(&bindings, true, true);
        assert!(
            active
                .iter()
                .any(|entry| entry.action == AppAction::CloseFloatingPane)
        );
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

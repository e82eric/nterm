use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Default, Deserialize)]
pub(crate) enum EnterAction {
    #[default]
    Navigate,
    SendInput,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) enum Position {
    Top,
    Center,
    #[default]
    Bottom,
    Cursor,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Preview {
    Text {
        text: String,
    },
    Ansi {
        text: String,
    },
    Scrollback {
        #[serde(default = "default_preview_rows")]
        rows: usize,
    },
    Command {
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(skip)]
        command: Option<std::rc::Rc<crate::picker::action::PreviewCommand>>,
    },
}

fn default_preview_rows() -> usize {
    12
}

impl Preview {
    fn validate(&self) -> Result<(), String> {
        if let Self::Scrollback { rows } = self {
            if !(1..=12).contains(rows) {
                return Err("scrollback preview rows must be between 1 and 12".into());
            }
        }
        if let Self::Command { args, cwd, command } = self {
            if command.is_some() && !args.is_empty() {
                return Err("preview command cannot specify both args and command".into());
            }
            if command.is_none() && args.first().is_none_or(String::is_empty) {
                return Err("preview command requires an executable".into());
            }
            if args.iter().any(|arg| arg.contains('\0'))
                || cwd.as_ref().is_some_and(|cwd| cwd.contains('\0'))
            {
                return Err("preview command contains NUL".into());
            }
        }
        Ok(())
    }

    pub(crate) fn set_command(
        &mut self,
        value: std::rc::Rc<crate::picker::action::PreviewCommand>,
    ) {
        if let Self::Command { command, .. } = self {
            *command = Some(value);
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PickerDefaults {
    pub(crate) unique: bool,
}

// Repeat the shared acceptance fields so each variant can reject unknown fields.
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Definition {
    Factory {
        #[serde(skip)]
        factory: Option<std::rc::Rc<crate::config::PickerFactory>>,
    },
    Scrollback {
        #[serde(default)]
        position: Position,
        #[serde(default)]
        regex: Option<String>,
        #[serde(default)]
        unique: Option<bool>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(default)]
        initial_query: Option<String>,
        #[serde(default)]
        preview: Option<Preview>,
        #[serde(default = "default_preview_visible")]
        preview_visible: bool,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::picker::action::Callback>>,
    },
    Items {
        #[serde(default)]
        position: Position,
        #[serde(default)]
        items: Vec<crate::picker::protocol::Item>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(default)]
        initial_query: Option<String>,
        #[serde(default)]
        preview: Option<Preview>,
        #[serde(default = "default_preview_visible")]
        preview_visible: bool,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::picker::action::Callback>>,
        #[serde(skip)]
        provider: Option<std::rc::Rc<crate::picker::action::ItemsProvider>>,
    },
    Command {
        #[serde(default)]
        position: Position,
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        line_continuation: Option<String>,
        #[serde(default)]
        unique: Option<bool>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(default)]
        initial_query: Option<String>,
        #[serde(default)]
        preview: Option<Preview>,
        #[serde(default = "default_preview_visible")]
        preview_visible: bool,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::picker::action::Callback>>,
    },
    FileSystem {
        #[serde(default)]
        position: Position,
        #[serde(default)]
        roots: Vec<String>,
        #[serde(default)]
        max_depth: Option<i32>,
        #[serde(default)]
        files_only: bool,
        #[serde(default)]
        directories_only: bool,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(default)]
        initial_query: Option<String>,
        #[serde(default)]
        preview: Option<Preview>,
        #[serde(default = "default_preview_visible")]
        preview_visible: bool,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::picker::action::Callback>>,
    },
}

fn default_preview_visible() -> bool {
    true
}

impl Definition {
    pub(crate) fn set_preview_command(
        &mut self,
        value: std::rc::Rc<crate::picker::action::PreviewCommand>,
    ) {
        match self {
            Self::Scrollback { preview, .. }
            | Self::Items { preview, .. }
            | Self::Command { preview, .. }
            | Self::FileSystem { preview, .. } => {
                if let Some(preview) = preview {
                    preview.set_command(value);
                }
            }
            Self::Factory { .. } => {}
        }
    }
    pub(crate) fn set_items_provider(
        &mut self,
        value: std::rc::Rc<crate::picker::action::ItemsProvider>,
    ) {
        if let Self::Items { provider, .. } = self {
            *provider = Some(value);
        }
    }
    pub(crate) fn set_callback(&mut self, value: std::rc::Rc<crate::picker::action::Callback>) {
        match self {
            Self::Scrollback { callback, .. }
            | Self::FileSystem { callback, .. }
            | Self::Command { callback, .. }
            | Self::Items { callback, .. } => *callback = Some(value),
            Self::Factory { .. } => {}
        }
    }

    fn acceptance(
        &self,
    ) -> (
        Option<std::rc::Rc<crate::picker::action::Callback>>,
        Option<EnterAction>,
    ) {
        match self {
            Self::Scrollback {
                callback,
                enter_action,
                ..
            }
            | Self::Items {
                callback,
                enter_action,
                ..
            }
            | Self::Command {
                callback,
                enter_action,
                ..
            }
            | Self::FileSystem {
                callback,
                enter_action,
                ..
            } => (callback.clone(), *enter_action),
            Self::Factory { .. } => (None, None),
        }
    }

    #[cfg(test)]
    pub(crate) fn callback(&self) -> Option<&std::rc::Rc<crate::picker::action::Callback>> {
        match self {
            Self::Scrollback { callback, .. }
            | Self::FileSystem { callback, .. }
            | Self::Command { callback, .. }
            | Self::Items { callback, .. } => callback.as_ref(),
            Self::Factory { .. } => None,
        }
    }
}
fn default_regex() -> String {
    r"\S(?:[^\r\n]*\S)?".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeyBinding {
    key: String,
    #[serde(default)]
    mods: String,
    picker: String,
}

#[derive(Clone)]
pub(crate) struct Options {
    pub(crate) position: Position,
    pub(crate) callback: Option<std::rc::Rc<crate::picker::action::Callback>>,
    pub(crate) source: Source,
    pub(crate) enter_action: EnterAction,
    pub(crate) params: BTreeMap<String, String>,
    pub(crate) preview: Option<Preview>,
    pub(crate) preview_visible: bool,
}

#[derive(Clone)]
pub(crate) enum Source {
    Scrollback { regex: Regex, unique: bool },
    FileSystem(FileOptions),
    Items(ItemsOptions),
    Command(CommandOptions),
}

#[derive(Clone)]
pub(crate) struct ItemsOptions {
    pub(crate) values: Vec<crate::picker::protocol::Item>,
    pub(crate) provider: Option<std::rc::Rc<crate::picker::action::ItemsProvider>>,
}

#[derive(Clone)]
pub(crate) struct CommandOptions {
    pub(crate) args: Vec<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) line_continuation: Option<String>,
    pub(crate) unique: bool,
}

#[derive(Clone)]
#[cfg_attr(not(windows), allow(dead_code))] // Reserved for NFM's future Linux backend.
pub(crate) struct FileOptions {
    pub(crate) roots: Vec<std::path::PathBuf>,
    pub(crate) max_depth: i32,
    pub(crate) files_only: bool,
    pub(crate) directories_only: bool,
}

impl Options {
    pub(crate) fn with_request(mut self, params: BTreeMap<String, String>) -> Result<Self, String> {
        if let Source::Items(items) = &mut self.source {
            if params.contains_key("items") {
                items.values = crate::picker::protocol::items(&params)?;
                items.provider = None;
            }
        }
        if let Source::FileSystem(files) = &mut self.source {
            if let Some(roots) = params.get("roots") {
                let roots: Vec<String> = serde_json::from_str(roots)
                    .map_err(|e| format!("invalid picker roots: {e}"))?;
                if roots.len() > 32
                    || roots
                        .iter()
                        .any(|root| root.is_empty() || root.contains('\0'))
                {
                    return Err("invalid picker roots".into());
                }
                files.roots = roots.into_iter().map(std::path::PathBuf::from).collect();
            }
        }
        self.params.extend(params);
        Ok(self)
    }
}
impl Default for Options {
    fn default() -> Self {
        Self {
            position: Position::Bottom,
            callback: None,
            source: Source::Scrollback {
                regex: Regex::new(&default_regex()).unwrap(),
                unique: false,
            },
            enter_action: EnterAction::Navigate,
            params: BTreeMap::new(),
            preview: None,
            preview_visible: true,
        }
    }
}

struct Binding {
    key: egui::Key,
    mods: egui::Modifiers,
    options: PickerEntry,
}

#[derive(Clone)]
pub(crate) enum PickerEntry {
    Static(Options),
    Factory(std::rc::Rc<crate::config::PickerFactory>, PickerDefaults),
}

impl PickerEntry {
    pub(crate) fn resolve(
        &self,
        context: &crate::config::PickerContext,
    ) -> Result<Options, String> {
        match self {
            Self::Static(options) => Ok(options.clone()),
            Self::Factory(factory, defaults) => factory.resolve(context, *defaults),
        }
    }

    pub(crate) fn needs_context(&self) -> bool {
        matches!(self, Self::Factory(..))
    }
}

pub(crate) struct Bindings {
    bindings: Vec<Binding>,
    definitions: BTreeMap<String, PickerEntry>,
}

impl Bindings {
    pub(crate) fn compile(
        definitions: Option<BTreeMap<String, Definition>>,
        keys: Option<Vec<KeyBinding>>,
    ) -> Result<Self, String> {
        Self::compile_with_defaults(definitions, keys, PickerDefaults::default())
    }

    pub(crate) fn compile_with_defaults(
        definitions: Option<BTreeMap<String, Definition>>,
        keys: Option<Vec<KeyBinding>>,
        defaults: PickerDefaults,
    ) -> Result<Self, String> {
        let definitions = definitions.unwrap_or_else(|| {
            BTreeMap::from([(
                "lines".into(),
                Definition::Scrollback {
                    position: Position::Bottom,
                    unique: None,
                    callback: None,
                    regex: None,
                    enter_action: None,
                    initial_query: None,
                    preview: None,
                    preview_visible: true,
                },
            )])
        });
        let mut compiled = BTreeMap::new();
        for (name, definition) in definitions {
            if name.trim().is_empty() {
                return Err("picker names must not be empty".into());
            }
            if let Definition::Factory { factory } = definition {
                compiled.insert(
                    name,
                    PickerEntry::Factory(factory.ok_or("missing picker factory")?, defaults),
                );
                continue;
            }
            let (callback, enter_action) = definition.acceptance();
            let position = match &definition {
                Definition::Scrollback { position, .. }
                | Definition::Items { position, .. }
                | Definition::Command { position, .. }
                | Definition::FileSystem { position, .. } => *position,
                Definition::Factory { .. } => unreachable!(),
            };
            let initial_query = match &definition {
                Definition::Scrollback { initial_query, .. }
                | Definition::Items { initial_query, .. }
                | Definition::Command { initial_query, .. }
                | Definition::FileSystem { initial_query, .. } => initial_query.clone(),
                Definition::Factory { .. } => unreachable!(),
            };
            let preview = match &definition {
                Definition::Scrollback { preview, .. }
                | Definition::Items { preview, .. }
                | Definition::Command { preview, .. }
                | Definition::FileSystem { preview, .. } => preview.clone(),
                Definition::Factory { .. } => unreachable!(),
            };
            let preview_visible = match &definition {
                Definition::Scrollback {
                    preview_visible, ..
                }
                | Definition::Items {
                    preview_visible, ..
                }
                | Definition::Command {
                    preview_visible, ..
                }
                | Definition::FileSystem {
                    preview_visible, ..
                } => *preview_visible,
                Definition::Factory { .. } => unreachable!(),
            };
            if let Some(preview) = &preview {
                preview
                    .validate()
                    .map_err(|error| format!("pickers.{name}.preview: {error}"))?;
            }
            let mut options = match definition {
                Definition::Factory { .. } => unreachable!(),
                Definition::Scrollback { regex, unique, .. } => {
                    let regex = Regex::new(&regex.unwrap_or_else(default_regex))
                        .map_err(|error| format!("pickers.{name}.regex: {error}"))?;
                    if regex.is_match("") {
                        return Err(format!("pickers.{name}.regex must not match empty text"));
                    }
                    Options {
                        position,
                        callback,
                        source: Source::Scrollback {
                            regex,
                            unique: unique.unwrap_or(defaults.unique),
                        },
                        enter_action: enter_action.unwrap_or(EnterAction::Navigate),
                        params: BTreeMap::new(),
                        preview: None,
                        preview_visible,
                    }
                }
                Definition::Items {
                    items, provider, ..
                } => {
                    crate::picker::protocol::validate_items(&items)?;
                    if callback.is_none() && matches!(enter_action, Some(EnterAction::Navigate)) {
                        return Err(format!(
                            "pickers.{name}: Items only supports enter_action SendInput"
                        ));
                    }
                    Options {
                        position,
                        callback,
                        source: Source::Items(ItemsOptions {
                            values: items,
                            provider,
                        }),
                        enter_action: EnterAction::SendInput,
                        params: BTreeMap::new(),
                        preview: None,
                        preview_visible,
                    }
                }
                Definition::Command {
                    args,
                    cwd,
                    line_continuation,
                    unique,
                    ..
                } => {
                    if args.is_empty()
                        || args[0].trim().is_empty()
                        || (args[0].starts_with('{') && args[0].ends_with('}'))
                    {
                        return Err(format!("pickers.{name}.args requires an executable"));
                    }
                    if args.iter().any(|arg| arg.contains('\0'))
                        || cwd.as_ref().is_some_and(|value| value.contains('\0'))
                        || line_continuation.as_ref().is_some_and(|value| {
                            value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
                        })
                    {
                        return Err(format!("pickers.{name}: command contains NUL"));
                    }
                    if callback.is_none() && matches!(enter_action, Some(EnterAction::Navigate)) {
                        return Err(format!(
                            "pickers.{name}: Command only supports enter_action SendInput"
                        ));
                    }
                    Options {
                        position,
                        callback,
                        source: Source::Command(CommandOptions {
                            args,
                            cwd,
                            line_continuation: line_continuation.filter(|value| !value.is_empty()),
                            unique: unique.unwrap_or(defaults.unique),
                        }),
                        enter_action: EnterAction::SendInput,
                        params: BTreeMap::new(),
                        preview: None,
                        preview_visible,
                    }
                }
                Definition::FileSystem {
                    roots,
                    max_depth,
                    files_only,
                    directories_only,
                    ..
                } => {
                    if callback.is_none() && matches!(enter_action, Some(EnterAction::Navigate)) {
                        return Err(format!(
                            "pickers.{name}: FileSystem only supports enter_action SendInput"
                        ));
                    }
                    if files_only && directories_only {
                        return Err(format!(
                            "pickers.{name}: files_only and directories_only cannot both be true"
                        ));
                    }
                    if max_depth.is_some_and(|depth| depth < 0) {
                        return Err(format!("pickers.{name}.max_depth must be nonnegative"));
                    }
                    if roots
                        .iter()
                        .any(|root| root.trim().is_empty() || root.contains('\0'))
                    {
                        return Err(format!("pickers.{name}.roots must contain nonempty paths"));
                    }
                    Options {
                        position,
                        callback,
                        source: Source::FileSystem(FileOptions {
                            roots: roots.into_iter().map(std::path::PathBuf::from).collect(),
                            max_depth: max_depth.unwrap_or(i32::MAX),
                            files_only,
                            directories_only,
                        }),
                        enter_action: EnterAction::SendInput,
                        params: BTreeMap::new(),
                        preview: None,
                        preview_visible,
                    }
                }
            };
            if let Some(query) = initial_query {
                options.params.insert("query".into(), query);
            }
            if matches!(&preview, Some(Preview::Scrollback { .. }))
                && !matches!(&options.source, Source::Scrollback { .. })
            {
                return Err(format!(
                    "pickers.{name}.preview: Scrollback requires a scrollback picker"
                ));
            }
            options.preview = preview;
            compiled.insert(name, PickerEntry::Static(options));
        }
        let keys = keys.unwrap_or_else(|| {
            if compiled.contains_key("lines") {
                vec![KeyBinding {
                    key: "f".into(),
                    mods: "CTRL|SHIFT".into(),
                    picker: "lines".into(),
                }]
            } else {
                vec![]
            }
        });
        let mut bindings: Vec<Binding> = Vec::new();
        for binding in keys {
            let key_name = if binding.key.len() == 1 && binding.key.is_ascii() {
                binding.key.to_uppercase()
            } else {
                binding.key.clone()
            };
            let key = egui::Key::from_name(&key_name)
                .ok_or_else(|| format!("unknown picker key {:?}", binding.key))?;
            let mut mods = egui::Modifiers::NONE;
            for part in binding
                .mods
                .split('|')
                .filter(|part| !part.trim().is_empty())
            {
                match part.trim().to_uppercase().as_str() {
                    "CTRL" => mods.ctrl = true,
                    "SHIFT" => mods.shift = true,
                    "ALT" => mods.alt = true,
                    "CMD" | "SUPER" if cfg!(target_os = "macos") => mods.mac_cmd = true,
                    _ => return Err(format!("unknown picker modifier {part:?}")),
                }
            }
            let reserved = (mods.ctrl
                && mods.shift
                && !mods.alt
                && !mods.mac_cmd
                && matches!(
                    key,
                    egui::Key::Slash
                        | egui::Key::Questionmark
                        | egui::Key::M
                        | egui::Key::ArrowUp
                        | egui::Key::ArrowDown
                ))
                || (mods.mac_cmd && !mods.ctrl && !mods.shift && !mods.alt && key == egui::Key::F)
                || (mods.alt
                    && !mods.ctrl
                    && !mods.shift
                    && !mods.mac_cmd
                    && key == egui::Key::Space);
            if reserved {
                return Err(format!(
                    "picker binding {}+{} conflicts with an app shortcut",
                    binding.mods, binding.key
                ));
            }
            if bindings
                .iter()
                .any(|existing| existing.key == key && same_mods(existing.mods, mods))
            {
                return Err(format!(
                    "duplicate picker binding {}+{}",
                    binding.mods, binding.key
                ));
            }
            let options = compiled
                .get(&binding.picker)
                .ok_or_else(|| format!("unknown picker {:?} in key binding", binding.picker))?
                .clone();
            bindings.push(Binding { key, mods, options });
        }
        Ok(Self {
            bindings,
            definitions: compiled,
        })
    }

    pub(crate) fn named(&self, name: &str) -> Option<PickerEntry> {
        self.definitions.get(name).cloned()
    }

    pub(crate) fn consume(&self, events: &mut Vec<egui::Event>) -> (Option<PickerEntry>, bool) {
        let mut incoming = std::mem::take(events).into_iter().peekable();
        let mut open = None;
        let mut consumed = false;
        while let Some(event) = incoming.next() {
            let binding = if let egui::Event::Key {
                key,
                physical_key,
                modifiers,
                ..
            } = &event
            {
                self.bindings.iter().find(|binding| {
                    (binding.key == *key || Some(binding.key) == *physical_key)
                        && same_mods(binding.mods, *modifiers)
                })
            } else {
                None
            };
            if let Some(binding) = binding {
                let egui::Event::Key {
                    pressed, repeat, ..
                } = event
                else {
                    unreachable!()
                };
                consumed = true;
                if pressed && !repeat {
                    open = Some(binding.options.clone());
                }
                if pressed && matches!(incoming.peek(), Some(egui::Event::Text(_))) {
                    incoming.next();
                }
            } else {
                events.push(event);
            }
        }
        (open, consumed)
    }
}

impl Default for Bindings {
    fn default() -> Self {
        Self::compile(None, None).unwrap()
    }
}

fn same_mods(a: egui::Modifiers, b: egui::Modifiers) -> bool {
    // egui's `command` mirrors Ctrl on Windows/Linux, so compare physical
    // modifier fields rather than this platform-dependent alias.
    (a.ctrl, a.shift, a.alt, a.mac_cmd) == (b.ctrl, b.shift, b.alt, b.mac_cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picker_type_accepts_position_and_defaults_to_bottom() {
        for kind in ["Scrollback", "Items", "Command", "FileSystem"] {
            for (name, expected) in [
                ("Top", Position::Top),
                ("Center", Position::Center),
                ("Bottom", Position::Bottom),
                ("Cursor", Position::Cursor),
            ] {
                let source = if kind == "Command" {
                    format!(r#"{{"type":"{kind}","args":["cmd"],"position":"{name}"}}"#)
                } else {
                    format!(r#"{{"type":"{kind}","position":"{name}"}}"#)
                };
                let definition: Definition = serde_json::from_str(&source).unwrap();
                let bindings =
                    Bindings::compile(Some(BTreeMap::from([("picker".into(), definition)])), None)
                        .unwrap();
                let options = bindings
                    .named("picker")
                    .unwrap()
                    .resolve(&crate::config::PickerContext::default())
                    .unwrap();
                assert_eq!(options.position, expected);
            }
        }
        assert!(
            serde_json::from_str::<Definition>(r#"{"type":"Scrollback","position":"Left"}"#)
                .is_err()
        );
    }

    #[test]
    fn every_picker_type_accepts_preview_visibility() {
        for kind in ["Scrollback", "Items", "Command", "FileSystem"] {
            for (field, expected) in [("", true), (",\"preview_visible\":false", false)] {
                let args = if kind == "Command" {
                    ",\"args\":[\"cmd\"]"
                } else {
                    ""
                };
                let source = format!(
                    "{{\"type\":\"{kind}\",\"preview\":{{\"type\":\"Text\",\"text\":\"x\"}}{args}{field}}}"
                );
                let definition: Definition = serde_json::from_str(&source).unwrap();
                let bindings =
                    Bindings::compile(Some(BTreeMap::from([("picker".into(), definition)])), None)
                        .unwrap();
                let options = bindings
                    .named("picker")
                    .unwrap()
                    .resolve(&crate::config::PickerContext::default())
                    .unwrap();
                assert_eq!(options.preview_visible, expected, "{kind}");
            }
        }
    }

    #[test]
    fn default_binding_consumes_its_text_and_ignores_repeat() {
        let bindings = Bindings::default();
        for repeat in [false, true] {
            let mut events = vec![
                egui::Event::Key {
                    key: egui::Key::F,
                    physical_key: Some(egui::Key::F),
                    pressed: true,
                    repeat,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        shift: true,
                        command: true,
                        ..Default::default()
                    },
                },
                egui::Event::Text("F".into()),
                egui::Event::Text("x".into()),
            ];
            let (open, consumed) = bindings.consume(&mut events);
            assert!(consumed);
            assert_eq!(open.is_some(), !repeat);
            assert!(matches!(&events[..], [egui::Event::Text(text)] if text == "x"));
        }
    }
}

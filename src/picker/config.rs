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

#[derive(Clone, Copy, Default, Deserialize)]
pub(crate) enum Sort {
    #[serde(alias = "score")]
    Score,
    #[default]
    #[serde(rename = "Source", alias = "SourceOrder", alias = "source")]
    SourceOrder,
}

impl Sort {
    pub(super) fn mode(self) -> nfm_search_core::search::SearchSortMode {
        match self {
            Self::Score => nfm_search_core::search::SearchSortMode::Score,
            Self::SourceOrder => nfm_search_core::search::SearchSortMode::SourceOrder,
        }
    }
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
    CommandPalette {
        #[serde(default)]
        position: Position,
        #[serde(default)]
        sort: Sort,
        #[serde(default)]
        initial_query: Option<String>,
        #[serde(default)]
        preview: Option<Preview>,
        #[serde(default)]
        preview_visible: bool,
    },
    Themes {
        #[serde(default)]
        position: Position,
        #[serde(default)]
        sort: Sort,
        #[serde(default)]
        initial_query: Option<String>,
        #[serde(default)]
        preview: Option<Preview>,
        #[serde(default)]
        preview_visible: bool,
    },
    ErrorLog {
        #[serde(default)]
        position: Position,
        #[serde(default)]
        sort: Sort,
        #[serde(default)]
        initial_query: Option<String>,
        #[serde(default)]
        preview: Option<Preview>,
        #[serde(default)]
        preview_visible: bool,
    },
    Factory {
        #[serde(skip)]
        factory: Option<std::rc::Rc<crate::config::PickerFactory>>,
    },
    Scrollback {
        #[serde(default)]
        position: Position,
        #[serde(default)]
        sort: Sort,
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
        sort: Sort,
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
        #[serde(default)]
        sort: Sort,
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        line_continuation: Option<String>,
        #[serde(default)]
        success_exit_codes: Option<Vec<i32>>,
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
        sort: Sort,
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
            | Self::FileSystem { preview, .. }
            | Self::ErrorLog { preview, .. }
            | Self::Themes { preview, .. }
            | Self::CommandPalette { preview, .. } => {
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
            Self::Factory { .. }
            | Self::ErrorLog { .. }
            | Self::Themes { .. }
            | Self::CommandPalette { .. } => {}
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
            Self::Factory { .. }
            | Self::ErrorLog { .. }
            | Self::Themes { .. }
            | Self::CommandPalette { .. } => (None, None),
        }
    }

    #[cfg(test)]
    pub(crate) fn callback(&self) -> Option<&std::rc::Rc<crate::picker::action::Callback>> {
        match self {
            Self::Scrollback { callback, .. }
            | Self::FileSystem { callback, .. }
            | Self::Command { callback, .. }
            | Self::Items { callback, .. } => callback.as_ref(),
            Self::Factory { .. }
            | Self::ErrorLog { .. }
            | Self::Themes { .. }
            | Self::CommandPalette { .. } => None,
        }
    }
}
fn default_regex() -> String {
    r"\S(?:[^\r\n]*\S)?".into()
}

pub(crate) struct KeyBinding {
    pub(crate) key: String,
    pub(crate) mods: String,
    pub(crate) target: BindingTarget,
}

pub(crate) enum BindingTarget {
    Action(crate::actions::AppAction),
    Picker(String),
}

impl<'de> Deserialize<'de> for KeyBinding {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawBinding {
            key: String,
            #[serde(default)]
            mods: String,
            picker: Option<String>,
            action: Option<String>,
        }
        let raw = RawBinding::deserialize(deserializer)?;
        let target = match (raw.action, raw.picker) {
            (Some(name), None) => BindingTarget::Action(
                crate::actions::AppAction::from_name(&name)
                    .unwrap_or(crate::actions::AppAction::Lua(name)),
            ),
            (None, Some(name)) => BindingTarget::Picker(name),
            _ => {
                return Err(serde::de::Error::custom(
                    "key binding must specify exactly one of action or picker",
                ));
            }
        };
        Ok(Self {
            key: raw.key,
            mods: raw.mods,
            target,
        })
    }
}

#[derive(Clone)]
pub(crate) struct Options {
    pub(crate) position: Position,
    pub(crate) sort: Sort,
    pub(crate) callback: Option<std::rc::Rc<crate::picker::action::Callback>>,
    pub(crate) source: Source,
    pub(crate) enter_action: EnterAction,
    pub(crate) params: BTreeMap<String, String>,
    pub(crate) preview: Option<Preview>,
    pub(crate) preview_visible: bool,
}

#[derive(Clone)]
pub(crate) enum Source {
    Themes,
    ErrorLog,
    CommandPalette(Vec<crate::actions::Entry>),
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
    pub(crate) success_exit_codes: Option<Vec<i32>>,
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
            sort: Sort::default(),
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
    action: crate::actions::AppAction,
    key: egui::Key,
    mods: egui::Modifiers,
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
    actions: BTreeMap<String, std::rc::Rc<crate::actions::LuaAction>>,
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
        Self::compile_with_actions(definitions, keys, defaults, BTreeMap::new())
    }

    pub(crate) fn compile_with_actions(
        definitions: Option<BTreeMap<String, Definition>>,
        keys: Option<Vec<KeyBinding>>,
        defaults: PickerDefaults,
        actions: BTreeMap<String, std::rc::Rc<crate::actions::LuaAction>>,
    ) -> Result<Self, String> {
        let definitions = definitions.unwrap_or_else(|| {
            BTreeMap::from([
                (
                    "lines".into(),
                    Definition::Scrollback {
                        position: Position::Bottom,
                        sort: Sort::default(),
                        unique: None,
                        callback: None,
                        regex: None,
                        enter_action: None,
                        initial_query: None,
                        preview: None,
                        preview_visible: true,
                    },
                ),
                (
                    "commands".into(),
                    Definition::CommandPalette {
                        position: Position::Cursor,
                        sort: Sort::default(),
                        initial_query: None,
                        preview: None,
                        preview_visible: false,
                    },
                ),
            ])
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
                | Definition::FileSystem { position, .. }
                | Definition::ErrorLog { position, .. }
                | Definition::Themes { position, .. }
                | Definition::CommandPalette { position, .. } => *position,
                Definition::Factory { .. } => unreachable!(),
            };
            let sort = match &definition {
                Definition::Command { sort, .. }
                | Definition::Scrollback { sort, .. }
                | Definition::Items { sort, .. }
                | Definition::FileSystem { sort, .. }
                | Definition::ErrorLog { sort, .. }
                | Definition::Themes { sort, .. }
                | Definition::CommandPalette { sort, .. } => *sort,
                Definition::Factory { .. } => unreachable!(),
            };
            let initial_query = match &definition {
                Definition::Scrollback { initial_query, .. }
                | Definition::Items { initial_query, .. }
                | Definition::Command { initial_query, .. }
                | Definition::FileSystem { initial_query, .. }
                | Definition::ErrorLog { initial_query, .. }
                | Definition::Themes { initial_query, .. }
                | Definition::CommandPalette { initial_query, .. } => initial_query.clone(),
                Definition::Factory { .. } => unreachable!(),
            };
            let preview = match &definition {
                Definition::Scrollback { preview, .. }
                | Definition::Items { preview, .. }
                | Definition::Command { preview, .. }
                | Definition::FileSystem { preview, .. }
                | Definition::ErrorLog { preview, .. }
                | Definition::Themes { preview, .. }
                | Definition::CommandPalette { preview, .. } => preview.clone(),
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
                }
                | Definition::ErrorLog {
                    preview_visible, ..
                }
                | Definition::Themes {
                    preview_visible, ..
                }
                | Definition::CommandPalette {
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
                Definition::Themes { .. } => Options {
                    position,
                    sort,
                    callback: None,
                    source: Source::Themes,
                    enter_action: EnterAction::Navigate,
                    params: BTreeMap::new(),
                    preview: None,
                    preview_visible: false,
                },
                Definition::ErrorLog { .. } => Options {
                    position,
                    sort,
                    callback: None,
                    source: Source::ErrorLog,
                    enter_action: EnterAction::Navigate,
                    params: BTreeMap::new(),
                    preview: None,
                    preview_visible: false,
                },
                Definition::CommandPalette { .. } => Options {
                    position,
                    sort,
                    callback: None,
                    source: Source::CommandPalette(Vec::new()),
                    enter_action: EnterAction::SendInput,
                    params: BTreeMap::new(),
                    preview: None,
                    preview_visible,
                },
                Definition::Factory { .. } => unreachable!(),
                Definition::Scrollback { regex, unique, .. } => {
                    let regex = Regex::new(&regex.unwrap_or_else(default_regex))
                        .map_err(|error| format!("pickers.{name}.regex: {error}"))?;
                    if regex.is_match("") {
                        return Err(format!("pickers.{name}.regex must not match empty text"));
                    }
                    Options {
                        position,
                        sort,
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
                        sort,
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
                    success_exit_codes,
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
                    if success_exit_codes.as_ref().is_some_and(|codes| {
                        codes.is_empty() || codes.len() > 16 || codes.iter().any(|code| *code < 0)
                    }) {
                        return Err(format!(
                            "pickers.{name}.success_exit_codes must contain 1 to 16 nonnegative exit codes"
                        ));
                    }
                    Options {
                        position,
                        sort,
                        callback,
                        source: Source::Command(CommandOptions {
                            args,
                            cwd,
                            line_continuation: line_continuation.filter(|value| !value.is_empty()),
                            unique: unique.unwrap_or(defaults.unique),
                            success_exit_codes,
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
                        sort,
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
            let mut keys = crate::actions::default_bindings();
            if compiled.contains_key("lines") {
                keys.push(KeyBinding {
                    key: "f".into(),
                    mods: "CTRL|SHIFT".into(),
                    target: BindingTarget::Picker("lines".into()),
                });
            }
            if compiled.get("commands").is_some_and(|entry| {
                matches!(
                    entry,
                    PickerEntry::Static(Options {
                        source: Source::CommandPalette(_),
                        ..
                    })
                )
            }) {
                keys.push(KeyBinding {
                    key: "Space".into(),
                    mods: "CTRL|SHIFT".into(),
                    target: BindingTarget::Picker("commands".into()),
                });
            }
            keys
        });
        let mut bindings: Vec<Binding> = Vec::new();
        for binding in keys {
            let key_name = if binding.key.len() == 1 && binding.key.is_ascii() {
                binding.key.to_uppercase()
            } else {
                binding.key.clone()
            };
            let key = egui::Key::from_name(&key_name)
                .ok_or_else(|| format!("unknown key binding key {:?}", binding.key))?;
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
                    _ => return Err(format!("unknown key binding modifier {part:?}")),
                }
            }
            if bindings
                .iter()
                .any(|existing| existing.key == key && same_mods(existing.mods, mods))
            {
                return Err(format!(
                    "duplicate key binding {}+{}",
                    binding.mods, binding.key
                ));
            }
            let action = match binding.target {
                BindingTarget::Action(action) => {
                    if let crate::actions::AppAction::Lua(name) = &action {
                        if !actions.contains_key(name) {
                            return Err(format!("unknown action {name:?} in key binding"));
                        }
                    }
                    action
                }
                BindingTarget::Picker(name) => {
                    if !compiled.contains_key(&name) {
                        return Err(format!("unknown picker {name:?} in key binding"));
                    }
                    crate::actions::AppAction::OpenPicker(name)
                }
            };
            bindings.push(Binding { action, key, mods });
        }
        Ok(Self {
            bindings,
            definitions: compiled,
            actions,
        })
    }

    pub(crate) fn named(&self, name: &str) -> Option<PickerEntry> {
        self.definitions.get(name).cloned()
    }

    pub(crate) fn lua_action(&self, name: &str) -> Option<std::rc::Rc<crate::actions::LuaAction>> {
        self.actions.get(name).cloned()
    }

    pub(crate) fn lua_actions(&self) -> impl Iterator<Item = (&str, &crate::actions::LuaAction)> {
        self.actions
            .iter()
            .map(|(name, action)| (name.as_str(), action.as_ref()))
    }

    pub(crate) fn has_themes_picker(&self) -> bool {
        self.definitions.values().any(|entry| {
            matches!(entry, PickerEntry::Static(options) if matches!(options.source, Source::Themes))
        })
    }

    pub(crate) fn has_error_log_picker(&self) -> bool {
        self.definitions.values().any(|entry| {
            matches!(entry, PickerEntry::Static(options) if matches!(options.source, Source::ErrorLog))
        })
    }

    pub(crate) fn palette_entries(&self) -> impl Iterator<Item = (&str, Option<String>)> {
        self.definitions
            .iter()
            .filter(|(_, entry)| match entry {
                PickerEntry::Factory(..) => true,
                PickerEntry::Static(options) => match &options.source {
                    Source::CommandPalette(_) => false,
                    Source::Themes | Source::ErrorLog => true,
                    Source::Items(items) => !items.values.is_empty() || items.provider.is_some(),
                    Source::Command(command) => command.args.iter().all(|arg| {
                        arg.strip_prefix('{')
                            .and_then(|arg| arg.strip_suffix('}'))
                            .is_none_or(|name| options.params.contains_key(name))
                    }),
                    _ => true,
                },
            })
            .map(|(name, _)| {
                (
                    name.as_str(),
                    self.shortcut_for(&crate::actions::AppAction::OpenPicker(name.clone())),
                )
            })
    }

    pub(crate) fn shortcut_for(&self, action: &crate::actions::AppAction) -> Option<String> {
        let text = self
            .bindings
            .iter()
            .filter(|binding| binding.action == *action)
            .map(|binding| crate::actions::shortcut_label(binding.key, binding.mods))
            .collect::<Vec<_>>()
            .join(", ");
        (!text.is_empty()).then_some(text)
    }

    #[cfg(test)]
    pub(crate) fn consume(
        &self,
        events: &mut Vec<egui::Event>,
    ) -> (Option<crate::actions::AppAction>, bool) {
        self.consume_when(events, |_, _| true)
    }

    pub(crate) fn consume_when(
        &self,
        events: &mut Vec<egui::Event>,
        allows: impl Fn(&crate::actions::AppAction, egui::Modifiers) -> bool,
    ) -> (Option<crate::actions::AppAction>, bool) {
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
                        && allows(&binding.action, binding.mods)
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
                    open = Some(binding.action.clone());
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
    use crate::actions::{ActionContext, AppAction};

    fn key_event(
        key: egui::Key,
        modifiers: egui::Modifiers,
        pressed: bool,
        repeat: bool,
    ) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            modifiers,
            pressed,
            repeat,
        }
    }

    #[test]
    fn omitted_keys_preserve_the_original_action_bindings() {
        let bindings = Bindings::default();
        let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        for (key, mods, expected) in [
            (egui::Key::N, ctrl_shift, AppAction::NewWindow),
            (egui::Key::E, ctrl_shift, AppAction::ToggleFloatingPane),
            (egui::Key::W, ctrl_shift, AppAction::CloseFloatingPane),
            (egui::Key::Slash, ctrl_shift, AppAction::OpenSearch),
            (egui::Key::M, ctrl_shift, AppAction::EnterCopyMode),
            (
                egui::Key::Space,
                egui::Modifiers::ALT,
                AppAction::OpenQuickSelect,
            ),
            (egui::Key::ArrowUp, ctrl_shift, AppAction::PreviousCommand),
            (egui::Key::ArrowDown, ctrl_shift, AppAction::NextCommand),
        ] {
            let mut events = vec![key_event(key, mods, true, false)];
            assert_eq!(bindings.consume(&mut events), (Some(expected), true));
            assert!(events.is_empty());
        }
        let mut events = vec![
            egui::Event::Key {
                key: egui::Key::Questionmark,
                physical_key: Some(egui::Key::Slash),
                modifiers: ctrl_shift,
                pressed: true,
                repeat: false,
            },
            egui::Event::Text("?".into()),
        ];
        assert_eq!(
            bindings.consume(&mut events),
            (Some(AppAction::OpenSearch), true)
        );
        assert!(events.is_empty());
    }

    #[test]
    fn action_rebinding_replaces_defaults_and_updates_palette_shortcuts() {
        let keys = serde_json::from_value(serde_json::json!([
            { "key": "k", "mods": "CTRL", "action": "ToggleFloatingPane" },
            { "key": "k", "mods": "ALT", "action": "ToggleFloatingPane" },
            { "key": "e", "mods": "CTRL|SHIFT", "picker": "lines" },
            { "key": "x", "mods": "CTRL|SHIFT", "action": "ExitCopyMode" }
        ]))
        .unwrap();
        let bindings = Bindings::compile(None, Some(keys)).unwrap();
        let mut events = vec![key_event(egui::Key::K, egui::Modifiers::CTRL, true, false)];
        assert_eq!(
            bindings.consume(&mut events),
            (Some(AppAction::ToggleFloatingPane), true)
        );
        let mut events = vec![key_event(
            egui::Key::E,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            true,
            false,
        )];
        assert_eq!(
            bindings.consume(&mut events),
            (Some(AppAction::OpenPicker("lines".into())), true)
        );
        let mut events = vec![key_event(
            egui::Key::N,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            true,
            false,
        )];
        assert_eq!(bindings.consume(&mut events), (None, false));
        assert_eq!(events.len(), 1);
        let catalog = crate::actions::catalog(&bindings, true, true);
        assert_eq!(
            catalog
                .iter()
                .find(|entry| entry.action == AppAction::ToggleFloatingPane)
                .unwrap()
                .shortcut
                .as_deref(),
            Some("Ctrl+K, Alt+K")
        );
        assert_eq!(
            catalog
                .iter()
                .find(|entry| entry.action == AppAction::OpenSearch)
                .unwrap()
                .shortcut,
            None
        );
        assert_eq!(
            catalog
                .iter()
                .find(|entry| entry.action == AppAction::ExitCopyMode)
                .unwrap()
                .shortcut
                .as_deref(),
            Some("Ctrl+Shift+X")
        );
    }

    #[test]
    fn explicit_empty_keys_leave_every_action_unbound() {
        let bindings = Bindings::compile(None, Some(Vec::new())).unwrap();
        assert!(
            crate::actions::catalog(&bindings, true, false)
                .iter()
                .all(|entry| entry.shortcut.is_none())
        );
        for (key, mods) in [
            (egui::Key::N, egui::Modifiers::CTRL | egui::Modifiers::SHIFT),
            (egui::Key::M, egui::Modifiers::CTRL | egui::Modifiers::SHIFT),
            (egui::Key::Space, egui::Modifiers::ALT),
        ] {
            let mut events = vec![key_event(key, mods, true, false)];
            assert_eq!(bindings.consume(&mut events), (None, false));
            assert_eq!(events.len(), 1);
        }
    }

    #[test]
    fn actions_consume_paired_text_repeats_and_releases_without_retriggering() {
        let bindings = Bindings::default();
        for (pressed, repeat) in [(true, false), (true, true), (false, false)] {
            let mut events = vec![key_event(
                egui::Key::N,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                pressed,
                repeat,
            )];
            if pressed {
                events.push(egui::Event::Text("N".into()));
            }
            events.push(egui::Event::Paste("unrelated".into()));
            assert_eq!(
                bindings.consume(&mut events),
                ((pressed && !repeat).then_some(AppAction::NewWindow), true)
            );
            assert_eq!(events, vec![egui::Event::Paste("unrelated".into())]);
        }
    }

    #[test]
    fn scoped_actions_preserve_modal_query_input() {
        let keys =
            serde_json::from_value(serde_json::json!([{ "key": "s", "action": "OpenSearch" }]))
                .unwrap();
        let bindings = Bindings::compile(None, Some(keys)).unwrap();
        let original = vec![
            key_event(egui::Key::S, egui::Modifiers::NONE, true, false),
            egui::Event::Text("s".into()),
        ];
        for context in [
            ActionContext {
                picker: true,
                ..Default::default()
            },
            ActionContext {
                quick_select: true,
                ..Default::default()
            },
            ActionContext {
                palette: true,
                ..Default::default()
            },
        ] {
            let mut events = original.clone();
            assert_eq!(
                bindings.consume_when(&mut events, |action, mods| context.allows(action, mods)),
                (None, false)
            );
            assert_eq!(events, original);
        }
        let search = ActionContext {
            search: true,
            ..Default::default()
        };
        let mut events = original.clone();
        assert_eq!(
            bindings.consume_when(&mut events, |action, mods| search.allows(action, mods)),
            (None, false)
        );
        assert_eq!(events, original);
        let defaults = Bindings::default();
        let mut events = vec![key_event(
            egui::Key::Slash,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            true,
            false,
        )];
        assert_eq!(
            defaults.consume_when(&mut events, |action, mods| search.allows(action, mods)),
            (Some(AppAction::OpenSearch), true)
        );
        assert!(events.is_empty());
        assert!(!search.allows(&AppAction::NewWindow, egui::Modifiers::NONE));
        assert!(search.allows(&AppAction::NewWindow, egui::Modifiers::CTRL));
        assert!(
            !ActionContext {
                palette: true,
                ..Default::default()
            }
            .allows(&AppAction::NewWindow, egui::Modifiers::CTRL)
        );
        assert!(
            ActionContext {
                quick_select: true,
                ..Default::default()
            }
            .allows(&AppAction::OpenQuickSelect, egui::Modifiers::ALT)
        );
        assert!(!ActionContext::default().allows(&AppAction::ExitCopyMode, egui::Modifiers::CTRL));
        assert!(
            ActionContext {
                copying: true,
                ..Default::default()
            }
            .allows(&AppAction::ExitCopyMode, egui::Modifiers::CTRL)
        );
    }

    #[test]
    fn every_picker_accepts_sort_and_defaults_to_source_order() {
        for kind in [
            "CommandPalette",
            "Scrollback",
            "Items",
            "Command",
            "FileSystem",
        ] {
            for sort in [
                None,
                Some("Source"),
                Some("SourceOrder"),
                Some("source"),
                Some("Score"),
            ] {
                let mut definition = serde_json::json!({ "type": kind });
                if kind == "Command" {
                    definition["args"] = serde_json::json!(["cmd"]);
                }
                if let Some(sort) = sort {
                    definition["sort"] = serde_json::json!(sort);
                }
                let definitions =
                    BTreeMap::from([("test".into(), serde_json::from_value(definition).unwrap())]);
                let bindings = Bindings::compile(Some(definitions), Some(Vec::new())).unwrap();
                let options = bindings
                    .named("test")
                    .unwrap()
                    .resolve(&Default::default())
                    .unwrap();
                assert_eq!(
                    matches!(options.sort, Sort::Score),
                    sort == Some("Score"),
                    "{kind}: {sort:?}"
                );
            }
        }
    }

    #[test]
    fn invalid_targets_and_cross_target_duplicates_are_rejected() {
        for keys in [
            serde_json::json!([{ "key": "k", "action": "Missing" }]),
            serde_json::json!([{ "key": "k", "action": "OpenPicker" }]),
            serde_json::json!([{ "key": "k" }]),
            serde_json::json!([{ "key": "k", "action": "OpenSearch", "picker": "lines" }]),
            serde_json::json!([{ "key": "k", "action": "OpenSearch" }, { "key": "k", "picker": "lines" }]),
        ] {
            let result = serde_json::from_value(keys)
                .map_err(|error| error.to_string())
                .and_then(|keys| Bindings::compile(None, Some(keys)));
            assert!(result.is_err());
        }
    }

    #[test]
    fn configured_special_pickers_replace_only_matching_palette_builtins() {
        for themes in [false, true] {
            for errors in [false, true] {
                let mut definitions = serde_json::Map::new();
                // Detection uses picker type, not its name or position.
                if themes {
                    definitions.insert(
                        "appearance".into(),
                        serde_json::json!({
                            "type": "Themes", "position": "Cursor"
                        }),
                    );
                }
                if errors {
                    definitions.insert(
                        "diagnostics".into(),
                        serde_json::json!({
                            "type": "ErrorLog", "position": "Cursor"
                        }),
                    );
                }
                let bindings = Bindings::compile(
                    Some(serde_json::from_value(definitions.into()).unwrap()),
                    Some(Vec::new()),
                )
                .unwrap();
                let entries = crate::actions::catalog(&bindings, false, false);
                for (configured, builtin, name) in [
                    (themes, AppAction::OpenThemePicker, "appearance"),
                    (errors, AppAction::OpenErrorLog, "diagnostics"),
                ] {
                    assert_eq!(
                        entries.iter().any(|entry| entry.action == builtin),
                        !configured
                    );
                    assert_eq!(
                        entries
                            .iter()
                            .any(|entry| { entry.action == AppAction::OpenPicker(name.into()) }),
                        configured
                    );
                }
            }
        }
    }

    #[test]
    fn palettes_use_normal_bindings_and_exclude_all_palette_definitions() {
        let definitions = serde_json::from_value(serde_json::json!({
            "commands": { "type": "CommandPalette" },
            "alternate": { "type": "CommandPalette", "position": "Top", "initial_query": "floating" },
            "lines": { "type": "Scrollback" }
        })).unwrap();
        let keys = serde_json::from_value(serde_json::json!([
            { "key": "k", "mods": "CTRL|SHIFT", "picker": "alternate" }
        ]))
        .unwrap();
        let bindings = Bindings::compile(Some(definitions), Some(keys)).unwrap();
        assert!(
            bindings
                .palette_entries()
                .all(|(name, _)| name != "commands" && name != "alternate")
        );
        let mods = egui::Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        };
        for repeat in [false, true] {
            let mut events = vec![
                egui::Event::Key {
                    key: egui::Key::K,
                    physical_key: Some(egui::Key::K),
                    pressed: true,
                    repeat,
                    modifiers: mods,
                },
                egui::Event::Text("K".into()),
                egui::Event::Text("unrelated".into()),
            ];
            let (open, consumed) = bindings.consume(&mut events);
            assert!(consumed);
            assert_eq!(open.is_some(), !repeat);
            if let Some(entry) = open {
                let crate::actions::AppAction::OpenPicker(name) = entry else {
                    panic!("expected picker action")
                };
                let options = bindings
                    .named(&name)
                    .unwrap()
                    .resolve(&Default::default())
                    .unwrap();
                assert!(matches!(options.source, Source::CommandPalette(_)));
                assert_eq!(
                    options.params.get("query").map(String::as_str),
                    Some("floating")
                );
            }
            assert!(matches!(&events[..], [egui::Event::Text(text)] if text == "unrelated"));
        }
        assert!(
            serde_json::from_str::<Definition>(
                r#"{"type":"CommandPalette","enter_action":"SendInput"}"#
            )
            .is_err()
        );
    }

    use super::*;

    #[test]
    fn every_picker_type_accepts_position_and_defaults_to_bottom() {
        for kind in [
            "Scrollback",
            "Items",
            "Command",
            "FileSystem",
            "CommandPalette",
        ] {
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
        for kind in [
            "Scrollback",
            "Items",
            "Command",
            "FileSystem",
            "CommandPalette",
        ] {
            for (field, expected) in [
                ("", kind != "CommandPalette"),
                (",\"preview_visible\":false", false),
                (",\"preview_visible\":true", true),
            ] {
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

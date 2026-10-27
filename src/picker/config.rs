use crate::config::keybindings::same_mods;
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Default, Deserialize)]
pub(crate) enum EnterAction {
    #[default]
    Navigate,
    SendInput,
    /// Enter only closes the picker, as in the messages picker.
    #[serde(skip)]
    Close,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
pub(crate) enum Position {
    Top,
    Center,
    #[default]
    Bottom,
    Cursor,
    FullPane,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
pub(crate) enum Scope {
    #[default]
    Pane,
    Workspace,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct BuiltinPresentation {
    pub(crate) position: Option<Position>,
    pub(crate) preview_visible: Option<bool>,
    pub(crate) result_rows: Option<usize>,
    pub(crate) preview_rows: Option<usize>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct BuiltinPickers {
    pub(crate) line_break: BuiltinPresentation,
}

impl BuiltinPickers {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for (name, value, max) in [
            ("result_rows", self.line_break.result_rows, 256),
            ("preview_rows", self.line_break.preview_rows, 12),
        ] {
            if value.is_some_and(|value| value == 0 || value > max) {
                return Err(format!(
                    "builtin_pickers.line_break.{name} must be between 1 and {max}"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
pub(crate) enum Sort {
    #[default]
    #[serde(alias = "score")]
    Score,
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
        /// Wrap at the preview width without inserting hard line breaks.
        #[serde(default)]
        wrap: bool,
    },
    Ansi {
        text: String,
    },
    Dynamic {
        #[serde(skip)]
        resolve: Option<std::rc::Rc<crate::lua_callbacks::PreviewResolver>>,
        #[serde(default)]
        max_bytes: Option<usize>,
        #[serde(default)]
        max_scrollback_lines: Option<usize>,
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
        #[serde(default)]
        max_bytes: Option<usize>,
        #[serde(default)]
        max_scrollback_lines: Option<usize>,
    },
}

fn default_preview_rows() -> usize {
    12
}
fn default_result_rows() -> usize {
    super::RESULT_ROWS
}
pub(crate) fn file_syntax(value: &str) -> nfm_preview_file::SyntaxChoice {
    match value {
        "Auto" => nfm_preview_file::SyntaxChoice::Auto,
        "PlainText" => nfm_preview_file::SyntaxChoice::PlainText,
        name => nfm_preview_file::SyntaxChoice::Named(name.into()),
    }
}

impl Preview {
    fn validate(&self) -> Result<(), String> {
        if let Self::Dynamic {
            resolve,
            max_bytes,
            max_scrollback_lines,
        } = self
        {
            validate_preview_limits(*max_bytes, *max_scrollback_lines)?;
            if max_bytes.is_some_and(|bytes| bytes > 256 * 1024 * 1024) {
                return Err("Dynamic preview max_bytes must not exceed 256 MiB".into());
            }
            if resolve.is_none() {
                return Err("Dynamic preview requires a resolve function".into());
            }
        }
        if let Self::Scrollback { rows } = self {
            if !(1..=12).contains(rows) {
                return Err("scrollback preview rows must be between 1 and 12".into());
            }
        }
        if let Self::Command {
            args,
            cwd,
            max_bytes,
            max_scrollback_lines,
        } = self
        {
            validate_preview_limits(*max_bytes, *max_scrollback_lines)?;
            if args.first().is_none_or(String::is_empty) {
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

    pub(crate) fn set_resolver(
        &mut self,
        value: std::rc::Rc<crate::lua_callbacks::PreviewResolver>,
    ) {
        if let Self::Dynamic { resolve, .. } = self {
            *resolve = Some(value);
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PickerDefaults {
    pub(crate) unique: bool,
    pub(crate) preview_max_bytes: Option<usize>,
    pub(crate) preview_max_scrollback_lines: Option<usize>,
}

fn validate_preview_limits(bytes: Option<usize>, lines: Option<usize>) -> Result<(), String> {
    if bytes.is_some_and(|value| value == 0 || value.checked_mul(2).is_none()) {
        return Err("max_bytes must be positive and fit after newline normalization".into());
    }
    if lines == Some(0) {
        return Err("max_scrollback_lines must be positive".into());
    }
    Ok(())
}

impl PickerDefaults {
    pub(crate) fn validate(&self) -> Result<(), String> {
        validate_preview_limits(self.preview_max_bytes, self.preview_max_scrollback_lines)
    }
}

/// Shared picker settings; external Lua/IPC definitions remain flat.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PickerPresentation {
    pub(crate) scope: Scope,
    pub(crate) position: Position,
    pub(crate) result_rows: usize,
    pub(crate) sort: Sort,
    pub(crate) initial_query: Option<String>,
    #[serde(skip)]
    pub(crate) keys: Vec<PickerKey>,
    pub(crate) preview: Option<Preview>,
    pub(crate) preview_visible: bool,
}

impl Default for PickerPresentation {
    fn default() -> Self {
        Self {
            scope: Scope::Pane,
            position: Position::default(),
            result_rows: default_result_rows(),
            sort: Sort::default(),
            initial_query: None,
            keys: Vec::new(),
            preview: None,
            preview_visible: true,
        }
    }
}

pub(crate) struct Definition {
    pub(crate) presentation: PickerPresentation,
    pub(crate) source: DefinitionSource,
}

impl<'de> Deserialize<'de> for Definition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut source = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        // Extract common fields before strict source deserialization. Keeping the
        // residual map preserves unknown-field and cross-source validation.
        let mut common = serde_json::Map::new();
        for field in [
            "scope",
            "position",
            "result_rows",
            "sort",
            "initial_query",
            "keys",
            "preview",
            "preview_visible",
        ] {
            if let Some(value) = source.remove(field) {
                common.insert(field.into(), value);
            }
        }
        if source.get("type").and_then(serde_json::Value::as_str) == Some("Factory")
            && !common.is_empty()
        {
            return Err(D::Error::custom(
                "Factory does not accept presentation fields",
            ));
        }
        if !common.contains_key("preview_visible")
            && matches!(
                source.get("type").and_then(serde_json::Value::as_str),
                Some("Themes" | "ErrorLog" | "CommandPalette")
            )
        {
            common.insert("preview_visible".into(), false.into());
        }
        if !common.contains_key("scope")
            && source.get("type").and_then(serde_json::Value::as_str) == Some("CommandPalette")
        {
            common.insert("scope".into(), "Workspace".into());
        }
        let presentation = serde_json::from_value(common.into()).map_err(D::Error::custom)?;
        let source = serde_json::from_value(source.into()).map_err(D::Error::custom)?;
        Ok(Self {
            presentation,
            source,
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum DefinitionSource {
    CommandPalette {},
    Themes {},
    ErrorLog {},
    Factory {
        #[serde(skip)]
        factory: Option<std::rc::Rc<crate::config::PickerFactory>>,
    },
    Scrollback {
        #[serde(default)]
        regex: Option<String>,
        #[serde(default)]
        unique: Option<bool>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    },
    Items {
        #[serde(default)]
        items: Vec<crate::picker::wire::Item>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
        #[serde(skip)]
        provider: Option<std::rc::Rc<crate::lua_callbacks::ItemsProvider>>,
    },
    Command {
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
        columns: Option<Vec<String>>,
        #[serde(default)]
        delimiter: Option<String>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    },
    /// The lines of a text file, newest first with `reverse`.
    File {
        path: String,
        #[serde(default)]
        reverse: bool,
        #[serde(default)]
        line_continuation: Option<String>,
        #[serde(default)]
        unique: Option<bool>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    },
    FileSystem {
        #[serde(default)]
        roots: Vec<String>,
        #[serde(default)]
        max_depth: Option<i32>,
        #[serde(default)]
        files_only: bool,
        #[serde(default)]
        directories_only: bool,
        #[serde(default)]
        relative: bool,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    },
}

pub(crate) type PickerKey = KeyBinding<std::rc::Rc<crate::lua_callbacks::Callback>>;

impl Definition {
    pub(crate) fn factory(factory: std::rc::Rc<crate::config::PickerFactory>) -> Self {
        Self {
            presentation: PickerPresentation::default(),
            source: DefinitionSource::Factory {
                factory: Some(factory),
            },
        }
    }

    /// Resolve one definition without constructing or compiling shortcut bindings.
    pub(crate) fn resolve(
        self,
        name: &str,
        defaults: PickerDefaults,
    ) -> Result<PickerEntry, String> {
        defaults.validate()?;
        if name.trim().is_empty() {
            return Err("picker names must not be empty".into());
        }
        if let DefinitionSource::Factory { factory } = self.source {
            return Ok(PickerEntry::Factory(
                factory.ok_or("missing picker factory")?,
                defaults,
            ));
        }
        let mut definition = self;
        let result_rows = definition.presentation.result_rows;
        if !(1..=256).contains(&result_rows) {
            return Err(format!(
                "pickers.{name}.result_rows must be between 1 and 256"
            ));
        }
        let (callback, enter_action) = definition.acceptance();
        let position = definition.presentation.position;
        let scope = definition.presentation.scope;
        let sort = definition.presentation.sort;
        let initial_query = definition.presentation.initial_query.take();
        let mut preview = definition.presentation.preview.take();
        let preview_visible = definition.presentation.preview_visible
            && !matches!(
                definition.source,
                DefinitionSource::Themes { .. } | DefinitionSource::ErrorLog { .. }
            );
        if let Some(
            Preview::Command {
                max_bytes,
                max_scrollback_lines,
                ..
            }
            | Preview::Dynamic {
                max_bytes,
                max_scrollback_lines,
                ..
            },
        ) = &mut preview
        {
            *max_bytes = Some(
                max_bytes
                    .or(defaults.preview_max_bytes)
                    .unwrap_or(10 * 1024 * 1024),
            );
            *max_scrollback_lines = Some(
                max_scrollback_lines
                    .or(defaults.preview_max_scrollback_lines)
                    .unwrap_or(8192),
            );
        }
        if let Some(preview) = &preview {
            preview
                .validate()
                .map_err(|error| format!("pickers.{name}.preview: {error}"))?;
        }
        let local_keys = definition
            .keys_mut()
            .map(|keys| std::mem::take(keys))
            .unwrap_or_default();
        let (source, enter_action) = match definition.source {
            DefinitionSource::Themes { .. } => (Source::Themes, EnterAction::Navigate),
            DefinitionSource::ErrorLog { .. } => (Source::ErrorLog, EnterAction::Navigate),
            DefinitionSource::CommandPalette { .. } => {
                (Source::CommandPalette(Vec::new()), EnterAction::SendInput)
            }
            DefinitionSource::Factory { .. } => unreachable!(),
            DefinitionSource::Scrollback { regex, unique, .. } => {
                let regex = Regex::new(&regex.unwrap_or_else(default_regex))
                    .map_err(|error| format!("pickers.{name}.regex: {error}"))?;
                if regex.is_match("") {
                    return Err(format!("pickers.{name}.regex must not match empty text"));
                }
                (
                    Source::Scrollback {
                        regex,
                        unique: unique.unwrap_or(defaults.unique),
                    },
                    enter_action.unwrap_or(EnterAction::Navigate),
                )
            }
            DefinitionSource::Items {
                items, provider, ..
            } => {
                crate::picker::wire::validate_items(&items)?;
                if callback.is_none() && matches!(enter_action, Some(EnterAction::Navigate)) {
                    return Err(format!(
                        "pickers.{name}: Items only supports enter_action SendInput"
                    ));
                }
                (
                    Source::Items(ItemsOptions {
                        values: items,
                        provider,
                    }),
                    EnterAction::SendInput,
                )
            }
            DefinitionSource::Command {
                args,
                cwd,
                line_continuation,
                unique,
                success_exit_codes,
                columns,
                delimiter,
                ..
            } => {
                if args.is_empty()
                    || args[0].trim().is_empty()
                    || (args[0].starts_with('{') && args[0].ends_with('}'))
                {
                    return Err(format!("pickers.{name}.args requires an executable"));
                }
                if let Some(index) = args.iter().position(|arg| arg.contains('\0')) {
                    return Err(format!("pickers.{name}.args[{index}] contains NUL"));
                }
                if cwd.as_ref().is_some_and(|value| value.contains('\0')) {
                    return Err(format!("pickers.{name}.cwd contains NUL"));
                }
                if line_continuation
                    .as_ref()
                    .is_some_and(|value| value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n')))
                {
                    return Err(format!(
                        "pickers.{name}.line_continuation cannot contain NUL or a newline; use a literal continuation marker"
                    ));
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
                let columns = match (columns, delimiter) {
                    (None, None) => None,
                    (None, Some(_)) => {
                        return Err(format!("pickers.{name}.delimiter requires columns"));
                    }
                    (Some(names), delimiter) => {
                        if names.len() > 32 {
                            return Err(format!(
                                "pickers.{name}.columns allows at most 32 columns"
                            ));
                        }
                        nfm_picker_sources::structured::StructuredSchema::new(names.clone())
                            .map_err(|error| format!("pickers.{name}.columns: {error}"))?;
                        let delimiter = delimiter.unwrap_or_else(|| "\t".into());
                        if delimiter.is_empty()
                            || delimiter.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
                        {
                            return Err(format!(
                                "pickers.{name}.delimiter must be nonempty without NUL or newlines"
                            ));
                        }
                        // Structured rows are parsed per line and not deduplicated.
                        if line_continuation.as_ref().is_some_and(|v| !v.is_empty())
                            || unique == Some(true)
                        {
                            return Err(format!(
                                "pickers.{name}.columns cannot be combined with line_continuation or unique"
                            ));
                        }
                        Some(CommandColumns { names, delimiter })
                    }
                };
                (
                    Source::Command(CommandOptions {
                        args,
                        cwd,
                        line_continuation: line_continuation.filter(|value| !value.is_empty()),
                        unique: columns.is_none() && unique.unwrap_or(defaults.unique),
                        success_exit_codes,
                        columns,
                    }),
                    EnterAction::SendInput,
                )
            }
            DefinitionSource::File {
                path,
                reverse,
                line_continuation,
                unique,
                ..
            } => {
                if path.trim().is_empty() || path.contains('\0') {
                    return Err(format!("pickers.{name}.path must be a nonempty path"));
                }
                if line_continuation
                    .as_ref()
                    .is_some_and(|value| value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n')))
                {
                    return Err(format!(
                        "pickers.{name}.line_continuation cannot contain NUL or a newline; use a literal continuation marker"
                    ));
                }
                if callback.is_none() && matches!(enter_action, Some(EnterAction::Navigate)) {
                    return Err(format!(
                        "pickers.{name}: File only supports enter_action SendInput"
                    ));
                }
                (
                    Source::TextFile(TextFileOptions {
                        path,
                        reverse,
                        line_continuation: line_continuation.filter(|value| !value.is_empty()),
                        unique: unique.unwrap_or(defaults.unique),
                    }),
                    EnterAction::SendInput,
                )
            }
            DefinitionSource::FileSystem {
                roots,
                max_depth,
                files_only,
                directories_only,
                relative,
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
                (
                    Source::FileSystem(FileOptions {
                        roots: roots.into_iter().map(std::path::PathBuf::from).collect(),
                        max_depth: max_depth.unwrap_or(i32::MAX),
                        files_only,
                        directories_only,
                        relative,
                    }),
                    EnterAction::SendInput,
                )
            }
        };
        // A file streams through the command picker's source.
        let label = matches!(source, Source::TextFile(_)).then_some("FILE PICKER");
        let mut options = Options {
            scope,
            position,
            result_rows,
            sort,
            callback,
            keys: local_keys,
            source,
            enter_action,
            params: BTreeMap::new(),
            preview: None,
            preview_visible,
            label,
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
        Ok(PickerEntry::Static(options))
    }

    pub(crate) fn keys_mut(&mut self) -> Option<&mut Vec<PickerKey>> {
        if matches!(self.source, DefinitionSource::Factory { .. }) {
            None
        } else {
            Some(&mut self.presentation.keys)
        }
    }

    pub(crate) fn set_preview_resolver(
        &mut self,
        value: std::rc::Rc<crate::lua_callbacks::PreviewResolver>,
    ) {
        if let Some(preview) = &mut self.presentation.preview {
            preview.set_resolver(value);
        }
    }

    pub(crate) fn set_items_provider(
        &mut self,
        value: std::rc::Rc<crate::lua_callbacks::ItemsProvider>,
    ) {
        if let DefinitionSource::Items { provider, .. } = &mut self.source {
            *provider = Some(value);
        }
    }
    pub(crate) fn set_callback(&mut self, value: std::rc::Rc<crate::lua_callbacks::Callback>) {
        match &mut self.source {
            DefinitionSource::Scrollback { callback, .. }
            | DefinitionSource::FileSystem { callback, .. }
            | DefinitionSource::Command { callback, .. }
            | DefinitionSource::File { callback, .. }
            | DefinitionSource::Items { callback, .. } => *callback = Some(value),
            DefinitionSource::Factory { .. }
            | DefinitionSource::ErrorLog { .. }
            | DefinitionSource::Themes { .. }
            | DefinitionSource::CommandPalette { .. } => {}
        }
    }

    fn acceptance(
        &self,
    ) -> (
        Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
        Option<EnterAction>,
    ) {
        match &self.source {
            DefinitionSource::Scrollback {
                callback,
                enter_action,
                ..
            }
            | DefinitionSource::Items {
                callback,
                enter_action,
                ..
            }
            | DefinitionSource::Command {
                callback,
                enter_action,
                ..
            }
            | DefinitionSource::File {
                callback,
                enter_action,
                ..
            }
            | DefinitionSource::FileSystem {
                callback,
                enter_action,
                ..
            } => (callback.clone(), *enter_action),
            DefinitionSource::Factory { .. }
            | DefinitionSource::ErrorLog { .. }
            | DefinitionSource::Themes { .. }
            | DefinitionSource::CommandPalette { .. } => (None, None),
        }
    }

    #[cfg(test)]
    pub(crate) fn callback(&self) -> Option<&std::rc::Rc<crate::lua_callbacks::Callback>> {
        match &self.source {
            DefinitionSource::Scrollback { callback, .. }
            | DefinitionSource::FileSystem { callback, .. }
            | DefinitionSource::Command { callback, .. }
            | DefinitionSource::File { callback, .. }
            | DefinitionSource::Items { callback, .. } => callback.as_ref(),
            DefinitionSource::Factory { .. }
            | DefinitionSource::ErrorLog { .. }
            | DefinitionSource::Themes { .. }
            | DefinitionSource::CommandPalette { .. } => None,
        }
    }
}
fn default_regex() -> String {
    r"\S(?:[^\r\n]*\S)?".into()
}

pub(crate) use crate::config::keybindings::{BindingTrigger, KeyBinding};

#[derive(Clone)]
pub(crate) enum BindingTarget {
    Action(crate::actions::AppAction),
    Picker(String),
}

pub(crate) const BUILTIN_NAMES: [&str; 4] = ["themes", "command_palette", "lines", "words"];

pub(crate) fn builtin_defaults(name: &str) -> serde_json::Value {
    match name {
        "themes" => {
            serde_json::json!({"type":"Themes", "position":"Cursor", "sort":"Score", "preview_visible":false})
        }
        "command_palette" => {
            serde_json::json!({"type":"CommandPalette", "scope":"Workspace", "position":"Center", "preview_visible":false})
        }
        "lines" => {
            serde_json::json!({"type":"Scrollback", "enter_action":"Navigate", "preview":{"type":"Scrollback", "rows":12}})
        }
        "words" => {
            serde_json::json!({"type":"Scrollback", "regex":"\\S+", "enter_action":"SendInput", "unique":true, "preview_visible":false, "preview":{"type":"Scrollback", "rows":12}})
        }
        _ => unreachable!("validated builtin picker name"),
    }
}

#[derive(Clone)]
pub(crate) struct Options {
    pub(crate) scope: Scope,
    pub(crate) result_rows: usize,
    pub(crate) position: Position,
    pub(crate) sort: Sort,
    pub(crate) callback: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    pub(crate) keys: Vec<PickerKey>,
    pub(crate) source: Source,
    pub(crate) enter_action: EnterAction,
    pub(crate) params: BTreeMap<String, String>,
    pub(crate) preview: Option<Preview>,
    pub(crate) preview_visible: bool,
    /// The header label, in place of the one named for the source.
    pub(crate) label: Option<&'static str>,
}

#[derive(Clone)]
pub(crate) enum Source {
    Themes,
    ErrorLog,
    CommandPalette(Vec<crate::actions::Entry>),
    Workspaces(Vec<crate::actions::Entry>),
    Scrollback { regex: Regex, unique: bool },
    FileSystem(FileOptions),
    Items(ItemsOptions),
    Command(CommandOptions),
    TextFile(TextFileOptions),
}

#[derive(Clone)]
pub(crate) struct ItemsOptions {
    pub(crate) values: Vec<crate::picker::wire::Item>,
    pub(crate) provider: Option<std::rc::Rc<crate::lua_callbacks::ItemsProvider>>,
}

#[derive(Clone)]
pub(crate) struct CommandOptions {
    pub(crate) args: Vec<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) line_continuation: Option<String>,
    pub(crate) unique: bool,
    pub(crate) success_exit_codes: Option<Vec<i32>>,
    /// Split each output line into these columns for a structured picker.
    pub(crate) columns: Option<CommandColumns>,
}

#[derive(Clone)]
pub(crate) struct TextFileOptions {
    /// A path, or `{name}` for a request parameter. Relative paths resolve
    /// against the `cwd` parameter.
    pub(crate) path: String,
    pub(crate) reverse: bool,
    pub(crate) line_continuation: Option<String>,
    pub(crate) unique: bool,
}

#[derive(Clone)]
pub(crate) struct CommandColumns {
    pub(crate) names: Vec<String>,
    pub(crate) delimiter: String,
}

#[derive(Clone)]
#[cfg_attr(not(windows), allow(dead_code))] // Reserved for NFM's future Linux backend.
pub(crate) struct FileOptions {
    pub(crate) roots: Vec<std::path::PathBuf>,
    pub(crate) max_depth: i32,
    pub(crate) files_only: bool,
    pub(crate) directories_only: bool,
    /// Show paths relative to their root. With several roots the same
    /// relative path can come from more than one of them.
    pub(crate) relative: bool,
}

impl Options {
    /// Commands and previews run relative to the shell's directory (`pwd`)
    /// unless the request or the command definition chooses one.
    pub(crate) fn default_cwd(&mut self, pwd: Option<String>) {
        if self.params.contains_key("cwd") {
            return;
        }
        let cwd = match &self.source {
            Source::Command(command) if command.cwd.is_some() => command.cwd.clone(),
            _ => pwd,
        };
        if let Some(cwd) = cwd {
            self.params.insert("cwd".into(), cwd);
        }
    }
    pub(crate) fn with_request(mut self, params: BTreeMap<String, String>) -> Result<Self, String> {
        if let Source::Items(items) = &mut self.source {
            if params.contains_key("items") {
                items.values = crate::picker::wire::items(&params)?;
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
            if let Some(relative) = params.get("relative") {
                files.relative = relative
                    .parse()
                    .map_err(|_| format!("invalid picker relative: {relative}"))?;
            }
        }
        self.params.extend(params);
        Ok(self)
    }
}
impl Default for Options {
    fn default() -> Self {
        Self {
            scope: Scope::Pane,
            result_rows: default_result_rows(),
            position: Position::Bottom,
            sort: Sort::default(),
            callback: None,
            keys: Vec::new(),
            source: Source::Scrollback {
                regex: Regex::new(&default_regex()).unwrap(),
                unique: false,
            },
            enter_action: EnterAction::Navigate,
            params: BTreeMap::new(),
            preview: None,
            preview_visible: true,
            label: None,
        }
    }
}

type Binding = KeyBinding<crate::actions::AppAction>;

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

#[derive(Clone)]
pub(crate) struct Bindings {
    pub(crate) workspace_appearance: crate::terminal::workspace::Appearance,
    pub(crate) default_layout: crate::terminal::workspace::Layout,
    pub(crate) startup: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    pub(crate) pipe_policy: super::pipe::Policy,
    pub(crate) builtin_pickers: BuiltinPickers,
    pub(crate) cursor_scroll_padding: u32,
    pub(crate) prompt_patterns: std::rc::Rc<crate::config::prompts::Patterns>,
    bindings: Vec<Binding>,
    model: std::rc::Rc<crate::config::model::Model>,
}

impl Bindings {
    pub(crate) fn with_workspaces(
        mut self,
        workspaces: crate::terminal::workspace::Config,
        startup: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    ) -> Result<Self, String> {
        workspaces.appearance.validate()?;
        self.workspace_appearance = workspaces.appearance;
        self.default_layout = workspaces.default_layout;
        self.startup = startup;
        Ok(self)
    }

    pub(crate) fn with_prompt_patterns(
        mut self,
        patterns: std::rc::Rc<crate::config::prompts::Patterns>,
    ) -> Self {
        self.prompt_patterns = patterns;
        self
    }

    pub(crate) fn with_pipe_policy(mut self, policy: super::pipe::Policy) -> Self {
        self.pipe_policy = policy;
        self
    }
    pub(crate) fn pipe_stream(options: &super::pipe::StreamOptions) -> Result<PickerEntry, String> {
        let value = serde_json::json!({"type":"Items","items":[],"position":options.position,"result_rows":options.result_rows.unwrap_or(7),"initial_query":options.initial_query,"preview_visible":options.preview_visible,"preview":options.preview});
        if let Some(preview) = value.get("preview") {
            if preview
                .get("max_bytes")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|n| n > 10 * 1024 * 1024)
                || preview
                    .get("max_scrollback_lines")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|n| n > 8192)
            {
                return Err("IPC preview exceeds server limits".into());
            }
        }
        let definition: Definition = serde_json::from_value(value).map_err(|e| e.to_string())?;
        definition.resolve("ipc", PickerDefaults::default())
    }
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
        let model = std::rc::Rc::new(crate::config::model::Model::compile(
            definitions,
            defaults,
            actions,
        )?);
        let compiled = &model.pickers;
        let actions = &model.actions;
        // TODO(picker-defaults): the default Ctrl+Shift+F and Ctrl+Shift+Space
        // bindings prefer custom pickers named `lines` and `commands` over
        // the built-ins. This existed for the removed implicit default
        // pickers; now it only rebinds a default shortcut when a user happens
        // to use one of those names. Always bind `builtin.*` here instead and
        // let users rebind through `keys`.
        let keys = keys.unwrap_or_else(|| {
            let mut keys = crate::actions::default_bindings();
            {
                keys.push(KeyBinding {
                    trigger: BindingTrigger::Key(egui::Key::F),
                    description: String::new(),
                    mods: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    action: BindingTarget::Picker(
                        if compiled.contains_key("lines") {
                            "lines"
                        } else {
                            "builtin.lines"
                        }
                        .into(),
                    ),
                });
            }
            let legacy_commands = compiled.get("commands").is_some_and(|entry| {
                matches!(
                    entry,
                    PickerEntry::Static(Options {
                        source: Source::CommandPalette(_),
                        ..
                    })
                )
            });
            {
                keys.push(KeyBinding {
                    trigger: BindingTrigger::Key(egui::Key::Space),
                    description: String::new(),
                    mods: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    action: BindingTarget::Picker(
                        if legacy_commands {
                            "commands"
                        } else {
                            "builtin.command_palette"
                        }
                        .into(),
                    ),
                });
            }
            keys
        });
        let mut bindings: Vec<Binding> = Vec::new();
        for binding in keys {
            let BindingTrigger::Key(key) = binding.trigger else {
                return Err("application bindings require a keyboard trigger".into());
            };
            let mods = binding.mods;
            if bindings.iter().any(|existing| {
                existing.trigger == binding.trigger && same_mods(existing.mods, mods)
            }) {
                return Err(format!(
                    "duplicate key binding {}",
                    crate::actions::shortcut_label(key, mods)
                ));
            }
            let action = match binding.action {
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
            bindings.push(Binding {
                action,
                trigger: binding.trigger,
                mods,
                description: binding.description,
            });
        }
        Ok(Self {
            bindings,
            workspace_appearance: Default::default(),
            default_layout: Default::default(),
            startup: None,
            pipe_policy: super::pipe::Policy::default(),
            model,
            builtin_pickers: BuiltinPickers::default(),
            cursor_scroll_padding: nfm_egui::copy_mode::SCROLL_PADDING_ROWS,
            prompt_patterns: Default::default(),
        })
    }

    pub(crate) fn with_cursor_scroll_padding(mut self, rows: u32) -> Self {
        self.cursor_scroll_padding = rows;
        self
    }

    pub(crate) fn with_builtin_pickers(mut self, builtins: BuiltinPickers) -> Self {
        self.builtin_pickers = builtins;
        self
    }

    pub(crate) fn named(&self, name: &str) -> Option<PickerEntry> {
        self.model.picker(name)
    }

    pub(crate) fn lua_action(&self, name: &str) -> Option<std::rc::Rc<crate::actions::LuaAction>> {
        self.model.action(name)
    }

    pub(crate) fn lua_actions(&self) -> impl Iterator<Item = (&str, &crate::actions::LuaAction)> {
        self.model
            .actions
            .iter()
            .map(|(name, action)| (name.as_str(), action.as_ref()))
    }

    pub(crate) fn has_themes_picker(&self) -> bool {
        self.model.pickers.values().any(|entry| {
            matches!(entry, PickerEntry::Static(options) if matches!(options.source, Source::Themes))
        })
    }

    pub(crate) fn has_error_log_picker(&self) -> bool {
        self.model.pickers.values().any(|entry| {
            matches!(entry, PickerEntry::Static(options) if matches!(options.source, Source::ErrorLog))
        })
    }

    pub(crate) fn palette_entries(&self) -> impl Iterator<Item = (&str, Option<String>)> {
        self.model
            .pickers
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
                    Source::TextFile(file) => file
                        .path
                        .strip_prefix('{')
                        .and_then(|path| path.strip_suffix('}'))
                        .is_none_or(|name| options.params.contains_key(name)),
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
            .map(|binding| binding.trigger.label(binding.mods))
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
                    (binding.trigger == BindingTrigger::Key(*key)
                        || physical_key
                            .is_some_and(|key| binding.trigger == BindingTrigger::Key(key)))
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
                if pressed && (!repeat || binding.action.allows_key_repeat()) {
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

#[cfg(test)]
mod tests {
    use crate::actions::{ActionContext, AppAction};
    use crate::terminal::workspace_ui::WorkspaceAction;

    #[test]
    fn held_workspace_navigation_repeats_but_creation_does_not() {
        let bindings = super::Bindings::default();
        let mods = egui::Modifiers::CTRL | egui::Modifiers::ALT;
        for (key, repeat, expected) in [
            (
                egui::Key::J,
                false,
                Some(AppAction::Workspace(WorkspaceAction::FocusDown)),
            ),
            (
                egui::Key::J,
                true,
                Some(AppAction::Workspace(WorkspaceAction::FocusDown)),
            ),
            (
                egui::Key::J,
                true,
                Some(AppAction::Workspace(WorkspaceAction::FocusDown)),
            ),
            (egui::Key::C, true, None),
            (
                egui::Key::C,
                false,
                Some(AppAction::Workspace(WorkspaceAction::CreatePane)),
            ),
        ] {
            let mut events = vec![key_event(key, mods, true, repeat)];
            assert_eq!(
                bindings.consume_when(&mut events, |a, m| ActionContext::default().allows(a, m)),
                (expected, true)
            );
            assert!(events.is_empty());
        }
        let mut events = vec![key_event(egui::Key::J, mods, false, false)];
        assert_eq!(bindings.consume(&mut events), (None, true));
        for (key, action) in [
            (egui::Key::ArrowLeft, WorkspaceAction::ResizeLeft),
            (egui::Key::ArrowRight, WorkspaceAction::ResizeRight),
            (egui::Key::ArrowUp, WorkspaceAction::ResizeUp),
            (egui::Key::ArrowDown, WorkspaceAction::ResizeDown),
        ] {
            for repeat in [false, true] {
                let mut events = vec![key_event(key, mods | egui::Modifiers::SHIFT, true, repeat)];
                assert_eq!(
                    bindings.consume(&mut events),
                    (Some(AppAction::Workspace(action)), true)
                );
                assert!(events.is_empty());
            }
        }
    }

    #[test]
    fn picker_scope_defaults_and_overrides_are_strict() {
        for (value, expected) in [
            (
                serde_json::json!({"type":"CommandPalette"}),
                super::Scope::Workspace,
            ),
            (
                serde_json::json!({"type":"CommandPalette","scope":"Pane"}),
                super::Scope::Pane,
            ),
            (
                serde_json::json!({"type":"Items","items":[]}),
                super::Scope::Pane,
            ),
            (
                serde_json::json!({"type":"Items","items":[],"scope":"Workspace"}),
                super::Scope::Workspace,
            ),
        ] {
            let definition: super::Definition = serde_json::from_value(value).unwrap();
            let options = definition
                .resolve("scope-test", Default::default())
                .unwrap()
                .resolve(&Default::default())
                .unwrap();
            assert_eq!(options.scope, expected);
        }
        assert!(
            serde_json::from_value::<super::Definition>(
                serde_json::json!({"type":"CommandPalette","scope":"Window"})
            )
            .is_err()
        );
        let defaults = super::Bindings::default()
            .named("builtin.command_palette")
            .unwrap()
            .resolve(&Default::default())
            .unwrap();
        assert_eq!(defaults.scope, super::Scope::Workspace);
        assert_eq!(defaults.position, super::Position::Center);
    }

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
            { "key": "e", "mods": "CTRL|SHIFT", "picker": "builtin.lines" },
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
            (Some(AppAction::OpenPicker("builtin.lines".into())), true)
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
    fn every_picker_accepts_sort_and_defaults_to_score() {
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
                    sort.is_none() || sort == Some("Score"),
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
                        builtin != AppAction::OpenThemePicker && !configured
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
    fn shared_presentation_keeps_flat_schema_and_strict_source_validation() {
        for kind in [
            "Scrollback",
            "Items",
            "Command",
            "FileSystem",
            "Themes",
            "ErrorLog",
            "CommandPalette",
        ] {
            let mut value = serde_json::json!({"type":kind});
            if kind == "Command" {
                value["args"] = serde_json::json!(["cmd"]);
            }
            let defaults: Definition = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(defaults.presentation.result_rows, default_result_rows());
            assert_eq!(
                defaults.presentation.preview_visible,
                !matches!(kind, "Themes" | "ErrorLog" | "CommandPalette")
            );
            value["position"] = "FullPane".into();
            value["result_rows"] = 6.into();
            value["sort"] = "SourceOrder".into();
            value["initial_query"] = "needle".into();
            value["preview_visible"] = true.into();
            value["preview"] = serde_json::json!({"type":"Text", "text":"preview", "wrap":true});
            let definition: Definition = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(definition.presentation.position, Position::FullPane);
            assert_eq!(definition.presentation.result_rows, 6);
            assert_eq!(
                definition.presentation.initial_query.as_deref(),
                Some("needle")
            );
            assert!(matches!(
                definition.presentation.preview,
                Some(Preview::Text { wrap: true, .. })
            ));
            for (field, invalid) in [
                ("result_row", serde_json::json!(6)),
                ("presentation", serde_json::json!({"result_rows":6})),
                (
                    if kind == "Scrollback" {
                        "roots"
                    } else {
                        "regex"
                    },
                    serde_json::json!("unexpected"),
                ),
            ] {
                let mut invalid_value = value.clone();
                invalid_value[field] = invalid;
                assert!(
                    serde_json::from_value::<Definition>(invalid_value).is_err(),
                    "{kind}.{field}"
                );
            }
        }
        assert!(
            serde_json::from_value::<Definition>(
                serde_json::json!({"type":"Factory", "result_rows":6})
            )
            .is_err()
        );
    }

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
                ("FullPane", Position::FullPane),
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

    #[test]
    fn file_picker_resolves_and_validates_its_options() {
        let resolve = |definition: serde_json::Value| {
            let definitions =
                BTreeMap::from([("test".into(), serde_json::from_value(definition).unwrap())]);
            Bindings::compile(Some(definitions), Some(Vec::new()))
                .and_then(|bindings| bindings.named("test").unwrap().resolve(&Default::default()))
        };
        let options = resolve(serde_json::json!({
            "type": "File", "path": "{history}", "reverse": true,
            "line_continuation": "`", "unique": true,
        }))
        .unwrap();
        assert_eq!(options.label, Some("FILE PICKER"));
        let Source::TextFile(file) = options.source else {
            panic!("expected a file source");
        };
        assert!(file.reverse && file.unique);
        assert_eq!(file.line_continuation.as_deref(), Some("`"));
        for invalid in [
            serde_json::json!({ "type": "File", "path": "" }),
            serde_json::json!({ "type": "File", "path": "x", "line_continuation": "\n" }),
            serde_json::json!({ "type": "File", "path": "x", "enter_action": "Navigate" }),
        ] {
            assert!(resolve(invalid).is_err());
        }
    }

    #[test]
    fn default_cwd_prefers_request_then_command_then_shell() {
        let command = |cwd: Option<&str>| Options {
            source: Source::Command(CommandOptions {
                args: vec!["fd".into()],
                cwd: cwd.map(str::to_owned),
                line_continuation: None,
                unique: false,
                success_exit_codes: None,
                columns: None,
            }),
            ..Default::default()
        };
        let shell = Some("/home/user/project".to_owned());

        // The shell's directory fills in when nothing else chose one.
        let mut options = command(None);
        options.default_cwd(shell.clone());
        assert_eq!(options.params["cwd"], "/home/user/project");

        // A command definition's cwd wins over the shell's directory.
        let mut options = command(Some("/srv"));
        options.default_cwd(shell.clone());
        assert_eq!(options.params["cwd"], "/srv");

        // A request's cwd (e.g. from the picker pipe) is never replaced.
        let mut options = command(None);
        options.params.insert("cwd".into(), "/tmp".into());
        options.default_cwd(shell.clone());
        assert_eq!(options.params["cwd"], "/tmp");

        // Other pickers get it too, for previews.
        let mut options = Options::default();
        options.default_cwd(shell);
        assert_eq!(options.params["cwd"], "/home/user/project");

        // Without OSC 7 nothing is set and commands use the app's directory.
        let mut options = command(None);
        options.default_cwd(None);
        assert!(!options.params.contains_key("cwd"));
    }

    #[test]
    fn command_columns_are_validated() {
        let compile = |extra: &str| {
            let definition: Definition = serde_json::from_str(&format!(
                r#"{{"type":"Command","args":["git","log"]{extra}}}"#
            ))
            .unwrap();
            definition.resolve("log", PickerDefaults::default())
        };
        let Ok(PickerEntry::Static(options)) = compile(r#","columns":["Hash","Subject"]"#) else {
            panic!("columns should compile");
        };
        let Source::Command(command) = options.source else {
            panic!("expected a command");
        };
        let columns = command.columns.unwrap();
        assert_eq!(columns.names, ["Hash", "Subject"]);
        assert_eq!(columns.delimiter, "\t");
        for (extra, message) in [
            (r#","delimiter":",""#, "requires columns"),
            (r#","columns":[]"#, "at least one column"),
            (r#","columns":["A","a"]"#, "columns"),
            (r##","columns":["A"],"delimiter":"""##, "delimiter"),
            (r#","columns":["A"],"unique":true"#, "cannot be combined"),
            (
                r#","columns":["A"],"line_continuation":"`""#,
                "cannot be combined",
            ),
        ] {
            let Err(error) = compile(extra) else {
                panic!("{extra} should be rejected");
            };
            assert!(error.contains(message), "{extra}: {error}");
        }
    }
}

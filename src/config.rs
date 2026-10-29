pub(crate) mod keybindings;
pub(crate) mod model;
pub(crate) mod prompts;
pub(crate) mod quick_selects;

use mlua::{Lua, LuaSerdeExt};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct PickerContext {
    pub(crate) line: String,
    pub(crate) line_before_cursor: String,
    pub(crate) cursor_column: u32,
    pub(crate) recent_lines: Vec<String>,
    /// The shell's working directory from OSC 7, if it reported one.
    pub(crate) cwd: Option<String>,
}

pub(crate) struct PickerFactory {
    lua: std::rc::Rc<Lua>,
    key: mlua::RegistryKey,
    name: String,
    builtin: Option<String>,
}

impl PickerFactory {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn resolve(
        &self,
        context: &PickerContext,
        defaults: crate::picker::config::PickerDefaults,
    ) -> Result<crate::picker::config::Options, String> {
        let function: mlua::Function = self
            .lua
            .registry_value(&self.key)
            .map_err(|e| e.to_string())?;
        let argument = self.lua.create_table().map_err(|e| e.to_string())?;
        argument
            .set("line", context.line.as_str())
            .map_err(|e| e.to_string())?;
        argument
            .set("line_before_cursor", context.line_before_cursor.as_str())
            .map_err(|e| e.to_string())?;
        argument
            .set("cursor_column", context.cursor_column)
            .map_err(|e| e.to_string())?;
        argument
            .set("recent_lines", context.recent_lines.clone())
            .map_err(|e| e.to_string())?;
        argument
            .set("cwd", context.cwd.as_deref())
            .map_err(|e| e.to_string())?;
        let table: mlua::Table = function
            .call(argument)
            .map_err(|e| format!("pickers.{}: {e}", self.name))?;
        self.lua
            .set_app_data(crate::preview_worker::FactoryContext(serde_json::json!({
                "line": context.line, "line_before_cursor": context.line_before_cursor,
                "cursor_column": context.cursor_column, "recent_lines": context.recent_lines,
                "cwd": context.cwd,
            })));
        let definition = if let Some(name) = &self.builtin {
            parse_builtin_table(&self.lua, name, &table)
        } else {
            parse_picker_table(&self.lua, &self.name, &table)
        };
        self.lua
            .remove_app_data::<crate::preview_worker::FactoryContext>();
        let definition = definition?;
        if matches!(
            definition.source,
            crate::picker::config::DefinitionSource::Factory { .. }
        ) {
            return Err(format!(
                "pickers.{}: factory must return a picker table",
                self.name
            ));
        }
        match definition.resolve(&self.name, defaults)? {
            crate::picker::config::PickerEntry::Static(options) => Ok(options),
            crate::picker::config::PickerEntry::Factory(..) => Err(format!(
                "pickers.{}: factory must return a picker table",
                self.name
            )),
        }
    }
}

fn parse_picker_table(
    lua: &std::rc::Rc<Lua>,
    name: &str,
    definition: &mlua::Table,
) -> Result<crate::picker::config::Definition, String> {
    // Strip callable fields from a copy: a factory may return the same table
    // on multiple invocations, and its action/provider must remain available.
    let copy = lua.create_table().map_err(|e| e.to_string())?;
    for pair in definition.clone().pairs::<mlua::Value, mlua::Value>() {
        let (key, value) = pair.map_err(|e| e.to_string())?;
        copy.raw_set(key, value).map_err(|e| e.to_string())?;
    }
    let definition = &copy;
    if matches!(
        definition
            .raw_get::<_, mlua::Value>("type")
            .map_err(|e| e.to_string())?,
        mlua::Value::Nil
    ) {
        definition
            .raw_set("type", "Scrollback")
            .map_err(|e| e.to_string())?;
    }
    let local_keys = keybindings::parse_picker_keys(
        lua,
        name,
        definition
            .raw_get::<_, Option<mlua::Table>>("keys")
            .map_err(|e| e.to_string())?,
    )?;
    definition
        .raw_set("keys", mlua::Value::Nil)
        .map_err(|e| e.to_string())?;
    let mut callback = None;
    let action: mlua::Value = definition.raw_get("action").map_err(|e| e.to_string())?;
    if !matches!(action, mlua::Value::Nil) {
        if matches!(
            definition
                .raw_get::<_, String>("type")
                .map_err(|e| e.to_string())?
                .as_str(),
            "CommandPalette" | "Themes" | "ErrorLog"
        ) {
            return Err(format!(
                "pickers.{name}: CommandPalette, Themes, and ErrorLog use built-in actions"
            ));
        }
        let mlua::Value::Function(function) = action else {
            return Err(format!("pickers.{name}.action must be a function"));
        };
        let key = lua
            .create_registry_value(function)
            .map_err(|e| e.to_string())?;
        definition
            .raw_set("action", mlua::Value::Nil)
            .map_err(|e| e.to_string())?;
        callback = Some(std::rc::Rc::new(crate::lua_callbacks::Callback {
            lua: lua.clone(),
            key,
            name: name.into(),
        }));
    }
    let mut provider = None;
    if let mlua::Value::Function(function) = definition
        .raw_get::<_, mlua::Value>("items")
        .map_err(|e| e.to_string())?
    {
        if definition
            .raw_get::<_, String>("type")
            .map_err(|e| e.to_string())?
            != "Items"
        {
            return Err(format!("pickers.{name}.items requires type Items"));
        }
        let key = lua
            .create_registry_value(function)
            .map_err(|e| e.to_string())?;
        definition
            .raw_set("items", mlua::Value::Nil)
            .map_err(|e| e.to_string())?;
        provider = Some(std::rc::Rc::new(crate::lua_callbacks::ItemsProvider {
            lua: lua.clone(),
            key,
        }));
    }
    let mut preview_resolver = None;
    if let Some(preview) = definition
        .raw_get::<_, Option<mlua::Table>>("preview")
        .map_err(|e| e.to_string())?
    {
        let resolve: mlua::Value = preview.raw_get("resolve").map_err(|e| e.to_string())?;
        if !matches!(resolve, mlua::Value::Nil) {
            if preview
                .raw_get::<_, String>("type")
                .map_err(|e| e.to_string())?
                != "Dynamic"
            {
                return Err(format!(
                    "pickers.{name}.preview.resolve requires type Dynamic"
                ));
            }
            let mlua::Value::Function(function) = resolve else {
                return Err(format!("pickers.{name}.preview.resolve must be a function"));
            };
            let key = lua
                .create_registry_value(function)
                .map_err(|e| e.to_string())?;
            let copy_preview = lua.create_table().map_err(|e| e.to_string())?;
            for pair in preview.clone().pairs::<mlua::Value, mlua::Value>() {
                let (key, value) = pair.map_err(|e| e.to_string())?;
                copy_preview
                    .raw_set(key, value)
                    .map_err(|e| e.to_string())?;
            }
            copy_preview
                .raw_set("resolve", mlua::Value::Nil)
                .map_err(|e| e.to_string())?;
            definition
                .raw_set("preview", copy_preview)
                .map_err(|e| e.to_string())?;
            preview_resolver = Some(std::rc::Rc::new(crate::lua_callbacks::PreviewResolver {
                lua: lua.clone(),
                key,
                name: name.into(),
                worker: lua
                    .app_data_ref::<crate::preview_worker::ConfigSource>()
                    .map(|source| {
                        crate::preview_worker::Worker::new(
                            source.clone(),
                            name.into(),
                            lua.app_data_ref::<crate::preview_worker::FactoryContext>()
                                .map(|context| context.clone()),
                        )
                    }),
            }));
        }
        for field in ["command", "file"] {
            if !matches!(
                preview
                    .raw_get::<_, mlua::Value>(field)
                    .map_err(|e| e.to_string())?,
                mlua::Value::Nil
            ) {
                return Err(format!(
                    "pickers.{name}.preview.{field} is no longer supported; use type='Dynamic' with resolve=function(item, params) instead"
                ));
            }
        }
    }
    let mut parsed: crate::picker::config::Definition = lua
        .from_value(mlua::Value::Table(definition.clone()))
        .map_err(|e| format!("pickers.{name}: {e}"))?;
    if let Some(keys) = parsed.keys_mut() {
        *keys = local_keys;
    }
    if let Some(callback) = callback {
        parsed.set_callback(callback);
    }
    if let Some(provider) = provider {
        parsed.set_items_provider(provider);
    }
    if let Some(resolve) = preview_resolver {
        parsed.set_preview_resolver(resolve);
    }
    Ok(parsed)
}

/// Merge `builtin_pickers.<name>` overrides onto the built-in defaults.
// TODO(picker-defaults): one of the two places built-in defaults are applied;
// see Model::compile. `line_break` is configured separately through
// BuiltinPickers because it only opens from another picker or copy mode.
fn parse_builtin_table(
    lua: &std::rc::Rc<Lua>,
    name: &str,
    overrides: &mlua::Table,
) -> Result<crate::picker::config::Definition, String> {
    let defaults = crate::picker::config::builtin_defaults(name);
    let mlua::Value::Table(base) = lua.to_value(&defaults).map_err(|e| e.to_string())? else {
        unreachable!("builtin defaults are a table")
    };
    for pair in overrides.clone().pairs::<String, mlua::Value>() {
        let (field, value) = pair.map_err(|e| e.to_string())?;
        if field == "type" || field == "regex" || field == "prompts" {
            return Err(format!("builtin_pickers.{name}.{field} is built in"));
        }
        base.raw_set(field, value).map_err(|e| e.to_string())?;
    }
    parse_picker_table(lua, &format!("builtin.{name}"), &base)
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AppConfig {
    font: FontConfig,
    terminal: TerminalConfig,
    colors: ColorConfig,
    theme_colors: std::collections::BTreeMap<String, crate::colors::UiOverrides>,
    floating: FloatingConfig,
    workspaces: crate::terminal::workspace::Config,
    #[serde(skip)]
    startup: Option<std::rc::Rc<crate::lua_callbacks::Callback>>,
    picker_defaults: crate::picker::config::PickerDefaults,
    builtin_pickers: crate::picker::config::BuiltinPickers,
    cursor_scroll_padding: Option<u32>,
    #[serde(default = "prompts::defaults")]
    prompt_patterns: Vec<String>,
    #[serde(skip)]
    compiled_prompts: std::rc::Rc<prompts::Patterns>,
    #[serde(skip)]
    quick_selects: quick_selects::Definitions,
    picker_pipe: crate::picker::pipe::Policy,
    // TODO(picker-defaults): `None` used to select default custom pickers;
    // it now means the same as an empty table, so this can be a plain map.
    // `keys` stays optional: `None` keeps the default bindings and `{}`
    // removes them.
    pickers: Option<std::collections::BTreeMap<String, crate::picker::config::Definition>>,
    keys: Option<Vec<crate::picker::config::KeyBinding>>,
    #[serde(skip)]
    actions: std::collections::BTreeMap<String, std::rc::Rc<crate::actions::LuaAction>>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FontConfig {
    family: Option<String>,
    size: Option<f32>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TerminalConfig {
    scrollback_limit: Option<u64>,
    command: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ColorConfig {
    theme: Option<String>,
    ui: crate::colors::UiOverrides,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FloatingConfig {
    padding: Option<FloatingPadding>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct FloatingPadding {
    pub(crate) top: f32,
    pub(crate) bottom: f32,
    pub(crate) left: f32,
    pub(crate) right: f32,
}

impl FloatingPadding {
    fn validate(self) -> Result<Self, String> {
        for (name, value) in [
            ("top", self.top),
            ("bottom", self.bottom),
            ("left", self.left),
            ("right", self.right),
        ] {
            if !value.is_finite() || !(0.0..100.0).contains(&value) {
                return Err(format!("floating.padding.{name} must be between 0 and 100"));
            }
        }
        if self.top + self.bottom >= 95.0 || self.left + self.right >= 95.0 {
            return Err("floating.padding must leave at least 5% width and height".into());
        }
        Ok(self)
    }
}

impl AppConfig {
    fn parse(source: &str, path: &Path) -> Result<Self, String> {
        let lua = std::rc::Rc::new(Lua::new());
        crate::preview_worker::install_ui_api(&lua).map_err(|e| e.to_string())?;
        let modules = crate::preview_worker::module_path(path);
        crate::preview_worker::set_module_path(&lua, &modules).map_err(|e| e.to_string())?;
        lua.set_app_data(crate::preview_worker::ConfigSource(
            source.into(),
            format!("@{}", path.display()),
            modules,
        ));
        let nterm = lua.create_table().map_err(|error| error.to_string())?;
        crate::terminal::workspace_ui::install_lua_helpers(&lua, &nterm)
            .map_err(|e| e.to_string())?;
        nterm
            .set(
                "json_parse",
                lua.create_function(|lua, source: String| {
                    let value: serde_json::Value =
                        serde_json::from_str(&source).map_err(mlua::Error::external)?;
                    lua.to_value(&value)
                })
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        nterm
            .set(
                "json_encode_pretty",
                lua.create_function(|lua, value: mlua::Value| {
                    let json: serde_json::Value = lua.from_value(value)?;
                    serde_json::to_string_pretty(&json).map_err(mlua::Error::external)
                })
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        lua.globals()
            .set("nterm", nterm)
            .map_err(|error| error.to_string())?;
        let name = format!("@{}", path.display());
        let value: mlua::Value = lua
            .load(source)
            .set_name(name)
            .eval()
            .map_err(|error| error.to_string())?;
        if !matches!(value, mlua::Value::Table(_)) {
            return Err("config must return a table".into());
        }
        let startup = if let mlua::Value::Table(root) = &value {
            let function: Option<mlua::Function> =
                root.get("startup").map_err(|e| e.to_string())?;
            root.raw_set("startup", mlua::Value::Nil)
                .map_err(|e| e.to_string())?;
            function
                .map(|f| {
                    lua.create_registry_value(f).map(|key| {
                        std::rc::Rc::new(crate::lua_callbacks::Callback {
                            lua: lua.clone(),
                            key,
                            name: "startup".into(),
                        })
                    })
                })
                .transpose()
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        let mut parsed_keys = None;
        let mut parsed_pickers = None;
        let mut actions = std::collections::BTreeMap::new();
        if let mlua::Value::Table(root) = &value {
            if let Some(keys) = root
                .get::<_, Option<mlua::Table>>("keys")
                .map_err(|e| e.to_string())?
            {
                parsed_keys = Some(keybindings::parse_application_keys(&lua, keys)?);
                root.raw_set("keys", mlua::Value::Nil)
                    .map_err(|e| e.to_string())?;
            }
            if let Some(configured) = root
                .get::<_, Option<mlua::Table>>("actions")
                .map_err(|e| e.to_string())?
            {
                for pair in configured.pairs::<String, mlua::Value>() {
                    let (name, definition) = pair.map_err(|e| e.to_string())?;
                    if name.trim().is_empty()
                        || crate::actions::AppAction::from_name(&name).is_some()
                    {
                        return Err(format!("invalid or reserved action name {name:?}"));
                    }
                    let (label, callback) = match definition {
                        mlua::Value::Function(function) => (name.clone(), function),
                        mlua::Value::Table(table) => {
                            for field in table.clone().pairs::<String, mlua::Value>() {
                                let (field, _) = field.map_err(|e| e.to_string())?;
                                if field != "label" && field != "action" {
                                    return Err(format!("actions.{name}: unknown field {field}"));
                                }
                            }
                            (
                                table
                                    .get::<_, Option<String>>("label")
                                    .map_err(|e| e.to_string())?
                                    .unwrap_or(name.clone()),
                                table
                                    .get::<_, mlua::Function>("action")
                                    .map_err(|e| e.to_string())?,
                            )
                        }
                        _ => return Err(format!("actions.{name} must be a function or table")),
                    };
                    if label.trim().is_empty() {
                        return Err(format!("actions.{name}.label must not be empty"));
                    }
                    let key = lua
                        .create_registry_value(callback)
                        .map_err(|e| e.to_string())?;
                    actions.insert(
                        name.clone(),
                        std::rc::Rc::new(crate::actions::LuaAction {
                            label,
                            callback: std::rc::Rc::new(crate::lua_callbacks::Callback {
                                lua: lua.clone(),
                                key,
                                name,
                            }),
                        }),
                    );
                }
                root.raw_set("actions", mlua::Value::Nil)
                    .map_err(|e| e.to_string())?;
            }
            if let Some(pickers) = root
                .get::<_, Option<mlua::Table>>("pickers")
                .map_err(|e| e.to_string())?
            {
                let mut definitions = std::collections::BTreeMap::new();
                for pair in pickers.pairs::<String, mlua::Value>() {
                    let (name, value) = pair.map_err(|e| e.to_string())?;
                    let definition = match value {
                        mlua::Value::Table(table) => parse_picker_table(&lua, &name, &table)?,
                        mlua::Value::Function(function) => {
                            let key = lua
                                .create_registry_value(function)
                                .map_err(|e| e.to_string())?;
                            crate::picker::config::Definition::factory(std::rc::Rc::new(
                                PickerFactory {
                                    lua: lua.clone(),
                                    key,
                                    name: name.clone(),
                                    builtin: None,
                                },
                            ))
                        }
                        _ => return Err(format!("pickers.{name} must be a table or function")),
                    };
                    if name.starts_with("builtin.") {
                        return Err("custom picker names cannot start with builtin.".into());
                    }
                    definitions.insert(name, definition);
                }
                parsed_pickers = Some(definitions);
                root.raw_set("pickers", mlua::Value::Nil)
                    .map_err(|e| e.to_string())?;
            }
            if let Some(builtins) = root
                .get::<_, Option<mlua::Table>>("builtin_pickers")
                .map_err(|e| e.to_string())?
            {
                for pair in builtins.clone().pairs::<String, mlua::Value>() {
                    let (name, value) = pair.map_err(|e| e.to_string())?;
                    if name == "line_break" {
                        continue;
                    }
                    if !crate::picker::config::BUILTIN_NAMES.contains(&name.as_str()) {
                        return Err(format!("unknown builtin picker {name:?}"));
                    }
                    let definition = match value {
                        mlua::Value::Table(table) => parse_builtin_table(&lua, &name, &table)?,
                        mlua::Value::Function(function) => {
                            let key = lua
                                .create_registry_value(function)
                                .map_err(|e| e.to_string())?;
                            crate::picker::config::Definition::factory(std::rc::Rc::new(
                                PickerFactory {
                                    lua: lua.clone(),
                                    key,
                                    name: format!("builtin.{name}"),
                                    builtin: Some(name.clone()),
                                },
                            ))
                        }
                        _ => {
                            return Err(format!(
                                "builtin_pickers.{name} must be a table or function"
                            ));
                        }
                    };
                    // TODO(picker-defaults): built-in overrides share the
                    // custom `pickers` map under `builtin.*` keys, which is
                    // why custom names cannot start with `builtin.`, and
                    // Model::compile fills in defaults for whichever slots
                    // are still empty. Keep overrides in their own map and
                    // pass both to Model::compile instead.
                    parsed_pickers
                        .get_or_insert_with(Default::default)
                        .insert(format!("builtin.{name}"), definition);
                    builtins
                        .raw_set(name, mlua::Value::Nil)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        let parsed_quick_selects = match &value {
            mlua::Value::Table(root) => quick_selects::parse(&lua, root)?,
            _ => Default::default(),
        };
        let mut config: Self = lua.from_value(value).map_err(|error| error.to_string())?;
        config.compiled_prompts =
            std::rc::Rc::new(prompts::Patterns::compile(&config.prompt_patterns)?);
        config.quick_selects = parsed_quick_selects;
        config.builtin_pickers.validate()?;
        config.workspaces.appearance.validate()?;
        config.picker_defaults.validate()?;
        config.keys = parsed_keys;
        config.pickers = parsed_pickers;
        config.actions = actions;
        config.startup = startup;
        Ok(config)
    }

    fn ghostty_args(&self) -> Result<Vec<String>, String> {
        let mut args = Vec::new();
        if let Some(family) = &self.font.family {
            validate_text("font.family", family)?;
            // Ghostty font-family is repeatable: clear existing configured
            // families so this setting replaces them instead of appending.
            args.push("--font-family=".into());
            args.push(format!("--font-family={family}"));
        }
        if let Some(size) = self.font.size {
            if !size.is_finite() || size <= 0.0 {
                return Err("font.size must be a finite positive number (points)".into());
            }
            args.push(format!("--font-size={size}"));
        }
        if let Some(limit) = self.terminal.scrollback_limit {
            args.push(format!("--scrollback-limit-bytes={limit}"));
        }
        if let Some(command) = &self.terminal.command {
            validate_text("terminal.command", command)?;
            args.push(format!("--command={command}"));
        }
        if let Some(theme) = &self.colors.theme {
            validate_text("colors.theme", theme)?;
            args.push(format!("--theme={theme}"));
        }
        Ok(args)
    }
}

fn validate_text(key: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err(format!("{key} must be a nonempty single-line string"));
    }
    Ok(())
}

fn default_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var_os("HOME");
    let mut paths = Vec::new();
    if let Some(home) = home {
        paths.push(PathBuf::from(home).join("nterm.lua"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(directory) = exe.parent() {
            paths.push(directory.join("nterm.lua"));
        }
    }
    paths
}

fn read_config(
    explicit: Option<&Path>,
    defaults: &[PathBuf],
    mut read: impl FnMut(&Path) -> std::io::Result<String>,
) -> Result<Option<(PathBuf, String)>, String> {
    let paths: Vec<&Path> = match explicit {
        Some(path) => vec![path],
        None => defaults.iter().map(PathBuf::as_path).collect(),
    };
    for path in paths {
        match read(path) {
            Ok(source) => return Ok(Some((path.to_owned(), source))),
            Err(error) if explicit.is_none() && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Ok(None)
}

#[derive(Clone)]
pub(crate) struct ReloadSource {
    path: Option<PathBuf>,
    cli_theme: bool,
    cli_font_family: bool,
    cli_font_size: bool,
}

pub(crate) struct ReloadedConfig {
    pub(crate) theme: Option<String>,
    pub(crate) font_family: Option<String>,
    pub(crate) font_size: Option<f32>,
    pub(crate) bindings: crate::picker::config::Bindings,
    pub(crate) padding: Option<FloatingPadding>,
    pub(crate) colors: crate::colors::ColorOverrides,
}

impl ReloadSource {
    pub(crate) fn reload(&self) -> Result<ReloadedConfig, String> {
        let (path, source) = read_config(self.path.as_deref(), &default_paths(), |path| {
            std::fs::read_to_string(path)
        })?
        .ok_or("No Lua configuration file found")?;
        let load = || {
            let config = AppConfig::parse(&source, &path)?;
            config.ghostty_args()?;
            Ok(ReloadedConfig {
                font_family: if self.cli_font_family {
                    None
                } else {
                    config.font.family
                },
                font_size: if self.cli_font_size {
                    None
                } else {
                    config.font.size
                },
                theme: if self.cli_theme {
                    None
                } else {
                    config.colors.theme
                },
                bindings: crate::picker::config::Bindings::compile_with_actions(
                    config.pickers,
                    config.keys,
                    config.picker_defaults,
                    config.actions,
                )?
                .with_workspaces(config.workspaces, config.startup)?
                .with_builtin_pickers(config.builtin_pickers)
                .with_pipe_policy(config.picker_pipe)
                .with_prompt_patterns(config.compiled_prompts)
                .with_quick_selects(config.quick_selects)?
                .with_cursor_scroll_padding(
                    config
                        .cursor_scroll_padding
                        .unwrap_or(nfm_egui::copy_mode::SCROLL_PADDING_ROWS),
                ),
                padding: config
                    .floating
                    .padding
                    .map(FloatingPadding::validate)
                    .transpose()?,
                colors: crate::colors::ColorOverrides {
                    global: config.colors.ui,
                    themes: config.theme_colors,
                },
            })
        };
        load().map_err(|error: String| format!("{}: {error}", path.display()))
    }
}

#[cfg(test)]
fn prepare(
    args: Vec<String>,
    issues: &mut Vec<String>,
) -> Result<
    (
        Vec<String>,
        crate::picker::config::Bindings,
        Option<FloatingPadding>,
        crate::colors::ColorOverrides,
    ),
    String,
> {
    prepare_with_reload(args, issues)
        .map(|(args, bindings, padding, colors, _)| (args, bindings, padding, colors))
}

pub(crate) fn prepare_with_reload(
    args: Vec<String>,
    issues: &mut Vec<String>,
) -> Result<
    (
        Vec<String>,
        crate::picker::config::Bindings,
        Option<FloatingPadding>,
        crate::colors::ColorOverrides,
        ReloadSource,
    ),
    String,
> {
    let mut incoming = args.into_iter();
    let program = incoming.next().unwrap_or_else(|| "nterm".into());
    let mut remaining = Vec::new();
    let mut explicit = None;
    while let Some(arg) = incoming.next() {
        if arg == "-e" || arg == "--" {
            remaining.push(arg);
            remaining.extend(incoming);
            break;
        }
        let path = if arg == "--lua-config" {
            Some(incoming.next().ok_or("--lua-config requires a path")?)
        } else {
            arg.strip_prefix("--lua-config=").map(str::to_owned)
        };
        if let Some(path) = path {
            if path.is_empty() {
                return Err("--lua-config requires a path".into());
            }
            explicit = Some(PathBuf::from(path));
        } else {
            remaining.push(arg);
        }
    }
    let mut prepared = vec![
        program,
        "--copy-on-select=none".into(),
        "--right-click-action=copy".into(),
        "--selection-clear-on-typing=false".into(),
    ];
    let mut pickers = crate::picker::config::Bindings::default();
    let mut floating_padding = None;
    let mut ui_colors = crate::colors::ColorOverrides::default();
    let mut reload_path = explicit.clone();
    let loaded = (|| {
        if let Some((path, source)) = read_config(explicit.as_deref(), &default_paths(), |path| {
            std::fs::read_to_string(path)
        })? {
            reload_path = Some(path.clone());
            let (options, bindings, padding, overrides) = AppConfig::parse(&source, &path)
                .and_then(|config| {
                    Ok((
                        config.ghostty_args()?,
                        crate::picker::config::Bindings::compile_with_actions(
                            config.pickers,
                            config.keys,
                            config.picker_defaults,
                            config.actions,
                        )?
                        .with_workspaces(config.workspaces, config.startup)?
                        .with_builtin_pickers(config.builtin_pickers)
                        .with_pipe_policy(config.picker_pipe)
                        .with_prompt_patterns(config.compiled_prompts)
                        .with_quick_selects(config.quick_selects)?
                        .with_cursor_scroll_padding(
                            config
                                .cursor_scroll_padding
                                .unwrap_or(nfm_egui::copy_mode::SCROLL_PADDING_ROWS),
                        ),
                        config
                            .floating
                            .padding
                            .map(FloatingPadding::validate)
                            .transpose()?,
                        crate::colors::ColorOverrides {
                            global: config.colors.ui,
                            themes: config.theme_colors,
                        },
                    ))
                })
                .map_err(|error| format!("{}: {error}", path.display()))?;
            return Ok(Some((options, bindings, padding, overrides)));
        }
        Ok::<_, String>(None)
    })();
    match loaded {
        Ok(Some((options, bindings, padding, overrides))) => {
            prepared.extend(options);
            pickers = bindings;
            floating_padding = padding;
            ui_colors = overrides;
        }
        Ok(None) => {}
        Err(error) => issues.push(format!("Lua configuration was not loaded. Using defaults for Lua settings.\nFix the configuration and restart the app.\n\n{error}")),
    }
    // Ghostty loads its normal files first, then these arguments. Explicit
    // command-line values come last and override the Lua configuration.
    if prepared.iter().any(|arg| arg.starts_with("--font-family="))
        && remaining
            .iter()
            .take_while(|arg| *arg != "-e" && *arg != "--")
            .any(|arg| arg == "--font-family" || arg.starts_with("--font-family="))
    {
        prepared.push("--font-family=".into());
    }
    let cli_theme = remaining
        .iter()
        .take_while(|arg| *arg != "-e" && *arg != "--")
        .any(|arg| arg == "--theme" || arg.starts_with("--theme="));
    let cli_font_family = remaining
        .iter()
        .take_while(|arg| *arg != "-e" && *arg != "--")
        .any(|arg| arg == "--font-family" || arg.starts_with("--font-family="));
    let cli_font_size = remaining
        .iter()
        .take_while(|arg| *arg != "-e" && *arg != "--")
        .any(|arg| arg == "--font-size" || arg.starts_with("--font-size="));
    prepared.extend(remaining);
    Ok((
        prepared,
        pickers,
        floating_padding,
        ui_colors,
        ReloadSource {
            path: reload_path,
            cli_theme,
            cli_font_family,
            cli_font_size,
        },
    ))
}

#[cfg(test)]
fn prepare_args(args: Vec<String>) -> Result<Vec<String>, String> {
    let mut issues = Vec::new();
    let (args, _, _, _) = prepare(args, &mut issues)?;
    match issues.into_iter().next() {
        Some(error) => Err(error),
        None => Ok(args),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_configuration_compiles_helpers_startup_and_appearance() {
        let config = parse(include_str!("../workspaces.example.lua")).unwrap();
        assert!(config.startup.is_some());
        assert_eq!(config.workspaces.appearance.inner_gap, 10.0);
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        assert!(
            bindings
                .shortcut_for(&crate::actions::AppAction::Workspace(
                    crate::terminal::workspace_ui::WorkspaceAction::SetTile
                ))
                .is_some()
        );
        assert!(parse("return {workspaces={inner_gap=-1}}").is_err());
        assert!(parse("return {workspaces={inner_gapp=1}}").is_err());
        assert!(parse("return {workspaces={default_layout='sideways'}}").is_err());
        let config =
            parse("return {workspaces={default_layout='horizontal_grid',inner_gap=3}}").unwrap();
        assert_eq!(
            config.workspaces.default_layout,
            crate::terminal::workspace::Layout::HorizontalGrid
        );
        assert_eq!(config.workspaces.appearance.inner_gap, 3.0);
        assert!(parse("return {startup=123}").is_err());
    }
    #[test]
    fn picker_local_keys_parse_validate_and_keep_lua_functions() {
        let mut config = parse("return { builtin_pickers = { lines = { keys = {{key='Enter',mods='CTRL',action=function(app,item) app:close_picker(); app:paste(item.value) end}} } } }").unwrap();
        let keys = config
            .pickers
            .as_mut()
            .unwrap()
            .get_mut("builtin.lines")
            .unwrap()
            .keys_mut()
            .unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(
            keys[0].trigger,
            keybindings::BindingTrigger::Key(egui::Key::Enter)
        );
        assert!(keys[0].mods.ctrl);
        assert_eq!(keys[0].description, "Run picker Lua action");
        let commands = keys[0]
            .action
            .call("line", "line", false, None, None, &Default::default())
            .unwrap();
        assert!(
            matches!(&commands[..], [crate::lua_callbacks::Command::ClosePicker, crate::lua_callbacks::Command::Paste(text)] if text == "line")
        );
        for binding in [
            "{key='Enter',action='Paste'}",
            "{key='UnknownKey',action=function() end}",
            "{key='Enter',mods='TYPO',action=function() end}",
            "{key='Enter',extra=true,action=function() end}",
        ] {
            assert!(parse(&format!("return {{pickers={{x={{keys={{{binding}}}}}}}}}")).is_err());
        }
        assert!(parse("return {pickers={x={keys={{key='Enter',action=function() end},{key='Enter',action=function() end}}}}}").is_err());
        let mut described = parse("return {pickers={x={keys={{key='Enter',description='Paste selected line',action=function() end}}}}}").unwrap();
        assert_eq!(
            described
                .pickers
                .as_mut()
                .unwrap()
                .get_mut("x")
                .unwrap()
                .keys_mut()
                .unwrap()[0]
                .description,
            "Paste selected line"
        );
    }
    #[test]
    fn reload_fonts_respect_cli_options_but_ignore_shell_arguments() {
        let directory = std::env::temp_dir().join(format!(
            "ghostty-reload-font-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("nterm.lua");
        std::fs::write(&path, "return {font={family='Consolas', size=19}}").unwrap();
        for (tail, family, size) in [
            (vec![], Some("Consolas"), Some(19.0)),
            (vec!["--font-family=Other"], None, Some(19.0)),
            (vec!["--font-size", "23"], Some("Consolas"), None),
            (
                vec!["-e", "echo", "--font-size=23", "--font-family=Other"],
                Some("Consolas"),
                Some(19.0),
            ),
        ] {
            let mut args = vec!["ghostty".into(), format!("--lua-config={}", path.display())];
            args.extend(tail.into_iter().map(str::to_owned));
            let (_, _, _, _, reload) = super::prepare_with_reload(args, &mut Vec::new()).unwrap();
            let loaded = reload.reload().unwrap();
            assert_eq!(loaded.font_family.as_deref(), family);
            assert_eq!(loaded.font_size, size);
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn reload_validates_before_replacing_config_and_keeps_old_callbacks_alive() {
        let directory = std::env::temp_dir().join(format!(
            "ghostty-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("nterm.lua");
        std::fs::write(
            &path,
            "return { actions = { Old = function(app) app:paste('old') end } }",
        )
        .unwrap();
        let source = super::ReloadSource {
            path: Some(path.clone()),
            cli_theme: false,
            cli_font_family: false,
            cli_font_size: false,
        };
        let old = source.reload().unwrap();
        std::fs::write(&path, "return { font = { size = -1 } }").unwrap();
        assert!(source.reload().is_err());
        std::fs::write(&path, "return { colors = {theme = 'Gruvbox Dark'}, actions = { New = function(app) app:paste('new') end }, keys = {{key='r', mods='CTRL|SHIFT', action='ReloadConfig'}} }").unwrap();
        let new = source.reload().unwrap();
        assert!(new.bindings.lua_action("Old").is_none());
        assert!(new.bindings.lua_action("New").is_some());
        assert_eq!(new.theme.as_deref(), Some("Gruvbox Dark"));
        assert!(
            matches!(&old.bindings.lua_action("Old").unwrap().callback.call_action(String::new()).unwrap()[0], crate::lua_callbacks::Command::Paste(text) if text == "old")
        );
        let entry = crate::actions::catalog(&new.bindings, false, false)
            .into_iter()
            .find(|entry| entry.action == crate::actions::AppAction::ReloadConfig)
            .unwrap();
        assert_eq!(entry.label, "Reload configuration");
        assert_eq!(entry.shortcut.as_deref(), Some("Ctrl+Shift+R"));
        assert!(entry.action.is_global());
        std::fs::remove_file(&path).unwrap();
        assert!(source.reload().is_err());
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn theme_overrides_parse_and_validate_like_global_colors() {
        let config = parse(
            r##"return {
            colors = { ui = { warning = '#abcdef' } },
            theme_colors = { ['Gruvbox Dark'] = { accent = '#fabd2f' } }
        }"##,
        )
        .unwrap();
        let palette =
            config.theme_colors["Gruvbox Dark"].apply(crate::colors::UiPalette::default());
        assert_eq!(crate::colors::rgb(palette.accent), 0xfabd2f);
        assert!(
            parse(r##"return { theme_colors = { ['Gruvbox Dark'] = { accent = '#bad' } } }"##)
                .is_err()
        );
        assert!(
            parse(r##"return { theme_colors = { ['Gruvbox Dark'] = { unknown = '#fabd2f' } } }"##)
                .is_err()
        );
    }

    #[test]
    fn lua_ui_overrides_parse_and_invalid_colors_report_config_issue() {
        let config = parse(r##"return { colors = { theme = 'Gruvbox Dark', ui = { accent = '#123456', warning = 'abcdef' } } }"##).unwrap();
        let palette = config.colors.ui.apply(crate::colors::UiPalette::default());
        assert_eq!(crate::colors::rgb(palette.accent), 0x123456);
        assert_eq!(crate::colors::rgb(palette.warning), 0xabcdef);
        assert!(
            config
                .ghostty_args()
                .unwrap()
                .contains(&"--theme=Gruvbox Dark".to_owned())
        );
        assert!(parse(r##"return { colors = { ui = { accent = '#bad' } } }"##).is_err());
    }
    #[test]
    fn invalid_lua_config_starts_with_defaults_and_keeps_diagnostic() {
        let path =
            std::env::temp_dir().join(format!("ghostty-feedback-{}.lua", std::process::id()));
        for source in [
            "return {\nfont = { size =",
            "return { font = { size = 'large' } }",
            "return { colors = { ui = { accent = '#invalid' } } }",
        ] {
            std::fs::write(&path, source).unwrap();
            let mut issues = Vec::new();
            let result = super::prepare(
                vec!["ghostty".into(), format!("--lua-config={}", path.display())],
                &mut issues,
            );
            let (args, _, padding, overrides) = result.unwrap();
            assert_eq!(issues.len(), 1);
            assert!(issues[0].contains(&path.display().to_string()));
            assert!(!args.iter().any(|arg| arg.starts_with("--font-size=")));
            assert!(padding.is_none());
            assert!(overrides.global.accent.is_none());
            assert!(overrides.themes.is_empty());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_explicit_config_preserves_cli_and_reports_issue() {
        let path = std::env::temp_dir().join(format!("ghostty-missing-{}.lua", std::process::id()));
        let mut issues = Vec::new();
        let (args, _, padding, _) = super::prepare(
            vec![
                "ghostty".into(),
                format!("--lua-config={}", path.display()),
                "--font-size=18".into(),
                "-e".into(),
                "cmd.exe".into(),
            ],
            &mut issues,
        )
        .unwrap();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains(&path.display().to_string()));
        assert!(issues[0].contains("Using defaults"));
        assert!(padding.is_none());
        assert!(args.ends_with(&["--font-size=18".into(), "-e".into(), "cmd.exe".into()]));
    }

    #[test]
    fn named_lua_actions_bind_and_appear_in_palette() {
        let config = parse(
            r#"return {
            actions = { Save = { label = 'Save selection', action = function(app)
                app:show_toast('Saved', app:get_selection_text(), 1500)
            end }, Unbound = function(app) app:show_toast('Hello', 'world') end },
            keys = {{key='s', mods='CTRL|SHIFT', action='Save'}}
        }"#,
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let catalog = crate::actions::catalog(&bindings, false, false);
        assert!(catalog.iter().any(|entry| entry.id == "action.Save"
            && entry.label == "Save selection"
            && entry.shortcut.as_deref() == Some("Ctrl+Shift+S")));
        assert!(
            catalog
                .iter()
                .any(|entry| entry.id == "action.Unbound" && entry.shortcut.is_none())
        );
        let commands = bindings
            .lua_action("Save")
            .unwrap()
            .callback
            .call_action("selected text".into())
            .unwrap();
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::ShowToast { title, message, duration_ms: 1500 } if title == "Saved" && message == "selected text")
        );
        assert_eq!(commands.len(), 1);
        for source in [
            "return {actions={NewWindow=function() end}}",
            "return {actions={bad={label='', action=function() end}}}",
            "return {actions={bad={action=function() end, typo=true}}}",
            "return {actions={bad='not a function'}}",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn save_snippet_preserves_json_and_reports_success_and_errors() {
        let directory = std::env::temp_dir().join(format!(
            "ghostty-snippet-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("snippets.json");
        std::fs::write(
            &path,
            r#"{"snippets":[{"input":"old","extra":42}],"metadata":{"keep":true},"empty":[]}"#,
        )
        .unwrap();
        let sample = include_str!("../nterm.lua");
        let begin = sample.find("local snippets_path = ").unwrap();
        let end = begin + sample[begin..].find('\n').unwrap();
        let source = format!(
            "{}local snippets_path = {}{}",
            &sample[..begin],
            serde_json::to_string(&path.to_string_lossy()).unwrap(),
            &sample[end..]
        );
        let config = parse(&source).unwrap();
        let callback = &config.actions["SaveSnippet"].callback;
        let commands = callback
            .call_action("echo café\nsecond line".into())
            .unwrap();
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::ShowToast { title, .. } if title == "Snippet saved")
        );
        assert_eq!(commands.len(), 1);
        let bytes = std::fs::read(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved["snippets"][0]["extra"], 42);
        assert_eq!(saved["snippets"][1]["input"], "echo café\nsecond line");
        assert_eq!(saved["metadata"]["keep"], true);
        assert_eq!(saved["empty"], serde_json::json!([]));
        let commands = callback.call_action(String::new()).unwrap();
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::ShowToast { title, message, .. } if title == "Save snippet" && message == "Select some text first.")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, "invalid JSON").unwrap();
        let commands = callback.call_action("keep selection".into()).unwrap();
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::ShowToast { message, .. } if message.contains("Unable to parse"))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid JSON");
        std::fs::remove_file(&path).unwrap();
        let commands = callback.call_action("keep selection".into()).unwrap();
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::ShowToast { message, .. } if message.contains("Unable to read"))
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn command_picker_exit_codes_are_configurable_and_validated() {
        let config = parse("return { pickers = { command = { type = 'Command', args = {'cmd.exe'}, success_exit_codes = {0, 1} } } }").unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_defaults(
            config.pickers,
            config.keys,
            config.picker_defaults,
        )
        .unwrap();
        let options = bindings
            .named("command")
            .unwrap()
            .resolve(&Default::default())
            .unwrap();
        assert!(
            matches!(options.source, crate::picker::config::Source::Command(command) if command.success_exit_codes == Some(vec![0, 1]))
        );
        let config = parse("return { pickers = { command = { type = 'Command', args = {'cmd.exe'}, success_exit_codes = {-1} } } }").unwrap();
        assert!(
            crate::picker::config::Bindings::compile_with_defaults(
                config.pickers,
                config.keys,
                config.picker_defaults
            )
            .is_err()
        );
    }

    #[test]
    fn error_log_is_a_named_picker_with_cursor_position() {
        let config = parse("return { pickers = { errors = { type = 'ErrorLog', position = 'Cursor' } }, keys = { { key = 'h', mods = 'CTRL|SHIFT', picker = 'errors' } } }").unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_defaults(
            config.pickers,
            config.keys,
            config.picker_defaults,
        )
        .unwrap();
        let options = bindings
            .named("errors")
            .unwrap()
            .resolve(&Default::default())
            .unwrap();
        assert!(matches!(
            options.source,
            crate::picker::config::Source::ErrorLog
        ));
        assert_eq!(options.position, crate::picker::config::Position::Cursor);
        assert!(
            parse(
                "return { pickers = { errors = { type = 'ErrorLog', action = function() end } } }"
            )
            .is_err()
        );
    }

    #[test]
    fn themes_is_a_named_picker_with_cursor_position() {
        let config = parse("return { pickers = { themes = { type = 'Themes', position = 'Cursor', initial_query = 'Rose Pine' } }, keys = { { key = 't', mods = 'CTRL|SHIFT', picker = 'themes' } } }").unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_defaults(
            config.pickers,
            config.keys,
            config.picker_defaults,
        )
        .unwrap();
        let options = bindings
            .named("themes")
            .unwrap()
            .resolve(&Default::default())
            .unwrap();
        assert!(matches!(
            options.source,
            crate::picker::config::Source::Themes
        ));
        assert_eq!(options.position, crate::picker::config::Position::Cursor);
        assert_eq!(
            options.params.get("query").map(String::as_str),
            Some("Rose Pine")
        );
        assert!(
            parse("return { pickers = { themes = { type = 'Themes', action = function() end } } }")
                .is_err()
        );
    }

    #[test]
    fn text_preview_wrap_lua_options_validate() {
        for (field, expected) in [
            ("", false),
            (", wrap = false", false),
            (", wrap = true", true),
        ] {
            let config = parse(&"return {pickers={test={type='Items', items={{text='example', value='example'}}, preview={type='Text', text='long line'WRAP}}}}".replace("WRAP", field)).unwrap();
            let bindings = crate::picker::config::Bindings::compile_with_defaults(
                config.pickers,
                config.keys,
                config.picker_defaults,
            )
            .unwrap();
            let options = bindings
                .named("test")
                .unwrap()
                .resolve(&Default::default())
                .unwrap();
            assert!(
                matches!(options.preview, Some(crate::picker::config::Preview::Text { wrap, .. }) if wrap == expected)
            );
        }
        assert!(parse("return {pickers={test={type='Items', items={{text='example', value='example'}}, preview={type='Text', text='line', wrap='true'}}}}").is_err());
    }

    #[test]
    fn full_pane_lua_named_and_factory_pickers_accept_result_rows() {
        let config = parse("return {pickers={fixed={type='Items', items={{text='item',value='value'}}, position='FullPane', result_rows=5, preview={type='Text',text='preview',wrap=true}}, dynamic=function() return {type='Items',items={{text='item',value='value'}},position='FullPane',result_rows=9} end}}").unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_defaults(
            config.pickers,
            config.keys,
            config.picker_defaults,
        )
        .unwrap();
        for (name, rows) in [("fixed", 5), ("dynamic", 9)] {
            let options = bindings
                .named(name)
                .unwrap()
                .resolve(&Default::default())
                .unwrap();
            assert_eq!(options.position, crate::picker::config::Position::FullPane);
            assert_eq!(options.result_rows, rows);
        }
    }

    #[test]
    fn palette_is_a_named_picker_and_defers_picker_factories() {
        let config = parse("return { pickers = { commands = { type = 'CommandPalette', position = 'Center', preview_visible = false }, dynamic = function() error('factory must not run during catalog enumeration') end }, keys = { { key = 'k', mods = 'CTRL|SHIFT', picker = 'commands' } } }").unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_defaults(
            config.pickers,
            config.keys,
            config.picker_defaults,
        )
        .unwrap();
        let options = bindings
            .named("commands")
            .unwrap()
            .resolve(&Default::default())
            .unwrap();
        assert!(matches!(
            options.source,
            crate::picker::config::Source::CommandPalette(_)
        ));
        assert_eq!(options.position, crate::picker::config::Position::Center);
        assert!(!options.preview_visible);
        let entries = crate::actions::catalog(&bindings, false, false);
        assert!(
            entries.iter().any(
                |entry| entry.action == crate::actions::AppAction::OpenPicker("dynamic".into())
            )
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.action
                    == crate::actions::AppAction::OpenPicker("commands".into()))
        );
        assert!(parse("return { command_palette = { key = 'k' } }").is_err());
    }
    #[test]
    fn picker_callbacks_survive_config_parsing() {
        let config = parse("return { pickers = { files = { type = 'FileSystem', action = function(app, item) app:paste(item.path) end } } }").unwrap();
        let callback = config.pickers.as_ref().unwrap()["files"]
            .callback()
            .unwrap();
        assert!(
            matches!(&callback.call("file.txt", "file.txt", true, None, None, &Default::default()).unwrap()[0], crate::lua_callbacks::Command::Paste(text) if text == "file.txt")
        );
        assert!(parse("return { pickers = { files = { action = 'invalid' } } }").is_err());
    }

    #[test]
    fn sample_completion_callback_sends_edit_sequence_then_pastes_value() {
        let config = parse(include_str!("../nterm.lua")).unwrap();
        let callback = config.pickers.as_ref().unwrap()["PowerShellCompletion"]
            .callback()
            .unwrap();
        let params = std::collections::BTreeMap::from([
            ("replaceBefore".into(), "2".into()),
            ("replaceAfter".into(), "1".into()),
        ]);
        let commands = callback
            .call("shown", "inserted", false, None, None, &params)
            .unwrap();
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::SendText(text) if text == "\x08\x08\x1b[3~")
        );
        assert!(
            matches!(&commands[1], crate::lua_callbacks::Command::Paste(text) if text == "inserted")
        );
    }

    #[test]
    fn sample_codex_picker_uses_terminal_context_and_replaces_unicode_input() {
        let config = parse(include_str!("../nterm.lua")).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let entry = bindings.named("CodexSuggestions").unwrap();
        let options = entry
            .resolve(&PickerContext {
                line: "PS # echo café trailing".into(),
                line_before_cursor: "PS # echo café".into(),
                recent_lines: vec!["previous command".into(), "previous output".into()],
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(
            options.sort,
            crate::picker::config::Sort::SourceOrder
        ));
        assert!(!options.preview_visible);
        let crate::picker::config::Source::Command(command) = options.source else {
            panic!("expected command");
        };
        assert_eq!(&command.args[..2], &["codex", "exec"]);
        let prompt = command.args.last().unwrap();
        assert!(prompt.contains("PS # echo café"));
        assert!(!prompt.contains("trailing"));
        assert!(prompt.contains("previous command\nprevious output"));
        let actions = options
            .callback
            .unwrap()
            .call(
                "echo done",
                "echo done",
                false,
                None,
                None,
                &Default::default(),
            )
            .unwrap();
        assert!(
            matches!(&actions[0], crate::lua_callbacks::Command::ReplacePrompt { text, before: 10, after: 0 } if text == "echo done")
        );
        assert!(
            entry
                .resolve(&PickerContext {
                    line_before_cursor: "PS > echo".into(),
                    recent_lines: vec!["output".into()],
                    ..Default::default()
                })
                .is_err()
        );
        assert!(
            entry
                .resolve(&PickerContext {
                    line_before_cursor: "PS # echo".into(),
                    ..Default::default()
                })
                .is_err()
        );
    }

    #[test]
    fn sample_ripgrep_callback_replaces_prompt_and_opens_match() {
        let config = parse(include_str!("../nterm.lua")).unwrap();
        let callback = config.pickers.as_ref().unwrap()["Ripgrep"]
            .callback()
            .unwrap();
        let params = std::collections::BTreeMap::from([
            ("cursor".into(), "2".into()),
            ("length".into(), "4".into()),
        ]);
        let commands = callback
            .call(
                "C:\\it's here.txt:42:3:text",
                "C:\\it's here.txt:42:3:text",
                false,
                None,
                None,
                &params,
            )
            .unwrap();
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::SendText(text) if text == "\x08\x08\x1b[3~\x1b[3~")
        );
        assert!(
            matches!(&commands[1], crate::lua_callbacks::Command::SendText(text) if text == "nvim +42 'C:\\it''s here.txt'\r")
        );
    }
    use super::*;

    fn parse(source: &str) -> Result<AppConfig, String> {
        AppConfig::parse(source, Path::new("test-config.lua"))
    }

    #[test]
    fn builtin_picker_overrides_are_partial_and_validated() {
        let config = parse("return { builtin_pickers = { line_break = { preview_visible = false, position = 'Center', result_rows = 5, preview_rows = 8 } } }").unwrap();
        let settings = config.builtin_pickers.line_break;
        assert_eq!(settings.preview_visible, Some(false));
        assert_eq!(
            settings.position,
            Some(crate::picker::config::Position::Center)
        );
        assert_eq!(settings.result_rows, Some(5));
        assert_eq!(settings.preview_rows, Some(8));
        let partial =
            parse("return { builtin_pickers = { line_break = { preview_visible = false } } }")
                .unwrap();
        assert!(partial.builtin_pickers.line_break.position.is_none());
        assert!(
            parse("return {}")
                .unwrap()
                .builtin_pickers
                .line_break
                .preview_visible
                .is_none()
        );
        for source in [
            "return { builtin_pickers = { unknown = {} } }",
            "return { builtin_pickers = { line_break = { source = 'Items' } } }",
            "return { builtin_pickers = { line_break = { result_rows = 0 } } }",
            "return { builtin_pickers = { line_break = { preview_rows = 13 } } }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn builtin_picker_defaults_overrides_factories_and_namespaces() {
        let config = parse(r#"return {
            builtin_pickers = {
                themes = {position='Center'},
                lines = {preview_visible=false},
                words = function(context) return {initial_query=context.line_before_cursor, position='Cursor', action=function(app, item) app:replace_prompt(item.value, 2, 0) end} end,
            },
            pickers = {lines = {type='Items', items={{text='custom', value='custom'}}}},
            keys = {{key='f', mods='CTRL', builtin_picker='lines'}, {key='p', builtin_picker='command_palette'}, {key='w', builtin_picker='words'}},
        }"#).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let context = PickerContext {
            line_before_cursor: "ab".into(),
            ..Default::default()
        };
        let lines = bindings
            .named("builtin.lines")
            .unwrap()
            .resolve(&context)
            .unwrap();
        assert!(!lines.preview_visible);
        assert!(matches!(
            lines.source,
            crate::picker::config::Source::Scrollback { .. }
        ));
        assert!(matches!(
            bindings
                .named("lines")
                .unwrap()
                .resolve(&context)
                .unwrap()
                .source,
            crate::picker::config::Source::Items(_)
        ));
        let words = bindings
            .named("builtin.words")
            .unwrap()
            .resolve(&context)
            .unwrap();
        assert_eq!(words.params.get("query").map(String::as_str), Some("ab"));
        assert!(words.callback.is_some());
        assert!(matches!(
            words.enter_action,
            crate::picker::config::EnterAction::SendInput
        ));
        match words.source {
            crate::picker::config::Source::Scrollback {
                regex,
                unique,
                prompts,
            } => {
                assert_eq!(regex.as_str(), r"\S+");
                assert!(unique);
                assert!(!prompts);
            }
            _ => panic!("word source"),
        }
        assert_eq!(
            bindings
                .named("builtin.themes")
                .unwrap()
                .resolve(&context)
                .unwrap()
                .position,
            crate::picker::config::Position::Center
        );
        assert!(bindings.named("builtin.command_palette").is_some());
        for source in [
            "return {builtin_pickers={words={type='Items'}}}",
            "return {builtin_pickers={words={regex='.*'}}}",
            "return {pickers={['builtin.words']={}}}",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn alt_workspace_bindings_reach_application_dispatch() {
        let source=std::env::var_os("GHOSTTY_TEST_LUA_CONFIG")
            .map(|path|std::fs::read_to_string(path).unwrap())
            .unwrap_or_else(||r#"local keys=nterm.workspace_keys('ALT'); for _,b in ipairs(keys) do if b.action=='SetDeck' then b.key='d' elseif b.action=='SetGrid' then b.key='g' end end; return {keys=keys}"#.into());
        let config = parse(&source).unwrap();
        let mut bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        if let Some(path) = std::env::var_os("GHOSTTY_TEST_LUA_CONFIG") {
            let mut issues = Vec::new();
            let (_, loaded, _, _, _) = super::prepare_with_reload(
                vec![
                    "nterm".into(),
                    format!("--lua-config={}", std::path::Path::new(&path).display()),
                ],
                &mut issues,
            )
            .unwrap();
            assert!(issues.is_empty(), "startup config failed: {issues:?}");
            bindings = loaded;
        }
        for (key, name) in [
            (egui::Key::H, "FocusLeft"),
            (egui::Key::J, "FocusDown"),
            (egui::Key::K, "FocusUp"),
            (egui::Key::L, "FocusRight"),
            (egui::Key::D, "SetDeck"),
            (egui::Key::G, "SetGrid"),
            (egui::Key::T, "SetTile"),
            (egui::Key::M, "SetMonocle"),
            (egui::Key::C, "CreatePane"),
            (egui::Key::W, "WorkspacePicker"),
        ] {
            let expected = crate::actions::AppAction::Workspace(
                crate::terminal::workspace_ui::WorkspaceAction::parse(name).unwrap(),
            );
            let mut events = vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            }];
            assert_eq!(
                bindings.consume_when(&mut events, |action, mods| {
                    crate::actions::ActionContext::default().allows(action, mods)
                }),
                (Some(expected), true),
                "Alt+{}",
                key.name()
            );
            assert!(events.is_empty());
        }
    }

    #[test]
    fn config_file_compiles_and_resolves_builtin_pickers() {
        // This also permits validating a migrated user config without launching
        // the application or running command/item providers.
        let source = match std::env::var_os("GHOSTTY_TEST_LUA_CONFIG") {
            Some(path) => std::fs::read_to_string(path).unwrap(),
            None => include_str!("../nterm.lua").into(),
        };
        let config = parse(&source).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap()
        .with_workspaces(config.workspaces, config.startup)
        .unwrap()
        .with_builtin_pickers(config.builtin_pickers)
        .with_pipe_policy(config.picker_pipe)
        .with_prompt_patterns(config.compiled_prompts)
        .with_quick_selects(config.quick_selects)
        .unwrap()
        .with_cursor_scroll_padding(
            config
                .cursor_scroll_padding
                .unwrap_or(nfm_egui::copy_mode::SCROLL_PADDING_ROWS),
        );
        let context = PickerContext {
            line_before_cursor: "PS C:\\src> foo".into(),
            ..Default::default()
        };
        for name in crate::picker::config::BUILTIN_NAMES {
            bindings
                .named(&format!("builtin.{name}"))
                .unwrap()
                .resolve(&context)
                .unwrap();
        }
        assert!(
            !bindings
                .builtin_pickers
                .line_break
                .preview_visible
                .unwrap_or(true)
        );
    }

    #[test]
    fn sample_config_includes_multiple_floating_pane_shortcuts() {
        let config = parse(include_str!("../nterm.lua")).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        for action in [
            crate::actions::AppAction::CreateFloatingPane,
            crate::actions::AppAction::NextFloatingPane,
            crate::actions::AppAction::PreviousFloatingPane,
        ] {
            assert!(
                bindings.shortcut_for(&action).is_some(),
                "missing {action:?}"
            );
        }
    }
    #[test]
    fn named_pickers_compile_and_reject_bad_bindings_and_regexes() {
        let example = parse(include_str!("../nterm.lua")).unwrap();
        assert!(
            crate::picker::config::Bindings::compile_with_actions(
                example.pickers,
                example.keys,
                example.picker_defaults,
                example.actions
            )
            .is_ok()
        );
        let config = parse(r#"return {pickers = {paths = {type = 'Scrollback', regex = '\\S+', enter_action = 'SendInput'}}, keys = {{key = 'p', mods = 'CTRL|SHIFT', picker = 'paths'}}}"#).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let mut events = vec![egui::Event::Key {
            key: egui::Key::P,
            physical_key: Some(egui::Key::P),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        }];
        let (options, consumed) = bindings.consume(&mut events);
        assert!(consumed && events.is_empty());
        let crate::actions::AppAction::OpenPicker(name) = options.unwrap() else {
            panic!("expected picker action")
        };
        let options = bindings
            .named(&name)
            .unwrap()
            .resolve(&PickerContext::default())
            .unwrap();
        assert!(matches!(
            options.enter_action,
            crate::picker::config::EnterAction::SendInput
        ));
        let crate::picker::config::Source::Scrollback { regex, .. } = options.source else {
            panic!("expected scrollback")
        };
        assert_eq!(regex.find("x y").unwrap().as_str(), "x");
        for source in [
            "return {pickers={x={regex='['}}}",
            "return {pickers={x={regex='a*'}}}",
            "return {keys={{key='p', picker='missing'}}}",
            "return {keys={{key='f', mods='BAD', picker='lines'}}}",
            "return {keys={{key='p', action='UnknownAction'}}}",
            "return {keys={{key='p', action='OpenSearch', picker='lines'}}}",
            "return {keys={{key='p'}}}",
            "return {keys={{key='p', picker='lines'}, {key='p', picker='lines'}}}",
            "return {pickers={x={type='FileSystem', enter_action='Navigate'}}}",
            "return {pickers={x={type='FileSystem', files_only=true, directories_only=true}}}",
            "return {pickers={x={type='FileSystem', max_depth=-1}}}",
            "return {pickers={x={type='Command', args={'{program}'}}}}",
            "return {pickers={x={type='Command', args={'cmd'}, result_rows=0}}}",
            "return {pickers={x={type='Command', args={'cmd'}, result_rows=257}}}",
            "return {pickers={x={type='Command', args={'cmd'}, sort='Invalid'}}}",
        ] {
            let result = parse(source).and_then(|config| {
                crate::picker::config::Bindings::compile_with_actions(
                    config.pickers,
                    config.keys,
                    config.picker_defaults,
                    config.actions,
                )
            });
            assert!(result.is_err(), "{source}");
        }
        assert!(parse("return {pickers={x={type='Command'}}}").is_err());
        let files = parse("return {pickers={files={type='FileSystem', roots={'G:/src'}, max_depth=2, files_only=true}},keys={{key='o',mods='CTRL|SHIFT',picker='files'}}}").unwrap();
        assert!(
            crate::picker::config::Bindings::compile_with_actions(
                files.pickers,
                files.keys,
                files.picker_defaults,
                files.actions
            )
            .is_ok()
        );
        let history = parse(
            "return {pickers={history={type='Command', args={'history.exe'}, line_continuation='`'}}}",
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            history.pickers,
            history.keys,
            history.picker_defaults,
            history.actions,
        )
        .unwrap();
        let crate::picker::config::Source::Command(command) = bindings
            .named("history")
            .unwrap()
            .resolve(&PickerContext::default())
            .unwrap()
            .source
        else {
            panic!("expected command picker");
        };
        assert_eq!(command.line_continuation.as_deref(), Some("`"));
        assert!(parse("return {pickers={x={enter_action='Bad'}}}").is_err());
        let empty = parse("return {keys={}}").unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            empty.pickers,
            empty.keys,
            empty.picker_defaults,
            empty.actions,
        )
        .unwrap();
        assert!(bindings.named("lines").is_none());
        assert!(bindings.named("commands").is_none());
        assert!(matches!(
            bindings
                .named("builtin.lines")
                .unwrap()
                .resolve(&PickerContext::default())
                .unwrap()
                .preview,
            Some(crate::picker::config::Preview::Scrollback { rows: 12 })
        ));
        assert!(bindings.named("missing").is_none());
        // Without pickers or keys, the default shortcuts open the built-ins.
        let defaults = parse("return {}").unwrap();
        let default_bindings = crate::picker::config::Bindings::compile_with_actions(
            defaults.pickers,
            defaults.keys,
            defaults.picker_defaults,
            defaults.actions,
        )
        .unwrap();
        for (key, expected) in [
            (egui::Key::F, "builtin.lines"),
            (egui::Key::Space, "builtin.command_palette"),
        ] {
            let mut events = vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            }];
            let (open, _) = default_bindings.consume(&mut events);
            assert_eq!(
                open,
                Some(crate::actions::AppAction::OpenPicker(expected.into()))
            );
        }
        let mut events = vec![egui::Event::Key {
            key: egui::Key::F,
            physical_key: Some(egui::Key::F),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        }];
        let (open, consumed) = bindings.consume(&mut events);
        assert!(!consumed && open.is_none() && events.len() == 1);
    }

    #[test]
    fn ripgrep_pipe_parameters_resolve_to_command_picker() {
        let config = parse(include_str!("../nterm.lua")).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let request: crate::picker::pipe::Open = serde_json::from_str(r#"{"kind":"named","name":"Ripgrep","params":{"pattern":"ghostty","cwd":"C:\\src\\ghostty2","cursor":"0","length":"0"}}"#).unwrap();
        let crate::picker::pipe::Open::Named { name, params, .. } = request else {
            panic!("expected named picker")
        };
        let options = bindings
            .named(&name)
            .unwrap()
            .resolve(&PickerContext::default())
            .unwrap()
            .with_request(params)
            .unwrap();
        assert!(matches!(
            options.source,
            crate::picker::config::Source::Command(_)
        ));
        assert_eq!(options.params["pattern"], "ghostty");
    }

    #[test]
    fn filesystem_pipe_parameters_override_configured_roots() {
        let config = parse(
            r#"return { pickers = { files = { type = 'FileSystem', roots = { [[C:\default]] } } } }"#,
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let request: crate::picker::pipe::Open = serde_json::from_str(
            r#"{"kind":"named","name":"files","params":{"roots":"[\"G:\\\\src\",\"C:\\\\src\"]"}}"#,
        )
        .unwrap();
        let crate::picker::pipe::Open::Named { name, params, .. } = request else {
            panic!("expected named picker")
        };
        let options = bindings
            .named(&name)
            .unwrap()
            .resolve(&PickerContext::default())
            .unwrap()
            .with_request(params)
            .unwrap();
        let crate::picker::config::Source::FileSystem(files) = options.source else {
            panic!("expected filesystem picker");
        };
        assert_eq!(
            files.roots,
            [
                std::path::PathBuf::from(r"G:\src"),
                std::path::PathBuf::from(r"C:\src")
            ]
        );
    }

    #[test]
    fn filesystem_relative_is_configurable_and_overridable_per_request() {
        let config = parse(
            r#"return { pickers = {
                files = { type = 'FileSystem', roots = { '.' } },
                relative = { type = 'FileSystem', roots = { '.' }, relative = true },
            } }"#,
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let relative = |name: &str, params: &[(&str, &str)]| {
            let params = params
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            bindings
                .named(name)
                .unwrap()
                .resolve(&PickerContext::default())
                .unwrap()
                .with_request(params)
                .map(|options| match options.source {
                    crate::picker::config::Source::FileSystem(files) => files.relative,
                    _ => panic!("expected filesystem picker"),
                })
        };
        assert_eq!(relative("files", &[]), Ok(false));
        assert_eq!(relative("relative", &[]), Ok(true));
        assert_eq!(relative("files", &[("relative", "true")]), Ok(true));
        assert_eq!(relative("relative", &[("relative", "false")]), Ok(false));
        assert!(relative("files", &[("relative", "yes")]).is_err());
    }

    #[test]
    fn lua_items_provider_rereads_file_each_time() {
        let path =
            std::env::temp_dir().join(format!("nterm-picker-items-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"snippets":[{"input":"first"}]}"#).unwrap();
        let source = format!(
            r#"return {{ pickers = {{ snippets = {{ type = 'Items', items = function()
                local file = assert(io.open([[{}]], 'rb'))
                local data = nterm.json_parse(file:read('*a'))
                file:close()
                local items = {{}}
                for _, snippet in ipairs(data.snippets or {{}}) do
                    items[#items + 1] = {{ text = snippet.input, value = snippet.input }}
                end
                return items
            end }} }} }}"#,
            path.display()
        );
        let config = parse(&source).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let options = bindings
            .named("snippets")
            .unwrap()
            .resolve(&PickerContext::default())
            .unwrap();
        let crate::picker::config::Source::Items(items) = options.source else {
            panic!("expected items picker");
        };
        let provider = items.provider.unwrap();
        assert_eq!(provider.read().unwrap()[0].value, "first");
        std::fs::write(&path, r#"{"snippets":[{"input":"second"}]}"#).unwrap();
        assert_eq!(provider.read().unwrap()[0].value, "second");
        std::fs::write(&path, r#"{"snippets":[]}"#).unwrap();
        assert!(provider.read().unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn picker_schemas_reject_fields_from_other_sources() {
        for source in [
            "return {pickers={x={type='FileSystem', regex='x'}}}",
            "return {pickers={x={roots={}}}}",
            "return {pickers={x={type='Scrollback', max_depth=1}}}",
            "return {pickers={x={type='Scrollback', files_only=false}}}",
            "return {pickers={x={type='Scrollback', directories_only=false}}}",
            "return {pickers={x={type='Scrollback', unknown=true}}}",
            "return {pickers={x={type='FileSystem', unknown=true}}}",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        for kind in ["Scrollback", "FileSystem"] {
            let config = parse(&format!("return {{pickers={{x={{type='{kind}', enter_action='SendInput', action=function(app,item) app:paste(item.text) end}}}}}}" )).unwrap();
            assert!(config.pickers.as_ref().unwrap()["x"].callback().is_some());
            assert!(
                crate::picker::config::Bindings::compile_with_actions(
                    config.pickers,
                    config.keys,
                    config.picker_defaults,
                    config.actions
                )
                .is_ok()
            );
        }
        let config = parse("return {pickers={x={regex='x'}}}").unwrap();
        assert!(matches!(
            config.pickers.as_ref().unwrap()["x"].source,
            crate::picker::config::DefinitionSource::Scrollback { .. }
        ));
    }

    #[test]
    fn picker_preview_accepts_text_ansi_and_command_sources() {
        let config = parse(
            r#"return { pickers = {
            text = { type='Scrollback', preview={type='Text', text='{item}'} },
            ansi = { type='Items', preview={type='Ansi', text='\\27[31m{item}'} },
            command = { type='FileSystem', preview={type='Command', args={'cat', '{item}'}} },
        } }"#,
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        for name in ["text", "ansi", "command"] {
            let options = bindings
                .named(name)
                .unwrap()
                .resolve(&Default::default())
                .unwrap();
            assert!(options.preview.is_some());
        }
        let invalid = parse("return {pickers={x={preview={type='Command', args={''}}}}}").unwrap();
        assert!(
            crate::picker::config::Bindings::compile_with_actions(
                invalid.pickers,
                invalid.keys,
                invalid.picker_defaults,
                invalid.actions
            )
            .is_err()
        );
    }

    #[test]
    fn cursor_scroll_padding_accepts_overrides_and_rejects_invalid_values() {
        assert_eq!(parse("return {}").unwrap().cursor_scroll_padding, None);
        assert_eq!(
            crate::picker::config::Bindings::default().cursor_scroll_padding,
            2
        );
        for value in [0, 2, 5] {
            let config = parse(&format!("return {{ cursor_scroll_padding={value} }}")).unwrap();
            let bindings = crate::picker::config::Bindings::default()
                .with_cursor_scroll_padding(config.cursor_scroll_padding.unwrap());
            assert_eq!(bindings.cursor_scroll_padding, value);
        }
        for value in ["-1", "1.5", "'two'", "4294967296"] {
            assert!(parse(&format!("return {{ cursor_scroll_padding={value} }}")).is_err());
        }
    }

    #[test]
    fn prompt_patterns_are_validated_and_replace_defaults() {
        let defaults = parse("return {}").unwrap();
        assert!(
            defaults
                .compiled_prompts
                .prefix_end("PS C:\\src> ")
                .is_some()
        );
        let custom = parse(r#"return { prompt_patterns = { [[^custom>\s*]] } }"#).unwrap();
        assert_eq!(custom.compiled_prompts.prefix_end("custom> run"), Some(8));
        assert_eq!(custom.compiled_prompts.prefix_end("PS C:> "), None);
        assert!(
            parse("return { prompt_patterns = { '[' } }")
                .err()
                .unwrap()
                .contains("prompt_patterns[1]")
        );
        assert!(parse("return { prompt_patterns = 123 }").is_err());
        assert_eq!(
            parse("return { prompt_patterns = {} }")
                .unwrap()
                .compiled_prompts
                .prefix_end("PS C:> "),
            None
        );
    }

    #[test]
    fn quick_selects_compile_bind_and_appear_in_the_palette() {
        let bindings = |source: &str| {
            let config = parse(source)?;
            crate::picker::config::Bindings::compile_with_actions(
                config.pickers,
                config.keys,
                config.picker_defaults,
                config.actions,
            )?
            .with_quick_selects(config.quick_selects)
        };
        let compiled = bindings(
            r#"return {
                quick_selects = {
                    addresses = { patterns = { [[0x[0-9a-fA-F]+]] }, action = "Copy" },
                    hashes = { patterns = { [[\b[0-9a-f]{7,40}\b]], [[sha:(\w+)]] }, label = "Pick a hash", alphabet = "JKL" },
                    urls = { patterns = { [[https?://\S+]] }, action = function(app, item) app:paste("<" .. item.text .. ">") end },
                },
                quick_select_defaults = { alphabet = "fjdk", patterns = { [[\w+\.\w+]] } },
                keys = { { key = "a", mods = "ALT|SHIFT", quick_select = "addresses" } },
            }"#,
        )
        .unwrap();
        use nfm_egui::quick_select::Alphabet;
        let named = &compiled.quick_selects.named;
        let addresses = &named["addresses"];
        assert!(matches!(addresses.action, quick_selects::Accept::Copy));
        assert_eq!(addresses.label, "Quick select: addresses");
        // Without its own alphabet a quick select uses the default one.
        assert_eq!(addresses.alphabet, Alphabet::new("fjdk").unwrap());
        assert_eq!(named["hashes"].patterns.len(), 2);
        assert!(matches!(
            named["hashes"].action,
            quick_selects::Accept::Navigate
        ));
        assert_eq!(named["hashes"].alphabet, Alphabet::new("jkl").unwrap());
        let default = &compiled.quick_selects.default;
        assert!(default.patterns[0].is_match("main.rs"));
        assert!(!default.patterns[0].is_match("main"));
        // Linewise yanks keep hinting words with the default alphabet.
        let words = &compiled.quick_selects.words;
        assert!(words.patterns[0].is_match("main"));
        assert_eq!(words.alphabet, Alphabet::new("fjdk").unwrap());
        let quick_selects::Accept::Lua(callback) = &named["urls"].action else {
            panic!("urls action should be a Lua callback");
        };
        let url = "https://example.com";
        let commands = callback
            .call_with_selection(
                url,
                url,
                false,
                Some(1),
                None,
                &Default::default(),
                String::new(),
            )
            .unwrap();
        assert!(matches!(
            commands.as_slice(),
            [crate::lua_callbacks::Command::Paste(text)] if text == "<https://example.com>"
        ));
        // Without patterns the built-in quick select keeps hinting words.
        let plain = bindings("return { quick_select_defaults = { alphabet = 'ab' } }").unwrap();
        let default = &plain.quick_selects.default;
        assert!(std::rc::Rc::ptr_eq(default, &plain.quick_selects.words));
        assert_eq!(default.alphabet, Alphabet::new("ab").unwrap());
        let action = crate::actions::AppAction::OpenQuickSelect(Some("addresses".into()));
        assert!(compiled.shortcut_for(&action).is_some());
        let palette = crate::actions::catalog(&compiled, false, false);
        let entry = palette
            .iter()
            .find(|entry| entry.id == "quick_select.hashes")
            .unwrap();
        assert_eq!(entry.label, "Pick a hash");

        for (source, error) in [
            (
                "return { quick_selects = { bad = { patterns = { '[' } } } }",
                "quick_selects.bad.patterns[1]",
            ),
            (
                "return { quick_selects = { bad = { patterns = {} } } }",
                "quick_selects.bad.patterns must not be empty",
            ),
            (
                "return { quick_selects = { bad = { patterns = { 'x' }, action = 'Run' } } }",
                "Run",
            ),
            (
                "return { quick_selects = { bad = { patterns = { 'x' }, alphabet = 'aa' } } }",
                "quick_selects.bad.alphabet: alphabet repeats",
            ),
            (
                "return { quick_select_defaults = { patterns = {} } }",
                "quick_select_defaults.patterns must not be empty",
            ),
            (
                "return { quick_select_defaults = { alphabet = 'a1' } }",
                "quick_select_defaults.alphabet",
            ),
            (
                "return { keys = { { key = 'a', mods = 'ALT', quick_select = 'missing' } } }",
                "unknown quick select \"missing\"",
            ),
            (
                "return { quick_selects = { q = { patterns = { 'x' } } }, keys = { { key = 'a', mods = 'ALT', quick_select = 'q', action = 'ReloadConfig' } } }",
                "exactly one of",
            ),
        ] {
            let message = bindings(source).err().unwrap();
            assert!(message.contains(error), "{source}: {message}");
        }
    }

    #[test]
    fn picker_pipe_permissions_are_opt_in_and_strict() {
        assert!(!parse("return {}").unwrap().picker_pipe.enabled);
        let config =
            parse("return { picker_pipe={enabled=true,allow_stream=true,named={'files'}} }")
                .unwrap();
        assert!(config.picker_pipe.enabled);
        assert!(config.picker_pipe.allow_stream);
        assert!(parse("return { picker_pipe={allow_definitions=true} }").is_err());
        assert_eq!(config.picker_pipe.named, vec!["files"]);
        assert!(parse("return { picker_pipe={enabled=true,unknown=true} }").is_err());
    }

    #[test]
    fn preview_limits_inherit_defaults_and_allow_static_and_factory_overrides() {
        for factory in [false, true] {
            for (fields, expected) in [
                ("", (123456, 456)),
                (",max_bytes=98765", (98765, 456)),
                (",max_scrollback_lines=789", (123456, 789)),
            ] {
                let definition = format!(
                    "{{type='Items',items={{{{text='one',value='one'}}}},preview={{type='Command',args={{'bat'}}{fields}}}}}"
                );
                let definition = if factory {
                    format!("function(context) return {definition} end")
                } else {
                    definition
                };
                let config = parse(&format!("return {{picker_defaults={{preview_max_bytes=123456,preview_max_scrollback_lines=456}},pickers={{x={definition}}}}}")).unwrap();
                let bindings = crate::picker::config::Bindings::compile_with_defaults(
                    config.pickers,
                    config.keys,
                    config.picker_defaults,
                )
                .unwrap();
                let options = bindings
                    .named("x")
                    .unwrap()
                    .resolve(&PickerContext::default())
                    .unwrap();
                let Some(crate::picker::config::Preview::Command {
                    max_bytes,
                    max_scrollback_lines,
                    ..
                }) = options.preview
                else {
                    panic!("missing command preview")
                };
                assert_eq!(
                    (max_bytes.unwrap(), max_scrollback_lines.unwrap()),
                    expected
                );
            }
        }
        for field in ["preview_max_bytes", "preview_max_scrollback_lines"] {
            assert!(parse(&format!("return {{ picker_defaults={{ {field}=0 }} }}")).is_err());
        }
        for field in ["max_bytes", "max_scrollback_lines"] {
            for value in ["0", "-1", "1.5"] {
                let config = parse(&format!(
                    "return {{pickers={{x={{type='Items',preview={{type='Command',args={{'bat'}},{field}={value}}}}}}}}}"
                ));
                if let Ok(config) = config {
                    assert!(
                        crate::picker::config::Bindings::compile_with_defaults(
                            config.pickers,
                            config.keys,
                            config.picker_defaults
                        )
                        .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn removed_preview_callbacks_report_worker_migration_at_load_time() {
        for (kind, field) in [("Command", "command"), ("File", "file")] {
            let preview =
                format!("{{type='{kind}', {field}=function() error('must not execute') end}}");
            for source in [
                format!("return {{pickers={{x={{preview={preview}}}}}}}"),
                format!("return {{builtin_pickers={{lines={{preview={preview}}}}}}}"),
            ] {
                let error = parse(&source).err().expect("removed callback accepted");
                assert!(error.contains(&format!("preview.{field}")), "{error}");
                assert!(
                    error.contains("Dynamic") && error.contains("resolve"),
                    "{error}"
                );
            }
        }
        let config = parse("return {pickers={x=function() return {preview={type='File',file=function() end}} end}}").unwrap();
        let bindings =
            crate::picker::config::Bindings::compile(config.pickers, config.keys).unwrap();
        assert!(
            bindings
                .named("x")
                .unwrap()
                .resolve(&PickerContext::default())
                .err()
                .unwrap()
                .contains("Dynamic")
        );
    }

    #[test]
    fn file_preview_configuration_is_rejected() {
        assert!(parse("return {pickers={x={preview={type='File'}}}}").is_err());
    }

    #[test]
    fn dynamic_preview_resolves_owned_text_command_and_empty_plans() {
        use crate::picker::config::{Bindings, Preview};
        use nfm_preview_file::resolved::{CommandOutput, PreviewPlan};
        for (result, kind) in [
            (
                "{type='Text',text='sample',name=item.value,syntax='ps1',center_line=7}",
                "File",
            ),
            (
                "{type='Command',args={'ffmpeg.exe','-i',item.value},output='Image'}",
                "Image",
            ),
            (
                "{type='Command',args={'cmd.exe','/c','echo text'},output='Text'}",
                "Text",
            ),
            (
                "{type='Command',args={'bat.exe',item.value},output='Ansi',center_line=2}",
                "Ansi",
            ),
            ("nil", "Empty"),
            (
                "{type='Text',lines={'fn main() {','}'},extension='.rs'}",
                "Snapshot",
            ),
            (
                "{type='Image',bytes=string.char(0,255,137,80)}",
                "ImageBytes",
            ),
        ] {
            let config = parse(&format!("return {{picker_defaults={{preview_max_bytes=2048}},pickers={{x={{type='Items',items=nterm.json_parse('[]'),preview={{type='Dynamic',resolve=function(item,params) assert(item.text=='display');return {result} end}}}}}}}}" )).unwrap();
            let defaults = config.picker_defaults;
            let bindings = Bindings::compile_with_actions(
                config.pickers,
                config.keys,
                defaults,
                config.actions,
            )
            .unwrap();
            let options = bindings
                .named("x")
                .unwrap()
                .resolve(&PickerContext::default())
                .unwrap();
            let Some(Preview::Dynamic {
                resolve: Some(resolve),
                max_bytes,
                ..
            }) = options.preview
            else {
                panic!()
            };
            assert_eq!(max_bytes, Some(2048));
            assert!(resolve.worker.is_some());
            let params =
                std::collections::BTreeMap::from([("cwd".into(), "C:/preview-root".into())]);
            match (
                kind,
                resolve
                    .call("display", "source.ps1", false, &params, 2048, 50)
                    .unwrap(),
            ) {
                ("File", Some(PreviewPlan::Text { request, .. })) => {
                    assert_eq!(request.name.as_deref(), Some("source.ps1"));
                    assert_eq!(request.center_line, Some(7));
                }
                (
                    "Image",
                    Some(PreviewPlan::Command {
                        command,
                        output: CommandOutput::Image,
                        ..
                    }),
                )
                | (
                    "Text",
                    Some(PreviewPlan::Command {
                        command,
                        output: CommandOutput::Text,
                        ..
                    }),
                )
                | (
                    "Ansi",
                    Some(PreviewPlan::Command {
                        command,
                        output: CommandOutput::Ansi,
                        ..
                    }),
                ) => {
                    assert_eq!(command.output_limit, 2048);
                    assert_eq!(command.working_directory, Some("C:/preview-root".into()));
                }
                ("Empty", None) => {}
                ("ImageBytes", Some(PreviewPlan::Image { bytes })) => {
                    assert_eq!(&*bytes, &[0, 255, 137, 80])
                }
                ("Snapshot", Some(PreviewPlan::Text { request, .. })) => {
                    assert_eq!(request.extension.as_deref(), Some(".rs"));
                    assert_eq!(
                        request.source.read(2048, &|| false).unwrap().text,
                        "fn main() {\n}"
                    );
                }
                _ => panic!("unexpected resolved preview for {kind}"),
            }
        }
        for preview in [
            "{type='Dynamic'}",
            "{type='Dynamic',resolve='oops'}",
            "{type='Text',text='',resolve=function() end}",
        ] {
            let result = parse(&format!("return {{pickers={{x={{preview={preview}}}}}}}"));
            assert!(
                result
                    .and_then(|config| Bindings::compile(config.pickers, config.keys))
                    .is_err()
            );
        }
        for result in [
            "{type='Unknown'}",
            "{type='Command',args={'x'},output='Bogus'}",
            "{type='Command',args={}}",
            "{type='Command',args={'x'},output='Image',center_line=1}",
            "{type='File',path='x',syntax='missing'}",
            "{type='File',path=''}",
            "{type='File',path='x',unknown=true}",
        ] {
            let config = parse(&format!("return {{pickers={{x={{preview={{type='Dynamic',resolve=function() return {result} end}}}}}}}}" )).unwrap();
            let bindings = Bindings::compile(config.pickers, config.keys).unwrap();
            let options = bindings
                .named("x")
                .unwrap()
                .resolve(&PickerContext::default())
                .unwrap();
            let Some(Preview::Dynamic {
                resolve: Some(resolve),
                ..
            }) = options.preview
            else {
                panic!()
            };
            assert!(
                resolve
                    .call("x", "x", false, &Default::default(), 2048, 50)
                    .is_err(),
                "{result}"
            );
        }
    }

    #[test]
    fn preview_helpers_can_be_captured_during_config_loading_and_extended_sample_compiles() {
        let config = parse("local run = preview.run; return {pickers={x={type='Items',items={{text='x',value='x'}},preview={type='Dynamic',resolve=function(item) return {type='Text',text=run{args={'cmd.exe','/c','echo text'}}.stdout} end}}}}").unwrap();
        crate::picker::config::Bindings::compile(config.pickers, config.keys).unwrap();
        let config = parse(include_str!("../nterm.example.lua")).unwrap();
        crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap()
        .with_quick_selects(config.quick_selects)
        .unwrap();
    }
    #[test]
    fn lua_ripgrep_preview_preserves_windows_path_syntax_and_match_location() {
        nfm_preview_file::text::TextPreviewOptions {
            syntax: nfm_preview_file::SyntaxChoice::Named("C#".into()),
            ..Default::default()
        }
        .validate()
        .unwrap();
        let source = include_str!("../nterm.example.lua");
        let start = source.find("local function rg_preview()").unwrap();
        let end = source.find("local pipe_help_preview =").unwrap();
        let lua = mlua::Lua::new();
        let resolver: mlua::Function = lua.load(format!("local preview_syntax={{ps1='PowerShell'}}; local function preview_path(path,cwd) return path end; local function preview_read_text(path) assert(path=='C:\\\\folder\\\\file.ps1'); return 'file content', binary_test end; {} return rg_preview().resolve",&source[start..end])).eval().unwrap();
        for binary in [false, true] {
            lua.globals().set("binary_test", binary).unwrap();
            let item = lua.create_table().unwrap();
            item.set("value", "C:\\folder\\file.ps1:42:7:matching text")
                .unwrap();
            let output: mlua::Table = resolver.call((item, lua.create_table().unwrap())).unwrap();
            assert_eq!(output.get::<_, String>("text").unwrap(), "file content");
            assert_eq!(
                output.get::<_, String>("syntax").unwrap(),
                if binary { "PlainText" } else { "Auto" }
            );
            assert_eq!(
                output.get::<_, Option<String>>("extension").unwrap(),
                if binary { None } else { Some("ps1".into()) }
            );
            assert_eq!(
                output.get::<_, Option<usize>>("center_line").unwrap(),
                if binary { None } else { Some(42) }
            );
            assert_eq!(
                output.get::<_, Option<usize>>("highlight_line").unwrap(),
                if binary { None } else { Some(42) }
            );
        }
        let item = lua.create_table().unwrap();
        item.set("value", "not a ripgrep match").unwrap();
        assert!(matches!(
            resolver
                .call::<_, mlua::Value>((item, lua.create_table().unwrap()))
                .unwrap(),
            mlua::Value::Nil
        ));
    }
    #[test]
    fn lua_file_preview_samples_binary_files_and_preserves_utf8_boundaries() {
        let source = include_str!("../nterm.example.lua");
        let start = source.find("local function preview_path(").unwrap();
        let end = source[start..]
            .find("local function preview_directory(")
            .unwrap()
            + start;
        let lua = mlua::Lua::new();
        lua.globals()
            .set("preview", lua.create_table().unwrap())
            .unwrap();
        let reader: mlua::Function = lua
            .load(format!("{}\nreturn preview_read_text", &source[start..end]))
            .eval()
            .unwrap();
        let root =
            std::env::temp_dir().join(format!("ghostty-lua-binary-sample-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("large.dll");
        std::fs::File::create(&binary)
            .unwrap()
            .set_len(11 * 1024 * 1024)
            .unwrap();
        let (text, is_binary): (String, bool) = reader
            .call((binary.to_str().unwrap(), "large.dll"))
            .unwrap();
        assert!(is_binary);
        assert_eq!(
            text,
            format!("Binary file\nlarge.dll\n{} bytes", 11 * 1024 * 1024)
        );
        for (name, contents) in [
            ("boundary.txt", "a".repeat(4095) + "猫 text"),
            ("empty.txt", String::new()),
            ("normal.txt", "hello\nworld".into()),
        ] {
            let path = root.join(name);
            std::fs::write(&path, &contents).unwrap();
            let (text, is_binary): (String, bool) =
                reader.call((path.to_str().unwrap(), name)).unwrap();
            assert!(!is_binary, "{name}");
            assert_eq!(text, contents);
        }
        let large_text = root.join("large.txt");
        std::fs::write(&large_text, vec![b'a'; 10 * 1024 * 1024 + 1]).unwrap();
        let (text, binary): (String, bool) = reader
            .call((large_text.to_str().unwrap(), "large.txt"))
            .unwrap();
        assert!(!binary);
        assert!(text.starts_with("aaaa"));
        assert!(text.ends_with("[Preview truncated: file exceeds the 10 MiB text limit.]\n"));
        assert!(text.len() <= 10 * 1024 * 1024);
        std::fs::write(&large_text, "猫".repeat(4 * 1024 * 1024)).unwrap();
        let (text, binary): (String, bool) = reader
            .call((large_text.to_str().unwrap(), "large.txt"))
            .unwrap();
        assert!(!binary);
        assert!(text.starts_with("猫"));
        assert!(text.contains("Preview truncated:"));
        assert!(text.len() <= 10 * 1024 * 1024);
        // Parallel subprocess tests can retain inherited Windows file handles.
        if let Err(error) = std::fs::remove_dir_all(&root) {
            assert_eq!(error.raw_os_error(), Some(32), "fixture cleanup: {error}");
        }
    }
    #[cfg(windows)]
    #[test]
    fn lua_directory_items_preview_preserves_unicode_and_shell_characters() {
        let source = include_str!("../nterm.example.lua");
        let path_start = source.find("local function preview_path(").unwrap();
        let path_end = source.find("local function preview_read_bytes(").unwrap();
        let start = source.find("local function preview_directory(").unwrap();
        let end = source[start..]
            .find("local function file_preview(")
            .unwrap()
            + start;
        let lua = mlua::Lua::new();
        let preview = lua.create_table().unwrap();
        preview
            .set(
                "directory_items",
                lua.create_function(|lua, path: String| {
                    let items = crate::preview_worker::directory_items(
                        std::path::Path::new(&path),
                        4096,
                        || false,
                    )
                    .map_err(mlua::Error::external)?;
                    match items {
                        Some(items) => lua.to_value(&items),
                        None => Ok(mlua::Value::Nil),
                    }
                })
                .unwrap(),
            )
            .unwrap();
        lua.globals().set("preview", preview).unwrap();
        let reader: mlua::Function = lua
            .load(format!(
                "{}\n{}\nreturn preview_directory",
                &source[path_start..path_end],
                &source[start..end]
            ))
            .eval()
            .unwrap();
        let root =
            std::env::temp_dir().join(format!("ghostty-cmd-dir-{} % ! &", std::process::id()));
        std::fs::create_dir_all(root.join("folder 猫 🚀")).unwrap();
        std::fs::write(root.join("file % ! & 猫.txt"), "").unwrap();
        let entries: String = reader
            .call((root.to_str().unwrap(), mlua::Value::Nil))
            .unwrap();
        assert!(entries.contains("file % ! & 猫.txt"));
        assert!(entries.contains("folder 猫 🚀"));
        assert!(entries.contains("folder 猫 🚀/"));
        assert!(crate::preview_worker::directory_items(&root, 1, || false).is_err());
        assert!(crate::preview_worker::directory_items(&root, 4096, || true).is_err());
        assert!(entries.contains("\n"));
        let entries: String = reader
            .call(("folder 猫 🚀", root.to_str().unwrap()))
            .unwrap();
        assert!(!entries.is_empty());
        let entries: Option<String> = reader
            .call(("file % ! & 猫.txt", root.to_str().unwrap()))
            .unwrap();
        assert!(entries.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn picker_defaults_unique_is_inherited_and_overridden_in_static_and_factory_pickers() {
        for (global, global_value) in [
            ("", false),
            ("picker_defaults={unique=false},", false),
            ("picker_defaults={unique=true},", true),
        ] {
            for (local, override_value) in [
                ("", None),
                (", unique=false", Some(false)),
                (", unique=true", Some(true)),
            ] {
                for kind in ["Scrollback", "Command"] {
                    for factory in [false, true] {
                        let args = if kind == "Command" {
                            ", args={'history.exe'}"
                        } else {
                            ""
                        };
                        let definition = format!("{{type='{kind}'{args}{local}}}");
                        let definition = if factory {
                            format!("function(context) return {definition} end")
                        } else {
                            definition
                        };
                        let config =
                            parse(&format!("return {{{global}pickers={{x={definition}}}}}"))
                                .unwrap();
                        let bindings = crate::picker::config::Bindings::compile_with_defaults(
                            config.pickers,
                            config.keys,
                            config.picker_defaults,
                        )
                        .unwrap();
                        let options = bindings
                            .named("x")
                            .unwrap()
                            .resolve(&PickerContext::default())
                            .unwrap();
                        let unique = match options.source {
                            crate::picker::config::Source::Scrollback { unique, .. } => unique,
                            crate::picker::config::Source::Command(options) => options.unique,
                            _ => unreachable!(),
                        };
                        assert_eq!(
                            unique,
                            override_value.unwrap_or(global_value),
                            "{global} {local} {kind} factory={factory}"
                        );
                    }
                }
            }
        }
        let config = parse("return {picker_defaults={unique=true}}").unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_defaults(
            config.pickers,
            config.keys,
            config.picker_defaults,
        )
        .unwrap();
        assert!(matches!(
            bindings
                .named("builtin.lines")
                .unwrap()
                .resolve(&PickerContext::default())
                .unwrap()
                .source,
            crate::picker::config::Source::Scrollback { unique: true, .. }
        ));
        assert!(parse("return {picker_defaults={unique='yes'}}").is_err());
        assert!(parse("return {picker_defaults={unknown=true}}").is_err());
    }

    #[test]
    fn command_unique_accepts_lua_and_defaults_to_false() {
        for (field, expected) in [
            ("", false),
            (", unique=false", false),
            (", unique=true", true),
        ] {
            let config = parse(&format!("return {{pickers={{history={{type='Command', args={{'history.exe'}}, line_continuation='`'{field}}}}}}}"))
                .unwrap();
            let bindings = crate::picker::config::Bindings::compile_with_actions(
                config.pickers,
                config.keys,
                config.picker_defaults,
                config.actions,
            )
            .unwrap();
            let options = bindings
                .named("history")
                .unwrap()
                .resolve(&PickerContext::default())
                .unwrap();
            let crate::picker::config::Source::Command(options) = options.source else {
                panic!("expected command");
            };
            assert_eq!(options.unique, expected);
        }
    }

    #[test]
    fn scrollback_unique_accepts_lua_and_defaults_to_false() {
        for (field, expected) in [
            ("", false),
            (", unique=false", false),
            (", unique=true", true),
        ] {
            let config = parse(&format!(
                "return {{pickers={{x={{type='Scrollback'{field}}}}}}}"
            ))
            .unwrap();
            let bindings = crate::picker::config::Bindings::compile_with_actions(
                config.pickers,
                config.keys,
                config.picker_defaults,
                config.actions,
            )
            .unwrap();
            let options = bindings
                .named("x")
                .unwrap()
                .resolve(&PickerContext::default())
                .unwrap();
            let crate::picker::config::Source::Scrollback { unique, .. } = options.source else {
                panic!("expected scrollback");
            };
            assert_eq!(unique, expected);
        }
        assert!(parse("return {pickers={x={type='Items', unique=true}}}").is_err());
    }

    #[test]
    fn styled_scrollback_preview_requires_scrollback_source() {
        let config = parse(
            "return {pickers={lines={type='Scrollback', preview={type='Scrollback', rows=8}}}}",
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let options = bindings
            .named("lines")
            .unwrap()
            .resolve(&Default::default())
            .unwrap();
        assert!(matches!(
            options.preview,
            Some(crate::picker::config::Preview::Scrollback { rows: 8 })
        ));
        for source in [
            "return {pickers={x={type='FileSystem', preview={type='Scrollback'}}}}",
            "return {pickers={x={type='Scrollback', preview={type='Scrollback', rows=0}}}}",
        ] {
            let config = parse(source).unwrap();
            assert!(
                crate::picker::config::Bindings::compile_with_actions(
                    config.pickers,
                    config.keys,
                    config.picker_defaults,
                    config.actions
                )
                .is_err()
            );
        }
    }

    #[test]
    fn function_picker_uses_cursor_context_and_validates_returned_schema() {
        let config = parse(
            r#"return {
            pickers = {
                dynamic = function(context)
                    if context.line_before_cursor == 'rg test' then
                        return { type = 'Command', args = {'rg', '{query}'},
                            initial_query = 'test', enter_action = 'SendInput' }
                    end
                    return { type = 'Scrollback', regex = 'word', initial_query = context.line }
                end,
            },
            keys = {{ key = 'p', mods = 'ALT', picker = 'dynamic' }},
        }"#,
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let entry = bindings.named("dynamic").unwrap();
        assert!(entry.needs_context());
        let command = entry
            .resolve(&PickerContext {
                line: "rg test".into(),
                line_before_cursor: "rg test".into(),
                cursor_column: 7,
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(
            command.source,
            crate::picker::config::Source::Command(_)
        ));
        assert_eq!(command.params["query"], "test");
        let scrollback = entry
            .resolve(&PickerContext {
                line: "other".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(
            scrollback.source,
            crate::picker::config::Source::Scrollback { .. }
        ));
        assert_eq!(scrollback.params["query"], "other");
        let config =
            parse("return {pickers={bad=function() return {type='FileSystem', regex='x'} end}}")
                .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        assert!(
            bindings
                .named("bad")
                .unwrap()
                .resolve(&PickerContext::default())
                .is_err()
        );
        let config = parse("local picker={type='Items', action=function() end}; return {pickers={reused=function() return picker end}}") .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let reused = bindings.named("reused").unwrap();
        for _ in 0..2 {
            assert!(
                reused
                    .resolve(&PickerContext::default())
                    .unwrap()
                    .callback
                    .is_some()
            );
        }
    }

    #[test]
    fn function_picker_ignores_text_after_cursor() {
        let config = parse(
            r#"return { pickers = { words = function(context)
            local before = context.line_before_cursor:match('(%S+)$') or ''
            return { type = 'Scrollback', regex = [[\S+]], initial_query = before,
                action = function(app, item)
                    app:replace_prompt(item.value, utf8.len(before) or 0, 0)
                end }
        end } }"#,
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let options = bindings
            .named("words")
            .unwrap()
            .resolve(&PickerContext {
                line: "PS C:\\src> foobar ghost suggestion".into(),
                line_before_cursor: "PS C:\\src> foo".into(),
                cursor_column: 14,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(options.params["query"], "foo");
        let commands = options
            .callback
            .unwrap()
            .call(
                "replacement",
                "replacement",
                false,
                None,
                None,
                &Default::default(),
            )
            .unwrap();
        assert!(
            matches!(&commands[0], crate::lua_callbacks::Command::ReplacePrompt { text, before: 3, after: 0 } if text == "replacement")
        );
    }

    #[test]
    fn config_discovery_uses_first_file_and_only_falls_back_when_missing() {
        let explicit = Path::new("argument.lua");
        let defaults = vec![
            PathBuf::from("home/nterm.lua"),
            PathBuf::from("exe/nterm.lua"),
        ];
        let loaded = read_config(Some(explicit), &defaults, |path| {
            Ok(path.display().to_string())
        })
        .unwrap()
        .unwrap();
        assert_eq!(loaded.0, explicit);
        let loaded = read_config(None, &defaults, |path| Ok(path.display().to_string()))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.0, defaults[0]);
        let loaded = read_config(None, &defaults, |path| {
            if path == defaults[0] {
                Err(std::io::ErrorKind::NotFound.into())
            } else {
                Ok("return {}".into())
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(loaded.0, defaults[1]);
        assert!(
            read_config(
                None,
                &defaults,
                |_| Err(std::io::ErrorKind::NotFound.into())
            )
            .unwrap()
            .is_none()
        );
        assert!(
            read_config(Some(explicit), &defaults, |_| Err(
                std::io::ErrorKind::NotFound.into()
            ))
            .is_err()
        );
        assert!(
            read_config(None, &defaults, |_| Err(
                std::io::ErrorKind::PermissionDenied.into()
            ))
            .is_err()
        );
    }

    #[test]
    fn lua_settings_translate_to_ghostty_options() {
        let config = parse(
            r#"local size = 6 * 2
            return { font = { family = "JetBrains Mono", size = size },
                terminal = { scrollback_limit = 0, command = "pwsh.exe" },
                colors = { theme = "Gruvbox Dark" } }"#,
        )
        .unwrap();
        assert_eq!(
            config.ghostty_args().unwrap(),
            [
                "--font-family=",
                "--font-family=JetBrains Mono",
                "--font-size=12",
                "--scrollback-limit-bytes=0",
                "--command=pwsh.exe",
                "--theme=Gruvbox Dark"
            ]
        );
        assert!(
            parse("return {}")
                .unwrap()
                .ghostty_args()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn floating_padding_accepts_asymmetric_percentages_and_rejects_invalid_sizes() {
        let config = parse(
            "return { floating = { padding = { top = 10, bottom = 20, left = 5, right = 15 } } }",
        )
        .unwrap();
        assert_eq!(
            config.floating.padding.unwrap().validate().unwrap(),
            FloatingPadding {
                top: 10.0,
                bottom: 20.0,
                left: 5.0,
                right: 15.0,
            }
        );
        assert!(
            parse("return { floating = { padding = { left = -1 } } }")
                .unwrap()
                .floating
                .padding
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(
            parse("return { floating = { padding = { left = 50, right = 45 } } }")
                .unwrap()
                .floating
                .padding
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(
            parse("return { floating = { padding = { top = math.huge } } }")
                .unwrap()
                .floating
                .padding
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(parse("return { floating = { padding = { width = 10 } } }").is_err());
    }

    #[test]
    fn rejects_invalid_config_with_source_location() {
        assert!(parse("return {font = {szie = 12}}").is_err());
        assert!(parse("return {terminal = {scrollback_limit = -1}}").is_err());
        assert!(parse("return 12").is_err());
        assert!(parse("return {").err().unwrap().contains("test-config.lua"));
        for source in [
            "return {font = {size = 0}}",
            "return {font = {size = math.huge}}",
            "return {font = {family = ''}}",
            "return {terminal = {command = ''}}",
        ] {
            assert!(parse(source).unwrap().ghostty_args().is_err());
        }
    }

    #[test]
    fn explicit_path_is_removed_and_cli_overrides_precede_child_arguments() {
        let path = std::env::temp_dir().join(format!("nterm-config-{}.lua", std::process::id()));
        std::fs::write(&path, "return {font = {family = 'Lua Font', size = 12}}").unwrap();
        let result = prepare_args(vec![
            "app".into(),
            "--lua-config".into(),
            path.to_string_lossy().into_owned(),
            "--font-family=CLI Font".into(),
            "--font-size=18".into(),
            "-e".into(),
            "child".into(),
            "--lua-config=child-option".into(),
        ]);
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            result.unwrap(),
            [
                "app",
                "--copy-on-select=none",
                "--right-click-action=copy",
                "--selection-clear-on-typing=false",
                "--font-family=",
                "--font-family=Lua Font",
                "--font-size=12",
                "--font-family=",
                "--font-family=CLI Font",
                "--font-size=18",
                "-e",
                "child",
                "--lua-config=child-option"
            ]
        );
    }

    #[test]
    fn picker_factories_receive_shell_cwd() {
        let config = parse(
            "return { pickers = { here = function(context) return { type = 'Items', initial_query = context.cwd or 'none', items = { { text = 'x', value = 'x' } } } end } }",
        )
        .unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        for (cwd, expected) in [
            (Some("/home/user/project"), "/home/user/project"),
            (None, "none"),
        ] {
            let options = bindings
                .named("here")
                .unwrap()
                .resolve(&PickerContext {
                    cwd: cwd.map(str::to_owned),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(options.params["query"], expected);
        }
    }

    fn compile(config: AppConfig) -> crate::picker::config::Bindings {
        crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap()
    }

    #[test]
    fn lua_modules_load_only_from_absolute_directories() {
        let config = std::env::temp_dir().join("nterm.lua");
        let path = crate::preview_worker::module_path(&config);
        let templates: Vec<_> = path.split(';').collect();
        // The config's own directory comes first, so it can override bundled modules.
        assert_eq!(
            Path::new(templates[0]),
            std::env::temp_dir().join("?.lua").as_path()
        );
        assert!(templates.iter().all(|t| Path::new(t).is_absolute()));
        let lua = Lua::new();
        crate::preview_worker::set_module_path(&lua, &path).unwrap();
        let (search, native): (String, String) = lua
            .load("return package.path, package.cpath")
            .eval()
            .unwrap();
        assert_eq!(search, path);
        assert_eq!(native, "");
    }

    #[test]
    fn config_requires_modules_from_its_directory() {
        let directory =
            std::env::temp_dir().join(format!("ghostty-lua-modules-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("extra.lua"),
            "return { picker = { type = 'Items', items = { { text = 'module', value = 'x' } } } }",
        )
        .unwrap();
        let config = AppConfig::parse(
            "return { pickers = { extra = require('extra').picker } }",
            &directory.join("nterm.lua"),
        );
        std::fs::remove_dir_all(&directory).unwrap();
        let bindings = compile(config.unwrap());
        assert!(bindings.named("extra").is_some());
        // Lua's default ./?.lua is gone, so a missing module is an error.
        assert!(parse("return { x = require('extra') }").is_err());
    }

    #[test]
    fn bundled_git_module_quotes_and_parses_and_builds_pickers() {
        let config = parse(
            r#"
            local git = require('nterm.git')
            local posix = { quote = git.quote.posix }
            assert(git.quote.posix('feature/x') == 'feature/x')
            assert(git.quote.posix('$(touch${IFS}x)') == "'$(touch${IFS}x)'")
            assert(git.quote.posix("it's") == [['it'\''s']])
            assert(git.quote.powershell("it's") == "'it''s'")
            assert(git.quote.powershell('a\u{2019}b') == "'a\u{2019}\u{2019}b'")
            assert(git.switch_command(' \torigin/feat/x\tremote\t2026-01-01\tA\tS', posix)
              == 'git switch feat/x')
            assert(git.switch_command("*\tit's\tlocal\t2026-01-01\tA\tS",
              { quote = git.quote.posix, require_clean = true })
              == [[git diff --stat --exit-code HEAD && git switch 'it'\''s']])
            assert(git.switch_command('not a branch row') == nil)
            assert(git.unquote_path('plain name.txt') == 'plain name.txt')
            assert(git.unquote_path('"a\\tb \\"q\\" \\\\"') == 'a\tb "q" \\')
            assert(git.unquote_path('"\\303\\251.txt"') == '\u{e9}.txt')
            local code, path = git.status_entry('R  old.txt -> "new\\tname.txt"')
            assert(code == 'R ' and path == 'new\tname.txt')
            code, path = git.status_entry('?? "new file.txt"')
            assert(code == '??' and path == 'new file.txt')
            code, path = git.status_entry(' M src/app.rs')
            assert(code == ' M' and path == 'src/app.rs')
            local prompt = '^%$ '
            return { pickers = {
              commits = git.commits(),
              branches = git.branches({ prompt = prompt }),
              status = git.status({ prompt = prompt }),
              cherry_pick = git.cherry_pick(),
              cherry_pick_commits = git.cherry_pick_commits({ prompt = prompt }),
              stashes = git.stashes({ prompt = prompt }),
              reflog = git.reflog(),
              file_history = git.file_history({ prompt = prompt }),
              file_history_commits = git.file_history_commits(),
              fixups = git.fixups({ prompt = prompt }),
              tags = git.tags({ prompt = prompt }),
            } }
            "#,
        )
        .unwrap();
        let bindings = compile(config);
        let Some(repository) = git_fixture() else {
            return;
        };
        let context = PickerContext {
            line: "$ git log b.txt".into(),
            line_before_cursor: "$ git log b.txt".into(),
            ..Default::default()
        };
        // Picker, column count (None: plain lines) and the expected rows.
        let expected: [(&str, Option<usize>, &[&str]); 11] = [
            (
                "commits",
                Some(5),
                &["second on main", "feature two", "feature one", "first"],
            ),
            ("branches", Some(6), &["main", "feature/x", "base"]),
            (
                "status",
                None,
                &[" M a.txt", "A  d.txt", "?? \"new file.txt\""],
            ),
            ("cherry_pick", Some(6), &["main", "feature/x", "base"]),
            // "feature one" is already on main, so only "feature two" is offered.
            ("cherry_pick_commits", Some(4), &["feature two"]),
            ("stashes", Some(3), &["work in progress"]),
            ("reflog", Some(4), &["HEAD@{0}"]),
            // Tracked files, including the staged d.txt but not untracked ones.
            ("file_history", None, &["a.txt", "b.txt", "d.txt"]),
            ("file_history_commits", Some(4), &["second on main"]),
            ("fixups", Some(4), &["second on main"]),
            ("tags", Some(3), &["v1\t"]),
        ];
        let params = std::collections::BTreeMap::from([
            ("range".to_owned(), "HEAD...feature/x".to_owned()),
            ("path".to_owned(), "b.txt".to_owned()),
        ]);
        for (name, columns, rows) in expected {
            let options = bindings.named(name).unwrap().resolve(&context).unwrap();
            if name == "file_history" {
                // The word at the cursor starts the file search.
                assert_eq!(options.params["query"], "b.txt");
            }
            let crate::picker::config::Source::Command(command) = &options.source else {
                panic!("{name} should be a Command picker");
            };
            assert_eq!(command.args[0], "git", "{name} runs git directly");
            assert_eq!(
                command.columns.as_ref().map(|c| c.names.len()),
                columns,
                "{name}"
            );
            let args: Vec<_> = command
                .args
                .iter()
                .map(
                    |arg| match arg.strip_prefix('{').and_then(|a| a.strip_suffix('}')) {
                        Some(key) => params[key].clone(),
                        None => arg.clone(),
                    },
                )
                .collect();
            let output = std::process::Command::new(&args[0])
                .args(&args[1..])
                .current_dir(&repository)
                .output()
                .unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(
                output.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let lines: Vec<_> = stdout.lines().filter(|line| !line.is_empty()).collect();
            if name != "reflog" {
                assert_eq!(lines.len(), rows.len(), "{name}: {stdout}");
            }
            // The fixture's commits share a timestamp, so order is not checked.
            for row in rows {
                assert!(
                    lines.iter().any(|line| line.contains(row)),
                    "{name}: no row contains {row:?} in {stdout}"
                );
            }
            for line in &lines {
                if let Some(columns) = columns {
                    assert_eq!(line.split('\t').count(), columns, "{name}: {line:?}");
                }
                assert!(!line.contains("refs/"), "{name}: {line:?}");
            }
        }
        std::fs::remove_dir_all(&repository).unwrap();
    }

    /// A scratch repository: main with a commit cherry-picked from feature/x,
    /// main tracking base, a tag, a stash, and staged, modified and untracked
    /// files. None when git is not installed.
    fn git_fixture() -> Option<PathBuf> {
        let directory =
            std::env::temp_dir().join(format!("ghostty-git-module-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args([
                    "-c",
                    "advice.detachedHead=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .current_dir(&directory)
                .output()
        };
        if git(&["init", "-q"]).is_err() {
            eprintln!("git is not installed; skipping the output check");
            return None;
        }
        let write = |name: &str, text: &str| std::fs::write(directory.join(name), text).unwrap();
        let run = |args: &[&str]| {
            let output = git(args).unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        write("a.txt", "one\n");
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "first"]);
        run(&["branch", "base"]);
        run(&["switch", "-q", "-c", "feature/x"]);
        write("b.txt", "b\n");
        run(&["add", "b.txt"]);
        run(&["commit", "-q", "-m", "feature one"]);
        write("c.txt", "c\n");
        run(&["add", "c.txt"]);
        run(&["commit", "-q", "-m", "feature two"]);
        run(&["switch", "-q", "main"]);
        // Same change as "feature one", so --cherry-pick hides it.
        write("b.txt", "b\n");
        run(&["add", "b.txt"]);
        run(&["commit", "-q", "-m", "second on main"]);
        run(&["tag", "-a", "v1", "-m", "release one"]);
        run(&["branch", "--set-upstream-to=base"]);
        write("a.txt", "stashed\n");
        run(&["stash", "push", "-q", "-m", "work in progress"]);
        write("a.txt", "changed\n");
        write("d.txt", "d\n");
        run(&["add", "d.txt"]);
        write("new file.txt", "untracked\n");
        Some(directory)
    }
}

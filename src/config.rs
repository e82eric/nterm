use mlua::{Lua, LuaSerdeExt};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct PickerContext {
    pub(crate) line: String,
    pub(crate) line_before_cursor: String,
    pub(crate) cursor_column: u32,
    pub(crate) recent_lines: Vec<String>,
}

pub(crate) struct PickerFactory {
    lua: std::rc::Rc<Lua>,
    key: mlua::RegistryKey,
    name: String,
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
        let table: mlua::Table = function
            .call(argument)
            .map_err(|e| format!("pickers.{}: {e}", self.name))?;
        let definition = parse_picker_table(&self.lua, &self.name, &table)?;
        if matches!(
            definition,
            crate::picker::config::Definition::Factory { .. }
        ) {
            return Err(format!(
                "pickers.{}: factory must return a picker table",
                self.name
            ));
        }
        let bindings = crate::picker::config::Bindings::compile_with_defaults(
            Some(std::collections::BTreeMap::from([(
                self.name.clone(),
                definition,
            )])),
            Some(vec![]),
            defaults,
        )?;
        match bindings.named(&self.name) {
            Some(crate::picker::config::PickerEntry::Static(options)) => Ok(options),
            _ => Err(format!("pickers.{}: invalid picker definition", self.name)),
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
        callback = Some(std::rc::Rc::new(crate::picker::action::Callback {
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
        provider = Some(std::rc::Rc::new(crate::picker::action::ItemsProvider {
            lua: lua.clone(),
            key,
        }));
    }
    let mut preview_command = None;
    if let Some(preview) = definition
        .raw_get::<_, Option<mlua::Table>>("preview")
        .map_err(|e| e.to_string())?
    {
        let command: mlua::Value = preview.raw_get("command").map_err(|e| e.to_string())?;
        if !matches!(command, mlua::Value::Nil) {
            if preview
                .raw_get::<_, String>("type")
                .map_err(|e| e.to_string())?
                != "Command"
            {
                return Err(format!(
                    "pickers.{name}.preview.command requires type Command"
                ));
            }
            let mlua::Value::Function(function) = command else {
                return Err(format!("pickers.{name}.preview.command must be a function"));
            };
            let key = lua
                .create_registry_value(function)
                .map_err(|e| e.to_string())?;
            let copy_preview = lua.create_table().map_err(|e| e.to_string())?;
            for pair in preview.pairs::<mlua::Value, mlua::Value>() {
                let (key, value) = pair.map_err(|e| e.to_string())?;
                copy_preview
                    .raw_set(key, value)
                    .map_err(|e| e.to_string())?;
            }
            copy_preview
                .raw_set("command", mlua::Value::Nil)
                .map_err(|e| e.to_string())?;
            definition
                .raw_set("preview", copy_preview)
                .map_err(|e| e.to_string())?;
            preview_command = Some(std::rc::Rc::new(crate::picker::action::PreviewCommand {
                lua: lua.clone(),
                key,
                name: name.into(),
            }));
        }
    }
    let mut parsed: crate::picker::config::Definition = lua
        .from_value(mlua::Value::Table(definition.clone()))
        .map_err(|e| format!("pickers.{name}: {e}"))?;
    if let Some(callback) = callback {
        parsed.set_callback(callback);
    }
    if let Some(provider) = provider {
        parsed.set_items_provider(provider);
    }
    if let Some(command) = preview_command {
        parsed.set_preview_command(command);
    }
    Ok(parsed)
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AppConfig {
    font: FontConfig,
    terminal: TerminalConfig,
    colors: ColorConfig,
    theme_colors: std::collections::BTreeMap<String, crate::colors::UiOverrides>,
    floating: FloatingConfig,
    picker_defaults: crate::picker::config::PickerDefaults,
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
        let ghostty = lua.create_table().map_err(|error| error.to_string())?;
        ghostty
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
        ghostty
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
            .set("ghostty", ghostty)
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
        let mut parsed_pickers = None;
        let mut actions = std::collections::BTreeMap::new();
        if let mlua::Value::Table(root) = &value {
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
                            callback: std::rc::Rc::new(crate::picker::action::Callback {
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
                            crate::picker::config::Definition::Factory {
                                factory: Some(std::rc::Rc::new(PickerFactory {
                                    lua: lua.clone(),
                                    key,
                                    name: name.clone(),
                                })),
                            }
                        }
                        _ => return Err(format!("pickers.{name} must be a table or function")),
                    };
                    definitions.insert(name, definition);
                }
                parsed_pickers = Some(definitions);
                root.raw_set("pickers", mlua::Value::Nil)
                    .map_err(|e| e.to_string())?;
            }
        }
        let mut config: Self = lua.from_value(value).map_err(|error| error.to_string())?;
        config.pickers = parsed_pickers;
        config.actions = actions;
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
        paths.push(PathBuf::from(home).join("ghostty.lua"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(directory) = exe.parent() {
            paths.push(directory.join("ghostty.lua"));
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

pub(crate) fn prepare(
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
    let mut incoming = args.into_iter();
    let program = incoming.next().unwrap_or_else(|| "ghostty_egui".into());
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
    let loaded = (|| {
        if let Some((path, source)) = read_config(explicit.as_deref(), &default_paths(), |path| {
            std::fs::read_to_string(path)
        })? {
            let (options, bindings, padding, overrides) = AppConfig::parse(&source, &path)
                .and_then(|config| {
                    Ok((
                        config.ghostty_args()?,
                        crate::picker::config::Bindings::compile_with_actions(
                            config.pickers,
                            config.keys,
                            config.picker_defaults,
                            config.actions,
                        )?,
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
    prepared.extend(remaining);
    Ok((prepared, pickers, floating_padding, ui_colors))
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
            matches!(&commands[0], crate::picker::action::Command::ShowToast { title, message, duration_ms: 1500 } if title == "Saved" && message == "selected text")
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
        let sample = include_str!("../ghostty.lua");
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
            matches!(&commands[0], crate::picker::action::Command::ShowToast { title, .. } if title == "Snippet saved")
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
            matches!(&commands[0], crate::picker::action::Command::ShowToast { message, .. } if message == "No text is selected")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, "invalid JSON").unwrap();
        let commands = callback.call_action("keep selection".into()).unwrap();
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0], crate::picker::action::Command::ShowToast { message, .. } if message.contains("Unable to parse"))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid JSON");
        std::fs::remove_file(&path).unwrap();
        let commands = callback.call_action("keep selection".into()).unwrap();
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0], crate::picker::action::Command::ShowToast { message, .. } if message.contains("Unable to read"))
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
            matches!(&callback.call("file.txt", "file.txt", true, None, None, &Default::default()).unwrap()[0], crate::picker::action::Command::Paste(text) if text == "file.txt")
        );
        assert!(parse("return { pickers = { files = { action = 'invalid' } } }").is_err());
    }

    #[test]
    fn sample_completion_callback_sends_edit_sequence_then_pastes_value() {
        let config = parse(include_str!("../ghostty.lua")).unwrap();
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
            matches!(&commands[0], crate::picker::action::Command::SendText(text) if text == "\x08\x08\x1b[3~")
        );
        assert!(
            matches!(&commands[1], crate::picker::action::Command::Paste(text) if text == "inserted")
        );
    }

    #[test]
    fn sample_codex_picker_uses_terminal_context_and_replaces_unicode_input() {
        let config = parse(include_str!("../ghostty.lua")).unwrap();
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
            matches!(&actions[0], crate::picker::action::Command::ReplacePrompt { text, before: 10, after: 0 } if text == "echo done")
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
        let config = parse(include_str!("../ghostty.lua")).unwrap();
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
            matches!(&commands[0], crate::picker::action::Command::SendText(text) if text == "\x08\x08\x1b[3~\x1b[3~")
        );
        assert!(
            matches!(&commands[1], crate::picker::action::Command::SendText(text) if text == "nvim +42 'C:\\it''s here.txt'\r")
        );
    }
    use super::*;

    fn parse(source: &str) -> Result<AppConfig, String> {
        AppConfig::parse(source, Path::new("test-config.lua"))
    }

    #[test]
    fn named_pickers_compile_and_reject_bad_bindings_and_regexes() {
        let example = parse(include_str!("../ghostty.lua")).unwrap();
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
            "return {pickers={x={type='Command', args={'cmd'}, result_rows=31}}}",
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
        assert!(bindings.named("lines").is_some());
        assert!(bindings.named("missing").is_none());
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
    fn ripgrep_osc_request_resolves_to_command_picker() {
        let config = parse(include_str!("../ghostty.lua")).unwrap();
        let bindings = crate::picker::config::Bindings::compile_with_actions(
            config.pickers,
            config.keys,
            config.picker_defaults,
            config.actions,
        )
        .unwrap();
        let payload = br#"StreamingPicker;secret;{"name":"Ripgrep","params":{"pattern":"ghostty","cwd":"C:\\src\\ghostty2","cursor":"0","length":"0"}}"#;
        let request = crate::picker::protocol::parse(payload, "secret").unwrap();
        let options = bindings
            .named(&request.name)
            .unwrap()
            .resolve(&PickerContext::default())
            .unwrap()
            .with_request(request.params)
            .unwrap();
        assert!(matches!(
            options.source,
            crate::picker::config::Source::Command(_)
        ));
        assert_eq!(options.params["pattern"], "ghostty");
    }

    #[test]
    fn filesystem_osc_request_overrides_configured_roots() {
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
        let payload = br#"StreamingPicker;secret;{"name":"files","params":{"roots":"[\"G:\\\\src\",\"C:\\\\src\"]"}}"#;
        let request = crate::picker::protocol::parse(payload, "secret").unwrap();
        let options = bindings
            .named(&request.name)
            .unwrap()
            .resolve(&PickerContext::default())
            .unwrap()
            .with_request(request.params)
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
    fn lua_items_provider_rereads_file_each_time() {
        let path =
            std::env::temp_dir().join(format!("ghostty-picker-items-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"snippets":[{"input":"first"}]}"#).unwrap();
        let source = format!(
            r#"return {{ pickers = {{ snippets = {{ type = 'Items', items = function()
                local file = assert(io.open([[{}]], 'rb'))
                local data = ghostty.json_parse(file:read('*a'))
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
            config.pickers.as_ref().unwrap()["x"],
            crate::picker::config::Definition::Scrollback { .. }
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
    fn preview_command_callback_selects_file_or_directory_command() {
        let config = parse(
            r#"return { pickers = { files = { type='FileSystem',
            preview={type='Command', command=function(item, params)
                if item.is_directory then
                    return {args={'dir', item.path}, cwd=params.cwd}
                end
                return {args={'bat', item.path}, center_line=42}
            end}
        } } }"#,
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
            .named("files")
            .unwrap()
            .resolve(&Default::default())
            .unwrap();
        let Some(crate::picker::config::Preview::Command {
            command: Some(command),
            ..
        }) = options.preview
        else {
            panic!("expected callback preview");
        };
        let params = [("cwd".into(), "C:/work".into())].into();
        let directory = std::env::temp_dir();
        let directory = command
            .call(directory.to_str().unwrap(), true, &params)
            .unwrap()
            .unwrap();
        assert_eq!(directory.args[0], "dir");
        assert_eq!(directory.cwd.as_deref(), Some("C:/work"));
        let file = command
            .call("missing-file.txt", true, &params)
            .unwrap()
            .unwrap();
        assert_eq!(file.args, ["bat", "missing-file.txt"]);
        assert_eq!(file.center_line, Some(42));
        assert_eq!(directory.center_line, None);
    }

    #[test]
    fn preview_command_callback_rejects_invalid_results() {
        for result in [
            "{args={}}",
            "{args={'bat'}, extra=true}",
            "{args={'bat', '\\0'}}",
            "{args={'bat'}, center_line=-1}",
            "{args={'bat'}, center_line=1.5}",
        ] {
            let source = format!(
                "return {{pickers={{x={{preview={{type='Command', command=function() return {result} end}}}}}}}}"
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
                .named("x")
                .unwrap()
                .resolve(&Default::default())
                .unwrap();
            let Some(crate::picker::config::Preview::Command {
                command: Some(command),
                ..
            }) = options.preview
            else {
                panic!("expected callback preview");
            };
            assert!(command.call("file", false, &Default::default()).is_err());
        }
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
                .named("lines")
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
            matches!(&commands[0], crate::picker::action::Command::ReplacePrompt { text, before: 3, after: 0 } if text == "replacement")
        );
    }

    #[test]
    fn config_discovery_uses_first_file_and_only_falls_back_when_missing() {
        let explicit = Path::new("argument.lua");
        let defaults = vec![
            PathBuf::from("home/ghostty.lua"),
            PathBuf::from("exe/ghostty.lua"),
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
        let path =
            std::env::temp_dir().join(format!("ghostty-egui-config-{}.lua", std::process::id()));
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
}

use mlua::{Lua, LuaSerdeExt};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AppConfig {
    font: FontConfig,
    terminal: TerminalConfig,
    colors: ColorConfig,
    pickers: Option<std::collections::BTreeMap<String, crate::picker_config::Definition>>,
    keys: Option<Vec<crate::picker_config::KeyBinding>>,
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
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ColorConfig {
    theme: Option<String>,
}

impl AppConfig {
    fn parse(source: &str, path: &Path) -> Result<Self, String> {
        let lua = Lua::new();
        let name = format!("@{}", path.display());
        let value: mlua::Value = lua
            .load(source)
            .set_name(name)
            .eval()
            .map_err(|error| error.to_string())?;
        if !matches!(value, mlua::Value::Table(_)) {
            return Err("config must return a table".into());
        }
        lua.from_value(value).map_err(|error| error.to_string())
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
        paths.push(PathBuf::from(home).join("config.lua"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(directory) = exe.parent() {
            paths.push(directory.join("config.lua"));
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
) -> Result<(Vec<String>, crate::picker_config::Bindings), String> {
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
    let mut prepared = vec![program];
    let mut pickers = crate::picker_config::Bindings::default();
    if let Some((path, source)) = read_config(explicit.as_deref(), &default_paths(), |path| {
        std::fs::read_to_string(path)
    })? {
        let (options, bindings) = AppConfig::parse(&source, &path)
            .and_then(|config| {
                Ok((
                    config.ghostty_args()?,
                    crate::picker_config::Bindings::compile(config.pickers, config.keys)?,
                ))
            })
            .map_err(|error| format!("{}: {error}", path.display()))?;
        prepared.extend(options);
        pickers = bindings;
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
    Ok((prepared, pickers))
}

#[cfg(test)]
fn prepare_args(args: Vec<String>) -> Result<Vec<String>, String> {
    prepare(args).map(|(args, _)| args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Result<AppConfig, String> {
        AppConfig::parse(source, Path::new("test-config.lua"))
    }

    #[test]
    fn named_pickers_compile_and_reject_bad_bindings_and_regexes() {
        let example = parse(include_str!("../config.lua")).unwrap();
        assert!(crate::picker_config::Bindings::compile(example.pickers, example.keys).is_ok());
        let config = parse(r#"return {pickers = {paths = {type = 'Scrollback', regex = '\\S+', enter_action = 'SendInput'}}, keys = {{key = 'p', mods = 'CTRL|SHIFT', picker = 'paths'}}}"#).unwrap();
        let bindings =
            crate::picker_config::Bindings::compile(config.pickers, config.keys).unwrap();
        let mut events = vec![egui::Event::Key {
            key: egui::Key::P,
            physical_key: Some(egui::Key::P),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        }];
        let (options, consumed) = bindings.consume(&mut events);
        assert!(consumed && events.is_empty());
        let options = options.unwrap();
        assert!(matches!(
            options.enter_action,
            crate::picker_config::EnterAction::SendInput
        ));
        assert_eq!(options.regex.find("x y").unwrap().as_str(), "x");
        for source in [
            "return {pickers={x={regex='['}}}",
            "return {pickers={x={regex='a*'}}}",
            "return {keys={{key='p', picker='missing'}}}",
            "return {keys={{key='f', mods='BAD', picker='lines'}}}",
            "return {keys={{key='/', mods='CTRL|SHIFT', picker='lines'}}}",
            "return {keys={{key='p', picker='lines'}, {key='p', picker='lines'}}}",
        ] {
            let config = parse(source).unwrap();
            assert!(
                crate::picker_config::Bindings::compile(config.pickers, config.keys).is_err(),
                "{source}"
            );
        }
        assert!(parse("return {pickers={x={type='Command'}}}").is_err());
        assert!(parse("return {pickers={x={enter_action='Bad'}}}").is_err());
        let empty = parse("return {keys={}}").unwrap();
        let bindings = crate::picker_config::Bindings::compile(empty.pickers, empty.keys).unwrap();
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
    fn config_discovery_uses_first_file_and_only_falls_back_when_missing() {
        let explicit = Path::new("argument.lua");
        let defaults = vec![
            PathBuf::from("home/config.lua"),
            PathBuf::from("exe/config.lua"),
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
                terminal = { scrollback_limit = 0 }, colors = { theme = "Gruvbox Dark" } }"#,
        )
        .unwrap();
        assert_eq!(
            config.ghostty_args().unwrap(),
            [
                "--font-family=",
                "--font-family=JetBrains Mono",
                "--font-size=12",
                "--scrollback-limit-bytes=0",
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
    fn rejects_invalid_config_with_source_location() {
        assert!(parse("return {font = {szie = 12}}").is_err());
        assert!(parse("return {terminal = {scrollback_limit = -1}}").is_err());
        assert!(parse("return 12").is_err());
        assert!(parse("return {").err().unwrap().contains("test-config.lua"));
        for source in [
            "return {font = {size = 0}}",
            "return {font = {size = math.huge}}",
            "return {font = {family = ''}}",
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

//! Keybinding parsing shared by application shortcuts and picker-local Lua actions.
use crate::picker::config::{BindingTarget, PickerKey};
use mlua::{Lua, Table};
use serde::Deserialize;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingTrigger {
    Key(egui::Key),
    DoubleClick,
}
impl BindingTrigger {
    pub(crate) fn label(self, mods: egui::Modifiers) -> String {
        match self {
            Self::Key(key) => crate::actions::shortcut_label(key, mods),
            Self::DoubleClick => "Double click".into(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct KeyBinding<A = BindingTarget> {
    pub(crate) trigger: BindingTrigger,
    pub(crate) mods: egui::Modifiers,
    pub(crate) description: String,
    pub(crate) action: A,
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
            builtin_picker: Option<String>,
            quick_select: Option<String>,
            action: Option<String>,
        }
        let raw = RawBinding::deserialize(deserializer)?;
        let target = crate::config::keybindings::application_target(
            raw.action,
            raw.picker,
            raw.builtin_picker,
            raw.quick_select,
        )
        .map_err(serde::de::Error::custom)?;
        Ok(Self {
            trigger: BindingTrigger::Key(parse_key(&raw.key).ok_or_else(|| {
                serde::de::Error::custom(format!("unknown key binding key {:?}", raw.key))
            })?),
            mods: parse_modifiers(&raw.mods).map_err(|part| {
                serde::de::Error::custom(format!("unknown key binding modifier {part:?}"))
            })?,
            description: String::new(),
            action: target,
        })
    }
}

pub(crate) fn normalize_key_name(name: &str) -> String {
    if name.len() == 1 && name.is_ascii() {
        name.to_uppercase()
    } else {
        name.to_owned()
    }
}

pub(crate) fn parse_key(name: &str) -> Option<egui::Key> {
    egui::Key::from_name(&normalize_key_name(name))
}

pub(crate) fn parse_modifiers(value: &str) -> Result<egui::Modifiers, &str> {
    let mut mods = egui::Modifiers::NONE;
    for part in value.split('|').filter(|part| !part.trim().is_empty()) {
        match part.trim().to_uppercase().as_str() {
            "CTRL" => mods.ctrl = true,
            "SHIFT" => mods.shift = true,
            "ALT" => mods.alt = true,
            _ => return Err(part),
        }
    }
    Ok(mods)
}

struct ParsedBinding {
    trigger: BindingTrigger,
    mods: egui::Modifiers,
    description: Option<String>,
}

enum ParsedAction {
    Named(String),
    Lua(Rc<crate::lua_callbacks::Callback>),
    None,
}

fn parse_action(
    lua: &Rc<Lua>,
    context: &str,
    value: mlua::Value<'_>,
) -> Result<ParsedAction, String> {
    match value {
        mlua::Value::Nil => Ok(ParsedAction::None),
        mlua::Value::String(value) => Ok(ParsedAction::Named(
            value
                .to_str()
                .map_err(|e| format!("{context}: {e}"))?
                .into(),
        )),
        mlua::Value::Function(function) => {
            Ok(ParsedAction::Lua(Rc::new(crate::lua_callbacks::Callback {
                lua: lua.clone(),
                key: lua
                    .create_registry_value(function)
                    .map_err(|e| format!("{context}: {e}"))?,
                name: context.into(),
            })))
        }
        _ => Err(format!("{context}.action must be a string or function")),
    }
}

/// Parse common binding fields once; each context resolves its allowed targets.
fn parse_keys<T>(
    lua: &Rc<Lua>,
    context: &str,
    keys: Table<'_>,
    fields: &[&str],
    allow_mouse: bool,
    mut resolve: impl FnMut(ParsedBinding, ParsedAction, Table<'_>) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    let mut bindings = Vec::new();
    let mut seen = Vec::new();
    for (index, table) in keys.sequence_values::<Table>().enumerate() {
        let location = format!("{context}[{}]", index + 1);
        let table = table.map_err(|e| format!("{location}: {e}"))?;
        for pair in table.clone().pairs::<String, mlua::Value>() {
            let (field, _) = pair.map_err(|e| format!("{location}: {e}"))?;
            if !fields.contains(&field.as_str()) {
                return Err(format!("{location}: unknown field {field}"));
            }
        }
        let key_name = normalize_key_name(
            &table
                .get::<_, String>("key")
                .map_err(|e| format!("{location}.key: {e}"))?,
        );
        let trigger = if allow_mouse && key_name == "DoubleClick" {
            BindingTrigger::DoubleClick
        } else {
            BindingTrigger::Key(
                parse_key(&key_name)
                    .ok_or_else(|| format!("{location}: unknown key {key_name}"))?,
            )
        };
        let modifiers = table
            .get::<_, Option<String>>("mods")
            .map_err(|e| format!("{location}.mods: {e}"))?
            .unwrap_or_default();
        let mods = parse_modifiers(&modifiers)
            .map_err(|part| format!("{location}: unknown modifier {part}"))?;
        if trigger == BindingTrigger::DoubleClick && !same_mods(mods, egui::Modifiers::NONE) {
            return Err(format!(
                "{location}: DoubleClick does not support modifiers"
            ));
        }
        if seen.iter().any(|(existing_key, existing_mods)| {
            *existing_key == trigger && same_mods(*existing_mods, mods)
        }) {
            return Err(format!("{location}: duplicate binding {key_name}"));
        }
        seen.push((trigger, mods));
        let description = table
            .get::<_, Option<String>>("description")
            .map_err(|e| format!("{location}.description: {e}"))?;
        let action = parse_action(
            lua,
            &location,
            table
                .get("action")
                .map_err(|e| format!("{location}.action: {e}"))?,
        )?;
        bindings.push(resolve(
            ParsedBinding {
                trigger,
                mods,
                description,
            },
            action,
            table,
        )?);
    }
    Ok(bindings)
}

pub(crate) fn parse_picker_keys(
    lua: &Rc<Lua>,
    name: &str,
    keys: Option<Table<'_>>,
) -> Result<Vec<PickerKey>, String> {
    let context = format!("pickers.{name}.keys");
    let Some(keys) = keys else {
        return Ok(Vec::new());
    };
    parse_keys(
        lua,
        &context,
        keys,
        &["key", "mods", "action", "description"],
        true,
        |binding, action, _| {
            let action = match action {
                ParsedAction::Lua(callback) => callback,
                _ => return Err(format!("{context}.action must be a function")),
            };
            Ok(PickerKey {
                description: binding
                    .description
                    .unwrap_or_else(|| "Run picker Lua action".into()),
                trigger: binding.trigger,
                mods: binding.mods,
                action,
            })
        },
    )
}

pub(crate) fn application_target(
    action: Option<String>,
    picker: Option<String>,
    builtin: Option<String>,
    quick_select: Option<String>,
) -> Result<crate::picker::config::BindingTarget, String> {
    use crate::picker::config::{BUILTIN_NAMES, BindingTarget};
    match (action, picker, builtin, quick_select) {
        (Some(name), None, None, None) => Ok(BindingTarget::Action(
            crate::actions::AppAction::from_name(&name)
                .unwrap_or(crate::actions::AppAction::Lua(name)),
        )),
        (None, Some(name), None, None) => Ok(BindingTarget::Picker(name)),
        (None, None, Some(name), None) if BUILTIN_NAMES.contains(&name.as_str()) => {
            Ok(BindingTarget::Picker(format!("builtin.{name}")))
        }
        (None, None, None, Some(name)) => Ok(BindingTarget::Action(
            crate::actions::AppAction::OpenQuickSelect(Some(name)),
        )),
        _ => Err(
            "key binding must specify exactly one of action, picker, quick_select, or a known builtin_picker"
                .into(),
        ),
    }
}

pub(crate) fn parse_application_keys(
    lua: &Rc<Lua>,
    keys: Table<'_>,
) -> Result<Vec<crate::picker::config::KeyBinding>, String> {
    parse_keys(
        lua,
        "keys",
        keys,
        &[
            "key",
            "mods",
            "action",
            "picker",
            "builtin_picker",
            "quick_select",
        ],
        false,
        |binding, action, table| {
            let action = match action {
            ParsedAction::Named(name) => Some(name),
            ParsedAction::None => None,
            ParsedAction::Lua(_) => return Err("application keybindings require a named action; define Lua functions in actions".into()),
        };
            Ok(crate::picker::config::KeyBinding {
                trigger: binding.trigger,
                mods: binding.mods,
                description: String::new(),
                action: application_target(
                    action,
                    table
                        .get("picker")
                        .map_err(|e| format!("keys.picker: {e}"))?,
                    table
                        .get("builtin_picker")
                        .map_err(|e| format!("keys.builtin_picker: {e}"))?,
                    table
                        .get("quick_select")
                        .map_err(|e| format!("keys.quick_select: {e}"))?,
                )?,
            })
        },
    )
}

pub(crate) fn same_mods(a: egui::Modifiers, b: egui::Modifiers) -> bool {
    // `command` mirrors Ctrl; compare the physical modifiers instead.
    (a.ctrl, a.shift, a.alt) == (b.ctrl, b.shift, b.alt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_and_picker_keys_share_normalization_and_duplicate_checks() {
        let lua = Rc::new(Lua::new());
        let application = parse_application_keys(
            &lua,
            lua.load("return {{key='k',mods=' ctrl | SHIFT ',action='ReloadConfig'}}")
                .eval()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(application[0].trigger, BindingTrigger::Key(egui::Key::K));
        assert!(matches!(
            application[0].action,
            crate::picker::config::BindingTarget::Action(crate::actions::AppAction::ReloadConfig)
        ));
        let picker = parse_picker_keys(
            &lua,
            "fixture",
            Some(
                lua.load("return {{key='k',mods=' ctrl | SHIFT ',action=function() end}}")
                    .eval()
                    .unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(picker[0].trigger, BindingTrigger::Key(egui::Key::K));
        assert!(same_mods(application[0].mods, picker[0].mods));
        for action in ["'ReloadConfig'", "function() end"] {
            let table: Table = lua.load(format!("return {{{{key='k',mods='ctrl',action={action}}},{{key='K',mods='CTRL',action={action}}}}}")).eval().unwrap();
            let error = if action == "function() end" {
                parse_picker_keys(&lua, "fixture", Some(table))
                    .err()
                    .unwrap()
            } else {
                parse_application_keys(&lua, table).err().unwrap()
            };
            assert!(error.contains("duplicate binding K"));
        }
    }

    #[test]
    fn shared_parser_keeps_context_specific_targets_and_fields() {
        let lua = Rc::new(Lua::new());
        for source in [
            "return {{key='DoubleClick',action='ReloadConfig'}}",
            "return {{key='Enter',action=function() end}}",
            "return {{key='Enter',action='ReloadConfig',picker='lines'}}",
            "return {{key='Enter',builtin_picker='unknown'}}",
            "return {{key='Enter',description='label',action='ReloadConfig'}}",
        ] {
            assert!(
                parse_application_keys(&lua, lua.load(source).eval().unwrap()).is_err(),
                "{source}"
            );
        }
        for source in [
            "return {{key='Enter',picker='lines'}}",
            "return {{key='Enter',action='ReloadConfig'}}",
            "return {{key='Enter',action='Accept'}}",
            "return {{key='DoubleClick',mods='CTRL',action=function() end}}",
        ] {
            assert!(
                parse_picker_keys(&lua, "fixture", Some(lua.load(source).eval().unwrap())).is_err(),
                "{source}"
            );
        }
        let bindings = parse_application_keys(
            &lua,
            lua.load("return {{key='f',builtin_picker='lines'}}")
                .eval()
                .unwrap(),
        )
        .unwrap();
        assert!(
            matches!(&bindings[0].action, crate::picker::config::BindingTarget::Picker(name) if name == "builtin.lines")
        );
    }
}

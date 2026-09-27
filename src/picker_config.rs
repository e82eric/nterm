use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Default, Deserialize)]
pub(crate) enum EnterAction {
    #[default]
    Navigate,
    SendInput,
}

// Repeat the shared acceptance fields so each variant can reject unknown fields.
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Definition {
    Scrollback {
        #[serde(default)]
        regex: Option<String>,
        #[serde(default)]
        enter_action: Option<EnterAction>,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::picker_action::Callback>>,
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
        enter_action: Option<EnterAction>,
        #[serde(skip)]
        callback: Option<std::rc::Rc<crate::picker_action::Callback>>,
    },
}

impl Definition {
    pub(crate) fn set_callback(&mut self, value: std::rc::Rc<crate::picker_action::Callback>) {
        match self {
            Self::Scrollback { callback, .. } | Self::FileSystem { callback, .. } => {
                *callback = Some(value)
            }
        }
    }

    fn acceptance(
        &self,
    ) -> (
        Option<std::rc::Rc<crate::picker_action::Callback>>,
        Option<EnterAction>,
    ) {
        match self {
            Self::Scrollback {
                callback,
                enter_action,
                ..
            }
            | Self::FileSystem {
                callback,
                enter_action,
                ..
            } => (callback.clone(), *enter_action),
        }
    }

    #[cfg(test)]
    pub(crate) fn callback(&self) -> Option<&std::rc::Rc<crate::picker_action::Callback>> {
        match self {
            Self::Scrollback { callback, .. } | Self::FileSystem { callback, .. } => {
                callback.as_ref()
            }
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
    pub(crate) callback: Option<std::rc::Rc<crate::picker_action::Callback>>,
    pub(crate) source: Source,
    pub(crate) enter_action: EnterAction,
}

#[derive(Clone)]
pub(crate) enum Source {
    Scrollback(Regex),
    FileSystem(FileOptions),
}

#[derive(Clone)]
#[cfg_attr(not(windows), allow(dead_code))] // Reserved for NFM's future Linux backend.
pub(crate) struct FileOptions {
    pub(crate) roots: Vec<std::path::PathBuf>,
    pub(crate) max_depth: i32,
    pub(crate) files_only: bool,
    pub(crate) directories_only: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            callback: None,
            source: Source::Scrollback(Regex::new(&default_regex()).unwrap()),
            enter_action: EnterAction::Navigate,
        }
    }
}

struct Binding {
    key: egui::Key,
    mods: egui::Modifiers,
    options: Options,
}

pub(crate) struct Bindings(Vec<Binding>);

impl Bindings {
    pub(crate) fn compile(
        definitions: Option<BTreeMap<String, Definition>>,
        keys: Option<Vec<KeyBinding>>,
    ) -> Result<Self, String> {
        let definitions = definitions.unwrap_or_else(|| {
            BTreeMap::from([(
                "lines".into(),
                Definition::Scrollback {
                    callback: None,
                    regex: None,
                    enter_action: None,
                },
            )])
        });
        let mut compiled = BTreeMap::new();
        for (name, definition) in definitions {
            if name.trim().is_empty() {
                return Err("picker names must not be empty".into());
            }
            let (callback, enter_action) = definition.acceptance();
            let options = match definition {
                Definition::Scrollback { regex, .. } => {
                    let regex = Regex::new(&regex.unwrap_or_else(default_regex))
                        .map_err(|error| format!("pickers.{name}.regex: {error}"))?;
                    if regex.is_match("") {
                        return Err(format!("pickers.{name}.regex must not match empty text"));
                    }
                    Options {
                        callback,
                        source: Source::Scrollback(regex),
                        enter_action: enter_action.unwrap_or(EnterAction::Navigate),
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
                        callback,
                        source: Source::FileSystem(FileOptions {
                            roots: roots.into_iter().map(std::path::PathBuf::from).collect(),
                            max_depth: max_depth.unwrap_or(i32::MAX),
                            files_only,
                            directories_only,
                        }),
                        enter_action: EnterAction::SendInput,
                    }
                }
            };
            compiled.insert(name, options);
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
        Ok(Self(bindings))
    }

    pub(crate) fn consume(&self, events: &mut Vec<egui::Event>) -> (Option<Options>, bool) {
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
                self.0.iter().find(|binding| {
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

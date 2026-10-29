//! Named quick selects: hint labels over custom regex matches instead of words.
use nfm_egui::quick_select::Alphabet;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::rc::Rc;

/// A built-in result for typing a hint's label.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub(crate) enum Builtin {
    #[default]
    Navigate,
    Copy,
    Paste,
}

/// What typing a hint's label does. Holding Shift on the last letter always
/// pastes the match at the prompt, as in the built-in quick select.
#[derive(Clone)]
pub(crate) enum Accept {
    /// Enter copy mode at the start of the match.
    Navigate,
    /// Copy the match to the clipboard.
    Copy,
    /// Paste the match at the prompt without running it.
    Paste,
    /// Call `function(app, item)`, as a picker action.
    Lua(Rc<crate::lua_callbacks::Callback>),
}

impl From<Builtin> for Accept {
    fn from(builtin: Builtin) -> Self {
        match builtin {
            Builtin::Navigate => Self::Navigate,
            Builtin::Copy => Self::Copy,
            Builtin::Paste => Self::Paste,
        }
    }
}

/// A `quick_selects` entry. A function `action` is taken out of the table
/// before deserializing, see `parse`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    patterns: Vec<String>,
    #[serde(default)]
    action: Builtin,
    label: Option<String>,
    alphabet: Option<String>,
}

/// `quick_select_defaults`: the built-in quick select's patterns and the
/// alphabet every quick select uses unless it sets its own.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawDefaults {
    patterns: Option<Vec<String>>,
    alphabet: Option<String>,
}

pub(crate) struct Definition {
    pub(crate) label: String,
    pub(crate) patterns: Vec<regex::Regex>,
    pub(crate) action: Accept,
    pub(crate) alphabet: Alphabet,
}

pub(crate) struct QuickSelects {
    /// The built-in quick select: `quick_select_defaults.patterns`, or big
    /// words when none are configured.
    pub(crate) default: Rc<Definition>,
    /// Big words, for linewise yanks: every row with text gets a label.
    pub(crate) words: Rc<Definition>,
    pub(crate) named: BTreeMap<String, Rc<Definition>>,
}

impl Default for QuickSelects {
    fn default() -> Self {
        let words = Rc::new(Definition::words(Alphabet::default()));
        Self {
            default: words.clone(),
            words,
            named: BTreeMap::new(),
        }
    }
}

impl Definition {
    fn words(alphabet: Alphabet) -> Self {
        Self {
            label: "Quick select".into(),
            patterns: vec![nfm_egui::quick_select::big_words().clone()],
            action: Accept::Navigate,
            alphabet,
        }
    }
}

impl QuickSelects {
    pub(crate) fn get(&self, name: &str) -> Option<&Rc<Definition>> {
        self.named.get(name)
    }
}

pub(crate) type Definitions = Rc<QuickSelects>;

fn compile_patterns(field: &str, patterns: &[String]) -> Result<Vec<regex::Regex>, String> {
    if patterns.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    patterns
        .iter()
        .enumerate()
        .map(|(index, pattern)| {
            regex::Regex::new(pattern).map_err(|error| format!("{field}[{}]: {error}", index + 1))
        })
        .collect()
}

fn compile_alphabet(field: &str, letters: Option<&str>) -> Result<Option<Alphabet>, String> {
    letters
        .map(|letters| Alphabet::new(letters).map_err(|error| format!("{field}: {error}")))
        .transpose()
}

fn compile(
    defaults: RawDefaults,
    raw: BTreeMap<String, Raw>,
    mut callbacks: BTreeMap<String, Rc<crate::lua_callbacks::Callback>>,
) -> Result<Definitions, String> {
    let alphabet = compile_alphabet(
        "quick_select_defaults.alphabet",
        defaults.alphabet.as_deref(),
    )?
    .unwrap_or_default();
    let words = Rc::new(Definition::words(alphabet.clone()));
    let default = match &defaults.patterns {
        Some(patterns) => Rc::new(Definition {
            patterns: compile_patterns("quick_select_defaults.patterns", patterns)?,
            ..Definition::words(alphabet.clone())
        }),
        None => words.clone(),
    };
    let named = raw
        .into_iter()
        .map(|(name, raw)| {
            let patterns =
                compile_patterns(&format!("quick_selects.{name}.patterns"), &raw.patterns)?;
            let definition = Definition {
                label: raw.label.unwrap_or_else(|| format!("Quick select: {name}")),
                patterns,
                action: callbacks
                    .remove(&name)
                    .map_or_else(|| raw.action.into(), Accept::Lua),
                alphabet: compile_alphabet(
                    &format!("quick_selects.{name}.alphabet"),
                    raw.alphabet.as_deref(),
                )?
                .unwrap_or_else(|| alphabet.clone()),
            };
            Ok((name, Rc::new(definition)))
        })
        .collect::<Result<_, String>>()?;
    Ok(Rc::new(QuickSelects {
        default,
        words,
        named,
    }))
}

/// Take function `action`s out of the `quick_selects` table so the rest
/// deserializes, returning them by quick select name.
fn take_callbacks(
    lua: &Rc<mlua::Lua>,
    quick_selects: &mlua::Table,
) -> Result<BTreeMap<String, Rc<crate::lua_callbacks::Callback>>, String> {
    let mut callbacks = BTreeMap::new();
    for pair in quick_selects.clone().pairs::<String, mlua::Value>() {
        let (name, value) = pair.map_err(|e| e.to_string())?;
        let mlua::Value::Table(definition) = value else {
            continue;
        };
        let action: mlua::Value = definition.get("action").map_err(|e| e.to_string())?;
        if let mlua::Value::Function(function) = action {
            let key = lua
                .create_registry_value(function)
                .map_err(|e| e.to_string())?;
            definition
                .raw_set("action", mlua::Value::Nil)
                .map_err(|e| e.to_string())?;
            callbacks.insert(
                name.clone(),
                Rc::new(crate::lua_callbacks::Callback {
                    lua: lua.clone(),
                    key,
                    name: format!("quick_selects.{name}"),
                }),
            );
        }
    }
    Ok(callbacks)
}

/// Compile the `quick_selects` and `quick_select_defaults` tables of the
/// config `root`, then remove them so the rest of the config deserializes.
pub(crate) fn parse(lua: &Rc<mlua::Lua>, root: &mlua::Table) -> Result<Definitions, String> {
    use mlua::LuaSerdeExt;
    let table = |name: &str| {
        root.get::<_, Option<mlua::Table>>(name)
            .map_err(|e| format!("{name}: {e}"))
    };
    let mut callbacks = BTreeMap::new();
    let mut raw = BTreeMap::new();
    if let Some(quick_selects) = table("quick_selects")? {
        callbacks = take_callbacks(lua, &quick_selects)?;
        raw = lua
            .from_value(mlua::Value::Table(quick_selects))
            .map_err(|e| format!("quick_selects: {e}"))?;
    }
    let defaults = match table("quick_select_defaults")? {
        Some(defaults) => lua
            .from_value(mlua::Value::Table(defaults))
            .map_err(|e| format!("quick_select_defaults: {e}"))?,
        None => RawDefaults::default(),
    };
    for name in ["quick_selects", "quick_select_defaults"] {
        root.raw_set(name, mlua::Value::Nil)
            .map_err(|e| e.to_string())?;
    }
    compile(defaults, raw, callbacks)
}

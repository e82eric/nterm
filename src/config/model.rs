//! Typed picker and action configuration, rebuilt at startup and successful reload.
use crate::picker::config::*;
use std::{collections::BTreeMap, rc::Rc};

#[derive(Clone)]
pub(crate) struct Model {
    pub(crate) pickers: BTreeMap<String, PickerEntry>,
    pub(crate) actions: BTreeMap<String, Rc<crate::actions::LuaAction>>,
    defaults: PickerDefaults,
}
impl Model {
    pub(crate) fn compile(
        definitions: Option<BTreeMap<String, Definition>>,
        defaults: PickerDefaults,
        actions: BTreeMap<String, Rc<crate::actions::LuaAction>>,
    ) -> Result<Self, String> {
        defaults.validate()?;
        // Without custom pickers only the built-ins exist; default bindings
        // fall back to them.
        let mut definitions = definitions.unwrap_or_default();
        // TODO(picker-defaults): built-in defaults are applied twice: overrides
        // are merged onto builtin_defaults() when the config is parsed (and
        // again on open for factory overrides), and missing built-ins are
        // filled in here. Take overrides as a separate argument and build each
        // built-in once from either its override or its defaults.
        for name in BUILTIN_NAMES {
            definitions
                .entry(format!("builtin.{name}"))
                .or_insert_with(|| {
                    serde_json::from_value(builtin_defaults(name)).expect("valid builtin defaults")
                });
        }

        let pickers = definitions
            .into_iter()
            .map(|(name, definition)| {
                let entry = definition.resolve(&name, defaults)?;
                Ok((name, entry))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        Ok(Self {
            pickers,
            actions,
            defaults,
        })
    }
    pub(crate) fn picker(&self, name: &str) -> Option<PickerEntry> {
        self.pickers.get(name).map(|entry| match entry {
            PickerEntry::Static(options) => PickerEntry::Static(options.clone()),
            PickerEntry::Factory(factory, _) => {
                PickerEntry::Factory(factory.clone(), self.defaults)
            }
        })
    }
    pub(crate) fn action(&self, name: &str) -> Option<Rc<crate::actions::LuaAction>> {
        self.actions.get(name).cloned()
    }
}

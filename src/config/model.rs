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
        let mut definitions = definitions.unwrap_or_else(|| {
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
                        keys: Vec::new(),
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
                        keys: Vec::new(),
                        preview: None,
                        preview_visible: false,
                    },
                ),
            ])
        });
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

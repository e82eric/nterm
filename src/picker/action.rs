use crate::ghostty_ffi::{SnapshotCell, SnapshotCellRange};
use mlua::{Lua, LuaSerdeExt, RegistryKey, Table, UserData, UserDataMethods};
use serde::Deserialize;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub(crate) struct Callback {
    pub(crate) lua: Rc<Lua>,
    pub(crate) key: RegistryKey,
    pub(crate) name: String,
}

pub(crate) struct ItemsProvider {
    pub(crate) lua: Rc<Lua>,
    pub(crate) key: RegistryKey,
}

pub(crate) struct PreviewCommand {
    pub(crate) lua: Rc<Lua>,
    pub(crate) key: RegistryKey,
    pub(crate) name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreviewCommandSpec {
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) center_line: Option<usize>,
}

impl PreviewCommand {
    pub(crate) fn call(
        &self,
        value: &str,
        filesystem: bool,
        params: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<PreviewCommandSpec>, String> {
        let result = (|| -> mlua::Result<Option<PreviewCommandSpec>> {
            let function: mlua::Function = self.lua.registry_value(&self.key)?;
            let item = self.lua.create_table()?;
            item.set("text", value)?;
            item.set("value", value)?;
            if filesystem {
                item.set("path", value)?;
                item.set("is_directory", std::path::Path::new(value).is_dir())?;
            }
            let lua_params = self.lua.create_table()?;
            for (key, value) in params {
                lua_params.set(key.as_str(), value.as_str())?;
            }
            let selected: mlua::Value = function.call((item, lua_params))?;
            if matches!(selected, mlua::Value::Nil) {
                return Ok(None);
            }
            self.lua.from_value(selected).map(Some)
        })();
        let spec =
            result.map_err(|error| format!("picker {} preview.command: {error}", self.name))?;
        if let Some(spec) = &spec {
            if spec.args.first().is_none_or(String::is_empty)
                || spec.args.iter().any(|arg| arg.contains('\0'))
                || spec.cwd.as_ref().is_some_and(|cwd| cwd.contains('\0'))
            {
                return Err(format!(
                    "picker {} preview.command: invalid command",
                    self.name
                ));
            }
        }
        Ok(spec)
    }
}

impl ItemsProvider {
    pub(crate) fn read(&self) -> Result<Vec<crate::picker::protocol::Item>, String> {
        let function: mlua::Function = self
            .lua
            .registry_value(&self.key)
            .map_err(|error| error.to_string())?;
        let value: mlua::Value = function.call(()).map_err(|error| error.to_string())?;
        let items: Vec<crate::picker::protocol::Item> = self
            .lua
            .from_value(value)
            .map_err(|error| error.to_string())?;
        crate::picker::protocol::validate_items(&items)?;
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callback(source: &str) -> Callback {
        let lua = Rc::new(Lua::new());
        let key = lua
            .create_registry_value(lua.load(source).eval::<mlua::Function>().unwrap())
            .unwrap();
        Callback {
            lua,
            key,
            name: "test".into(),
        }
    }

    #[test]
    fn closures_keep_state_and_queue_actions_in_order() {
        let callback = callback(
            "local count = 0; return function(app, item) count = count + 1; app:paste(item.path); app:send_text(tostring(count) .. '\\n') end",
        );
        for expected in ["1\n", "2\n"] {
            let commands = callback
                .call(
                    "G:\\file.txt",
                    "G:\\file.txt",
                    true,
                    None,
                    None,
                    &Default::default(),
                )
                .unwrap();
            assert!(matches!(&commands[0], Command::Paste(text) if text == "G:\\file.txt"));
            assert!(matches!(&commands[1], Command::SendText(text) if text == expected));
        }
    }

    #[test]
    fn navigation_uses_snapshot_cells_and_rejects_files() {
        let callback = callback("return function(app, item) app:navigate(item) end");
        let cell = SnapshotCell { x: 3, y: 42 };
        let commands = callback
            .call(
                "word",
                "word",
                false,
                Some(7),
                Some(SnapshotCellRange {
                    start: cell,
                    end: cell,
                }),
                &Default::default(),
            )
            .unwrap();
        assert!(matches!(commands[0], Command::Navigate(target) if target == cell));
        assert!(
            callback
                .call("file", "file", true, Some(7), None, &Default::default())
                .is_err()
        );
    }

    #[test]
    fn errors_discard_actions_and_retained_app_handles_expire() {
        let callback = callback(
            "return function(app, item) if saved then saved:paste('late') else saved = app; app:paste('queued'); error('failed') end end",
        );
        assert!(
            callback
                .call("file", "file", true, None, None, &Default::default())
                .unwrap_err()
                .contains("failed")
        );
        assert!(
            callback
                .call("file", "file", true, None, None, &Default::default())
                .unwrap_err()
                .contains("already returned")
        );
    }

    #[test]
    fn item_value_and_request_params_reach_callback() {
        let callback = callback(
            "return function(app, item, params) app:send_text(string.rep('\\b', tonumber(params.replaceBefore)) .. string.rep('\\27[3~', tonumber(params.replaceAfter))); app:paste(item.value) end",
        );
        let params = std::collections::BTreeMap::from([
            ("replaceBefore".to_string(), "3".to_string()),
            ("replaceAfter".to_string(), "2".to_string()),
        ]);
        let commands = callback
            .call("display", "inserted", false, None, None, &params)
            .unwrap();
        assert!(
            matches!(&commands[0], Command::SendText(text) if text == "\x08\x08\x08\x1b[3~\x1b[3~")
        );
        assert!(matches!(&commands[1], Command::Paste(text) if text == "inserted"));
    }

    #[test]
    fn prompt_replacement_keeps_selected_text_and_both_cursor_sides() {
        let callback = callback(
            "return function(app, item, params) app:replace_prompt(item.value, tonumber(params.replaceBefore), tonumber(params.replaceAfter)) end",
        );
        let params = std::collections::BTreeMap::from([
            ("replaceBefore".into(), "3".into()),
            ("replaceAfter".into(), "2".into()),
        ]);
        let commands = callback
            .call("shown", "replacement", false, None, None, &params)
            .unwrap();
        assert!(
            matches!(&commands[0], Command::ReplacePrompt { text, before: 3, after: 2 } if text == "replacement")
        );
    }
}
#[derive(Debug)]
pub(crate) enum Command {
    Paste(String),
    SendText(String),
    ReplacePrompt {
        text: String,
        before: u32,
        after: u32,
    },
    Navigate(SnapshotCell),
}
struct App {
    commands: Rc<RefCell<Vec<Command>>>,
    active: Rc<Cell<bool>>,
    generation: Option<u64>,
}
impl UserData for App {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("paste", |_, app, text: String| {
            app.push(Command::Paste(text))
        });
        methods.add_method("send_text", |_, app, text: String| {
            app.push(Command::SendText(text))
        });
        methods.add_method(
            "replace_prompt",
            |_, app, (text, before, after): (String, u32, u32)| {
                if before > 4096 || after > 4096 || text.contains('\0') {
                    return Err(mlua::Error::external("invalid prompt replacement"));
                }
                app.push(Command::ReplacePrompt {
                    text,
                    before,
                    after,
                })
            },
        );
        methods.add_method("navigate", |_, app, item: Table| {
            if app.generation.is_none()
                || item.get::<_, Option<u64>>("snapshot_generation")? != app.generation
            {
                return Err(mlua::Error::external(
                    "navigate requires an item from this scrollback snapshot",
                ));
            }
            let range: Table = item.get("range")?;
            let start: Table = range.get("start")?;
            app.push(Command::Navigate(SnapshotCell {
                x: start.get("x")?,
                y: start.get("y")?,
            }))
        });
    }
}
impl App {
    fn push(&self, command: Command) -> mlua::Result<()> {
        if !self.active.get() {
            return Err(mlua::Error::external("picker action has already returned"));
        }
        if matches!(&command, Command::Paste(text) | Command::SendText(text) if text.contains('\0'))
        {
            return Err(mlua::Error::external("terminal text cannot contain NUL"));
        }
        self.commands.borrow_mut().push(command);
        Ok(())
    }
}
impl Callback {
    pub(crate) fn call(
        &self,
        text: &str,
        value: &str,
        filesystem: bool,
        generation: Option<u64>,
        range: Option<SnapshotCellRange>,
        params: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<Command>, String> {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let active = Rc::new(Cell::new(true));
        let result = (|| -> mlua::Result<()> {
            let item = self.lua.create_table()?;
            item.set("text", text)?;
            item.set("value", value)?;
            if filesystem {
                item.set("path", text)?;
            }
            if let Some(range) = range {
                item.set("snapshot_generation", generation)?;
                let bounds = self.lua.create_table()?;
                for (name, cell) in [("start", range.start), ("end", range.end)] {
                    let point = self.lua.create_table()?;
                    point.set("x", cell.x)?;
                    point.set("y", cell.y)?;
                    bounds.set(name, point)?;
                }
                item.set("range", bounds)?;
            }
            let function: mlua::Function = self.lua.registry_value(&self.key)?;
            let lua_params = self.lua.create_table()?;
            for (key, value) in params {
                lua_params.set(key.as_str(), value.as_str())?;
            }
            function.call((
                App {
                    commands: commands.clone(),
                    active: active.clone(),
                    generation: if filesystem { None } else { generation },
                },
                item,
                lua_params,
            ))
        })();
        active.set(false);
        result.map_err(|error| format!("picker {} action: {error}", self.name))?;
        Ok(commands.take())
    }
}

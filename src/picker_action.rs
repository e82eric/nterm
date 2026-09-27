use crate::ffi::{SnapshotCell, SnapshotCellRange};
use mlua::{Lua, RegistryKey, Table, UserData, UserDataMethods};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub(crate) struct Callback {
    pub(crate) lua: Rc<Lua>,
    pub(crate) key: RegistryKey,
    pub(crate) name: String,
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
            let commands = callback.call("G:\\file.txt", true, None, None).unwrap();
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
                false,
                Some(7),
                Some(SnapshotCellRange {
                    start: cell,
                    end: cell,
                }),
            )
            .unwrap();
        assert!(matches!(commands[0], Command::Navigate(target) if target == cell));
        assert!(callback.call("file", true, Some(7), None).is_err());
    }

    #[test]
    fn errors_discard_actions_and_retained_app_handles_expire() {
        let callback = callback(
            "return function(app, item) if saved then saved:paste('late') else saved = app; app:paste('queued'); error('failed') end end",
        );
        assert!(
            callback
                .call("file", true, None, None)
                .unwrap_err()
                .contains("failed")
        );
        assert!(
            callback
                .call("file", true, None, None)
                .unwrap_err()
                .contains("already returned")
        );
    }
}
#[derive(Debug)]
pub(crate) enum Command {
    Paste(String),
    SendText(String),
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
        filesystem: bool,
        generation: Option<u64>,
        range: Option<SnapshotCellRange>,
    ) -> Result<Vec<Command>, String> {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let active = Rc::new(Cell::new(true));
        let result = (|| -> mlua::Result<()> {
            let item = self.lua.create_table()?;
            item.set("text", text)?;
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
            function.call((
                App {
                    commands: commands.clone(),
                    active: active.clone(),
                    generation: if filesystem { None } else { generation },
                },
                item,
            ))
        })();
        active.set(false);
        result.map_err(|error| format!("picker {} action: {error}", self.name))?;
        Ok(commands.take())
    }
}

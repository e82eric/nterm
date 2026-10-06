//! Lua callbacks and the queued Ghostty commands they produce.
//! Shared by named actions, picker selections, item providers, and previews.

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
    pub(crate) fn read(&self) -> Result<Vec<crate::picker::wire::Item>, String> {
        let function: mlua::Function = self
            .lua
            .registry_value(&self.key)
            .map_err(|error| error.to_string())?;
        let value: mlua::Value = function.call(()).map_err(|error| error.to_string())?;
        let items: Vec<crate::picker::wire::Item> = self
            .lua
            .from_value(value)
            .map_err(|error| error.to_string())?;
        crate::picker::wire::validate_items(&items)?;
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
    fn floating_api_returns_live_ids_supports_names_and_expires_with_action() {
        use crate::terminal::floating::{Panes, Request, Target};
        let mut panes = Panes::default();
        let callback = callback(
            r#"local saved; return function(app)
            if saved then saved:get_floating_panes(); return end
            saved=app
            assert(app:get_floating_pane('missing') == nil)
            local a=app:create_floating_pane({name='tools',command='pwsh.exe',cwd='C:\\src'})
            local b=app:create_floating_pane()
            assert(a.id == 1 and a.name == 'tools' and a.visible and a.focused)
            assert(b.id == 2)
            assert(#app:get_floating_panes() == 2)
            assert(app:get_floating_pane('tools').id == a.id)
            app:restore_floating_pane(a.id)
            assert(app:get_floating_pane().id == a.id)
            app:toggle_floating_pane()
            assert(not app:get_floating_pane().visible)
            app:toggle_floating_pane()
            app:next_floating_pane()
            assert(app:get_floating_pane().id == b.id)
            app:previous_floating_pane()
            app:close_floating_pane('tools')
            assert(app:get_floating_pane('tools') == nil)
            assert(app:get_floating_pane().id == b.id)
            app:close_floating_pane()
            assert(#app:get_floating_panes() == 0)
        end"#,
        );
        let mut dispatch = |request| {
            let result = match request {
                Request::Create(options) => {
                    panes.validate_create(&options)?;
                    let id = panes.insert(options.name, ());
                    serde_json::to_value(panes.description(id)).unwrap()
                }
                Request::Get(target) => {
                    let id = target.as_ref().and_then(|t| panes.resolve(t)).or_else(|| {
                        if target.is_none() {
                            panes.last_id()
                        } else {
                            None
                        }
                    });
                    serde_json::to_value(id.and_then(|id| panes.description(id))).unwrap()
                }
                Request::List => serde_json::to_value(panes.descriptions()).unwrap(),
                Request::Restore(target) => {
                    let id = panes.resolve(&target).ok_or("missing pane")?;
                    panes.restore(id);
                    serde_json::Value::Null
                }
                Request::Toggle => {
                    if panes.visible_id().is_some() {
                        panes.hide();
                    } else if let Some(id) = panes.last_id() {
                        panes.restore(id);
                    }
                    serde_json::Value::Null
                }
                Request::Next | Request::Previous => {
                    panes.cycle(matches!(request, Request::Next));
                    serde_json::Value::Null
                }
                Request::Close(target) => {
                    let id = target
                        .as_ref()
                        .and_then(|t| panes.resolve(t))
                        .or_else(|| panes.last_id());
                    if let Some(id) = id {
                        panes.remove(id);
                    }
                    serde_json::Value::Null
                }
            };
            Ok(result)
        };
        assert!(
            callback
                .with_floating(&mut dispatch, || callback.call_action(String::new()))
                .unwrap()
                .is_empty()
        );
        assert!(
            callback
                .with_floating(&mut dispatch, || callback.call_action(String::new()))
                .unwrap_err()
                .contains("already returned")
        );
        assert!(panes.resolve(&Target::Id(1)).is_none());
    }
    #[test]
    fn floating_creation_errors_are_synchronous_and_options_are_strict() {
        let failing = callback(
            "return function(app) local ok,err=pcall(function() app:create_floating_pane({name='bad'}) end); assert(not ok and string.find(tostring(err),'spawn failed')); app:show_toast('Error handled','yes') end",
        );
        let commands = failing
            .with_floating(&mut |_| Err("spawn failed".into()), || {
                failing.call_action(String::new())
            })
            .unwrap();
        assert_eq!(commands.len(), 1);
        for options in [
            "{unexpected=true}",
            "{name=''}",
            "{cwd=''}",
            "{command=''}",
            "{name='a' .. string.char(0)}",
        ] {
            let c = callback(&format!(
                "return function(app) app:create_floating_pane({options}) end"
            ));
            assert!(
                c.with_floating(&mut |_| panic!("invalid options reached host"), || c
                    .call_action(String::new()))
                    .is_err()
            );
        }
    }
    #[test]
    fn named_action_handles_expire_and_toasts_validate_duration() {
        let callback = callback(
            "local saved; return function(app) if saved then saved:get_selection_text() else saved = app; app:show_toast('Title', app:get_selection_text()) end end",
        );
        let commands = callback.call_action("selected".into()).unwrap();
        assert!(
            matches!(&commands[0], Command::ShowToast { message, duration_ms: 2000, .. } if message == "selected")
        );
        assert_eq!(commands.len(), 1);
        assert!(callback.call_action("next".into()).is_err());
        for duration in [0, 60001] {
            let callback = self::callback(&format!(
                "return function(app) app:show_toast('Title', 'Message', {duration}) end"
            ));
            assert!(callback.call_action(String::new()).is_err());
        }
        let callback =
            self::callback("return function(app) app:show_toast('Picker', 'Complete', 1000) end");
        assert!(matches!(
            &callback
                .call("", "", false, None, None, &Default::default())
                .unwrap()[0],
            Command::ShowToast {
                duration_ms: 1000,
                ..
            }
        ));
    }

    #[test]
    fn toast_table_supports_optional_headers_and_rejects_invalid_options() {
        let action = callback(
            "return function(app) app:show_toast({message='Reloaded'}); app:show_toast({header='Reload failed', message='Invalid font size', duration_ms=5000}) end",
        );
        let commands = action.call_action(String::new()).unwrap();
        assert!(
            matches!(&commands[0], Command::ShowToast {title, message, duration_ms:2000} if title.is_empty() && message == "Reloaded")
        );
        assert!(
            matches!(&commands[1], Command::ShowToast {title, message, duration_ms:5000} if title == "Reload failed" && message == "Invalid font size")
        );
        for args in [
            "{message='x', duration_ms=0}",
            "{message='x', duration_ms=60001}",
            "{header='Missing message'}",
            "{message='x', unexpected=true}",
            "{message='x'}, 'extra'",
        ] {
            let action = callback(&format!("return function(app) app:show_toast({args}) end"));
            assert!(action.call_action(String::new()).is_err(), "{args}");
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
    ClosePicker,
    Paste(String),
    SendText(String),
    ReplacePrompt {
        text: String,
        before: u32,
        after: u32,
    },
    Navigate(SnapshotCell),
    ShowToast {
        title: String,
        message: String,
        duration_ms: u64,
    },
}
struct App {
    commands: Rc<RefCell<Vec<Command>>>,
    active: Rc<Cell<bool>>,
    generation: Option<u64>,
    selection: String,
}
impl UserData for App {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        use crate::terminal::floating::{CreateOptions, Request, Target};
        methods.add_method(
            "create_floating_pane",
            |lua, app, options: Option<mlua::Value>| {
                let options: CreateOptions = match options {
                    Some(options) => lua.from_value(options)?,
                    None => Default::default(),
                };
                options.validate().map_err(mlua::Error::external)?;
                app.floating(lua, Request::Create(options))
            },
        );
        methods.add_method("toggle_floating_pane", |lua, app, ()| {
            app.floating(lua, Request::Toggle)
        });
        methods.add_method("restore_floating_pane", |lua, app, target: mlua::Value| {
            app.floating(lua, Request::Restore(lua.from_value::<Target>(target)?))
        });
        methods.add_method(
            "get_floating_pane",
            |lua, app, target: Option<mlua::Value>| {
                app.floating(
                    lua,
                    Request::Get(target.map(|v| lua.from_value::<Target>(v)).transpose()?),
                )
            },
        );
        methods.add_method("get_floating_panes", |lua, app, ()| {
            app.floating(lua, Request::List)
        });
        methods.add_method("next_floating_pane", |lua, app, ()| {
            app.floating(lua, Request::Next)
        });
        methods.add_method("previous_floating_pane", |lua, app, ()| {
            app.floating(lua, Request::Previous)
        });
        methods.add_method(
            "close_floating_pane",
            |lua, app, target: Option<mlua::Value>| {
                app.floating(
                    lua,
                    Request::Close(target.map(|v| lua.from_value::<Target>(v)).transpose()?),
                )
            },
        );
        methods.add_method("get_selection_text", |_, app, ()| {
            if !app.active.get() {
                return Err(mlua::Error::external("action has already returned"));
            }
            Ok(app.selection.clone())
        });
        methods.add_method("show_toast", |lua, app, args: mlua::MultiValue| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ToastOptions {
                #[serde(default)]
                header: Option<String>,
                message: String,
                #[serde(default)]
                duration_ms: Option<u64>,
            }
            let (title, message, duration) = if args.len() == 1
                && matches!(args.iter().next(), Some(mlua::Value::Table(_)))
            {
                let options: ToastOptions = lua.from_value(args.into_iter().next().unwrap())?;
                (
                    options.header.unwrap_or_default(),
                    options.message,
                    options.duration_ms,
                )
            } else {
                if !(2..=3).contains(&args.len()) {
                    return Err(mlua::Error::external(
                        "show_toast expects an options table or title, message, optional duration",
                    ));
                }
                <(String, String, Option<u64>) as mlua::FromLuaMulti>::from_lua_multi(args, lua)?
            };
            let duration_ms = duration.unwrap_or(2000);
            if !(1..=60000).contains(&duration_ms) {
                return Err(mlua::Error::external(
                    "toast duration must be between 1 and 60000 milliseconds",
                ));
            }
            app.push(Command::ShowToast {
                title,
                message,
                duration_ms,
            })
        });
        methods.add_method("paste", |_, app, text: String| {
            app.push(Command::Paste(text))
        });
        methods.add_method("close_picker", |_, app, ()| app.push(Command::ClosePicker));
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
    fn floating<'lua>(
        &self,
        lua: &'lua Lua,
        request: crate::terminal::floating::Request,
    ) -> mlua::Result<mlua::Value<'lua>> {
        if !self.active.get() {
            return Err(mlua::Error::external("action has already returned"));
        }
        let handler: mlua::Function = lua.named_registry_value("ghostty.floating_handler")?;
        handler.call(lua.to_value(&request)?)
    }
    fn push(&self, command: Command) -> mlua::Result<()> {
        if !self.active.get() {
            return Err(mlua::Error::external("action has already returned"));
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
    pub(crate) fn with_floating<R>(
        &self,
        handler: &mut impl FnMut(
            crate::terminal::floating::Request,
        ) -> Result<serde_json::Value, String>,
        call: impl FnOnce() -> Result<R, String>,
    ) -> Result<R, String> {
        self.lua
            .scope(|scope| {
                let function = scope.create_function_mut(|lua, value: mlua::Value| {
                    let request = lua.from_value(value)?;
                    let response = handler(request).map_err(mlua::Error::external)?;
                    // Missing panes are Lua nil, not the JSON null sentinel.
                    if response.is_null() {
                        Ok(mlua::Value::Nil)
                    } else {
                        lua.to_value(&response)
                    }
                })?;
                self.lua
                    .set_named_registry_value("ghostty.floating_handler", function)?;
                let result = call().map_err(mlua::Error::external);
                self.lua
                    .unset_named_registry_value("ghostty.floating_handler")?;
                result
            })
            .map_err(|e| e.to_string())
    }
    pub(crate) fn call_action(&self, selection: String) -> Result<Vec<Command>, String> {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let active = Rc::new(Cell::new(true));
        let result = (|| -> mlua::Result<()> {
            let function: mlua::Function = self.lua.registry_value(&self.key)?;
            function.call(App {
                commands: commands.clone(),
                active: active.clone(),
                generation: None,
                selection,
            })
        })();
        active.set(false);
        result.map_err(|error| format!("action {}: {error}", self.name))?;
        Ok(commands.take())
    }
    #[cfg(test)]
    pub(crate) fn call(
        &self,
        text: &str,
        value: &str,
        filesystem: bool,
        generation: Option<u64>,
        range: Option<SnapshotCellRange>,
        params: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<Command>, String> {
        self.call_with_selection(
            text,
            value,
            filesystem,
            generation,
            range,
            params,
            String::new(),
        )
    }

    pub(crate) fn call_with_selection(
        &self,
        text: &str,
        value: &str,
        filesystem: bool,
        generation: Option<u64>,
        range: Option<SnapshotCellRange>,
        params: &std::collections::BTreeMap<String, String>,
        selection: String,
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
                    selection,
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

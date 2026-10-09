//! A configuration-owned Lua state, accessed only by its preview thread.
use mlua::{Lua, LuaSerdeExt};
use nfm_egui::Cancellation;
use nfm_preview_file::resolved::PreviewPlan;
use std::{collections::BTreeMap, sync::mpsc, time::Duration};

#[derive(Clone)]
pub(crate) struct ConfigSource(pub String, pub String);
#[derive(Clone)]
pub(crate) struct FactoryContext(pub serde_json::Value);
#[derive(Clone)]
struct RequestContext(Cancellation, usize);
pub(crate) fn install_ui_api(lua: &Lua) -> mlua::Result<()> {
    let helpers = lua.create_table()?;
    for name in ["run"] {
        helpers.set(
            name,
            lua.create_function(|_, _: mlua::MultiValue| -> mlua::Result<()> {
                Err(mlua::Error::external(
                    "preview helpers are only available inside a Dynamic preview resolver",
                ))
            })?,
        )?;
    }
    lua.globals().set("preview", helpers)
}
struct Request {
    text: String,
    value: String,
    filesystem: bool,
    params: BTreeMap<String, String>,
    max_bytes: usize,
    max_lines: usize,
    cancel: Cancellation,
    reply: mpsc::Sender<Result<Option<PreviewPlan>, String>>,
}
#[derive(Clone)]
pub(crate) struct Worker(mpsc::Sender<Request>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_initialize_without_requests_on_load_and_reload() {
        let path =
            std::env::temp_dir().join(format!("ghostty-preview-init-{}.txt", std::process::id()));
        let quoted_path =
            serde_json::to_string(&path.to_string_lossy().replace('\\', "/")).unwrap();
        for generation in ["startup", "reload"] {
            let source = format!(
                "local f = assert(io.open({quoted_path}, 'w')); f:write('{generation}'); f:close(); return {{pickers={{x={{preview={{resolve=function() return nil end}}}}}}}}"
            );
            let _worker = Worker::new(ConfigSource(source, "@init-test".into()), "x".into(), None);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if std::fs::read_to_string(&path).ok().as_deref() == Some(generation) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "worker waited for a preview request"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        std::fs::remove_file(path).unwrap();
    }
}

impl Worker {
    pub(crate) fn new(source: ConfigSource, name: String, context: Option<FactoryContext>) -> Self {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("lua-preview".into())
            .spawn(move || {
                let lua = std::rc::Rc::new(Lua::new());
                let initialized = (|| -> mlua::Result<_> {
                    install_helpers(&lua)?;
                    // Configuration initialization has its own deadline, independent
                    // of selection requests. Start as soon as the worker is created.
                    let started = std::time::Instant::now();
                    lua.set_hook(
                        mlua::HookTriggers::new().every_nth_instruction(10_000),
                        move |_, _| {
                            if started.elapsed() > Duration::from_secs(5) {
                                Err(mlua::Error::external("Preview configuration timed out"))
                            } else {
                                Ok(())
                            }
                        },
                    );
                    let root: mlua::Table = lua.load(&source.0).set_name(&source.1).eval()?;
                    let (group, key) = name
                        .strip_prefix("builtin.")
                        .map_or(("pickers", name.as_str()), |key| ("builtin_pickers", key));
                    let entries: mlua::Table = root.get(group)?;
                    // Build factory options once, retaining the resulting closure environment.
                    let entry: mlua::Value = entries.get(key)?;
                    let definition = match entry {
                        mlua::Value::Table(table) => table,
                        mlua::Value::Function(factory) => factory.call::<_, mlua::Table>(
                            lua.to_value(
                                &context
                                    .as_ref()
                                    .ok_or_else(|| {
                                        mlua::Error::external("Missing picker factory context")
                                    })?
                                    .0,
                            )?,
                        )?,
                        _ => return Err(mlua::Error::external("Missing preview definition")),
                    };
                    let preview: mlua::Table = definition.get("preview")?;
                    let callback: mlua::Function = preview.get("resolve")?;
                    lua.create_registry_value(callback)
                })();
                lua.remove_hook();
                if let Err(error) = &initialized {
                    eprintln!("picker {name} preview initialization: {error}");
                }
                for request in rx {
                    if request.cancel.is_cancelled() {
                        continue;
                    }
                    let result = (|| -> Result<_, String> {
                        let entry = initialized.as_ref().map_err(ToString::to_string)?;
                        install(&lua, request.cancel.clone(), request.max_bytes)
                            .map_err(|e| e.to_string())?;
                        let callback: mlua::Function =
                            lua.registry_value(entry).map_err(|e| e.to_string())?;
                        let resolver = crate::lua_callbacks::PreviewResolver {
                            lua: lua.clone(),
                            key: lua
                                .create_registry_value(callback)
                                .map_err(|e| e.to_string())?,
                            name: name.clone(),
                            worker: None,
                        };
                        resolver.call(
                            &request.text,
                            &request.value,
                            request.filesystem,
                            &request.params,
                            request.max_bytes,
                            request.max_lines,
                        )
                    })();
                    lua.remove_hook();
                    let _ = request.reply.send(result);
                }
            })
            .expect("start Lua preview worker");
        Self(tx)
    }
    pub(crate) fn resolve(
        &self,
        text: String,
        value: String,
        filesystem: bool,
        params: BTreeMap<String, String>,
        max_bytes: usize,
        max_lines: usize,
        cancel: Cancellation,
    ) -> Result<Option<PreviewPlan>, String> {
        let (reply, rx) = mpsc::channel();
        self.0
            .send(Request {
                text,
                value,
                filesystem,
                params,
                max_bytes,
                max_lines,
                cancel: cancel.clone(),
                reply,
            })
            .map_err(|_| "Lua preview worker stopped")?;
        loop {
            if cancel.is_cancelled() {
                return Err("Preview cancelled".into());
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("Lua preview worker stopped".into()),
            }
        }
    }
}
fn install(lua: &Lua, cancel: Cancellation, limit: usize) -> mlua::Result<()> {
    lua.set_app_data(RequestContext(cancel.clone(), limit));
    let hook_cancel = cancel.clone();
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(10_000),
        move |_, _| {
            if hook_cancel.is_cancelled() {
                Err(mlua::Error::external("Preview cancelled"))
            } else {
                Ok(())
            }
        },
    );
    install_helpers(lua)
}
fn install_helpers(lua: &Lua) -> mlua::Result<()> {
    if lua
        .named_registry_value::<bool>("preview.helpers")
        .unwrap_or(false)
    {
        return Ok(());
    }
    let helpers = lua.create_table()?;
    helpers.set(
        "run",
        lua.create_function(move |lua, spec: mlua::Table| {
            let RequestContext(cancel, limit) = lua
                .app_data_ref::<RequestContext>()
                .ok_or_else(|| {
                    mlua::Error::external("preview.run is only available during a preview request")
                })?
                .clone();
            let args: Vec<String> = lua.from_value(spec.get("args")?)?;
            if args.is_empty() || args.iter().any(|a| a.contains('\0')) {
                return Err(mlua::Error::external("Invalid command arguments"));
            }
            let timeout = spec
                .get::<_, Option<u64>>("timeout_ms")?
                .unwrap_or(5000)
                .clamp(1, 120_000);
            let mut command = nfm_preview_file::resolved::CommandSpec::new(&args[0]);
            command.arguments = args[1..].to_vec();
            command.working_directory = spec.get::<_, Option<String>>("cwd")?.map(Into::into);
            command.output_limit = limit;
            let started = std::time::Instant::now();
            let result = nfm_preview_file::resolved::run(
                &command,
                || cancel.is_cancelled() || started.elapsed() >= Duration::from_millis(timeout),
                |_, _| true,
            )
            .map_err(mlua::Error::external)?;
            if cancel.is_cancelled() || started.elapsed() >= Duration::from_millis(timeout) {
                return Err(mlua::Error::external(
                    "Preview command cancelled or timed out",
                ));
            }
            if result.truncated {
                return Err(mlua::Error::external("Command exceeds max_bytes"));
            }
            let output = lua.create_table()?;
            output.set("stdout", lua.create_string(&result.stdout)?)?;
            output.set("stderr", lua.create_string(&result.stderr)?)?;
            output.set("exit_code", result.status.code())?;
            Ok(output)
        })?,
    )?;
    lua.globals().set("preview", helpers)?;
    let ghostty = lua.create_table()?;
    ghostty.set(
        "json_parse",
        lua.create_function(|lua, text: String| {
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(mlua::Error::external)?;
            lua.to_value(&value)
        })?,
    )?;
    ghostty.set(
        "json_encode_pretty",
        lua.create_function(|lua, value: mlua::Value| {
            let value: serde_json::Value = lua.from_value(value)?;
            serde_json::to_string_pretty(&value).map_err(mlua::Error::external)
        })?,
    )?;
    lua.globals().set("ghostty", ghostty)?;
    lua.set_named_registry_value("preview.helpers", true)
}

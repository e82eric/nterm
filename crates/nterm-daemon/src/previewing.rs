//! What shows for a row of a picker of `nterm-daemon.lua`, where the file
//! says: its `preview` is `{ type = "Dynamic", resolve = function(item,
//! params) ... end }`, as in a window's `nterm.lua`, and `resolve` is run
//! here, on the machine the daemon is on. The files it reads and the
//! programs it runs are this machine's.
//!
//! Each picker that is open has a Lua of its own for this, made when a row
//! of it is first rested on, on the thread that reads the picker's
//! previews. A `resolve` still running when another row is rested on, or
//! when the picker ends, is stopped: between any two steps of Lua, and
//! while it waits for a program.

use crate::configured::short;
use mlua::{Lua, LuaSerdeExt};
use nterm_session::protocol::PickerStep;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The most that shows for a row, in bytes, and that a program run for it
/// may print.
pub(crate) const MOST: usize = 10 * 1024 * 1024;
/// How long `resolve` may take for one row.
const MOST_TIME: Duration = Duration::from_secs(12);
/// How long a program `preview.run` runs may take, where it does not say.
const RUN_TIME: u64 = 5000;
/// The most entries `preview.directory_items` gives.
const MOST_ENTRIES: usize = 100_000;

/// What shows for a row.
#[derive(Debug, PartialEq)]
pub(crate) enum Shows {
    /// Nothing: `resolve` gave nil.
    Nothing,
    Text {
        text: String,
        name: Option<String>,
        extension: Option<String>,
        /// `PlainText`, or the name of a language, in place of going by
        /// the name.
        syntax: Option<String>,
        line: Option<u64>,
    },
    /// What a program printed, with its colours.
    Ansi { bytes: Vec<u8>, line: Option<u32> },
    /// A picture, as the file of one.
    Image { bytes: Vec<u8> },
}

/// The request being answered, which the helpers and the hook go by.
#[derive(Clone)]
struct Asked {
    request: u64,
    wanted: Arc<AtomicU64>,
    until: Instant,
}
impl Asked {
    /// Another row is wanted in its place, the picker has ended, or it has
    /// taken too long.
    fn over(&self) -> bool {
        self.wanted.load(Ordering::Acquire) != self.request || Instant::now() > self.until
    }
}

/// What a function of the file asked of `app`, for the picker's UI to do.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Acted {
    pub(crate) steps: Vec<PickerStep>,
    /// Whether it asked for the picker to close.
    pub(crate) close: bool,
}
/// The most steps one function asks for, and the most text in one.
const MOST_STEPS: usize = 256;
const MOST_TEXT: usize = 1024 * 1024;

/// `app`, as a function of the file is given it: what a window's has that
/// is done to the pane the picker is over. Each is kept as a step, and
/// anything else a window's has says that the daemon has not got it.
fn app(lua: &Lua) -> mlua::Result<mlua::Table<'_>> {
    fn step(lua: &Lua, step: PickerStep) -> mlua::Result<()> {
        let mut acted = lua
            .app_data_mut::<Acted>()
            .ok_or_else(|| mlua::Error::RuntimeError("app is for use in a picker's action".into()))?;
        if acted.steps.len() == MOST_STEPS {
            return Err(mlua::Error::RuntimeError(format!("more than {MOST_STEPS} things were asked of app")));
        }
        acted.steps.push(step);
        Ok(())
    }
    fn text(text: String) -> mlua::Result<String> {
        if text.len() > MOST_TEXT || text.contains('\0') {
            return Err(mlua::Error::RuntimeError("the text is too long, or has a NUL in it".into()));
        }
        Ok(text)
    }
    let app = lua.create_table()?;
    app.set(
        "send_text",
        lua.create_function(|lua, (_, given): (mlua::Value, String)| step(lua, PickerStep::SendText { text: text(given)? }))?,
    )?;
    app.set(
        "paste",
        lua.create_function(|lua, (_, given): (mlua::Value, String)| step(lua, PickerStep::Paste { text: text(given)? }))?,
    )?;
    app.set(
        "replace_prompt",
        lua.create_function(|lua, (_, given, before, after): (mlua::Value, String, u32, u32)| {
            if before > 4096 || after > 4096 {
                return Err(mlua::Error::RuntimeError("that is more of the prompt than is replaced".into()));
            }
            step(lua, PickerStep::ReplacePrompt { text: text(given)?, before, after })
        })?,
    )?;
    app.set(
        "show_toast",
        lua.create_function(|lua, (_, first, second, third): (mlua::Value, mlua::Value, Option<String>, Option<u64>)| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Options {
                #[serde(default)]
                header: Option<String>,
                message: String,
                #[serde(default)]
                duration_ms: Option<u64>,
            }
            // A table of what to show, or a title and a message.
            let (title, message, duration_ms) = match (first, second) {
                (options @ mlua::Value::Table(_), None) => {
                    let options: Options = lua.from_value(options)?;
                    (options.header, options.message, options.duration_ms)
                }
                (mlua::Value::String(title), Some(message)) => {
                    (Some(title.to_string_lossy().into_owned()), message, third)
                }
                _ => {
                    return Err(mlua::Error::RuntimeError(
                        "show_toast takes a table, or a title, a message and how long".into(),
                    ));
                }
            };
            if duration_ms.is_some_and(|duration| !(1..=60_000).contains(&duration)) {
                return Err(mlua::Error::RuntimeError("a toast shows for 1 to 60000 milliseconds".into()));
            }
            step(lua, PickerStep::Toast { title, message: text(message)?, duration_ms })
        })?,
    )?;
    app.set(
        "close_picker",
        lua.create_function(|lua, _: mlua::Value| {
            let mut acted = lua
                .app_data_mut::<Acted>()
                .ok_or_else(|| mlua::Error::RuntimeError("app is for use in a picker's action".into()))?;
            acted.close = true;
            Ok(())
        })?,
    )?;
    // Another picker of this machine, in place of this one. A window's
    // `push_picker` keeps the first under the second; here the first is over.
    for method in ["push_picker", "replace_picker"] {
        app.set(
            method,
            lua.create_function(|lua, (_, name, params): (mlua::Value, String, Option<BTreeMap<String, String>>)| {
                let params = params.unwrap_or_default();
                let long = params.iter().map(|(key, value)| key.len() + value.len()).sum::<usize>() > MOST_TEXT;
                if name.is_empty() || name.len() > 256 || long {
                    return Err(mlua::Error::RuntimeError("that is not a picker's name and what it is opened with".into()));
                }
                step(lua, PickerStep::Picker { name, params })
            })?,
        )?;
    }
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        lua.create_function(|_, (_, name): (mlua::Value, String)| -> mlua::Result<mlua::Value> {
            Err(mlua::Error::RuntimeError(format!("app:{name} is not something the daemon has")))
        })?,
    )?;
    app.set_metatable(Some(meta));
    Ok(app)
}

/// The Lua of one open picker's previews.
pub(crate) struct Previewer {
    lua: Lua,
    params: BTreeMap<String, String>,
    /// Whether its rows are files and folders, which are then `item.path`.
    files: bool,
    /// The request the picker's UI last asked for: see `Asked`.
    wanted: Arc<AtomicU64>,
}

impl Previewer {
    /// The `resolve` of the picker `name` of `file`.
    pub(crate) fn new(
        file: &Path,
        name: &str,
        params: BTreeMap<String, String>,
        files: bool,
        wanted: Arc<AtomicU64>,
    ) -> Result<Self, String> {
        let lua = crate::configured::open(file)?;
        {
            let pickers: Option<mlua::Table> = lua.named_registry_value("pickers").map_err(|error| short(&error))?;
            let picker: Option<mlua::Table> = pickers.and_then(|pickers| pickers.get(name).ok());
            let picker = picker.ok_or("the file no longer has this picker")?;
            let preview: Option<mlua::Table> = picker.get("preview").ok();
            let resolve: Option<mlua::Function> = preview.and_then(|preview| preview.get("resolve").ok());
            lua.set_named_registry_value("resolve", resolve).map_err(|error| short(&error))?;
            let action: Option<mlua::Function> = picker.get("action").ok();
            lua.set_named_registry_value("action", action).map_err(|error| short(&error))?;
            let keys: Option<mlua::Table> = picker.get("keys").ok();
            lua.set_named_registry_value("keys", keys).map_err(|error| short(&error))?;
            lua.set_named_registry_value("app", app(&lua).map_err(|error| short(&error))?)
                .map_err(|error| short(&error))?;
        }
        Ok(Self { lua, params, files, wanted })
    }

    /// Run what the file says is done with the row that shows as `text` and
    /// is `value`: its `action`. What it asks of `app` is not done here: it
    /// is given back, as the steps for the picker's UI to do in order, with
    /// whether the picker is to close.
    /// With `key`, it is the function of that one of the picker's `keys`,
    /// counted from nothing.
    pub(crate) fn act(&self, text: &str, value: &str, key: Option<usize>) -> Result<Acted, String> {
        let lua = &self.lua;
        let failed = |error: mlua::Error| short(&error);
        // Nothing stops it but how long it takes.
        let asked = Asked { request: 0, wanted: Arc::default(), until: Instant::now() + MOST_TIME };
        lua.set_app_data(asked.clone());
        lua.set_app_data(Acted::default());
        let stop = asked.clone();
        lua.set_hook(mlua::HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
            if stop.over() {
                return Err(mlua::Error::RuntimeError("it took too long".into()));
            }
            Ok(())
        });
        let ran = (|| -> Result<(), String> {
            let action: Option<mlua::Function> = match key {
                None => lua.named_registry_value("action").map_err(failed)?,
                Some(key) => {
                    let keys: Option<mlua::Table> = lua.named_registry_value("keys").map_err(failed)?;
                    let key: Option<mlua::Table> = keys.and_then(|keys| keys.raw_get(key + 1).ok());
                    key.and_then(|key| key.raw_get("action").ok())
                }
            };
            let action = action.ok_or("the file no longer says what is done with a row of this picker")?;
            let app: mlua::Table = lua.named_registry_value("app").map_err(failed)?;
            action.call::<_, ()>((app, self.item(text, value)?, self.given()?)).map_err(failed)
        })();
        lua.remove_hook();
        lua.remove_app_data::<Asked>();
        let acted = lua.remove_app_data::<Acted>().unwrap_or_default();
        ran.map(|()| acted)
    }

    /// The row, as a function of the file is given it.
    fn item(&self, text: &str, value: &str) -> Result<mlua::Table<'_>, String> {
        let failed = |error: mlua::Error| short(&error);
        let item = self.lua.create_table().map_err(failed)?;
        item.set("text", text).map_err(failed)?;
        item.set("value", value).map_err(failed)?;
        if self.files {
            item.set("path", value).map_err(failed)?;
        }
        Ok(item)
    }

    /// What the picker was opened with, as a function of the file is given it.
    fn given(&self) -> Result<mlua::Table<'_>, String> {
        let failed = |error: mlua::Error| short(&error);
        let params = self.lua.create_table().map_err(failed)?;
        for (name, value) in &self.params {
            params.set(name.as_str(), value.as_str()).map_err(failed)?;
        }
        Ok(params)
    }

    /// What shows for the row that shows as `text` and is `value`, asked
    /// for with `request`. None when it was stopped for another.
    pub(crate) fn resolve(&self, request: u64, text: &str, value: &str) -> Option<Result<Shows, String>> {
        let asked = Asked { request, wanted: self.wanted.clone(), until: Instant::now() + MOST_TIME };
        self.lua.set_app_data(asked.clone());
        let stop = asked.clone();
        self.lua.set_hook(mlua::HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
            if stop.over() {
                return Err(mlua::Error::RuntimeError("stopped".into()));
            }
            Ok(())
        });
        let shows = self.run(&asked, text, value);
        self.lua.remove_hook();
        self.lua.remove_app_data::<Asked>();
        if self.wanted.load(Ordering::Acquire) != request {
            return None;
        }
        Some(match shows {
            Err(_) if Instant::now() > asked.until => Err("The preview took too long".into()),
            shows => shows,
        })
    }

    fn run(&self, asked: &Asked, text: &str, value: &str) -> Result<Shows, String> {
        let lua = &self.lua;
        let failed = |error: mlua::Error| short(&error);
        let resolve: Option<mlua::Function> = lua.named_registry_value("resolve").map_err(failed)?;
        let resolve = resolve.ok_or("the file no longer has a preview for this picker")?;
        let item = lua.create_table().map_err(failed)?;
        item.set("text", text).map_err(failed)?;
        item.set("value", value).map_err(failed)?;
        if self.files {
            item.set("path", value).map_err(failed)?;
        }
        let params = lua.create_table().map_err(failed)?;
        for (name, value) in &self.params {
            params.set(name.as_str(), value.as_str()).map_err(failed)?;
        }
        let returned: mlua::Value = resolve.call((item, params)).map_err(failed)?;
        let table = match &returned {
            mlua::Value::Nil => return Ok(Shows::Nothing),
            mlua::Value::Table(table) => table,
            _ => return Err("resolve gave what is not a table".into()),
        };
        // Bytes are taken as they are: they need not be text.
        let kind: Option<String> = table.get("type").ok();
        let only = |allowed: &[&str]| -> Result<(), String> {
            for pair in table.clone().pairs::<String, mlua::Value>() {
                let (field, _) = pair.map_err(failed)?;
                if !allowed.contains(&field.as_str()) {
                    return Err(format!("{} has no field {field}", kind.as_deref().unwrap_or_default()));
                }
            }
            Ok(())
        };
        let bytes = || -> Result<Vec<u8>, String> {
            let bytes: mlua::String = table.get("bytes").map_err(|_| "bytes is not a string".to_owned())?;
            if bytes.as_bytes().len() > MOST {
                return Err(format!("it is more than {MOST} bytes"));
            }
            Ok(bytes.as_bytes().to_vec())
        };
        match kind.as_deref() {
            Some("Ansi") => {
                only(&["type", "bytes", "center_line"])?;
                let line: Option<u32> = table.get("center_line").map_err(|_| "center_line is not a line".to_owned())?;
                Ok(Shows::Ansi { bytes: bytes()?, line: line.filter(|line| *line > 0) })
            }
            Some("Image") => {
                only(&["type", "bytes"])?;
                let bytes = bytes()?;
                if bytes.is_empty() {
                    return Err("the picture is empty".into());
                }
                Ok(Shows::Image { bytes })
            }
            _ => match lua.from_value(returned.clone()).map_err(failed)? {
                Resolved::Text { text, lines, name, extension, syntax, _theme: _, center_line, highlight_line } => {
                    let text = match (text, lines) {
                        (Some(text), None) => text,
                        (None, Some(lines)) => lines.join("\n"),
                        _ => return Err("Text has one of text and lines".into()),
                    };
                    if text.len() > MOST {
                        return Err(format!("it is more than {MOST} bytes"));
                    }
                    let line = center_line.or(highlight_line).filter(|line| *line > 0);
                    Ok(Shows::Text { text, name, extension, syntax, line })
                }
                Resolved::Command { args, cwd, output, center_line, success_exit_codes } => {
                    if center_line.is_some() && output != Output::Ansi {
                        return Err("center_line is for a Command whose output is Ansi".into());
                    }
                    let cwd = cwd.or_else(|| self.params.get("cwd").cloned());
                    let printed = run(&args, cwd, MOST_TIME.as_millis() as u64, asked)?;
                    let code = printed.status.code();
                    if !code.is_some_and(|code| success_exit_codes.contains(&code)) {
                        let said = String::from_utf8_lossy(&printed.stderr).trim().to_owned();
                        return Err(if said.is_empty() { format!("{} failed", args[0]) } else { said });
                    }
                    Ok(match output {
                        Output::Ansi => Shows::Ansi { bytes: printed.stdout, line: center_line.filter(|line| *line > 0) },
                        Output::Image => Shows::Image { bytes: printed.stdout },
                        Output::Text => Shows::Text {
                            text: String::from_utf8_lossy(&printed.stdout).into_owned(),
                            name: None,
                            extension: None,
                            syntax: Some("PlainText".into()),
                            line: None,
                        },
                    })
                }
            },
        }
    }
}

/// What `resolve` gives that is not bytes, with the fields a window's has.
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Resolved {
    Text {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        lines: Option<Vec<String>>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        extension: Option<String>,
        #[serde(default)]
        syntax: Option<String>,
        /// The colours are the window's own.
        #[serde(default, rename = "theme")]
        _theme: Option<String>,
        #[serde(default)]
        center_line: Option<u64>,
        #[serde(default)]
        highlight_line: Option<u64>,
    },
    /// A program for the daemon to run, whose output is what shows.
    Command {
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        output: Output,
        #[serde(default)]
        center_line: Option<u32>,
        #[serde(default = "success")]
        success_exit_codes: Vec<i32>,
    },
}
#[derive(Clone, Copy, Default, PartialEq, Deserialize)]
enum Output {
    Text,
    #[default]
    Ansi,
    Image,
}
fn success() -> Vec<i32> {
    vec![0]
}

/// Run a program to its end, or until the request is over or `timeout`
/// milliseconds have gone.
fn run(
    args: &[String],
    cwd: Option<String>,
    timeout: u64,
    asked: &Asked,
) -> Result<nfm_preview_command::CommandOutput, String> {
    if args.first().is_none_or(String::is_empty) || args.iter().any(|argument| argument.contains('\0')) {
        return Err("args names no program to run, or has a NUL in it".into());
    }
    let mut command = nfm_preview_command::CommandSpec::new(&args[0]);
    command.arguments = args[1..].to_vec();
    command.working_directory = cwd.map(PathBuf::from);
    command.output_limit = MOST;
    let until = Instant::now() + Duration::from_millis(timeout);
    let over = || asked.over() || Instant::now() > until;
    let printed = nfm_preview_command::run(&command, over, |_, _| true);
    // Stopped is said the same whichever of the two noticed it.
    let stopped = format!("{} was stopped, or took too long", args[0]);
    let printed = printed.map_err(|error| if over() { stopped.clone() } else { error })?;
    if over() {
        return Err(stopped);
    }
    if printed.truncated {
        return Err(format!("{} printed more than {MOST} bytes", args[0]));
    }
    Ok(printed)
}

/// One entry of a folder, as `preview.directory_items` gives them.
#[derive(serde::Serialize)]
struct Entry {
    name: String,
    is_directory: bool,
}

/// What the file may call: `preview.run` and `preview.directory_items`,
/// which are for use in `resolve`, and `nterm.json_parse` and
/// `nterm.json_encode_pretty`. They are those a window's file has.
pub(crate) fn helpers(lua: &Lua) -> mlua::Result<()> {
    let asked = |lua: &Lua, what: &str| -> mlua::Result<Asked> {
        let asked = lua.app_data_ref::<Asked>().map(|asked| asked.clone());
        asked.ok_or_else(|| mlua::Error::RuntimeError(format!("{what} is for use in a preview's resolve")))
    };
    let preview = lua.create_table()?;
    preview.set(
        "run",
        lua.create_function(move |lua, spec: mlua::Table| {
            let asked = asked(lua, "preview.run")?;
            let args: Vec<String> = lua.from_value(spec.get("args")?)?;
            let timeout = spec.get::<_, Option<u64>>("timeout_ms")?.unwrap_or(RUN_TIME).clamp(1, 120_000);
            let cwd: Option<String> = spec.get("cwd")?;
            let printed = run(&args, cwd, timeout, &asked).map_err(mlua::Error::RuntimeError)?;
            let output = lua.create_table()?;
            output.set("stdout", lua.create_string(&printed.stdout)?)?;
            output.set("stderr", lua.create_string(&printed.stderr)?)?;
            output.set("exit_code", printed.status.code())?;
            Ok(output)
        })?,
    )?;
    preview.set(
        "directory_items",
        lua.create_function(move |lua, path: String| {
            let asked = asked(lua, "preview.directory_items")?;
            let failed = |error: std::io::Error| mlua::Error::RuntimeError(error.to_string());
            if !std::fs::metadata(&path).map_err(failed)?.is_dir() {
                return Ok(mlua::Value::Nil);
            }
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&path).map_err(failed)? {
                let entry = entry.map_err(failed)?;
                if asked.over() || entries.len() == MOST_ENTRIES {
                    return Err(mlua::Error::RuntimeError("the folder has too much in it, or it was stopped".into()));
                }
                entries.push(Entry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_directory: entry.file_type().map_err(failed)?.is_dir(),
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            lua.to_value(&entries)
        })?,
    )?;
    lua.globals().set("preview", preview)?;
    let nterm = lua.create_table()?;
    nterm.set(
        "json_parse",
        lua.create_function(|lua, text: String| {
            let value: serde_json::Value = serde_json::from_str(&text).map_err(mlua::Error::external)?;
            lua.to_value(&value)
        })?,
    )?;
    nterm.set(
        "json_encode_pretty",
        lua.create_function(|lua, value: mlua::Value| {
            let value: serde_json::Value = lua.from_value(value)?;
            serde_json::to_string_pretty(&value).map_err(mlua::Error::external)
        })?,
    )?;
    lua.globals().set("nterm", nterm)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of pickers in a folder of its own, with `more` beside it.
    fn file(test: &str, text: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("nterm-previewing-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(crate::configured::FILE);
        std::fs::write(&path, text).unwrap();
        path
    }
    fn previewer(path: &Path, name: &str, files: bool) -> (Previewer, Arc<AtomicU64>) {
        let wanted = Arc::new(AtomicU64::new(0));
        let folder = path.parent().unwrap().to_string_lossy().into_owned();
        let params = [("cwd".to_owned(), folder), ("word".to_owned(), "given".to_owned())].into_iter().collect();
        (Previewer::new(path, name, params, files, wanted.clone()).unwrap(), wanted)
    }
    /// Ask as a picker does: the request is the one wanted, then resolved.
    fn ask(previewer: &Previewer, wanted: &AtomicU64, request: u64, text: &str, value: &str) -> Result<Shows, String> {
        wanted.store(request, Ordering::Release);
        previewer.resolve(request, text, value).expect("it was not stopped")
    }

    #[test]
    fn a_function_of_the_file_says_what_shows_for_a_row() {
        let path = file(
            "kinds",
            r#"
            local function read(path)
              local file = assert(io.open(path, "rb"))
              local bytes = file:read("a")
              file:close()
              return bytes
            end
            return { pickers = { P = { type = "Items", preview = { type = "Dynamic", resolve = function(item, params)
              local where = params.cwd .. "/" .. item.value
              if item.value == "nothing" then return nil end
              if item.value == "told" then
                return { type = "Text", lines = { item.text, item.value, params.word, tostring(item.path) },
                         syntax = "PlainText" }
              end
              if item.value == "file.lua" then
                return { type = "Text", text = read(where), name = item.value, extension = "lua",
                         theme = "gruvbox-dark", center_line = 2, highlight_line = 2 }
              end
              if item.value == "dot.png" then return { type = "Image", bytes = read(where) } end
              if item.value == "folder" then
                local names = {}
                for _, entry in ipairs(preview.directory_items(params.cwd)) do
                  names[#names + 1] = entry.name .. (entry.is_directory and "/" or "")
                end
                return { type = "Text", text = table.concat(names, " ") }
              end
              if item.value == "not a folder" then
                return { type = "Text", text = tostring(preview.directory_items(params.cwd .. "/file.lua")) }
              end
              if item.value == "ran" then
                local result = preview.run { args = { "cmd.exe", "/c", "echo", "one & echo two" }, cwd = params.cwd }
                assert(result.exit_code == 0, result.stderr)
                return { type = "Ansi", bytes = result.stdout, center_line = 1 }
              end
              if item.value == "command" then
                return { type = "Command", args = { "cmd.exe", "/c", "cd" }, output = "Text" }
              end
              if item.value == "fails" then
                return { type = "Command", args = { "cmd.exe", "/c", "echo broken 1>&2 & exit 3" } }
              end
              if item.value == "allowed" then
                return { type = "Command", args = { "cmd.exe", "/c", "echo fine & exit 3" },
                         success_exit_codes = { 0, 3 }, center_line = 1 }
              end
              if item.value == "error" then error("it went wrong", 0) end
              if item.value == "typo" then return { type = "Text", text = "x", colour = "red" } end
              if item.value == "odd" then return 5 end
              return { type = "Text", text = "other" }
            end } } } }
            "#,
        );
        let folder = path.parent().unwrap();
        std::fs::write(folder.join("file.lua"), "local a = 1\nreturn a\n").unwrap();
        std::fs::write(folder.join("dot.png"), [137u8, 80, 78, 71, 0, 255, 10]).unwrap();
        std::fs::create_dir(folder.join("sub")).unwrap();
        let (previewer, wanted) = previewer(&path, "P", false);
        let mut request = 0;
        let mut shows = |value: &str| {
            request += 1;
            ask(&previewer, &wanted, request, "the text", value)
        };
        let text = |text: &str, syntax: Option<&str>| Shows::Text {
            text: text.into(),
            name: None,
            extension: None,
            syntax: syntax.map(str::to_owned),
            line: None,
        };

        assert_eq!(shows("nothing"), Ok(Shows::Nothing));
        // It is told the row, and what the picker was opened with.
        assert_eq!(shows("told"), Ok(text("the text\ntold\ngiven\nnil", Some("PlainText"))));
        // A file of this machine, read by Lua's own means.
        assert_eq!(
            shows("file.lua"),
            Ok(Shows::Text {
                text: "local a = 1\nreturn a\n".into(),
                name: Some("file.lua".into()),
                extension: Some("lua".into()),
                syntax: None,
                line: Some(2),
            })
        );
        // Bytes that are not text are kept as they are.
        assert_eq!(shows("dot.png"), Ok(Shows::Image { bytes: vec![137, 80, 78, 71, 0, 255, 10] }));
        assert_eq!(shows("folder"), Ok(text("dot.png file.lua nterm-daemon.lua sub/", None)));
        assert_eq!(shows("not a folder"), Ok(text("nil", None)));
        // A program, run with each argument whole.
        let Ok(Shows::Ansi { bytes, line }) = shows("ran") else { panic!("not what a program printed") };
        assert!(String::from_utf8_lossy(&bytes).contains("one & echo two") && line == Some(1));
        // One the daemon runs for it, where the picker was opened.
        let Ok(Shows::Text { text: printed, syntax, .. }) = shows("command") else { panic!("not text") };
        let named = folder.file_name().unwrap().to_string_lossy().into_owned();
        assert!(printed.trim().ends_with(&named), "{printed}");
        assert_eq!(syntax.as_deref(), Some("PlainText"));
        let failed = shows("fails").unwrap_err();
        assert!(failed.contains("broken"), "{failed}");
        let Ok(Shows::Ansi { bytes, line }) = shows("allowed") else { panic!("not what a program printed") };
        assert!(String::from_utf8_lossy(&bytes).contains("fine") && line == Some(1));
        // What goes wrong is said, and the next row is still answered.
        assert!(shows("error").unwrap_err().contains("it went wrong"));
        assert!(shows("typo").unwrap_err().contains("colour"));
        assert!(shows("odd").unwrap_err().contains("not a table"));
        assert_eq!(shows("anything"), Ok(text("other", None)));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_preview_is_stopped_for_another_row_and_one_that_never_ends_is_ended() {
        let path = file(
            "stopped",
            r#"return { pickers = { P = { type = "FileSystem", preview = { type = "Dynamic", resolve = function(item)
              if item.path == "loop" then while true do end end
              if item.path == "waits" then
                preview.run { args = { "cmd.exe", "/c", "ping -n 30 127.0.0.1 > nul" }, timeout_ms = 60000 }
              end
              if item.path == "slow" then
                local result = preview.run { args = { "cmd.exe", "/c", "ping -n 30 127.0.0.1 > nul" }, timeout_ms = 200 }
                return { type = "Text", text = "not reached" }
              end
              return { type = "Text", text = item.path }
            end } } } }"#,
        );
        let (previewer, wanted) = previewer(&path, "P", true);
        // Another row is rested on while Lua runs, and while a program does.
        for (request, value) in [(1, "loop"), (2, "waits")] {
            wanted.store(request, Ordering::Release);
            let other = wanted.clone();
            let next = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                other.store(request + 10, Ordering::Release);
            });
            let began = Instant::now();
            assert_eq!(previewer.resolve(request, value, value), None);
            assert!(began.elapsed() < Duration::from_secs(5), "{value} was not stopped: {:?}", began.elapsed());
            next.join().unwrap();
        }
        // A program that takes longer than it is given.
        let slow = ask(&previewer, &wanted, 20, "slow", "slow").unwrap_err();
        assert!(slow.contains("stopped, or took too long"), "{slow}");
        // The row after is answered, and a row of files is `item.path`.
        let shown = ask(&previewer, &wanted, 21, "a", "a.txt");
        assert!(matches!(shown, Ok(Shows::Text { text, .. }) if text == "a.txt"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_helpers_are_for_use_in_a_preview_and_a_picker_without_one_has_no_previewer() {
        let path = file(
            "helpers",
            r#"local early = pcall(preview.run, { args = { "cmd.exe", "/c", "echo" } })
            return { pickers = {
              Early = { type = "Items", items = nterm.json_parse('[{"text":"' .. tostring(early) .. '","value":"v"}]') },
              None = { type = "Items" },
            } }"#,
        );
        // Run as the file is read, a program is not started.
        let lua = crate::configured::open(&path).unwrap();
        let pickers: mlua::Table = lua.named_registry_value("pickers").unwrap();
        let early: mlua::Table = pickers.get("Early").unwrap();
        let items: Vec<mlua::Table> = early.get("items").unwrap();
        assert_eq!(items[0].get::<_, String>("text").unwrap(), "false");
        let none = Previewer::new(&path, "None", BTreeMap::new(), false, Arc::default()).unwrap();
        let made = none.resolve(0, "t", "v").unwrap();
        assert!(made.is_err_and(|error| error.contains("no longer has a preview")));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

#[cfg(test)]
mod acting_tests {
    use super::*;

    fn previewer(test: &str, text: &str) -> (Previewer, PathBuf) {
        let folder = std::env::temp_dir().join(format!("nterm-acting-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(crate::configured::FILE);
        std::fs::write(&path, text).unwrap();
        let params = [("cwd".to_owned(), folder.to_string_lossy().into_owned())].into_iter().collect();
        (Previewer::new(&path, "P", params, false, Arc::default()).unwrap(), folder)
    }

    #[test]
    fn what_an_action_asks_of_app_is_kept_as_steps_in_order() {
        let (previewer, folder) = previewer(
            "steps",
            r#"return { pickers = { P = { type = "Items", action = function(app, item, params)
              if item.value == "all" then
                app:send_text("cd " .. params.cwd .. "\r")
                app:paste(item.text)
                app:replace_prompt("git switch " .. item.value, 3, 0)
                app:show_toast("Title", "Message", 500)
                app:show_toast({ message = "Only a message" })
                app:close_picker()
              elseif item.value == "ran" then
                local printed = preview.run { args = { "cmd.exe", "/c", "echo", "from a program" } }
                app:send_text(printed.stdout)
              elseif item.value == "next" then
                app:push_picker("Other", { pattern = item.text })
              elseif item.value == "window" then
                app:navigate(item)
              elseif item.value == "wrong" then
                app:send_text("typed before it went wrong")
                error("it went wrong", 0)
              elseif item.value == "loop" then
                while true do end
              elseif item.value == "long" then
                app:show_toast("T", "M", 99999999)
              end
            end } } }"#,
        );
        let acted = previewer.act("the text", "all", None).unwrap();
        let cwd = folder.to_string_lossy().into_owned();
        assert_eq!(
            acted.steps,
            [
                PickerStep::SendText { text: format!("cd {cwd}\r") },
                PickerStep::Paste { text: "the text".into() },
                PickerStep::ReplacePrompt { text: "git switch all".into(), before: 3, after: 0 },
                PickerStep::Toast { title: Some("Title".into()), message: "Message".into(), duration_ms: Some(500) },
                PickerStep::Toast { title: None, message: "Only a message".into(), duration_ms: None },
            ]
        );
        assert!(acted.close);
        // It may run a program of this machine, as a preview may.
        let acted = previewer.act("t", "ran", None).unwrap();
        assert!(matches!(&acted.steps[..], [PickerStep::SendText { text }] if text.contains("from a program")));
        assert!(!acted.close);
        let acted = previewer.act("found", "next", None).unwrap();
        let with = [("pattern".to_owned(), "found".to_owned())].into_iter().collect();
        assert_eq!(acted.steps, [PickerStep::Picker { name: "Other".into(), params: with }]);
        // What only a window has says so, and nothing is done.
        assert!(previewer.act("t", "window", None).unwrap_err().contains("app:navigate is not something the daemon has"));
        // Of an action that goes wrong, nothing it asked for is done.
        assert!(previewer.act("t", "wrong", None).unwrap_err().contains("it went wrong"));
        assert!(previewer.act("t", "long", None).unwrap_err().contains("60000"));
        // One row's steps are not the next's.
        assert_eq!(previewer.act("t", "none", None).unwrap(), Acted::default());
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_picker_with_no_action_has_none_to_run_and_app_is_for_actions() {
        let (previewer, folder) = previewer(
            "none",
            r#"return { pickers = { P = { type = "Items", preview = { type = "Dynamic", resolve = function(item)
              return { type = "Text", text = item.value }
            end } } } }"#,
        );
        assert!(previewer.act("t", "v", None).unwrap_err().contains("no longer says what is done"));
        let _ = std::fs::remove_dir_all(folder);
    }
}

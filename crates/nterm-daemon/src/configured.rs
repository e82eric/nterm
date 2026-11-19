//! The pickers of `nterm-daemon.lua`: a file of the machine the daemon is
//! on, in which its user says what pickers there are beside the daemon's
//! own. It is written as the pickers of a window's `nterm.lua` are, and a
//! program is only ever run because this file names it: a UI, or a program
//! in a shell, says which picker and gives it text, and no more.
//!
//! The file is read when a picker of it is first asked for, and again when
//! it has changed since.

use mlua::{Lua, LuaSerdeExt};
use nterm_session::protocol::PickerBinding;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// The file's name, in the user's own folder or beside the daemon.
pub const FILE: &str = "nterm-daemon.lua";
/// How long the file may take to say what its pickers are.
const MOST_TIME: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 5000 });
/// The most pickers a file has, and rows a picker of fixed rows has.
const MOST_PICKERS: usize = 1000;
const MOST_ITEMS: usize = 100_000;

/// Where a picker is shown. Only `Cursor` moves one the daemon runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub(crate) enum Position {
    Top,
    Center,
    #[default]
    Bottom,
    Cursor,
    FullPane,
}

/// The order of the rows that match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub(crate) enum Sort {
    #[default]
    #[serde(alias = "score")]
    Score,
    #[serde(rename = "Source", alias = "SourceOrder", alias = "source")]
    SourceOrder,
}

/// One row of a picker of fixed rows: what it shows, and what it is.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Item {
    pub(crate) text: String,
    pub(crate) value: String,
}

/// What every kind of picker may say of itself.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Presentation {
    pub(crate) position: Position,
    pub(crate) sort: Sort,
    pub(crate) initial_query: Option<String>,
    pub(crate) preview_visible: bool,
    /// Read as a window's file has them, and of no use to the daemon: a
    /// window lays its pages out itself.
    scope: Option<String>,
    result_rows: Option<usize>,
}
impl Default for Presentation {
    fn default() -> Self {
        Self {
            position: Position::default(),
            sort: Sort::default(),
            initial_query: None,
            preview_visible: true,
            scope: None,
            result_rows: None,
        }
    }
}

/// What a picker lists, with the names a window's file gives the same.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Lists {
    /// The lines a program prints. An argument that is `{name}` is the text
    /// the picker was opened with under that name.
    Command {
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        line_continuation: Option<String>,
        #[serde(default)]
        success_exit_codes: Option<Vec<i32>>,
        #[serde(default)]
        unique: Option<bool>,
        #[serde(default)]
        enter_action: Option<String>,
        /// The names of the columns each line it prints is split into,
        /// which then show under those names, lined up.
        #[serde(default)]
        columns: Option<Vec<String>>,
        /// What a line is split at: a tab, unless this says.
        #[serde(default)]
        delimiter: Option<String>,
    },
    /// The lines of a file, the last first with `reverse`.
    File {
        path: String,
        #[serde(default)]
        reverse: bool,
        #[serde(default)]
        line_continuation: Option<String>,
        #[serde(default)]
        unique: Option<bool>,
        #[serde(default)]
        enter_action: Option<String>,
    },
    /// The rows written in the file, or with none written, those the
    /// program that opens it gives, as to the daemon's own `items`.
    Items {
        #[serde(default)]
        items: Option<Vec<Item>>,
        #[serde(default)]
        enter_action: Option<String>,
    },
    /// The files and folders under `roots`, or under the folder the picker
    /// was opened in with none.
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
        relative: bool,
        #[serde(default)]
        layout: Option<String>,
        #[serde(default)]
        enter_action: Option<String>,
    },
}

/// The most keys one picker has.
const MOST_KEYS: usize = 32;

/// A picker of the file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Defined {
    pub(crate) presentation: Presentation,
    pub(crate) lists: Lists,
    /// Whether the file says what shows for a row of it: see `previewing`.
    pub(crate) preview: bool,
    /// Whether it says what is done with the row chosen: its `action`.
    pub(crate) action: bool,
    /// Its own keys, each with a function of the file to run.
    pub(crate) keys: Vec<PickerBinding>,
    /// What it was called with, of one that is a function: its previews
    /// and actions are run in a Lua that calls it with the same.
    pub(crate) context: Option<serde_json::Value>,
}

/// The fields of a picker that are for every kind.
const COMMON: [&str; 6] = ["position", "sort", "initial_query", "preview_visible", "scope", "result_rows"];
/// The fields that are functions, or have them: what a row's preview is,
/// and what is done with the row chosen. The daemon does not run them yet,
/// and a picker that has them is one all the same.
const NOT_YET: [&str; 3] = ["action", "preview", "keys"];
/// The one kind of preview the daemon runs: a function that says what
/// shows for a row.
pub(crate) const PREVIEW: &str = "Dynamic";

/// What was read of the file: its pickers, or for one that could not be
/// one, why not.
#[derive(Default)]
struct Read {
    pickers: HashMap<String, Result<Defined, String>>,
    /// Those that are functions, called each time one is opened.
    functions: std::collections::HashSet<String>,
    /// Why the file gave no pickers at all.
    error: Option<String>,
}

/// The file, as last read.
pub(crate) struct Configured {
    /// The one file to read, in place of looking for it.
    named: Option<PathBuf>,
    read: Mutex<(Option<(PathBuf, SystemTime)>, Read)>,
}

impl Configured {
    pub(crate) fn new(named: Option<PathBuf>) -> Self {
        Self { named, read: Mutex::default() }
    }

    /// The first there is of: the file named, the one in the user's own
    /// folder, the one beside the daemon.
    fn path(&self) -> Option<PathBuf> {
        if let Some(named) = &self.named {
            return Some(named.clone());
        }
        let own = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
        let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
        [own, beside].into_iter().flatten().map(|folder| folder.join(FILE)).find(|path| path.is_file())
    }

    /// The file last read.
    pub(crate) fn file(&self) -> Option<PathBuf> {
        self.read.lock().unwrap().0.as_ref().map(|(path, _)| path.clone())
    }

    /// The picker called `name`: none when the file has no such picker, or
    /// there is no file.
    #[cfg(test)]
    pub(crate) fn picker(&self, name: &str) -> Result<Option<Defined>, String> {
        self.picker_for(name, &|| serde_json::Value::Null)
    }

    /// As `picker`, for a pane: `context` is what is at its cursor, read
    /// only for a picker that is a function.
    pub(crate) fn picker_for(
        &self,
        name: &str,
        context: &dyn Fn() -> serde_json::Value,
    ) -> Result<Option<Defined>, String> {
        let at = self.path().and_then(|path| {
            let changed = std::fs::metadata(&path).and_then(|file| file.modified()).ok()?;
            Some((path, changed))
        });
        let mut read = self.read.lock().unwrap();
        if read.0 != at {
            read.1 = match &at {
                Some((path, _)) => {
                    let read = Read::of(path);
                    let bad = read.pickers.values().filter(|picker| picker.is_err()).count();
                    crate::log::log(format_args!(
                        "{}: {} pickers, {bad} of them not understood{}",
                        path.display(),
                        read.pickers.len() + read.functions.len(),
                        read.error.as_ref().map(|error| format!(": {error}")).unwrap_or_default()
                    ));
                    read
                }
                None => Read::default(),
            };
            read.0 = at;
        }
        if read.1.functions.contains(name) {
            let Some((path, _)) = read.0.clone() else { return Ok(None) };
            drop(read);
            return called(&path, name, context()).map(Some).map_err(|why| format!("{FILE}: pickers.{name}: {why}"));
        }
        match (read.1.pickers.get(name), &read.1.error) {
            (Some(Ok(defined)), _) => Ok(Some(defined.clone())),
            (Some(Err(why)), _) => Err(format!("{FILE}: pickers.{name}: {why}")),
            (None, Some(error)) => Err(format!("no picker named {name}, and {FILE} could not be read: {error}")),
            (None, None) => Ok(None),
        }
    }
}

/// Run the file, leaving its `pickers` in Lua's registry under that name.
/// A file that never ends does not hold the daemon.
pub(crate) fn open(path: &Path) -> Result<Lua, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let lua = Lua::new();
    modules(&lua, path).map_err(|error| error.to_string())?;
    crate::previewing::helpers(&lua).map_err(|error| error.to_string())?;
    let until = Instant::now() + MOST_TIME;
    lua.set_hook(mlua::HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
        if Instant::now() > until {
            return Err(mlua::Error::RuntimeError("it took too long".into()));
        }
        Ok(())
    });
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    {
        let returned: mlua::Value =
            lua.load(&text).set_name(format!("@{name}")).eval().map_err(|error| short(&error))?;
        let mlua::Value::Table(config) = returned else {
            return Err("it does not return a table".into());
        };
        let pickers: Option<mlua::Table> = config.get("pickers").map_err(|_| "pickers is not a table".to_owned())?;
        lua.set_named_registry_value("pickers", pickers).map_err(|error| short(&error))?;
    }
    lua.remove_hook();
    Ok(lua)
}

/// The picker `name` of a file that was run: its table. One that is a
/// function is called for it with `context`, what is at the cursor of the
/// pane it opens over, and gives the table. A function that never ends
/// does not hold the daemon.
pub(crate) fn table_of<'lua>(
    lua: &'lua Lua,
    name: &str,
    context: Option<&serde_json::Value>,
) -> Result<mlua::Table<'lua>, String> {
    let pickers: Option<mlua::Table> = lua.named_registry_value("pickers").map_err(|error| short(&error))?;
    let picker: mlua::Value = match pickers {
        Some(pickers) => pickers.get(name).map_err(|error| short(&error))?,
        None => mlua::Value::Nil,
    };
    match picker {
        mlua::Value::Table(table) => Ok(table),
        mlua::Value::Function(function) => {
            let given = lua.to_value(context.unwrap_or(&serde_json::Value::Null)).map_err(|error| short(&error))?;
            let given = match given {
                mlua::Value::Table(given) => given,
                _ => lua.create_table().map_err(|error| short(&error))?,
            };
            let until = Instant::now() + MOST_TIME;
            lua.set_hook(mlua::HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
                if Instant::now() > until {
                    return Err(mlua::Error::RuntimeError("it took too long".into()));
                }
                Ok(())
            });
            let returned = function.call::<_, mlua::Value>(given);
            lua.remove_hook();
            match returned.map_err(|error| short(&error))? {
                mlua::Value::Table(table) => Ok(table),
                _ => Err("its function does not return a picker's table".into()),
            }
        }
        mlua::Value::Nil => Err("the file no longer has this picker".into()),
        _ => Err("it is not a table".into()),
    }
}

/// The picker `name` of `path`, which is a function: what it gives for
/// `context`. The file is run for it, so what the function keeps is the
/// one opening's alone.
fn called(path: &Path, name: &str, context: serde_json::Value) -> Result<Defined, String> {
    let lua = open(path)?;
    let table = table_of(&lua, name, Some(&context))?;
    let mut defined = define(&lua, mlua::Value::Table(table))?;
    defined.context = Some(context);
    Ok(defined)
}

impl Read {
    fn of(path: &Path) -> Self {
        Self::run(path).unwrap_or_else(|error| Self { error: Some(error), ..Default::default() })
    }

    fn run(path: &Path) -> Result<Self, String> {
        let lua = open(path)?;
        let pickers: Option<mlua::Table> = lua.named_registry_value("pickers").map_err(|error| short(&error))?;
        let mut read = Self::default();
        let Some(pickers) = pickers else { return Ok(read) };
        for pair in pickers.pairs::<String, mlua::Value>() {
            let (name, value) = pair.map_err(|_| "a picker's name is not text".to_owned())?;
            if read.pickers.len() == MOST_PICKERS {
                return Err(format!("it has more than {MOST_PICKERS} pickers"));
            }
            if matches!(value, mlua::Value::Function(_)) && !crate::picker::BUILT_IN.contains(&name.as_str()) {
                read.functions.insert(name);
                continue;
            }
            let defined = match crate::picker::BUILT_IN.contains(&name.as_str()) {
                true => Err("the daemon has a picker of that name of its own".to_owned()),
                false => define(&lua, value),
            };
            read.pickers.insert(name, defined);
        }
        Ok(read)
    }
}

/// An error of Lua's, without the lines of where in Lua's own code it was.
pub(crate) fn short(error: &mlua::Error) -> String {
    let first = error.to_string().lines().next().unwrap_or_default().to_owned();
    // What went wrong, without Lua's name for the kind of error it is.
    first.strip_prefix("runtime error: ").map(str::to_owned).unwrap_or(first)
}

/// Modules are found beside the file and in what is installed with the
/// daemon (`share/nterm/lua`, beside or above it), and nowhere else: not in
/// the folder the daemon runs in. None is of machine code.
fn modules(lua: &Lua, config: &Path) -> mlua::Result<()> {
    let mut folders = Vec::new();
    if let Some(folder) = config.parent().and_then(|folder| std::path::absolute(folder).ok()) {
        folders.push(folder);
    }
    folders.extend(installed().into_iter().map(|share| share.join("lua")));
    let path: Vec<String> = folders
        .iter()
        .filter_map(|folder| folder.to_str())
        // `;` is between the places, and `?` stands for the module's name.
        .filter(|folder| !folder.contains([';', '?']))
        .flat_map(|folder| [format!("{folder}\\?.lua"), format!("{folder}\\?\\init.lua")])
        .collect();
    let package: mlua::Table = lua.globals().get("package")?;
    package.set("path", path.join(";"))?;
    package.set("cpath", "")
}

/// Where what is installed with the daemon may be: `share\nterm` beside it
/// (deploy.ps1) or above it (build.ps1's `out\bin` and `out\share`).
pub(crate) fn installed() -> Vec<std::path::PathBuf> {
    let Some(folder) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) else {
        return Vec::new();
    };
    [Some(folder.as_path()), folder.parent()]
        .into_iter()
        .flatten()
        .map(|root| root.join("share").join("nterm"))
        .collect()
}

fn define(lua: &Lua, value: mlua::Value) -> Result<Defined, String> {
    let table = match value {
        mlua::Value::Table(table) => table,
        mlua::Value::Function(_) => return Err("a picker that is a function is not one the daemon runs yet".into()),
        _ => return Err("it is not a table".into()),
    };
    // What shows for a row is a function's to say, run when a row is
    // rested on: see `previewing`.
    let preview = match table.raw_get::<_, mlua::Value>("preview").map_err(|error| short(&error))? {
        mlua::Value::Nil => false,
        mlua::Value::Table(preview) => {
            let kind: Option<String> = preview.raw_get("type").ok();
            if kind.as_deref() != Some(PREVIEW) {
                return Err(format!(
                    "the daemon runs no preview of type {}: only {PREVIEW}",
                    kind.as_deref().unwrap_or("none")
                ));
            }
            if !matches!(preview.raw_get::<_, mlua::Value>("resolve"), Ok(mlua::Value::Function(_))) {
                return Err(format!("a {PREVIEW} preview has a function, resolve"));
            }
            true
        }
        _ => return Err("preview is not a table".into()),
    };
    // What is done with the row chosen is a function's to say too.
    let action = match table.raw_get::<_, mlua::Value>("action").map_err(|error| short(&error))? {
        mlua::Value::Nil => false,
        mlua::Value::Function(_) => true,
        _ => return Err("action is not a function".into()),
    };
    // Its own keys: what each is, and that it has a function to run.
    let keys = match table.raw_get::<_, mlua::Value>("keys").map_err(|error| short(&error))? {
        mlua::Value::Nil => Vec::new(),
        mlua::Value::Table(keys) => {
            let mut read = Vec::new();
            for (at, key) in keys.sequence_values::<mlua::Table>().enumerate() {
                let place = format!("keys[{}]", at + 1);
                let key = key.map_err(|_| format!("{place} is not a table"))?;
                for pair in key.clone().pairs::<String, mlua::Value>() {
                    let (field, _) = pair.map_err(|_| format!("{place} has a field that is not named"))?;
                    if !["key", "mods", "action", "description"].contains(&field.as_str()) {
                        return Err(format!("{place} has {field}, which a key has not"));
                    }
                }
                if !matches!(key.raw_get::<_, mlua::Value>("action"), Ok(mlua::Value::Function(_))) {
                    return Err(format!("{place}.action is not a function"));
                }
                let named = |field: &str| -> Result<Option<String>, String> {
                    key.raw_get::<_, Option<String>>(field).map_err(|_| format!("{place}.{field} is not text"))
                };
                read.push(PickerBinding {
                    key: named("key")?.filter(|key| !key.is_empty()).ok_or(format!("{place} has no key"))?,
                    mods: named("mods")?.unwrap_or_default(),
                    description: named("description")?.unwrap_or_default(),
                });
            }
            if read.len() > MOST_KEYS {
                return Err(format!("it has more than {MOST_KEYS} keys"));
            }
            read
        }
        _ => return Err("keys is not a table".into()),
    };
    // A copy without what the daemon does not run yet: the file's own
    // table is left as it was written.
    let copy = lua.create_table().map_err(|error| short(&error))?;
    for pair in table.pairs::<mlua::Value, mlua::Value>() {
        let (key, value) = pair.map_err(|error| short(&error))?;
        let not_yet = matches!(&key, mlua::Value::String(key) if NOT_YET.contains(&key.to_str().unwrap_or_default()));
        if !not_yet {
            copy.raw_set(key, value).map_err(|error| short(&error))?;
        }
    }
    if matches!(copy.raw_get::<_, mlua::Value>("items"), Ok(mlua::Value::Function(_))) {
        return Err("rows that a function gives are not ones the daemon runs yet".into());
    }
    let read: serde_json::Value = lua.from_value(mlua::Value::Table(copy)).map_err(|error| short(&error))?;
    let serde_json::Value::Object(mut fields) = read else {
        return Err("it is not a table of named fields".into());
    };
    // Lua has one kind of table: an empty one is read as a list of nothing
    // where a list is what is wanted.
    for list in ["items", "args", "roots", "success_exit_codes"] {
        if let Some(value) = fields.get_mut(list).filter(|value| value.as_object().is_some_and(|map| map.is_empty())) {
            *value = serde_json::Value::Array(Vec::new());
        }
    }
    let mut common = serde_json::Map::new();
    for field in COMMON {
        if let Some(value) = fields.remove(field) {
            common.insert(field.into(), value);
        }
    }
    match fields.get("type").and_then(serde_json::Value::as_str) {
        Some("Command" | "File" | "Items" | "FileSystem") => {}
        Some(other) => return Err(format!("the daemon runs no picker of type {other}")),
        None => return Err("it has no type".into()),
    }
    let presentation: Presentation = serde_json::from_value(common.into()).map_err(|error| error.to_string())?;
    let lists: Lists = serde_json::from_value(fields.into()).map_err(|error| error.to_string())?;
    match &lists {
        Lists::Command { args, .. } if args.first().is_none_or(String::is_empty) => {
            return Err("args names no program to run".into());
        }
        // Which program is run is for the file to say, and never for what
        // the picker is opened with.
        Lists::Command { args, .. } if args[0].starts_with('{') && args[0].ends_with('}') => {
            return Err("the program to run is not one to be given when the picker is opened".into());
        }
        Lists::Items { items: Some(items), .. } if items.len() > MOST_ITEMS => {
            return Err(format!("it has more than {MOST_ITEMS} rows"));
        }
        Lists::FileSystem { files_only: true, directories_only: true, .. } => {
            return Err("files_only and directories_only are not both to be set".into());
        }
        _ => {}
    }
    Ok(Defined { presentation, lists, preview, action, keys, context: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(test: &str, text: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("nterm-configured-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(FILE);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn the_pickers_of_the_file_are_read_as_those_of_a_windows_are() {
        let path = file(
            "read",
            r#"
            local home = os.getenv("USERPROFILE") or "none"
            return { pickers = {
              Commits = { type = "Command", args = {"git", "log", "--oneline", "{count}"}, cwd = home,
                          success_exit_codes = {0, 1}, unique = true, sort = "SourceOrder", position = "Cursor",
                          preview_visible = false, initial_query = "fix",
                          action = function(app, item) end, preview = { type = "Dynamic", resolve = function() end },
                          keys = { { key = "Enter", mods = "CTRL", action = function() end } } },
              Typed = { type = "File", path = "{history}", reverse = true, line_continuation = "`", unique = true },
              Fixed = { type = "Items", items = { { text = "One", value = "1" }, { text = "Two", value = "2" } } },
              Here = { type = "FileSystem", max_depth = 3, files_only = true, relative = true, layout = "NameDir" },
            } }
            "#,
        );
        let configured = Configured::new(Some(path.clone()));
        let commits = configured.picker("Commits").unwrap().unwrap();
        assert_eq!(
            commits.presentation,
            Presentation {
                position: Position::Cursor,
                sort: Sort::SourceOrder,
                initial_query: Some("fix".into()),
                preview_visible: false,
                ..Default::default()
            }
        );
        let Lists::Command { args, cwd, success_exit_codes, unique, .. } = commits.lists else { panic!() };
        assert_eq!(args, ["git", "log", "--oneline", "{count}"]);
        assert_eq!((cwd, success_exit_codes, unique), (std::env::var("USERPROFILE").ok(), Some(vec![0, 1]), Some(true)));
        let typed = configured.picker("Typed").unwrap().unwrap();
        assert!(matches!(typed.lists, Lists::File { reverse: true, unique: Some(true), .. }));
        assert_eq!(typed.presentation, Presentation::default());
        let Lists::Items { items: Some(items), .. } = configured.picker("Fixed").unwrap().unwrap().lists else { panic!() };
        assert_eq!(items[1], Item { text: "Two".into(), value: "2".into() });
        let here = configured.picker("Here").unwrap().unwrap();
        assert!(matches!(here.lists, Lists::FileSystem { max_depth: Some(3), files_only: true, relative: true, .. }));
        // One it does not have is none, not an error.
        assert_eq!(configured.picker("Other").unwrap(), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_picker_that_cannot_be_one_says_why_and_the_rest_are_still_pickers() {
        let path = file(
            "bad",
            r#"return { pickers = {
              Good = { type = "Items", items = {} },
              items = { type = "Items" },
              Made = function(context) return { type = "Scrollback" } end,
              Asked = { type = "Command", args = {"{program}", "x"} },
              None = { type = "Command", args = {} },
              Typo = { type = "Command", args = {"git"}, cwdd = "x" },
              Window = { type = "Scrollback" },
              Rows = { type = "Items", items = function() return {} end },
              Both = { type = "FileSystem", files_only = true, directories_only = true },
            } }"#,
        );
        let configured = Configured::new(Some(path.clone()));
        assert!(configured.picker("Good").unwrap().is_some());
        for (name, why) in [
            ("items", "of its own"),
            ("Made", "type Scrollback"),
            ("Asked", "not one to be given"),
            ("None", "no program"),
            ("Typo", "cwdd"),
            ("Window", "type Scrollback"),
            ("Rows", "a function gives"),
            ("Both", "not both"),
        ] {
            let error = configured.picker(name).unwrap_err();
            assert!(error.contains(why) && error.contains(&format!("pickers.{name}")), "{name}: {error}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_file_that_cannot_be_read_says_why_and_is_read_again_once_changed() {
        let path = file("again", "return { pickers = { One = { type = 'Items' } } ");
        let configured = Configured::new(Some(path.clone()));
        let error = configured.picker("One").unwrap_err();
        assert!(error.contains("could not be read") && error.contains("nterm-daemon.lua"), "{error}");

        let rewrite = |text: &str| {
            // A file's time changes by no less than some of a second.
            let before = std::fs::metadata(&path).unwrap().modified().unwrap();
            loop {
                std::fs::write(&path, text).unwrap();
                if std::fs::metadata(&path).unwrap().modified().unwrap() != before {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        rewrite("return { pickers = { One = { type = 'Items' } } }");
        assert!(configured.picker("One").unwrap().is_some());
        rewrite("return 5");
        assert!(configured.picker("One").unwrap_err().contains("does not return a table"));
        rewrite("while true do end");
        assert!(configured.picker("One").unwrap_err().contains("took too long"));
        // With no file there are no pickers, and that is no error.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(configured.picker("One").unwrap(), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_example_file_has_pickers_that_are_all_understood() {
        // With the modules that are installed with the daemon beside it,
        // as they are found beside any file.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = file("example", &std::fs::read_to_string(root.join("nterm-daemon.example.lua")).unwrap());
        let modules = path.parent().unwrap().join("nterm");
        std::fs::create_dir_all(&modules).unwrap();
        for module in ["files.lua", "git.lua", "history.lua", "ripgrep.lua"] {
            std::fs::copy(root.join("lua/nterm").join(module), modules.join(module)).unwrap();
        }
        let configured = Configured::new(Some(path.clone()));
        for name in ["Commits", "Ripgrep", "History", "Files", "Directories", "Near", "Servers", "GitCommits", "GitBranches", "GitStatus"] {
            assert!(configured.picker(name).unwrap().is_some(), "{name}");
        }
        // The pickers of git have columns, and say what shows for a row.
        let commits = configured.picker("GitCommits").unwrap().unwrap();
        let Lists::Command { columns, .. } = &commits.lists else { panic!("not a command") };
        assert_eq!(columns.as_ref().map(Vec::len), Some(5));
        assert!(commits.preview && commits.action);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn modules_are_found_beside_the_file_and_not_where_the_daemon_runs() {
        let path = file("modules", "local mine = require('mine') return { pickers = mine }");
        std::fs::write(path.parent().unwrap().join("mine.lua"), "return { M = { type = 'Items' } }").unwrap();
        let configured = Configured::new(Some(path.clone()));
        assert!(configured.picker("M").unwrap().is_some());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

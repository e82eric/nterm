//! The Lua states that run preview resolvers, off the UI thread.
use mlua::{Lua, LuaSerdeExt};
use nfm_egui::Cancellation;
use nfm_preview_file::resolved::PreviewPlan;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::Duration,
};

/// Config source text, its Lua chunk name, and its `package.path`.
#[derive(Clone)]
pub(crate) struct ConfigSource(pub String, pub String, pub String);

/// The `package.path` for a config file: modules beside it, then those
/// installed with the app (`share/nterm/lua`, beside or above the
/// executable). Absolute only: the current directory is never searched, so a
/// file in whatever directory the app was started from cannot replace a module.
pub(crate) fn module_path(config: &std::path::Path) -> String {
    let mut directories = Vec::new();
    if let Some(directory) = config
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .and_then(|directory| std::path::absolute(directory).ok())
    {
        directories.push(directory);
    }
    if let Some(executable) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    {
        for root in [Some(executable.as_path()), executable.parent()]
            .into_iter()
            .flatten()
        {
            directories.push(root.join("share").join("nterm").join("lua"));
        }
    }
    if cfg!(test) {
        directories.push(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lua"));
    }
    directories
        .iter()
        // `;` separates templates and `?` is the module placeholder.
        .filter_map(|directory| directory.to_str())
        .filter(|directory| !directory.contains([';', '?']))
        .flat_map(|directory| {
            let directory = std::path::Path::new(directory);
            [
                directory.join("?.lua"),
                directory.join("?").join("init.lua"),
            ]
        })
        .map(|template| template.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(";")
}

/// Load Lua modules only from `path`, and never native (C) modules.
pub(crate) fn set_module_path(lua: &Lua, path: &str) -> mlua::Result<()> {
    let package: mlua::Table = lua.globals().get("package")?;
    package.set("path", path)?;
    package.set("cpath", "")
}
#[derive(Clone)]
pub(crate) struct FactoryContext(pub serde_json::Value);
#[derive(Clone)]
struct RequestContext(Cancellation, usize);

#[derive(serde::Serialize)]
pub(crate) struct DirectoryItem {
    name: String,
    is_directory: bool,
}

pub(crate) fn directory_items(
    path: &std::path::Path,
    limit: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<DirectoryItem>>, String> {
    let check = || {
        if cancelled() {
            Err("Directory preview cancelled".to_owned())
        } else {
            Ok(())
        }
    };
    check()?;
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Ok(None);
    }
    let mut items = Vec::new();
    let mut bytes = 0usize;
    for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
        check()?;
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        bytes = bytes.saturating_add(name.len() + 2);
        if bytes > limit || items.len() >= 100_000 {
            return Err("Directory listing exceeds preview limits".into());
        }
        items.push(DirectoryItem {
            name,
            is_directory: entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir(),
        });
    }
    check()?;
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Some(items))
}
// Bound outstanding jobs, including jobs whose OS process creation has not
// returned yet. Cancellation releases Lua without abandoning child cleanup.
static COMMAND_JOBS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn command_job<T: Send + 'static>(
    cancelled: impl Fn() -> bool,
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    use std::sync::atomic::Ordering;
    loop {
        if cancelled() {
            return Err("Preview command cancelled or timed out".into());
        }
        if COMMAND_JOBS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < 4).then_some(count + 1)
            })
            .is_ok()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    if let Err(error) = std::thread::Builder::new()
        .name("preview-command".into())
        .spawn(move || {
            struct Slot;
            impl Drop for Slot {
                fn drop(&mut self) {
                    COMMAND_JOBS.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _slot = Slot;
            let _ = tx.send(job());
        })
    {
        COMMAND_JOBS.fetch_sub(1, Ordering::AcqRel);
        return Err(error.to_string());
    }
    loop {
        if cancelled() {
            return Err("Preview command cancelled or timed out".into());
        }
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(result) => return Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Preview command worker stopped".into());
            }
        }
    }
}
pub(crate) fn install_ui_api(lua: &Lua) -> mlua::Result<()> {
    let helpers = lua.create_table()?;
    for name in ["run", "directory_items"] {
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
    resolver: Arc<ResolverKey>,
    text: String,
    value: String,
    filesystem: bool,
    params: BTreeMap<String, String>,
    max_bytes: usize,
    max_lines: usize,
    cancel: Cancellation,
    reply: mpsc::Sender<Result<Option<PreviewPlan>, String>>,
}

/// Which preview resolver a request is for, as a lane needs it to find it.
struct ResolverKey {
    /// Its own for each `Worker` made: a picker a factory makes is made
    /// again each time it opens, with another resolver.
    id: u64,
    /// The picker whose resolver it is.
    name: String,
    /// What a factory was given to make that picker, if one made it.
    context: Option<FactoryContext>,
}

/// The most lanes a pool runs.
const LANES: usize = 4;

/// What the lanes of a pool wait on.
struct Queue {
    requests: VecDeque<Request>,
    /// Lanes running, and those of them waiting for a request.
    lanes: usize,
    idle: usize,
    /// The pool has gone: a lane ends when it finds nothing waiting.
    closed: bool,
}

struct Shared {
    source: ConfigSource,
    queue: Mutex<Queue>,
    ready: Condvar,
}

/// The lanes that run a configuration's Lua preview resolvers, for all of its
/// pickers. A lane is a thread with a Lua state of its own, in which it has
/// run the configuration again; a resolver is looked up there by its picker's
/// name when first asked for.
///
/// One lane does the work. Another is started only when a request finds every
/// lane busy, up to `LANES`: a resolver blocked in a native io call keeps only
/// its lane, and a newer selection runs on another. A configuration whose
/// previews are never shown starts none.
pub(crate) struct Pool(Arc<Shared>);

/// One picker's use of a pool.
#[derive(Clone)]
pub(crate) struct Worker {
    pool: Arc<Pool>,
    resolver: Arc<ResolverKey>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_command_releases_caller_before_job_finishes() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = cancelled.clone();
        let caller = std::thread::spawn(move || {
            command_job(
                || flag.load(std::sync::atomic::Ordering::Acquire),
                move || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    done_tx.send(()).unwrap();
                },
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        // Joining must not require releasing the blocked job.
        assert!(caller.join().unwrap().is_err());
        assert_eq!(command_job(|| false, || 42).unwrap(), 42);
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    /// A scratch directory, and it quoted for Lua source.
    fn scratch(name: &str) -> (std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!("nterm-preview-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let quoted = serde_json::to_string(&root.to_string_lossy().replace('\\', "/")).unwrap();
        (root, quoted)
    }
    fn files(root: &std::path::Path, prefix: &str) -> usize {
        std::fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .count()
    }
    fn resolve(worker: &Worker, text: &str) -> Result<Option<PreviewPlan>, String> {
        worker.resolve(
            text.into(),
            text.into(),
            false,
            BTreeMap::new(),
            4096,
            100,
            Cancellation::never(),
        )
    }

    #[test]
    fn pickers_share_a_lane_that_starts_with_the_first_request() {
        let (root, quoted) = scratch("shared");
        // Each run of the configuration leaves a file, and so does each
        // resolver called, named for its picker.
        let source = format!(
            "local keys=nterm.workspace_keys('ALT'); assert(#keys>0); \
             local root = {quoted}; \
             local function mark(kind) local f = assert(io.open(root .. '/' .. kind .. '-' .. tostring({{}}):gsub('[^%w]', ''), 'w')); f:close() end; \
             mark('config'); \
             local function picker(name) return {{preview={{resolve=function() mark(name); return nil end}}}} end; \
             return {{pickers={{a=picker('a'), b=picker('b')}}, builtin_pickers={{c=picker('c')}}}}"
        );
        let pool = Pool::new(ConfigSource(source, "@shared-test".into(), String::new()));
        let workers = ["a", "b", "builtin.c"].map(|name| pool.worker(name.into(), None));
        // Nothing runs for a preview nobody has asked for.
        assert_eq!((pool.lanes(), files(&root, "config")), (0, 0));
        for _ in 0..2 {
            for worker in &workers {
                assert!(resolve(worker, "item").unwrap().is_none());
            }
        }
        // One after another, they need one lane, which ran the configuration
        // once and found each picker's own resolver.
        assert_eq!((pool.lanes(), files(&root, "config")), (1, 1));
        assert_eq!([files(&root, "a-"), files(&root, "b-"), files(&root, "c-")], [2, 2, 2]);
        // A picker the configuration lacks says so, and the lane goes on.
        assert!(resolve(&pool.worker("missing".into(), None), "item").is_err());
        assert!(resolve(&workers[0], "item").unwrap().is_none());
        assert_eq!(pool.lanes(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_busy_lane_leaves_a_request_to_another_and_lanes_end_with_the_pool() {
        let (root, quoted) = scratch("busy");
        // `slow` spins until a file appears; the instruction hook lets it be
        // cancelled, and nothing else stops it.
        let source = format!(
            "local root = {quoted}; \
             return {{pickers={{ \
               slow={{preview={{resolve=function() \
                 local started = assert(io.open(root .. '/started', 'w')); started:close(); \
                 while true do local f = io.open(root .. '/release'); if f then f:close(); return nil end end \
               end}}}}, \
               fast={{preview={{resolve=function() return nil end}}}}}}}}"
        );
        let pool = Pool::new(ConfigSource(source, "@busy-test".into(), String::new()));
        let slow = pool.worker("slow".into(), None);
        let blocked = std::thread::spawn(move || resolve(&slow, "item"));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !root.join("started").exists() {
            assert!(std::time::Instant::now() < deadline, "the slow resolver did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pool.lanes(), 1);
        // The lane is busy: this one is answered by a second.
        assert!(resolve(&pool.worker("fast".into(), None), "item").unwrap().is_none());
        assert_eq!(pool.lanes(), 2);
        // Now one is free, and no third is started.
        assert!(resolve(&pool.worker("fast".into(), None), "item").unwrap().is_none());
        assert_eq!(pool.lanes(), 2);
        std::fs::write(root.join("release"), "").unwrap();
        assert!(blocked.join().unwrap().unwrap().is_none());

        let shared = pool.0.clone();
        drop(pool);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while shared.queue.lock().unwrap().lanes > 0 {
            assert!(std::time::Instant::now() < deadline, "lanes outlived their pool");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_factory_picker_opened_again_gets_the_resolver_made_for_it() {
        let source = "return {pickers={x=function(context) \
            return {preview={resolve=function() error(context.line) end}} end}}";
        let pool = Pool::new(ConfigSource(source.into(), "@factory-test".into(), String::new()));
        let text = |line: &str| {
            let context = FactoryContext(serde_json::json!({ "line": line }));
            let worker = pool.worker("x".into(), Some(context));
            // What the resolver was made with is in what it fails with.
            resolve(&worker, "item").unwrap_err()
        };
        let (first, second) = (text("first open"), text("second open"));
        assert!(first.contains("first open") && !first.contains("second open"), "{first}");
        assert!(second.contains("second open") && !second.contains("first open"), "{second}");
        assert_eq!(pool.lanes(), 1);
        // Without what the factory was given, it cannot be made.
        assert!(resolve(&pool.worker("x".into(), None), "item").is_err());
    }
}

impl Pool {
    pub(crate) fn new(source: ConfigSource) -> Arc<Self> {
        Arc::new(Self(Arc::new(Shared {
            source,
            queue: Mutex::new(Queue {
                requests: VecDeque::new(),
                lanes: 0,
                idle: 0,
                closed: false,
            }),
            ready: Condvar::new(),
        })))
    }

    /// The pool as the picker called `name` uses it. `context` is what a
    /// factory was given to make the picker, for a lane to make it again.
    pub(crate) fn worker(self: &Arc<Self>, name: String, context: Option<FactoryContext>) -> Worker {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Worker {
            pool: self.clone(),
            resolver: Arc::new(ResolverKey {
                id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                name,
                context,
            }),
        }
    }

    /// Queue a request, starting a lane for it if none is free.
    fn submit(&self, request: Request) -> Result<(), String> {
        let shared = &self.0;
        let mut queue = shared.queue.lock().unwrap();
        // Nobody waits for these any more.
        queue.requests.retain(|waiting| !waiting.cancel.is_cancelled());
        queue.requests.push_back(request);
        if queue.requests.len() > queue.idle && queue.lanes < LANES {
            let lane = shared.clone();
            let started = std::thread::Builder::new()
                .name(format!("lua-preview-{}", queue.lanes))
                .spawn(move || run_lane(&lane));
            match started {
                Ok(_) => queue.lanes += 1,
                // A lane that is running gets to it.
                Err(_) if queue.lanes > 0 => {}
                Err(error) => {
                    queue.requests.pop_back();
                    return Err(format!("Lua preview worker could not start: {error}"));
                }
            }
        }
        shared.ready.notify_one();
        Ok(())
    }

    #[cfg(test)]
    fn lanes(&self) -> usize {
        self.0.queue.lock().unwrap().lanes
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.0.queue.lock().unwrap().closed = true;
        self.0.ready.notify_all();
    }
}

/// Run `work` in `lua` with five seconds for the Lua in it. Running the
/// configuration and finding a resolver have this deadline, apart from any
/// request's.
fn within_deadline<T>(lua: &Lua, work: impl FnOnce() -> mlua::Result<T>) -> mlua::Result<T> {
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
    let result = work();
    lua.remove_hook();
    result
}

/// Run the configuration in a lane's Lua state and keep the table it returns.
fn load_config(lua: &Lua, source: &ConfigSource) -> mlua::Result<mlua::RegistryKey> {
    install_helpers(lua)?;
    set_module_path(lua, &source.2)?;
    within_deadline(lua, || {
        let root: mlua::Table = lua.load(&source.0).set_name(&source.1).eval()?;
        lua.create_registry_value(root)
    })
}

/// A picker's preview resolver in a lane's copy of the configuration.
fn find_resolver(
    lua: &Lua,
    config: &mlua::RegistryKey,
    wanted: &ResolverKey,
) -> mlua::Result<mlua::RegistryKey> {
    within_deadline(lua, || {
        let root: mlua::Table = lua.registry_value(config)?;
        let (group, key) = wanted
            .name
            .strip_prefix("builtin.")
            .map_or(("pickers", wanted.name.as_str()), |key| ("builtin_pickers", key));
        let entries: mlua::Table = root.get(group)?;
        let entry: mlua::Value = entries.get(key)?;
        let definition = match entry {
            mlua::Value::Table(table) => table,
            // Made as the factory made it, so that the resolver has the same
            // closure environment.
            mlua::Value::Function(factory) => factory.call::<_, mlua::Table>(
                lua.to_value(
                    &wanted
                        .context
                        .as_ref()
                        .ok_or_else(|| mlua::Error::external("Missing picker factory context"))?
                        .0,
                )?,
            )?,
            _ => return Err(mlua::Error::external("Missing preview definition")),
        };
        let preview: mlua::Table = definition.get("preview")?;
        let callback: mlua::Function = preview.get("resolve")?;
        lua.create_registry_value(callback)
    })
}

/// A lane: take requests until the pool has gone.
fn run_lane(shared: &Shared) {
    let lua = std::rc::Rc::new(Lua::new());
    let config = load_config(&lua, &shared.source);
    if let Err(error) = &config {
        eprintln!("preview configuration: {error}");
    }
    // By picker name, with the id of the `Worker` it was found for: a picker
    // opened again by its factory replaces the one before.
    let mut resolvers: HashMap<String, (u64, Result<mlua::RegistryKey, String>)> = HashMap::new();
    loop {
        // The queue is let go of before any Lua or host io.
        let request = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if let Some(request) = queue.requests.pop_front() {
                    break request;
                }
                if queue.closed {
                    queue.lanes -= 1;
                    return;
                }
                queue.idle += 1;
                queue = shared.ready.wait(queue).unwrap();
                queue.idle -= 1;
            }
        };
        if request.cancel.is_cancelled() {
            continue;
        }
        let wanted = &request.resolver;
        if resolvers
            .get(&wanted.name)
            .is_none_or(|(id, _)| *id != wanted.id)
        {
            let found = match &config {
                Ok(config) => find_resolver(&lua, config, wanted).map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if let Err(error) = &found {
                eprintln!("picker {} preview initialization: {error}", wanted.name);
            }
            resolvers.insert(wanted.name.clone(), (wanted.id, found));
            // The resolver this one replaced.
            lua.expire_registry_values();
        }
        let result = (|| -> Result<_, String> {
            let entry = resolvers[&wanted.name].1.as_ref().map_err(Clone::clone)?;
            install(&lua, request.cancel.clone(), request.max_bytes).map_err(|e| e.to_string())?;
            let callback: mlua::Function = lua.registry_value(entry).map_err(|e| e.to_string())?;
            let resolver = crate::lua_callbacks::PreviewResolver {
                lua: lua.clone(),
                key: lua
                    .create_registry_value(callback)
                    .map_err(|e| e.to_string())?,
                name: wanted.name.clone(),
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
}

impl Worker {
    /// A pool for one picker alone.
    #[cfg(test)]
    pub(crate) fn new(source: ConfigSource, name: String, context: Option<FactoryContext>) -> Self {
        Pool::new(source).worker(name, context)
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
        self.pool.submit(Request {
            resolver: self.resolver.clone(),
            text,
            value,
            filesystem,
            params,
            max_bytes,
            max_lines,
            cancel: cancel.clone(),
            reply,
        })?;
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
        "directory_items",
        lua.create_function(|lua, path: String| {
            let RequestContext(cancel, limit) = lua
                .app_data_ref::<RequestContext>()
                .ok_or_else(|| {
                    mlua::Error::external("preview.directory_items requires a preview request")
                })?
                .clone();
            let result =
                directory_items(std::path::Path::new(&path), limit, || cancel.is_cancelled());

            match result.map_err(mlua::Error::external)? {
                Some(items) => lua.to_value(&items),
                None => Ok(mlua::Value::Nil),
            }
        })?,
    )?;
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

            let job_cancel = cancel.clone();
            let result = command_job(
                || cancel.is_cancelled() || started.elapsed() >= Duration::from_millis(timeout),
                move || {
                    nfm_preview_file::resolved::run(
                        &command,
                        || {
                            job_cancel.is_cancelled()
                                || started.elapsed() >= Duration::from_millis(timeout)
                        },
                        |_, _| true,
                    )
                },
            )
            .map_err(mlua::Error::external)?;
            let result = result.map_err(mlua::Error::external)?;
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
    let nterm = lua.create_table()?;
    crate::terminal::workspace_ui::install_lua_helpers(lua, &nterm)?;
    nterm.set(
        "json_parse",
        lua.create_function(|lua, text: String| {
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(mlua::Error::external)?;
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
    lua.globals().set("nterm", nterm)?;
    lua.set_named_registry_value("preview.helpers", true)
}

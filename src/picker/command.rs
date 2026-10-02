//! Ghostty config/request adapter for NFM's streaming command source.
use crate::ghostty_ffi::SnapshotCellRange;
use crate::picker::config::CommandOptions;
pub(crate) use nfm_picker_sources::command::CommandValues;
use nfm_search_core::{
    snapshot_store::SnapshotStore,
    store::{PublishingStreamingItemStoreWithPayload, StreamingItemSnapshotWithPayload},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread::JoinHandle,
};
type Items = StreamingItemSnapshotWithPayload<SnapshotCellRange>;

fn resolve(
    options: &CommandOptions,
    params: &BTreeMap<String, String>,
) -> Result<(String, Vec<String>, Option<String>), String> {
    let mut args = Vec::with_capacity(options.args.len());
    for arg in &options.args {
        let resolved =
            if let Some(name) = arg.strip_prefix('{').and_then(|arg| arg.strip_suffix('}')) {
                params
                    .get(name)
                    .ok_or_else(|| format!("missing command picker parameter {name:?}"))?
                    .clone()
            } else {
                arg.clone()
            };
        if resolved.contains('\0') {
            return Err("command picker argument contains NUL".into());
        }
        args.push(resolved);
    }
    if args.first().is_none_or(String::is_empty) {
        return Err("command picker requires an executable".into());
    }
    let program = args.remove(0);
    let cwd = params.get("cwd").or(options.cwd.as_ref()).cloned();
    if cwd.as_ref().is_some_and(|cwd| cwd.contains('\0')) {
        return Err("command picker cwd contains NUL".into());
    }
    Ok((program, args, cwd))
}

pub(crate) fn start(
    options: CommandOptions,
    params: BTreeMap<String, String>,
    store: Arc<SnapshotStore<Items>>,
    cancelled: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    ctx: egui::Context,
    command_values: Option<CommandValues>,
) -> Option<JoinHandle<()>> {
    use nfm_picker_sources::command::{
        self, CommandControl, CommandSink, CommandSourceOptions, CommandSpec,
    };
    let (program, args, cwd) = match resolve(&options, &params) {
        Ok(command) => command,
        Err(message) => {
            crate::error_log::error("picker command", "Command failed", &message);
            *error.lock().unwrap() = Some(message);
            PublishingStreamingItemStoreWithPayload::new(store).complete();
            ctx.request_repaint();
            return None;
        }
    };
    let mut spec = CommandSpec::new(program);
    spec.arguments = args;
    spec.working_directory = cwd.map(std::path::PathBuf::from);
    spec.output_limit = 16 * 1024;
    let mut source = CommandSourceOptions::new(spec);
    source.line_continuation = options.line_continuation;
    source.unique = options.unique;
    if let Some(codes) = options.success_exit_codes {
        source.success_exit_codes = codes;
    }
    let sink = CommandSink {
        store,
        payload: SnapshotCellRange::default(),
        values: command_values,
        error,
    };
    let control = CommandControl {
        cancelled,
        changed: Arc::new(move || ctx.request_repaint()),
        failed: Arc::new(|message| {
            crate::error_log::error("picker command", "Command failed", message)
        }),
    };
    match command::start(source, sink, control) {
        Ok(worker) => Some(worker),
        Err(message) => {
            crate::error_log::error("picker command", "Worker failed", &message);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfm_search_core::store::ItemsSource;

    #[test]
    fn invalid_request_parameters_complete_the_source_without_a_worker() {
        let options = CommandOptions {
            args: vec!["cmd.exe".into(), "{missing}".into()],
            cwd: None,
            line_continuation: None,
            unique: false,
            success_exit_codes: None,
        };
        let store = Arc::new(SnapshotStore::<Items>::new());
        let error = Arc::new(Mutex::new(None));
        assert!(
            start(
                options,
                BTreeMap::new(),
                store.clone(),
                Arc::new(AtomicBool::new(false)),
                error.clone(),
                egui::Context::default(),
                None
            )
            .is_none()
        );
        assert!(store.is_done());
        assert!(error.lock().unwrap().as_ref().unwrap().contains("missing"));
    }

    #[test]
    fn command_adapter_publishes_before_exit_and_cancels_without_stalling() {
        // Use a directly spawned fixture, without inherited descendant pipes.
        let store = Arc::new(SnapshotStore::<Items>::new_append_only());
        let error = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        // Config has no environment override, so use a fixture argument.
        let args = vec![
            std::env::current_exe().unwrap().display().to_string(),
            "--exact".into(),
            "picker::command::tests::argument_fixture".into(),
            "--nocapture".into(),
            "--skip".into(),
            "ghostty-command-child-sentinel".into(),
        ];
        let worker = start(
            CommandOptions {
                args,
                cwd: None,
                line_continuation: None,
                unique: false,
                success_exit_codes: None,
            },
            BTreeMap::new(),
            store.clone(),
            cancelled.clone(),
            error.clone(),
            egui::Context::default(),
            None,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if store.snapshot().is_some_and(|s| {
                (0..s.len()).any(|i| s.get_string_lossy(i, &mut Vec::new()) == "adapter-first")
            }) {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!store.is_done());
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            worker.join().unwrap();
            done.send(()).unwrap();
        });
        finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(store.is_done());
        assert!(error.lock().unwrap().is_none());
    }

    #[test]
    fn argument_fixture() {
        if std::env::args().any(|arg| arg == "ghostty-command-child-sentinel") {
            use std::io::Write;
            println!("adapter-first");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }
    #[test]
    #[cfg(windows)]
    fn generic_exit_one_is_failure_and_retains_command_stderr() {
        let store = Arc::new(SnapshotStore::new());
        let error = Arc::new(Mutex::new(None));
        let worker = start(
            CommandOptions {
                args: vec![
                    "cmd.exe".into(),
                    "/C".into(),
                    "echo diagnostic 1>&2 & exit /b 1".into(),
                ],
                cwd: None,
                line_continuation: None,
                unique: false,
                success_exit_codes: None,
            },
            BTreeMap::new(),
            store,
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
            None,
        )
        .unwrap();
        worker.join().unwrap();
        let message = error.lock().unwrap().clone().unwrap();
        assert!(message.contains("diagnostic"));
        assert!(message.contains("cmd.exe"));
        assert!(message.contains("exited"));
    }
    #[test]
    #[cfg(windows)]
    fn configured_exit_one_can_be_accepted() {
        let error = Arc::new(Mutex::new(None));
        start(
            CommandOptions {
                args: vec!["cmd.exe".into(), "/C".into(), "exit /b 1".into()],
                cwd: None,
                line_continuation: None,
                unique: false,
                success_exit_codes: Some(vec![0, 1]),
            },
            BTreeMap::new(),
            Arc::new(SnapshotStore::new()),
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
            None,
        )
        .unwrap()
        .join()
        .unwrap();
        assert!(error.lock().unwrap().is_none());
    }

    #[test]
    fn resolves_each_argument_without_shell_expansion() {
        let options = CommandOptions {
            args: vec!["rg".into(), "--".into(), "{pattern}".into()],
            cwd: None,
            line_continuation: None,
            unique: false,
            success_exit_codes: None,
        };
        let params = BTreeMap::from([
            ("pattern".into(), "a{2} | b".into()),
            ("cwd".into(), "C:\\source".into()),
        ]);
        let (program, args, cwd) = resolve(&options, &params).unwrap();
        assert_eq!(program, "rg");
        assert_eq!(args, ["--", "a{2} | b"]);
        assert_eq!(cwd.as_deref(), Some("C:\\source"));
        assert!(resolve(&options, &BTreeMap::new()).is_err());
    }

    #[test]
    #[cfg(windows)]
    fn streams_process_output_into_picker_store() {
        let store = Arc::new(SnapshotStore::<Items>::new());
        let error = Arc::new(Mutex::new(None));
        let worker = start(
            CommandOptions {
                args: vec![
                    "cmd.exe".into(),
                    "/d".into(),
                    "/c".into(),
                    "echo first&& echo second&& echo first".into(),
                ],
                cwd: None,
                line_continuation: None,
                unique: false,
                success_exit_codes: None,
            },
            BTreeMap::new(),
            store.clone(),
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
            None,
        );
        worker.unwrap().join().unwrap();
        assert!(error.lock().unwrap().is_none());
        assert_eq!(store.snapshot().unwrap().len(), 3);
    }

    #[test]
    #[cfg(windows)]
    fn unique_command_continuation_keeps_accepted_value_by_result_index() {
        let store = Arc::new(SnapshotStore::<Items>::new());
        let values: CommandValues = Arc::default();
        let error = Arc::new(Mutex::new(None));
        let worker = start(
            CommandOptions {
                args: vec![
                    "pwsh.exe".into(),
                    "-NoProfile".into(),
                    "-Command".into(),
                    "[Console]::Write(('first' + [char]96 + [char]10 + 'second' + [char]10) * 2 + 'third' + [char]96 + [char]10 + 'fourth' + [char]10)".into(),
                ],
                cwd: None,
                line_continuation: Some("`".into()),
                unique: true,
                success_exit_codes: None,
            },
            BTreeMap::new(),
            store.clone(),
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
            Some(values.clone()),
        );
        worker.unwrap().join().unwrap();
        assert!(error.lock().unwrap().is_none());
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(
            snapshot.get_string_lossy(0, &mut Vec::new()),
            "first` second"
        );
        assert_eq!(values.lock().unwrap().get(&0).unwrap(), "first\nsecond");
        assert_eq!(
            snapshot.get_string_lossy(1, &mut Vec::new()),
            "third` fourth"
        );
        assert_eq!(values.lock().unwrap().get(&1).unwrap(), "third\nfourth");
    }
}

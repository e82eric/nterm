//! Ghostty config/request adapter for NFM's streaming command source.
use crate::ghostty_ffi::SnapshotCellRange;
use crate::picker::config::{CommandColumns, CommandOptions};
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
    let ipc = params.contains_key("__ipc_request");
    let budget_store = store.clone();
    let budget_cancel = cancelled.clone();
    let budget_error = error.clone();
    let usage = Mutex::new((0usize, 0usize));
    let sink = CommandSink {
        store,
        payload: SnapshotCellRange::default(),
        values: command_values,
        error,
    };
    let control = CommandControl {
        cancelled,
        changed: Arc::new(move || {
            if ipc {
                use nfm_search_core::store::ItemsSource;
                if let Some(snapshot) = budget_store.snapshot() {
                    let mut usage = usage.lock().unwrap();
                    let mut stack = [0u8; 1024];
                    let mut heap = Vec::new();
                    for index in usage.0..snapshot.len() {
                        // Include the separate continuation value and metadata
                        // conservatively in the retained-storage budget.
                        usage.1 = usage.1.saturating_add(
                            snapshot
                                .get_string(index, &mut stack, &mut heap)
                                .len()
                                .saturating_mul(2)
                                + 128,
                        );
                    }
                    usage.0 = snapshot.len();
                    if usage.0 > super::pipe::MAX_ITEMS || usage.1 > super::pipe::MAX_STORAGE {
                        *budget_error.lock().unwrap() =
                            Some("IPC command output exceeds server limits".into());
                        budget_cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                }
            }
            ctx.request_repaint();
        }),
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

type StructuredItems = nfm_picker_sources::structured::StructuredStreamingSnapshot;

/// One output line as a record with exactly `columns` fields. Missing fields
/// are empty; extra delimiters stay in the last field.
fn split_record(line: &[u8], delimiter: &str, columns: usize) -> csv::StringRecord {
    let text = String::from_utf8_lossy(line);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    let mut fields: Vec<&str> = text.splitn(columns, delimiter).collect();
    fields.resize(columns, "");
    csv::StringRecord::from(fields)
}

/// Add one output line as a row, recording the line itself as the row's
/// accepted value. Returns false once `max_rows` is reached.
fn add_row(
    rows: &mut nfm_picker_sources::structured::StructuredStreamingStore,
    values: &CommandValues,
    line: &[u8],
    columns: &CommandColumns,
    max_rows: usize,
) -> bool {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.is_empty() {
        return true;
    }
    if rows.len() >= max_rows {
        return false;
    }
    let record = split_record(line, &columns.delimiter, columns.names.len());
    // Values are installed before their rows are published.
    values
        .lock()
        .unwrap()
        .insert(rows.len(), String::from_utf8_lossy(line).into_owned());
    rows.add_record(&record)
        .expect("record matches column count");
    true
}

/// Run a command with `columns`: split each stdout line on the delimiter and
/// publish the rows to a structured store as they arrive. The store is
/// completed when the command finishes, fails or is cancelled.
pub(crate) fn start_structured(
    options: CommandOptions,
    columns: CommandColumns,
    params: BTreeMap<String, String>,
    store: Arc<SnapshotStore<StructuredItems>>,
    values: CommandValues,
    cancelled: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    ctx: egui::Context,
) -> Option<JoinHandle<()>> {
    use nfm_picker_sources::structured::{StructuredSchema, StructuredStreamingStore};
    use nfm_preview_command::{CommandSpec, Stream, no_match_exit, run_streaming};
    use std::sync::atomic::Ordering;
    let schema = StructuredSchema::new(columns.names.clone()).expect("validated command columns");
    let mut rows = StructuredStreamingStore::new(schema);
    let finish = {
        let (store, ctx) = (store.clone(), ctx.clone());
        move |rows: &mut StructuredStreamingStore| {
            store.publish(rows.snapshot());
            store.complete();
            ctx.request_repaint();
        }
    };
    let (program, args, cwd) = match resolve(&options, &params) {
        Ok(command) => command,
        Err(message) => {
            crate::error_log::error("picker command", "Command failed", &message);
            *error.lock().unwrap() = Some(message);
            finish(&mut rows);
            return None;
        }
    };
    let mut spec = CommandSpec::new(program);
    spec.arguments = args;
    spec.working_directory = cwd.map(std::path::PathBuf::from);
    spec.output_limit = 16 * 1024;
    let success_codes = options.success_exit_codes.clone().unwrap_or_else(|| {
        if no_match_exit(&spec.program.to_string_lossy()) {
            vec![0, 1]
        } else {
            vec![0]
        }
    });
    // Requests from npick share the IPC limits of plain command pickers.
    let max_rows = if params.contains_key("__ipc_request") {
        super::pipe::MAX_ITEMS
    } else {
        usize::MAX
    };
    let worker = std::thread::Builder::new()
        .name("picker structured command".into())
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                    let mut pending = Vec::new();
                    let mut too_many = false;
                    let output = run_streaming(
                        &spec,
                        || cancelled.load(Ordering::Acquire),
                        |stream, bytes| {
                            if stream != Stream::Stdout {
                                return true;
                            }
                            pending.extend_from_slice(bytes);
                            let mut start = 0;
                            while let Some(end) =
                                pending[start..].iter().position(|&byte| byte == b'\n')
                            {
                                too_many |= !add_row(
                                    &mut rows,
                                    &values,
                                    &pending[start..start + end],
                                    &columns,
                                    max_rows,
                                );
                                start += end + 1;
                            }
                            pending.drain(..start);
                            if start > 0 {
                                store.publish(rows.snapshot());
                                ctx.request_repaint();
                            }
                            !too_many
                        },
                    );
                    if cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    too_many |= !add_row(&mut rows, &values, &pending, &columns, max_rows);
                    if too_many {
                        return Err("command output exceeds the picker's row limit".into());
                    }
                    let output = output?;
                    if !output
                        .status
                        .code()
                        .is_some_and(|code| success_codes.contains(&code))
                    {
                        return Err(format!(
                            "{}\nProcess exited with {}\nStderr:\n{}",
                            spec.describe(),
                            output.status,
                            String::from_utf8_lossy(&output.stderr)
                        ));
                    }
                    Ok(())
                }));
            let result = result.unwrap_or_else(|_| Err("Command picker worker failed".into()));
            if let Err(message) = result {
                if !cancelled.load(Ordering::Acquire) {
                    crate::error_log::error("picker command", "Command failed", &message);
                    *error.lock().unwrap() = Some(message);
                }
            }
            finish(&mut rows);
        });
    match worker {
        Ok(worker) => Some(worker),
        Err(error) => {
            crate::error_log::error("picker command", "Worker failed", &error.to_string());
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
            columns: None,
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
                columns: None,
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
                columns: None,
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
                columns: None,
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
            columns: None,
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
                columns: None,
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
                columns: None,
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

    fn structured(
        script: &str,
        params: BTreeMap<String, String>,
    ) -> (
        Arc<SnapshotStore<StructuredItems>>,
        CommandValues,
        Arc<Mutex<Option<String>>>,
    ) {
        let store = Arc::new(SnapshotStore::new());
        let values: CommandValues = Arc::default();
        let error = Arc::new(Mutex::new(None));
        let columns = CommandColumns {
            names: vec!["Hash".into(), "Branch".into(), "Subject".into()],
            delimiter: "\t".into(),
        };
        let worker = start_structured(
            CommandOptions {
                args: vec!["sh".into(), "-c".into(), script.into()],
                cwd: None,
                line_continuation: None,
                unique: false,
                success_exit_codes: None,
                columns: Some(columns.clone()),
            },
            columns,
            params,
            store.clone(),
            values.clone(),
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
        );
        worker.unwrap().join().unwrap();
        (store, values, error)
    }

    #[test]
    fn records_split_into_exactly_the_configured_columns() {
        let fields = |line: &str, columns| {
            split_record(line.as_bytes(), "\t", columns)
                .iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(fields("a\tb\tc\r", 3), ["a", "b", "c"]);
        // Missing fields are empty; extra delimiters stay in the last column.
        assert_eq!(fields("a", 3), ["a", "", ""]);
        assert_eq!(fields("a\tb\tc\td", 3), ["a", "b", "c\td"]);
        assert_eq!(
            split_record("x :: y :: z".as_bytes(), " :: ", 2)
                .iter()
                .collect::<Vec<_>>(),
            ["x", "y :: z"]
        );
    }

    #[test]
    fn structured_command_streams_rows_with_a_header_and_original_lines() {
        let (store, values, error) = structured(
            "printf '9d33\\tmain\\tFirst commit\\n\\n0a50\\tresize\\tSecond\\tone\\r\\n3b3a\\tlinux\\tNo newline'",
            BTreeMap::new(),
        );
        assert!(store.is_done());
        assert!(error.lock().unwrap().is_none());
        let snapshot = store.snapshot().unwrap();
        // The blank line is skipped; the unterminated last line is kept.
        assert_eq!(snapshot.len(), 3);
        assert!(snapshot.header("").contains("Hash"));
        assert!(snapshot.header("").contains("Branch"));
        assert_eq!(snapshot.fields(1)["Branch"], "resize");
        assert_eq!(snapshot.fields(1)["Subject"], "Second\tone");
        let values = values.lock().unwrap();
        assert_eq!(values[&0], "9d33\tmain\tFirst commit");
        assert_eq!(values[&1], "0a50\tresize\tSecond\tone");
        assert_eq!(values[&2], "3b3a\tlinux\tNo newline");
    }

    #[test]
    fn structured_command_failure_keeps_rows_and_reports_stderr() {
        let (store, _, error) = structured(
            "printf 'a\\tb\\tc\\n'; echo broken >&2; exit 3",
            BTreeMap::new(),
        );
        assert!(store.is_done());
        assert_eq!(store.snapshot().unwrap().len(), 1);
        let error = error.lock().unwrap().clone().unwrap();
        assert!(error.contains("broken"), "{error}");
    }

    #[test]
    fn structured_command_bad_parameters_complete_without_a_worker() {
        let store = Arc::new(SnapshotStore::new());
        let error = Arc::new(Mutex::new(None));
        let columns = CommandColumns {
            names: vec!["One".into()],
            delimiter: "\t".into(),
        };
        let worker = start_structured(
            CommandOptions {
                args: vec!["sh".into(), "{missing}".into()],
                cwd: None,
                line_continuation: None,
                unique: false,
                success_exit_codes: None,
                columns: Some(columns.clone()),
            },
            columns,
            BTreeMap::new(),
            store.clone(),
            Arc::default(),
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
        );
        assert!(worker.is_none());
        assert!(store.is_done());
        assert!(error.lock().unwrap().as_ref().unwrap().contains("missing"));
    }
}

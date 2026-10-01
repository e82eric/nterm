use crate::ghostty_ffi::SnapshotCellRange;
use crate::picker::config::CommandOptions;
use nfm_search_core::snapshot_store::SnapshotStore;
use nfm_search_core::store::{
    PublishingStreamingItemStoreWithPayload, StreamingItemSnapshotWithPayload,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

type Items = StreamingItemSnapshotWithPayload<SnapshotCellRange>;
pub(crate) type CommandValues = Arc<Mutex<HashMap<usize, String>>>;

struct CommandItem {
    display: String,
    value: String,
}

fn finish_item(lines: &mut Vec<String>, continuation: Option<&str>) -> Option<CommandItem> {
    if lines.is_empty() {
        return None;
    }
    let display = lines.join(" ");
    let mut value = String::new();
    for line in lines.drain(..) {
        if let Some(stripped) = continuation.and_then(|suffix| line.strip_suffix(suffix)) {
            value.push_str(stripped);
            value.push('\n');
        } else {
            value.push_str(&line);
        }
    }
    Some(CommandItem { display, value })
}

fn publish_item(
    publisher: &mut PublishingStreamingItemStoreWithPayload<SnapshotCellRange>,
    command_values: Option<&CommandValues>,
    next_index: &mut usize,
    item: CommandItem,
    seen: &mut Option<HashSet<String>>,
) -> bool {
    if seen
        .as_mut()
        .is_some_and(|seen| !seen.insert(item.value.clone()))
    {
        return false;
    }
    if item.value != item.display {
        if let Some(values) = command_values {
            // add_item may publish automatically, so store the accepted value first.
            values.lock().unwrap().insert(*next_index, item.value);
        }
    }
    let index = publisher.add_item(item.display.as_bytes(), SnapshotCellRange::default());
    debug_assert_eq!(index as usize, *next_index);
    *next_index += 1;
    true
}

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
    let failure_store = store.clone();
    let failure_error = error.clone();
    let failure_ctx = ctx.clone();
    crate::worker::spawn(
        "picker command worker",
        move || {
            let mut publisher = PublishingStreamingItemStoreWithPayload::new(store);
            let continuation = options
                .line_continuation
                .as_deref()
                .filter(|value| !value.is_empty());
            let mut pending_lines = Vec::new();
            let mut next_index = 0;
            let mut seen = options.unique.then(HashSet::new);
            let result = (|| -> Result<(), String> {
                let (program, args, cwd) = resolve(&options, &params)?;
                let context = super::process::describe(
                    &std::iter::once(program.clone())
                        .chain(args.clone())
                        .collect::<Vec<_>>(),
                    cwd.as_deref(),
                );
                let mut command = Command::new(&program);
                command
                    .args(args)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                if let Some(cwd) = cwd {
                    command.current_dir(cwd);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                let mut child = super::process::ChildGuard(
                    command
                        .spawn()
                        .map_err(|e| format!("{context}\nUnable to start process: {e}"))?,
                );
                let stdout = child
                    .0
                    .stdout
                    .take()
                    .ok_or("command picker has no stdout")?;
                let stderr = child
                    .0
                    .stderr
                    .take()
                    .ok_or("command picker has no stderr")?;
                let stderr_reader = super::process::reader("picker stderr reader", move || {
                    super::process::read_bounded(stderr, super::process::STDERR_LIMIT)
                        .map_err(|error| format!("Unable to read stderr: {error}"))
                })?;
                let (lines_tx, lines_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(128);
                let stdout_reader = super::process::reader("picker stdout reader", move || {
                    let mut reader = BufReader::new(stdout);
                    loop {
                        let mut line = Vec::new();
                        match reader.read_until(b'\n', &mut line) {
                            Ok(0) => return Ok(()),
                            Err(error) => return Err(format!("Unable to read stdout: {error}")),
                            Ok(_) if lines_tx.send(line).is_err() => return Ok(()),
                            Ok(_) => {}
                        }
                    }
                })?;
                let mut batch = 0;
                loop {
                    if cancelled.load(Ordering::Acquire) {
                        let _ = child.0.kill();
                        break;
                    }
                    match lines_rx.recv_timeout(Duration::from_millis(30)) {
                        Ok(line) => {
                            let text = String::from_utf8_lossy(&line);
                            let text = text.trim_end_matches(['\r', '\n']);
                            if pending_lines.is_empty() && text.is_empty() {
                                continue;
                            }
                            pending_lines.push(text.to_owned());
                            if continuation.is_some_and(|suffix| text.ends_with(suffix)) {
                                continue;
                            }
                            if let Some(item) = finish_item(&mut pending_lines, continuation) {
                                let published = publish_item(
                                    &mut publisher,
                                    command_values.as_ref(),
                                    &mut next_index,
                                    item,
                                    &mut seen,
                                );
                                batch += usize::from(published);
                            }
                            if batch >= 128 {
                                publisher.publish();
                                batch = 0;
                                ctx.request_repaint();
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                if let Some(item) = finish_item(&mut pending_lines, continuation) {
                    publish_item(
                        &mut publisher,
                        command_values.as_ref(),
                        &mut next_index,
                        item,
                        &mut seen,
                    );
                }
                let status = loop {
                    if cancelled.load(Ordering::Acquire) {
                        let _ = child.0.kill();
                    }
                    match child.0.try_wait() {
                        Ok(Some(status)) => break status,
                        Ok(None) => thread::sleep(Duration::from_millis(25)),
                        Err(error) => {
                            return Err(format!("{context}\nUnable to wait for process: {error}"));
                        }
                    }
                };
                drop(lines_rx);
                let stdout_result = super::process::join(stdout_reader, "picker stdout reader");
                let stderr_result = super::process::join(stderr_reader, "picker stderr reader");
                if cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                stdout_result.map_err(|error| {
                    format!("{context}\nPartial results may be incomplete.\n{error}")
                })?;
                let stderr = stderr_result.map_err(|error| format!("{context}\n{error}"))?;
                let accepted = if let Some(codes) = &options.success_exit_codes {
                    status.code().is_some_and(|code| codes.contains(&code))
                } else {
                    status.success()
                        || (super::process::no_match_exit(&program) && status.code() == Some(1))
                };
                if !accepted {
                    return Err(format!(
                        "{context}\n{}",
                        super::process::status_error(status, &stderr)
                    ));
                }
                Ok(())
            })();
            if let Err(message) = result {
                if !cancelled.load(Ordering::Acquire) {
                    crate::error_log::error("picker command", "Command failed", &message);
                }
                *error.lock().unwrap() = Some(message);
            }
            publisher.complete();
            ctx.request_repaint();
        },
        move |message| {
            crate::error_log::error("picker command", "Worker failed", &message);
            *failure_error
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(message);
            PublishingStreamingItemStoreWithPayload::new(failure_store.clone()).complete();
            failure_ctx.request_repaint();
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfm_search_core::store::ItemsSource;
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
    fn continuation_joins_display_and_restores_newlines_in_value() {
        let mut lines = vec!["{ test`".into(), "line con`".into(), "}".into()];
        let item = finish_item(&mut lines, Some("`")).unwrap();
        assert_eq!(item.display, "{ test` line con` }");
        assert_eq!(item.value, "{ test\nline con\n}");
        assert!(lines.is_empty());

        let mut unfinished = vec!["one`".into(), "two`".into()];
        let item = finish_item(&mut unfinished, Some("`")).unwrap();
        assert_eq!(item.value, "one\ntwo\n");
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

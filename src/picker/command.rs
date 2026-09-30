use crate::ghostty_ffi::SnapshotCellRange;
use crate::picker::config::CommandOptions;
use nfm_search_core::snapshot_store::SnapshotStore;
use nfm_search_core::store::{
    PublishingStreamingItemStoreWithPayload, StreamingItemSnapshotWithPayload,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufRead, BufReader};
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
) -> JoinHandle<()> {
    thread::spawn(move || {
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
            let mut command = Command::new(program);
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
            let mut child = command
                .spawn()
                .map_err(|e| format!("command picker: {e}"))?;
            let stdout = child.stdout.take().ok_or("command picker has no stdout")?;
            let stderr = child.stderr.take().ok_or("command picker has no stderr")?;
            let stderr_reader = thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut first = Vec::new();
                let _ = reader.read_until(b'\n', &mut first);
                let _ = io::copy(&mut reader, &mut io::sink());
                String::from_utf8_lossy(&first).trim().to_owned()
            });
            let (lines_tx, lines_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(128);
            let stdout_reader = thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = Vec::new();
                    match reader.read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if lines_tx.send(line).is_err() => break,
                        Ok(_) => {}
                    }
                }
            });
            let mut batch = 0;
            loop {
                if cancelled.load(Ordering::Acquire) {
                    let _ = child.kill();
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
            let status = child.wait().map_err(|e| format!("command picker: {e}"))?;
            drop(lines_rx);
            let _ = stdout_reader.join();
            let stderr = stderr_reader.join().unwrap_or_default();
            // ripgrep uses exit code 1 for a valid search with no matches.
            if !cancelled.load(Ordering::Acquire) && !status.success() && status.code() != Some(1) {
                return Err(if stderr.is_empty() {
                    format!("command picker exited with {status}")
                } else {
                    format!("command picker: {stderr}")
                });
            }
            Ok(())
        })();
        if let Err(message) = result {
            *error.lock().unwrap() = Some(message);
        }
        publisher.complete();
        ctx.request_repaint();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfm_search_core::store::ItemsSource;

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
            },
            BTreeMap::new(),
            store.clone(),
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
            None,
        );
        worker.join().unwrap();
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
            },
            BTreeMap::new(),
            store.clone(),
            Arc::new(AtomicBool::new(false)),
            error.clone(),
            egui::Context::default(),
            Some(values.clone()),
        );
        worker.join().unwrap();
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

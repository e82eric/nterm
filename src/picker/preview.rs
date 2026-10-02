use crate::ghostty_ffi::{self, Host, SnapshotStyledRows};
use std::sync::Arc;

fn styled_rows(host: usize, generation: u64, first: u64, rows: u32) -> Result<Vec<u8>, String> {
    let mut result = SnapshotStyledRows::default();
    if !unsafe {
        ghostty_ffi::ghostty_egui_host_snapshot_styled_rows(
            host as *mut Host,
            generation,
            first,
            rows,
            &mut result,
        )
    } {
        return Err("Unable to read scrollback preview".into());
    }
    let bytes = if result.len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(result.text, result.len) }.to_vec()
    };
    unsafe { ghostty_ffi::ghostty_egui_host_snapshot_styled_rows_free(&mut result) };
    Ok(bytes)
}

/// Closing the lease serializes with active native reads. The NFM worker may
/// finish parsing its owned bytes afterwards, but can never read a freed host.
pub(crate) struct SnapshotPreview {
    host: usize,
    generation: Option<u64>,
    open: std::sync::Mutex<bool>,
}
impl SnapshotPreview {
    pub(crate) fn new(host: *mut Host, generation: Option<u64>) -> Self {
        Self {
            host: host as usize,
            generation,
            open: std::sync::Mutex::new(true),
        }
    }
    pub(crate) fn close(&self) {
        *self.open.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }
    fn read(
        &self,
        page: nfm_egui::byte_preview::PageRequest,
        cancel: nfm_egui::Cancellation,
    ) -> Result<nfm_egui::byte_preview::BytePage, String> {
        let open = self
            .open
            .lock()
            .map_err(|_| "Snapshot preview lease poisoned")?;
        if !*open || cancel.is_cancelled() {
            return Err("preview cancelled".into());
        }
        let generation = self.generation.ok_or("No scrollback snapshot available")?;
        let bytes = styled_rows(
            self.host,
            generation,
            page.first_row as u64,
            u32::from(page.rows),
        )?;
        Ok(nfm_egui::byte_preview::BytePage {
            bytes,
            first_row: page.first_row,
        })
    }
}

pub(super) fn scrollback_provider(
    reader: Arc<SnapshotPreview>,
    total_rows: u64,
    columns: usize,
) -> nfm_egui::byte_preview::BytePreviewProvider<super::source::Candidate> {
    use nfm_egui::byte_preview::{ByteDocument, ByteSource, CellPosition, Highlight};
    nfm_egui::byte_preview::BytePreviewProvider::new(
        move |selection: &nfm_egui::Selection<super::source::Candidate>, _cancel| {
            let range = selection.item.range.ok_or("No scrollback match selected")?;
            let reader = reader.clone();
            Ok(ByteDocument {
                source: ByteSource::Paged {
                    columns: columns
                        .try_into()
                        .map_err(|_| "Snapshot width exceeds u16")?,
                    total_rows: total_rows
                        .try_into()
                        .map_err(|_| "Snapshot row count exceeds usize")?,
                    read: Arc::new(move |page, cancel| reader.read(page, cancel)),
                },
                center_row: Some(range.start.y as usize),
                highlight: Some(Highlight {
                    start: CellPosition {
                        row: range.start.y as usize,
                        column: range.start.x as usize,
                    },
                    end: CellPosition {
                        row: range.end.y as usize,
                        column: range.end.x as usize,
                    },
                    background: crate::colors::palette().selection_background,
                }),
            })
        },
    )
}

/// Lua remains on its owning UI thread. Only owned command specifications are
/// transferred to NFM's preview worker; no process, VT or paging state lives here.
pub(crate) struct CommandPreview {
    source: super::config::Preview,
    key: Option<(usize, String, String)>,
}
impl CommandPreview {
    pub(crate) fn new(source: super::config::Preview) -> Self {
        Self { source, key: None }
    }
    pub(super) fn prepare(
        &mut self,
        ctx: &egui::Context,
        shared: &mut nfm_egui::Picker<super::source::Snapshot>,
        selection: &nfm_egui::Selection<super::source::Candidate>,
        value: &str,
        filesystem: bool,
        params: &std::collections::BTreeMap<String, String>,
    ) {
        let key = (selection.index, selection.text.clone(), value.to_owned());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.key = Some(key);
        let super::config::Preview::Command { args, cwd, command } = &self.source else {
            return;
        };
        let resolved = if let Some(command) = command {
            command.call(value, filesystem, params)
        } else {
            Ok(Some(crate::lua_callbacks::PreviewCommandSpec {
                args: args.iter().map(|a| a.replace("{item}", value)).collect(),
                cwd: cwd.as_ref().map(|c| c.replace("{item}", value)),
                center_line: None,
            }))
        };
        let expected_index = selection.index;
        let expected_text = selection.text.clone();
        let provider = nfm_egui::byte_preview::BytePreviewProvider::new(
            move |selected: &nfm_egui::Selection<super::source::Candidate>, cancel| {
                // A keyboard selection may change inside shared.show before the UI
                // thread resolves its Lua callback. Never execute the old command
                // for that newly selected item.
                if selected.index != expected_index || selected.text != expected_text {
                    return Err("Preview command is being resolved".into());
                }
                let result = match &resolved {
                    Ok(Some(spec)) => {
                        let program = spec
                            .args
                            .first()
                            .ok_or("preview command requires an executable")?;
                        let mut command = nfm_egui::byte_preview::CommandSpec::new(program);
                        command.arguments = spec.args[1..].to_vec();
                        command.working_directory = spec.cwd.as_ref().map(std::path::PathBuf::from);
                        command.output_limit = 64 * 1024;
                        let codes = if nfm_picker_sources::command::no_match_exit(program) {
                            &[0, 1][..]
                        } else {
                            &[0][..]
                        };
                        nfm_egui::byte_preview::command_document(
                            &command,
                            spec.center_line,
                            codes,
                            cancel,
                        )
                    }
                    Ok(None) => Ok(nfm_egui::byte_preview::ByteDocument::complete(Vec::new())),
                    Err(error) => Err(error.clone()),
                };
                if let Err(error) = &result {
                    if error != "preview cancelled" {
                        crate::error_log::error("picker preview", "Preview failed", error);
                    }
                }
                result
            },
        );
        shared.set_preview_provider(ctx, Arc::new(provider));
        shared.set_external_preview(false);
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_lease_close_waits_for_active_reads_and_blocks_future_reads() {
        let reader = Arc::new(SnapshotPreview::new(std::ptr::null_mut(), None));
        let active_read = reader.open.lock().unwrap();
        let closing_reader = reader.clone();
        let (done, closed) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closing_reader.close();
            done.send(()).unwrap();
        });
        assert!(closed.try_recv().is_err());
        drop(active_read);
        closed
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        closer.join().unwrap();
        assert!(!*reader.open.lock().unwrap());
    }
}

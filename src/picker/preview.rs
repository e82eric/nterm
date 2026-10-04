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
) -> impl nfm_egui::PreviewProvider<super::source::Candidate> {
    use nfm_egui::byte_preview::{ByteDocument, ByteSource, CellPosition, Highlight};
    let copy_reader = reader.clone();
    let provider = nfm_egui::byte_preview::BytePreviewProvider::new(
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
    );
    ScrollbackProvider {
        provider,
        document: Arc::new(SnapshotCopyDocument {
            reader: copy_reader,
            total_rows,
            columns,
        }),
        published: Default::default(),
    }
}

struct ScrollbackProvider {
    provider: nfm_egui::byte_preview::BytePreviewProvider<super::source::Candidate>,
    document: Arc<SnapshotCopyDocument>,
    published: std::sync::Mutex<Option<(u64, usize)>>,
}
impl nfm_egui::PreviewProvider<super::source::Candidate> for ScrollbackProvider {
    fn preview(
        &self,
        request: nfm_egui::PreviewRequest<super::source::Candidate>,
        cancel: nfm_egui::Cancellation,
    ) -> Result<nfm_egui::Preview, String> {
        self.preview_viewport(request, cancel)
            .map(|page| page.document)
    }
    fn preview_viewport(
        &self,
        request: nfm_egui::PreviewRequest<super::source::Candidate>,
        cancel: nfm_egui::Cancellation,
    ) -> Result<nfm_egui::preview::PreviewViewport, String> {
        let key = (request.document_version, request.selection.index);
        let page = self.provider.preview_viewport(request, cancel)?;
        *self.published.lock().map_err(|_| "preview poisoned")? = Some(key);
        Ok(page)
    }
    fn page_step(&self, rows: usize) -> usize {
        (rows / 2).max(1)
    }
    fn copy_document(
        &self,
        version: u64,
        index: usize,
    ) -> Result<Option<Arc<dyn nfm_egui::copy_document::CopyDocument>>, String> {
        let current = *self.published.lock().map_err(|_| "preview poisoned")?;
        if current != Some((version, index)) {
            return Ok(None);
        }
        Ok(Some(self.document.clone()))
    }
}

struct SnapshotCopyDocument {
    reader: Arc<SnapshotPreview>,
    total_rows: u64,
    columns: usize,
}
impl SnapshotCopyDocument {
    fn read<R>(&self, f: impl FnOnce(*mut Host, u64) -> Result<R, String>) -> Result<R, String> {
        let open = self
            .reader
            .open
            .lock()
            .map_err(|_| "snapshot lease poisoned")?;
        if !*open {
            return Err("snapshot closed".into());
        }
        let generation = self.reader.generation.ok_or("snapshot missing")?;
        f(self.reader.host as *mut Host, generation)
    }
}
impl nfm_egui::copy_document::CopyDocument for SnapshotCopyDocument {
    fn metadata(&self) -> Result<nfm_egui::copy_document::DocumentMetadata, String> {
        self.read(|host, revision| {
            let mut view = ghostty_ffi::SnapshotView::default();
            if !unsafe { ghostty_ffi::ghostty_egui_host_snapshot_view(host, revision, &mut view) } {
                return Err("snapshot expired".into());
            }
            Ok(nfm_egui::copy_document::DocumentMetadata {
                revision,
                total_rows: view.total_rows,
                columns: view.columns,
            })
        })
    }
    fn text_row(&self, y: u64) -> Result<Option<nfm_egui::copy_document::DocumentRow>, String> {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;
        self.read(|host, generation| {
            if y >= self.total_rows {
                return Ok(None);
            }
            let mut row = ghostty_ffi::SnapshotTextRow::default();
            if !unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_text_row(host, generation, y, &mut row)
            } {
                return Err("snapshot row unavailable".into());
            }
            let (text, columns) = if row.len == 0 {
                (String::new(), Vec::new())
            } else {
                unsafe {
                    (
                        String::from_utf8_lossy(std::slice::from_raw_parts(row.text, row.len))
                            .into_owned(),
                        std::slice::from_raw_parts(row.columns, row.len).to_vec(),
                    )
                }
            };
            let wraps_to_next = row.wraps_to_next;
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_text_row_free(&mut row) };
            let mut widths = vec![1; self.columns];
            for (byte, glyph) in text.grapheme_indices(true) {
                if let Some(&column) = columns.get(byte) {
                    let x = column as usize;
                    let width = glyph.width().max(1).min(2);
                    if x < widths.len() {
                        widths[x] = width as u8;
                    }
                    if width == 2 && x + 1 < widths.len() {
                        widths[x + 1] = 0;
                    }
                }
            }
            Ok(Some(nfm_egui::copy_document::DocumentRow {
                text: nfm_egui::copy_mode::TextRow {
                    text,
                    columns,
                    wraps_to_next,
                },
                widths,
            }))
        })
    }
    fn resolve_cell(
        &self,
        requested: nfm_egui::copy_mode::CellPosition,
        right: bool,
    ) -> Result<nfm_egui::copy_mode::CellPosition, String> {
        self.read(|host, generation| {
            let mut actual = ghostty_ffi::SnapshotCell::default();
            if unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_resolve_cell(
                    host,
                    generation,
                    requested,
                    right,
                    &mut actual,
                )
            } {
                Ok(actual)
            } else {
                Err("snapshot position unavailable".into())
            }
        })
    }
    fn selection_text(
        &self,
        selection: &nfm_egui::copy_mode::CopySelection,
    ) -> Result<String, String> {
        self.read(|host, generation| {
            let mut result = ghostty_ffi::SnapshotSelectionText::default();
            if !unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_selection_text(
                    host,
                    generation,
                    selection.range,
                    selection.trim,
                    selection.rectangle,
                    &mut result,
                )
            } {
                return Err("snapshot selection unavailable".into());
            }
            let mut text = if result.len == 0 {
                String::new()
            } else {
                unsafe {
                    String::from_utf8_lossy(std::slice::from_raw_parts(result.text, result.len))
                        .into_owned()
                }
            };
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_selection_text_free(&mut result) };
            if selection.linewise {
                text.push('\n');
            }
            Ok(text)
        })
    }
    fn viewport(
        &self,
        top: u64,
        rows: usize,
        appearance: &nfm_egui::Appearance,
    ) -> Result<nfm_egui::Preview, String> {
        self.read(|host, generation| {
            let bytes = styled_rows(host as usize, generation, top, rows as u32)?;
            let mut normalized = Vec::with_capacity(bytes.len());
            for (i, &byte) in bytes.iter().enumerate() {
                if byte == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
                    normalized.push(b'\r');
                }
                normalized.push(byte);
            }
            nfm_egui::preview::parse_ansi(
                &normalized,
                &nfm_egui::PreviewRequest {
                    selection: nfm_egui::Selection {
                        item: (),
                        text: String::new(),
                        index: 0,
                        source_version: generation,
                        snapshot: Arc::new(()),
                    },
                    document_version: generation,
                    initial_page: false,
                    columns: self.columns.try_into().map_err(|_| "snapshot too wide")?,
                    rows: rows.try_into().map_err(|_| "too many rows")?,
                    scroll_offset: 0,
                    foreground: appearance.palette.text,
                    background: appearance.palette.background,
                },
            )
        })
    }
}

/// Lua remains on its owning UI thread. Only owned command specifications are
/// transferred to NFM's preview worker; no process, VT or paging state lives here.
pub(crate) struct CommandPreview {
    source: super::config::Preview,
    key: Option<(usize, String, String)>,
    cycle: u64,
    document_key: Option<(u64, usize)>,
}
impl CommandPreview {
    pub(crate) fn new(source: super::config::Preview) -> Self {
        Self {
            source,
            key: None,
            cycle: 0,
            document_key: None,
        }
    }
    pub fn rendered(&self, trace: &super::diagnostics::Trace, key: Option<(u64, usize)>) {
        if key.is_some() && key.map(|key| key.1) == self.document_key.map(|key| key.1) {
            trace.mark("preview_frame_built", Some(self.cycle));
        }
    }
    pub(super) fn prepare(
        &mut self,
        ctx: &egui::Context,
        shared: &mut nfm_egui::Picker<super::source::Snapshot>,
        selection: &nfm_egui::Selection<super::source::Candidate>,
        value: &str,
        filesystem: bool,
        params: &std::collections::BTreeMap<String, String>,
        trace: Option<Arc<super::diagnostics::Trace>>,
    ) {
        let key = (selection.index, selection.text.clone(), value.to_owned());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.key = Some(key);
        self.cycle += 1;
        let cycle = self.cycle;
        self.document_key = Some((selection.source_version, selection.index));
        if let Some(trace) = &trace {
            trace.mark("preview_requested", Some(cycle));
        }
        let super::config::Preview::Command {
            args,
            cwd,
            command,
            max_bytes,
            max_scrollback_lines,
        } = &self.source
        else {
            return;
        };
        let max_bytes = max_bytes.unwrap_or(10 * 1024 * 1024);
        let max_scrollback_lines = max_scrollback_lines.unwrap_or(8192);
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
        if let Some(trace) = &trace {
            trace.mark("preview_resolved", Some(cycle));
        }
        let command_trace = trace.clone();
        let expected_text = selection.text.clone();
        let provider = nfm_egui::byte_preview::BytePreviewProvider::new(
            move |selected: &nfm_egui::Selection<super::source::Candidate>, cancel| {
                // A keyboard selection may change inside shared.show before the UI
                // thread resolves its Lua callback. Never execute the old command
                // for that newly selected item.
                if selected.index != expected_index || selected.text != expected_text {
                    return Err("Preview command is being resolved".into());
                }
                if let Some(trace) = &command_trace {
                    trace.mark("preview_worker_started", Some(cycle));
                }
                let result = match &resolved {
                    Ok(Some(spec)) => {
                        let program = spec
                            .args
                            .first()
                            .ok_or("preview command requires an executable")?;
                        let mut command = nfm_egui::byte_preview::CommandSpec::new(program);
                        if let Some(trace) = &command_trace {
                            command.observer = Some(super::diagnostics::command_observer(
                                trace.clone(),
                                Some(cycle),
                            ));
                        }
                        command.arguments = spec.args[1..].to_vec();
                        command.working_directory = spec.cwd.as_ref().map(std::path::PathBuf::from);
                        command.output_limit = max_bytes;
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
        let provider = provider.with_limits(max_bytes, max_scrollback_lines);
        if let Some(trace) = trace {
            shared.set_preview_provider(
                ctx,
                Arc::new(MeasuredPreview {
                    provider,
                    trace,
                    cycle,
                }),
            );
        } else {
            shared.set_preview_provider(ctx, Arc::new(provider));
        }
        shared.set_external_preview(false);
        ctx.request_repaint();
    }
}

struct MeasuredPreview {
    provider: nfm_egui::byte_preview::BytePreviewProvider<super::source::Candidate>,
    trace: Arc<super::diagnostics::Trace>,
    cycle: u64,
}
impl nfm_egui::PreviewProvider<super::source::Candidate> for MeasuredPreview {
    fn preview(
        &self,
        request: nfm_egui::PreviewRequest<super::source::Candidate>,
        cancel: nfm_egui::Cancellation,
    ) -> Result<nfm_egui::Preview, String> {
        self.preview_viewport(request, cancel)
            .map(|page| page.document)
    }
    fn preview_viewport(
        &self,
        request: nfm_egui::PreviewRequest<super::source::Candidate>,
        cancel: nfm_egui::Cancellation,
    ) -> Result<nfm_egui::preview::PreviewViewport, String> {
        let result = self.provider.preview_viewport(request, cancel);
        self.trace.mark(
            if result.is_ok() {
                "preview_document_ready"
            } else {
                "preview_failed"
            },
            Some(self.cycle),
        );
        result
    }
    fn copy_document(
        &self,
        version: u64,
        index: usize,
    ) -> Result<Option<Arc<dyn nfm_egui::copy_document::CopyDocument>>, String> {
        self.provider.copy_document(version, index)
    }
    fn page_step(&self, rows: usize) -> usize {
        self.provider.page_step(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_snapshot_copy_document_rejects_all_native_reads() {
        use nfm_egui::copy_document::CopyDocument;
        let reader = Arc::new(SnapshotPreview::new(std::ptr::null_mut(), Some(1)));
        reader.close();
        let doc = SnapshotCopyDocument {
            reader,
            total_rows: 10,
            columns: 20,
        };
        assert!(doc.metadata().is_err());
        assert!(doc.text_row(0).is_err());
        assert!(doc.resolve_cell(Default::default(), false).is_err());
        assert!(
            doc.viewport(0, 4, &nfm_egui::Appearance::default())
                .is_err()
        );
        assert!(
            doc.selection_text(&nfm_egui::copy_mode::CopySelection {
                range: Default::default(),
                linewise: false,
                trim: true,
                rectangle: false,
            })
            .is_err()
        );
    }
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

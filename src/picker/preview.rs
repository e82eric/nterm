use nfm_egui::styled_document::{DocumentRange, StyledDocument};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// Rust-owned content can outlive the host. Closing only rejects new requests;
/// immutable copy documents already published remain safe without a lease.
pub(crate) struct SnapshotPreview {
    document: Arc<OnceLock<Arc<StyledDocument>>>,
    open: AtomicBool,
}
impl SnapshotPreview {
    pub(crate) fn new(document: Arc<OnceLock<Arc<StyledDocument>>>) -> Self {
        Self {
            document,
            open: AtomicBool::new(true),
        }
    }
    pub(crate) fn close(&self) {
        self.open.store(false, Ordering::Release);
    }
    fn resolve(
        &self,
        range: crate::ghostty_ffi::SnapshotCellRange,
    ) -> Result<(Arc<StyledDocument>, DocumentRange), String> {
        if !self.open.load(Ordering::Acquire) {
            return Err("Scrollback preview closed".into());
        }
        let document = self
            .document
            .get()
            .ok_or("Scrollback document is loading")?
            .clone();
        let start = document
            .source_position(range.start.y as usize, range.start.x)
            .ok_or("Scrollback match unavailable")?;
        let end = document
            .source_position(range.end.y as usize, range.end.x)
            .ok_or("Scrollback match unavailable")?;
        Ok((document, DocumentRange { start, end }))
    }
}
pub(super) fn scrollback_provider(
    reader: Arc<SnapshotPreview>,
) -> impl nfm_egui::PreviewProvider<super::source::Candidate> {
    nfm_egui::styled_document::DocumentPreviewProvider::new(
        move |selection: &nfm_egui::Selection<super::source::Candidate>| {
            let range = selection.item.range.ok_or("No scrollback match selected")?;
            let (document, range) = reader.resolve(range)?;
            Ok((document, Some(range)))
        },
        crate::colors::palette().selection_background,
    )
}

#[cfg(test)]
mod document_tests {
    use super::*;
    #[test]
    fn scrollback_ranges_resolve_to_document_offsets_without_a_native_host() {
        use crate::ghostty_ffi::{SnapshotCell, SnapshotCellRange};
        let shared = Arc::new(OnceLock::new());
        let reader = SnapshotPreview::new(shared.clone());
        let range = SnapshotCellRange {
            start: SnapshotCell { x: 1, y: 30 },
            end: SnapshotCell { x: 1, y: 31 },
        };
        assert!(reader.resolve(range).is_err());
        let mut builder =
            nfm_egui::styled_document::DocumentBuilder::default().with_source_origin(30);
        builder.push_row("abc", &[0, 1, 2], true, &[]).unwrap();
        builder.push_row("def", &[0, 1, 2], false, &[]).unwrap();
        let document = builder.finish();
        assert!(shared.set(document.clone()).is_ok());
        let (retained, resolved) = reader.resolve(range).unwrap();
        assert!(Arc::ptr_eq(&document, &retained));
        assert_eq!(resolved.start.byte, 1);
        assert_eq!(resolved.end.byte, 4);
        assert_eq!(resolved.start.line, resolved.end.line);
        reader.close();
        assert!(reader.resolve(range).is_err());
        assert_eq!(retained.source_row(31).unwrap().text, "def");
    }
}

/// Dynamic Lua is resolved by a separate Lua owner; the UI only installs providers.
pub(crate) struct ResolvedPreview {
    source: super::config::Preview,
    key: Option<(usize, String, String)>,
}
impl ResolvedPreview {
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
        if let super::config::Preview::Dynamic {
            resolve: Some(resolve),
            max_bytes,
            max_scrollback_lines,
        } = &self.source
        {
            if let Some(worker) = &resolve.worker {
                let worker = worker.clone();
                let text = selection.text.clone();
                let expected_index = selection.index;
                let value = value.to_owned();
                let params = params.clone();
                let max_bytes = max_bytes.unwrap_or(10 * 1024 * 1024);
                let max_lines = max_scrollback_lines.unwrap_or(8192);
                let provider = nfm_preview_file::resolved::ResolvedPreviewProvider::new(
                    move |selected: &nfm_egui::Selection<super::source::Candidate>, cancel| {
                        if selected.index != expected_index || selected.text != text {
                            return Err("Preview is being resolved".into());
                        }
                        worker.resolve(
                            text.clone(),
                            value.clone(),
                            filesystem,
                            params.clone(),
                            max_bytes,
                            max_lines,
                            cancel,
                        )
                    },
                );
                shared.set_preview_provider(ctx, Arc::new(provider));
                shared.set_external_preview(false);
                ctx.request_repaint();
                return;
            }
        }
        let resolved = match &self.source {
            super::config::Preview::Dynamic { .. } => {
                Err("Dynamic preview requires a Lua worker".to_owned())
            }
            _ => return,
        };
        let expected = (
            selection.source_version,
            selection.index,
            selection.text.clone(),
        );
        let provider = nfm_preview_file::resolved::ResolvedPreviewProvider::new(
            move |selected: &nfm_egui::Selection<super::source::Candidate>, _| {
                if (
                    selected.source_version,
                    selected.index,
                    selected.text.as_str(),
                ) != (expected.0, expected.1, expected.2.as_str())
                {
                    return Err("Preview is being resolved".into());
                }
                resolved.clone()
            },
        );
        shared.set_preview_provider(ctx, Arc::new(provider));
        shared.set_external_preview(false);
        ctx.request_repaint();
    }
}

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
        _filesystem: bool,
        _params: &std::collections::BTreeMap<String, String>,
    ) {
        let key = (selection.index, selection.text.clone(), value.to_owned());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.key = Some(key);
        let super::config::Preview::Command {
            args,
            cwd,
            max_bytes,
            max_scrollback_lines,
        } = &self.source
        else {
            return;
        };
        let max_bytes = max_bytes.unwrap_or(10 * 1024 * 1024);
        let max_scrollback_lines = max_scrollback_lines.unwrap_or(8192);
        let resolved: Result<Option<crate::lua_callbacks::PreviewCommandSpec>, String> =
            Ok(Some(crate::lua_callbacks::PreviewCommandSpec {
                args: args.iter().map(|a| a.replace("{item}", value)).collect(),
                cwd: cwd.as_ref().map(|c| c.replace("{item}", value)),
                center_line: None,
            }));
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
        shared.set_preview_provider(ctx, Arc::new(provider));
        shared.set_external_preview(false);
        ctx.request_repaint();
    }
}

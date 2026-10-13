//! Coordinate an in-place frozen-terminal resize without retaining its old pages.
use super::*;

struct ModeAnchors {
    offset: usize,
    has_anchor: bool,
    range_count: usize,
    text: Option<String>,
}

fn capture<D: nfm_egui::copy_controller::CopyNavigationDocument>(
    mode: &CopyMode,
    document: &D,
    cells: &mut Vec<SnapshotCell>,
) -> Result<ModeAnchors, String> {
    let offset = cells.len();
    cells.push(mode.cursor);
    if let Some(anchor) = mode.visual_anchor {
        cells.push(anchor);
    }
    let mut read_error = None;
    let selection = mode.visual_selection_to(mode.cursor, |row| match document.text_row(row) {
        Ok(row) => row,
        Err(error) => {
            read_error = Some(error);
            None
        }
    });
    if let Some(error) = read_error {
        return Err(error);
    }
    if mode.visual_anchor.is_some() && selection.is_none() {
        return Err("Unable to preserve selection during resize".into());
    }
    let text = match mode.reflow_selection_text() {
        Some(text) => Some(text.to_owned()),
        None => selection
            .as_ref()
            .map(|s| document.selection_text(s))
            .transpose()?,
    };
    let ranges = if let Some(ranges) = mode.reflow_selection_ranges() {
        ranges.to_vec()
    } else if let Some(selection) = selection {
        if selection.rectangle {
            nfm_egui::copy_mode::rectangle_row_ranges(selection.range, 0, mode.total_rows())
        } else {
            vec![selection.range]
        }
    } else {
        Vec::new()
    };
    for range in &ranges {
        cells.extend([range.start, range.end]);
    }
    Ok(ModeAnchors {
        offset,
        has_anchor: mode.visual_anchor.is_some(),
        range_count: ranges.len(),
        text,
    })
}

fn restore<D: nfm_egui::copy_controller::CopyNavigationDocument>(
    mode: &mut CopyMode,
    anchors: ModeAnchors,
    cells: &[SnapshotCell],
    view: SnapshotView,
    document: &D,
) -> Result<(), String> {
    let cursor = cells[anchors.offset];
    let anchor = anchors.has_anchor.then(|| cells[anchors.offset + 1]);
    let start = anchors.offset + 1 + usize::from(anchors.has_anchor);
    let ranges = cells[start..start + anchors.range_count * 2]
        .chunks_exact(2)
        .map(|pair| SnapshotCellRange {
            start: pair[0],
            end: pair[1],
        })
        .collect();
    mode.apply_reflow(view, cursor, anchor, ranges, anchors.text);
    if mode.visual_mode == VisualMode::Linewise
        && let Some(range) = mode
            .reflow_selection_ranges()
            .and_then(|ranges| ranges.first())
            .copied()
    {
        // Reflow tracks text positions, so a pin in old trailing blanks can
        // land at the last text cell. Linewise highlighting still covers the
        // entire logical line, including all of its new wrapped rows.
        let mut read_error = None;
        let expanded = mode.linewise_range_between(range.start, range.end, |row| {
            match document.text_row(row) {
                Ok(row) => row,
                Err(error) => {
                    read_error = Some(error);
                    None
                }
            }
        });
        if let Some(error) = read_error {
            return Err(error);
        }
        let expanded = expanded.ok_or("Unable to restore linewise selection after resize")?;
        let text = mode.reflow_selection_text().map(str::to_owned);
        mode.apply_reflow(view, cursor, anchor, vec![expanded], text);
    }
    Ok(())
}

impl TerminalPane {
    pub(super) fn resize_snapshot(&mut self) -> Result<(), String> {
        let generation = self
            .copy_mode
            .generation
            .or(self.search.snapshot_generation)
            .or_else(|| self.picker.as_ref().and_then(|picker| picker.generation));
        let Some(generation) = generation else {
            return Ok(());
        };
        let size = unsafe { ghostty_ffi::ghostty_egui_host_surface_size(self.host) };
        let mut before = SnapshotView::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut before)
        } {
            return Err("Snapshot unavailable during resize".into());
        }
        if before.columns == u32::from(size.columns) && before.viewport_rows == u32::from(size.rows)
        {
            return Ok(());
        }
        if self
            .picker
            .as_mut()
            .is_some_and(|picker| picker.prepare_snapshot_resize())
        {
            let picker = self.picker.take().unwrap();
            let from_copy = picker.from_copy;
            drop(picker);
            self.handle_picker_action(crate::picker::Action::Cancel, Some(generation), from_copy);
            if !from_copy {
                return Ok(());
            }
        }
        self.search_worker.cancel_and_wait();
        for cursor in [&mut self.search.cursor, &mut self.copy_search_cursor] {
            if *cursor != 0 {
                unsafe { ghostty_ffi::ghostty_egui_host_snapshot_cursor_free(*cursor as *mut _) };
                *cursor = 0;
            }
        }
        let document = SnapshotNavigationDocument {
            host: self.host,
            generation,
            rows: before.total_rows,
            boundaries: None,
        };
        let mut cells = Vec::new();
        let mode = self
            .copy_mode
            .generation
            .map(|_| capture(&self.copy_mode, &document, &mut cells))
            .transpose()?;
        let origin = self
            .search
            .copy_origin
            .as_ref()
            .map(|mode| capture(mode, &document, &mut cells))
            .transpose()?;
        let purpose = self
            .quick_select
            .as_ref()
            .map(|quick| {
                if let QuickSelectPurpose::YankLines { origin } = quick.purpose {
                    let index = cells.len();
                    cells.push(origin);
                    Some(index)
                } else {
                    None
                }
            })
            .flatten();
        let mut map = ghostty_ffi::SnapshotReflowMap::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_resize(self.host, generation, &mut map)
        } {
            return Err("Unable to reflow snapshot".into());
        }
        for cell in &mut cells {
            *cell = map
                .translate(*cell)
                .ok_or("Unable to translate snapshot position after reflow")?;
        }
        // All controllers reuse the same indexed map; no terminal scans or
        // per-position FFI calls are needed. Release it before rebuilding UI.
        drop(map);
        let mut view = SnapshotView::default();
        if !unsafe {
            ghostty_ffi::ghostty_egui_host_snapshot_view(self.host, generation, &mut view)
        } {
            return Err("Resized snapshot unavailable".into());
        }
        let document = SnapshotNavigationDocument {
            host: self.host,
            generation,
            rows: view.total_rows,
            boundaries: None,
        };
        if let Some(anchors) = mode {
            restore(&mut self.copy_mode, anchors, &cells, view, &document)?;
        }
        if let Some(anchors) = origin {
            restore(
                self.search.copy_origin.as_mut().unwrap(),
                anchors,
                &cells,
                view,
                &document,
            )?;
        }
        self.prompt_boundaries = None;
        if let Some(quick) = &self.quick_select {
            let from_copy = quick.from_copy;
            let mut rebuilt = self.build_quick_select(generation, from_copy, view, false);
            if let (Some(quick), Some(index)) = (&mut rebuilt, purpose) {
                quick.purpose = QuickSelectPurpose::YankLines {
                    origin: cells[index],
                };
            }
            self.quick_select = rebuilt;
        }
        self.search.matches.clear();
        self.search.selected = None;
        self.search.accept_requested = false;
        if self.search.active {
            self.search.cursor = unsafe {
                ghostty_ffi::ghostty_egui_host_snapshot_cursor_new(self.host, generation)
            } as usize;
            if self.search.cursor == 0 {
                return Err("Unable to restart snapshot search".into());
            }
            self.queue_search();
        } else if let Some((pattern, regex)) = self.copy_search_query.clone() {
            self.restart_copy_search(pattern, regex, generation)?;
        }
        if self.copy_mode.generation.is_some()
            && self.quick_select.is_none()
            && self.search.snapshot_generation.is_none()
        {
            // Reflow preserves the old viewport's text position, but a shorter
            // viewport can leave the translated selection cursor off screen.
            // Reveal it using the same scroll padding as ordinary movement.
            if !self.publish_copy_decoration(generation, self.copy_mode.cursor, true) {
                return Err("Unable to reveal copy-mode cursor after resize".into());
            }
        } else {
            self.refresh_decorations();
        }
        Ok(())
    }

    fn restart_copy_search(
        &mut self,
        pattern: String,
        regex: bool,
        generation: u64,
    ) -> Result<(), String> {
        self.copy_search_cursor =
            unsafe { ghostty_ffi::ghostty_egui_host_snapshot_cursor_new(self.host, generation) }
                as usize;
        if self.copy_search_cursor == 0 {
            return Err("Unable to restart copy search".into());
        }
        let revision =
            self.search_worker
                .submit(generation, pattern, regex, self.copy_search_cursor)?;
        // Refresh matches without replaying a pending movement after resize.
        self.copy_search_refresh = Some(revision);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfm_egui::copy_controller::CopyNavigationDocument;

    struct Document;
    impl CopyNavigationDocument for Document {
        fn text_row(&self, y: u64) -> Result<Option<TextRow>, String> {
            Ok((y < 3).then(|| TextRow {
                text: "abcdefgh".into(),
                columns: (0..8).collect(),
                wraps_to_next: false,
            }))
        }
        fn resolve_cell(&self, cell: SnapshotCell, _: bool) -> Result<SnapshotCell, String> {
            Ok(cell)
        }
        fn selection_text(&self, _: &CopySelection) -> Result<String, String> {
            Ok("bcd\nbcd\nbcd".into())
        }
    }

    #[test]
    fn snapshot_resize_captures_every_block_segment_and_restores_exact_text() {
        let mut mode = CopyMode::default();
        let view = SnapshotView {
            cursor: SnapshotCell { x: 1, y: 0 },
            total_rows: 3,
            columns: 8,
            viewport_rows: 3,
            viewport_top: 0,
        };
        mode.start(1, view);
        mode.toggle_visual(VisualMode::Blockwise);
        mode.moved(Motion::CharacterJump, SnapshotCell { x: 3, y: 2 });
        let mut cells = Vec::new();
        let anchors = capture(&mode, &Document, &mut cells).unwrap();
        assert_eq!(anchors.range_count, 3);
        assert_eq!(anchors.text.as_deref(), Some("bcd\nbcd\nbcd"));
        // Cursor/anchor duplicate two of the six row-segment endpoints.
        assert_eq!(cells.len(), 8);
        for cell in &mut cells {
            cell.y += 10;
        }
        let resized = SnapshotView {
            total_rows: 20,
            columns: 4,
            ..view
        };
        restore(&mut mode, anchors, &cells, resized, &Document).unwrap();
        assert_eq!(mode.cursor, SnapshotCell { x: 3, y: 12 });
        assert_eq!(mode.visual_anchor, Some(SnapshotCell { x: 1, y: 10 }));
        assert_eq!(mode.reflow_selection_text(), Some("bcd\nbcd\nbcd"));
        assert_eq!(mode.reflow_selection_ranges().unwrap().len(), 3);
        let mut next = Vec::new();
        let second = capture(&mode, &Document, &mut next).unwrap();
        assert_eq!(second.text.as_deref(), Some("bcd\nbcd\nbcd"));
    }

    #[test]
    fn snapshot_resize_block_movement_keeps_rectangle_after_widening() {
        use nfm_egui::copy_controller::CopyController;
        let mut mode = CopyMode::default();
        let view = SnapshotView {
            cursor: SnapshotCell { x: 1, y: 0 },
            total_rows: 3,
            columns: 4,
            viewport_rows: 3,
            viewport_top: 0,
        };
        mode.start(1, view);
        mode.toggle_visual(VisualMode::Blockwise);
        mode.moved(Motion::CharacterJump, SnapshotCell { x: 3, y: 1 });
        let mut cells = Vec::new();
        let anchors = capture(&mode, &Document, &mut cells).unwrap();
        let widened = SnapshotView { columns: 8, ..view };
        restore(&mut mode, anchors, &cells, widened, &Document).unwrap();
        assert!(mode.reflow_selection_ranges().is_some());
        for motion in [Motion::Down, Motion::Left] {
            CopyController::new(&mut mode, &Document, widened)
                .execute(CopyAction::Motion {
                    motion,
                    count: 1,
                    explicit_count: false,
                })
                .unwrap();
            assert_eq!(mode.visual_mode, VisualMode::Blockwise);
            let selection = mode
                .visual_selection_to(mode.cursor, |row| Document.text_row(row).unwrap())
                .unwrap();
            assert!(selection.rectangle);
            assert_eq!(selection.range.start, SnapshotCell { x: 1, y: 0 });
        }
        assert_eq!(mode.cursor, SnapshotCell { x: 2, y: 2 });
        assert!(mode.reflow_selection_ranges().is_none());
    }

    #[test]
    fn snapshot_resize_linewise_selection_expands_translated_text_to_full_wrapped_rows() {
        struct WrappedDocument;
        impl CopyNavigationDocument for WrappedDocument {
            fn text_row(&self, y: u64) -> Result<Option<TextRow>, String> {
                Ok((y < 3).then(|| TextRow {
                    text: "abc".into(),
                    columns: vec![0, 1, 2],
                    wraps_to_next: y < 2,
                }))
            }
            fn resolve_cell(&self, cell: SnapshotCell, _: bool) -> Result<SnapshotCell, String> {
                Ok(cell)
            }
            fn selection_text(&self, _: &CopySelection) -> Result<String, String> {
                Ok("original selected text\n".into())
            }
        }
        let mut mode = CopyMode::default();
        let view = SnapshotView {
            cursor: SnapshotCell { x: 1, y: 0 },
            total_rows: 3,
            columns: 8,
            viewport_rows: 3,
            viewport_top: 0,
        };
        mode.start(1, view);
        mode.toggle_visual(VisualMode::Linewise);
        for columns in [4, 16, 6] {
            // Simulate translated endpoints landing inside a wrapped logical
            // line and at its last text cell instead of the row's right edge.
            let cells = [
                SnapshotCell { x: 1, y: 1 },
                SnapshotCell { x: 1, y: 1 },
                SnapshotCell { x: 0, y: 1 },
                SnapshotCell { x: 2, y: 2 },
            ];
            restore(
                &mut mode,
                ModeAnchors {
                    offset: 0,
                    has_anchor: true,
                    range_count: 1,
                    text: Some("original selected text\n".into()),
                },
                &cells,
                SnapshotView { columns, ..view },
                &WrappedDocument,
            )
            .unwrap();
            assert_eq!(
                mode.visual_selection_to(mode.cursor, |row| {
                    WrappedDocument.text_row(row).unwrap()
                })
                .unwrap()
                .range,
                SnapshotCellRange {
                    start: SnapshotCell { x: 0, y: 0 },
                    end: SnapshotCell {
                        x: columns - 1,
                        y: 2
                    },
                }
            );
            assert_eq!(
                mode.reflow_selection_text(),
                Some("original selected text\n")
            );
            let mut next = Vec::new();
            capture(&mode, &WrappedDocument, &mut next).unwrap();
            assert_eq!(next[2], SnapshotCell { x: 0, y: 0 });
            assert_eq!(
                next[3],
                SnapshotCell {
                    x: columns - 1,
                    y: 2
                }
            );
            mode.moved_with_rows(Motion::Down, SnapshotCell { x: 1, y: 2 }, |row| {
                WrappedDocument.text_row(row).unwrap()
            });
            assert_eq!(mode.visual_mode, VisualMode::Linewise);
            assert_eq!(mode.cursor, SnapshotCell { x: 1, y: 2 });
            assert_eq!(
                mode.visual_selection_to(mode.cursor, |row| {
                    WrappedDocument.text_row(row).unwrap()
                })
                .unwrap()
                .range
                .start,
                SnapshotCell { x: 0, y: 0 }
            );
        }
    }

    #[test]
    fn snapshot_resize_character_selection_captures_cursor_anchor_and_range() {
        let mut mode = CopyMode::default();
        let view = SnapshotView {
            cursor: SnapshotCell { x: 1, y: 0 },
            total_rows: 3,
            columns: 8,
            viewport_rows: 3,
            viewport_top: 0,
        };
        mode.start(1, view);
        mode.toggle_visual(VisualMode::Characterwise);
        mode.moved(Motion::Right, SnapshotCell { x: 3, y: 0 });
        let mut cells = Vec::new();
        capture(&mode, &Document, &mut cells).unwrap();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0], cells[3]);
        assert_eq!(cells[1], cells[2]);
    }
}

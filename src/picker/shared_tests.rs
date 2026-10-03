use super::*;
use nfm_search_core::{
    snapshot_store::SnapshotStore, store::PublishingStreamingItemStoreWithPayload,
};
use std::time::{Duration, Instant};

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}
fn frame(
    ctx: &egui::Context,
    picker: &mut Picker,
    events: Vec<egui::Event>,
) -> (Option<Action>, Vec<egui::OutputCommand>) {
    let mut action = None;
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            events,
            ..Default::default()
        },
        |ui| action = picker.show(ui.ctx(), 24.0),
    );
    output.textures_delta.clear();
    (action, output.platform_output.commands)
}
fn wait_selection(
    ctx: &egui::Context,
    picker: &mut Picker,
) -> nfm_egui::Selection<source::Candidate> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        frame(ctx, picker, vec![]);
        if let Some(selection) = picker.shared.selection() {
            return selection;
        }
        assert!(
            Instant::now() < deadline,
            "shared picker did not publish a selection"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(windows)]
fn wait_preview_text(
    ctx: &egui::Context,
    picker: &mut Picker,
    expected_first_line: &str,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, commands) = frame(
            ctx,
            picker,
            vec![key(
                egui::Key::C,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            )],
        );
        for command in commands {
            if let egui::OutputCommand::CopyText(text) = command {
                if text
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == expected_first_line)
                {
                    return text;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "Preview never showed {expected_first_line}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(windows)]
#[test]
fn command_preview_runs_through_nfm_using_the_accepted_value_and_cwd() {
    let ctx = egui::Context::default();
    let cwd = std::env::temp_dir().display().to_string();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![protocol::Item {
                    text: "display".into(),
                    value: "inserted".into(),
                }],
                provider: None,
            }),
            preview: Some(config::Preview::Command {
                args: vec![
                    "cmd.exe".into(),
                    "/d".into(),
                    "/c".into(),
                    "echo {item}& cd".into(),
                ],
                cwd: Some(cwd.clone()),
                command: None,
            }),
            ..Default::default()
        },
    );
    let text = wait_preview_text(&ctx, &mut picker, "inserted");
    assert!(
        text.to_lowercase()
            .contains(&cwd.trim_end_matches(['/', '\\']).to_lowercase())
    );
}

#[cfg(windows)]
#[test]
fn lua_command_preview_resolves_once_and_nfm_owns_centering_paging_and_selection_changes() {
    let ctx = egui::Context::default();
    let lua = std::rc::Rc::new(mlua::Lua::new());
    let function = lua.load(r#"calls = 0; return function(item, params)
        assert(params.marker == 'passed')
        calls = calls + 1
        return { args={'cmd.exe','/d','/c','for /L %i in (0,1,29) do @echo row%i'}, center_line=tonumber(item.value) }
    end"#).eval::<mlua::Function>().unwrap();
    let command = std::rc::Rc::new(crate::lua_callbacks::PreviewCommand {
        key: lua.create_registry_value(function).unwrap(),
        lua: lua.clone(),
        name: "test".into(),
    });
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![
                    protocol::Item {
                        text: "first".into(),
                        value: "20".into(),
                    },
                    protocol::Item {
                        text: "second".into(),
                        value: "1".into(),
                    },
                ],
                provider: None,
            }),
            preview: Some(config::Preview::Command {
                args: Vec::new(),
                cwd: None,
                command: Some(command),
            }),
            sort: config::Sort::SourceOrder,
            params: [("marker".into(), "passed".into())].into(),
            ..Default::default()
        },
    );
    wait_preview_text(&ctx, &mut picker, "row14");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::PageUp, egui::Modifiers::CTRL)],
    );
    wait_preview_text(&ctx, &mut picker, "row8");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::PageDown, egui::Modifiers::CTRL)],
    );
    wait_preview_text(&ctx, &mut picker, "row14");
    assert_eq!(lua.globals().get::<_, u32>("calls").unwrap(), 1);
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::N, egui::Modifiers::CTRL)],
    );
    wait_preview_text(&ctx, &mut picker, "row0");
    assert_eq!(lua.globals().get::<_, u32>("calls").unwrap(), 2);
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::P, egui::Modifiers::CTRL)],
    );
    assert!(!picker.shared.preview_visible());
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::P, egui::Modifiers::CTRL)],
    );
    wait_preview_text(&ctx, &mut picker, "row0");
    assert_eq!(lua.globals().get::<_, u32>("calls").unwrap(), 2);
}
fn items(ctx: &egui::Context) -> Picker {
    Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![protocol::Item {
                    text: "café shown".into(),
                    value: "inserted value".into(),
                }],
                provider: None,
            }),
            enter_action: config::EnterAction::SendInput,
            params: [("query".into(), "café".into())].into(),
            ..Default::default()
        },
    )
}
#[test]
fn unicode_display_copy_and_insert_value_survive_migration() {
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    assert_eq!(wait_selection(&ctx, &mut picker).text, "café shown");
    let (_, commands) = frame(&ctx, &mut picker, vec![egui::Event::Copy]);
    assert!(commands.contains(&egui::OutputCommand::CopyText("café shown".into())));
    assert_eq!(picker.shared.query(), "café");
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(matches!(action, Some(Action::SendInput(value)) if value == "inserted value"));
}

#[test]
fn complete_preview_uses_accepted_value_and_nfm_paging_and_copy() {
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![protocol::Item {
                    text: "display".into(),
                    value: "inserted".into(),
                }],
                provider: None,
            }),
            preview: Some(config::Preview::Ansi {
                text: "\x1b[31m{item}\nsecond".into(),
            }),
            ..Default::default()
        },
    );
    assert!(
        picker.command_preview.is_none(),
        "NFM owns the complete-document preview"
    );
    wait_selection(&ctx, &mut picker);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, commands) = frame(
            &ctx,
            &mut picker,
            vec![key(
                egui::Key::C,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            )],
        );
        if commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text.contains("inserted") && text.contains("second"))) { break; }
        assert!(Instant::now() < deadline, "NFM preview was not copied");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn structured_command_palette_returns_typed_action() {
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        true,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::CommandPalette(vec![crate::actions::Entry {
                id: "picker.files".into(),
                label: "Open files".into(),
                shortcut: Some("Ctrl+F".into()),
                action: crate::actions::AppAction::OpenPicker("files".into()),
            }]),
            params: [("query".into(), "/:Action=~files ".into())].into(),
            ..Default::default()
        },
    );
    let selection = wait_selection(&ctx, &mut picker);
    assert_eq!(
        picker.source.accepted_value(&SearchResult {
            node_index: selection.item.node_index,
            score: 0,
            path: selection.text
        }),
        "picker.files"
    );
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(
        matches!(action, Some(Action::Execute(crate::actions::AppAction::OpenPicker(name))) if name == "files")
    );
}

#[test]
fn lua_callback_retains_display_value_and_request_parameters() {
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    let lua = std::rc::Rc::new(mlua::Lua::new());
    let function = lua
        .load("return function() end")
        .eval::<mlua::Function>()
        .unwrap();
    let callback = std::rc::Rc::new(crate::lua_callbacks::Callback {
        key: lua.create_registry_value(function).unwrap(),
        lua,
        name: "fixture".into(),
    });
    picker.callback = Some(callback.clone());
    picker
        .request_params
        .insert("fixture".into(), "kept".into());
    wait_selection(&ctx, &mut picker);
    let (selected, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    let Some(Action::Custom(actual, text, filesystem, range, value, params)) = selected else {
        panic!("callback was not retained");
    };
    assert!(std::rc::Rc::ptr_eq(&actual, &callback));
    assert_eq!(text, "café shown");
    assert_eq!(value, "inserted value");
    assert!(!filesystem && range.is_none());
    assert_eq!(params["fixture"], "kept");
}

#[test]
fn command_palette_reload_entry_returns_reload_action() {
    let ctx = egui::Context::default();
    let entries = crate::actions::catalog(&config::Bindings::default(), false, false);
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        true,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::CommandPalette(entries),
            params: [("query".into(), "reload configuration".into())].into(),
            ..Default::default()
        },
    );
    wait_selection(&ctx, &mut picker);
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(matches!(
        action,
        Some(Action::Execute(crate::actions::AppAction::ReloadConfig))
    ));
}

#[test]
fn line_split_search_copy_accept_and_return_keep_document_coordinates() {
    use nfm_picker_sources::line_split::{Candidate as SplitCandidate, Kind};
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    wait_selection(&ctx, &mut picker);
    let original_query = picker.shared.query().to_owned();
    picker.position = config::Position::Top;
    picker.builtin_pickers.line_break = config::BuiltinPresentation {
        position: Some(config::Position::Center),
        preview_visible: Some(false),
        result_rows: Some(5),
        preview_rows: Some(8),
    };
    let original_preview_visible = picker.shared.preview_visible();
    let source = PickerSource::split_candidates(vec![SplitCandidate {
        text: "file name.txt".into(),
        row: 31,
        columns: 8..21,
        kind: Kind::TableField,
    }]);
    picker.enter_line_split(&ctx, source);
    assert_eq!(picker.position, config::Position::Center);
    assert!(!picker.shared.preview_visible());
    assert_eq!((picker.result_rows, picker.preview_rows), (5, 8));
    let selected = wait_selection(&ctx, &mut picker);
    assert_eq!(selected.text, "file name.txt");
    let range = selected.item.range.unwrap();
    assert_eq!(range.start, SnapshotCell { x: 8, y: 31 });
    assert_eq!(range.end, SnapshotCell { x: 20, y: 31 });
    let (_, copied) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::C, egui::Modifiers::CTRL)],
    );
    assert!(copied.iter().any(
        |command| matches!(command, egui::OutputCommand::CopyText(text) if text == "file name.txt")
    ));
    // Returning restores the old source/query; split mode never invokes the
    // parent's callback or Navigate action when accepting a fragment.
    assert!(
        matches!(picker.accept(selected), Some(Action::SendInput(text)) if text == "file name.txt")
    );
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::ArrowLeft, egui::Modifiers::ALT)],
    );
    wait_selection(&ctx, &mut picker);
    assert!(picker.split_parent.is_none());
    assert_eq!(picker.position, config::Position::Top);
    assert_eq!(picker.shared.preview_visible(), original_preview_visible);
    assert_eq!((picker.result_rows, picker.preview_rows), (RESULT_ROWS, 12));
    assert_eq!(picker.shared.query(), original_query);
}

#[test]
fn snapshot_byte_mapping_becomes_grapheme_cell_ranges() {
    let text = "界e\u{301}x ".to_owned();
    let columns = vec![0, 0, 0, 2, 2, 2, 3, 4];
    assert_eq!(text.len(), columns.len());
    let row = nfm_egui::copy_mode::TextRow {
        text,
        columns,
        wraps_to_next: false,
    };
    let line = source::split_line_cells(19, &row, 5);
    assert_eq!(line.cells[0].columns, 0..2);
    assert_eq!(line.cells[1].text, "e\u{301}");
    assert_eq!(line.cells[1].columns, 2..3);
    let result = nfm_picker_sources::line_split::split(&[line], 0, &[]);
    assert_eq!(result[0].columns, 0..4);
}

#[test]
fn final_line_split_grapheme_does_not_include_unoccupied_trailing_cells() {
    for (text, columns, end) in [
        ("file.txt", vec![10, 11, 12, 13, 14, 15, 16, 17], 18),
        ("file界", vec![10, 11, 12, 13, 14, 14, 14], 16),
        ("filee\u{301}", vec![10, 11, 12, 13, 14, 14, 14], 15),
        (
            "file👩‍💻",
            vec![10, 11, 12, 13, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14],
            16,
        ),
    ] {
        let row = nfm_egui::copy_mode::TextRow {
            text: text.into(),
            columns,
            wraps_to_next: false,
        };
        let line = source::split_line_cells(19, &row, 160);
        assert_eq!(line.cells.last().unwrap().columns.end, end, "{text}");
        let result = nfm_picker_sources::line_split::split(&[line], 0, &[]);
        let candidate = result
            .iter()
            .find(|candidate| candidate.text == text)
            .unwrap();
        assert_eq!(candidate.columns, 10..end, "{text}");
    }
}

#[test]
fn line_split_word_before_distant_exported_whitespace_has_exact_range() {
    for suffix in [" ", "\n"] {
        let row = nfm_egui::copy_mode::TextRow {
            text: format!("filename.txt{suffix}"),
            columns: (20..32).chain(std::iter::once(159)).collect(),
            wraps_to_next: false,
        };
        let line = source::split_line_cells(19, &row, 160);
        let result = nfm_picker_sources::line_split::split(&[line], 0, &[]);
        let candidate = result
            .iter()
            .find(|candidate| candidate.text == "filename.txt")
            .unwrap();
        assert_eq!(candidate.columns, 20..32, "{suffix:?}");
    }
}

#[cfg(windows)]
#[test]
fn command_source_order_survives_shared_search_and_navigation() {
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Command(config::CommandOptions {
                args: vec![
                    "cmd.exe".into(),
                    "/d".into(),
                    "/c".into(),
                    "echo xxa&& echo a".into(),
                ],
                cwd: None,
                line_continuation: None,
                unique: false,
                success_exit_codes: None,
            }),
            sort: config::Sort::SourceOrder,
            params: [("query".into(), "a".into())].into(),
            enter_action: config::EnterAction::SendInput,
            ..Default::default()
        },
    );
    assert_eq!(wait_selection(&ctx, &mut picker).text.trim(), "xxa");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let appearance = picker.appearance(&ctx);
        let mut ready = false;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let result = picker.shared.show(
                ui.ctx(),
                nfm_egui::Placement::new(ui.ctx().content_rect()),
                &appearance,
                false,
            );
            ready = result.matched == 2 && !result.busy;
        });
        output.textures_delta.clear();
        if ready {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::N, egui::Modifiers::CTRL)],
    );
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(matches!(action, Some(Action::SendInput(value)) if value.trim() == "a"));
}
#[test]
fn scrollback_selection_retains_owned_cell_range_across_publication() {
    let ctx = egui::Context::default();
    let store = Arc::new(SnapshotStore::new());
    let range = SnapshotCellRange {
        start: SnapshotCell { x: 2, y: 50 },
        end: SnapshotCell { x: 7, y: 51 },
    };
    let mut publisher = PublishingStreamingItemStoreWithPayload::new(store.clone());
    publisher.add_item(b"chosen", range);
    publisher.publish();
    let mut picker = items(&ctx);
    picker.source = PickerSource::test_scrollback(store);
    picker.shared = nfm_egui::Picker::new(
        "range-test",
        &ctx,
        picker.source.provider(),
        nfm_egui::PickerConfig::default(),
    );
    picker.enter_action = config::EnterAction::Navigate;
    let selection = wait_selection(&ctx, &mut picker);
    assert_eq!(selection.item.range, Some(range));
    publisher.add_item(b"later", SnapshotCellRange::default());
    publisher.complete();
    assert!(matches!(picker.accept(selection), Some(Action::Accept(cell)) if cell == range.start));
}
#[test]
fn preview_toggle_and_help_use_shared_control() {
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![protocol::Item {
                    text: "one".into(),
                    value: "one".into(),
                }],
                provider: None,
            }),
            preview: Some(config::Preview::Text {
                text: "Preview {item}".into(),
            }),
            preview_visible: false,
            enter_action: config::EnterAction::SendInput,
            ..Default::default()
        },
    );
    wait_selection(&ctx, &mut picker);
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::P, egui::Modifiers::CTRL)],
    );
    assert!(picker.shared.preview_visible());
    frame(
        &ctx,
        &mut picker,
        vec![key(
            egui::Key::Slash,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        )],
    );
    assert!(picker.shared.keybinding_help_visible());
    assert!(
        frame(
            &ctx,
            &mut picker,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)]
        )
        .0
        .is_none()
    );
    assert!(!picker.shared.keybinding_help_visible());
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::P, egui::Modifiers::CTRL)],
    );
    assert!(!picker.shared.preview_visible());
    assert!(matches!(
        frame(
            &ctx,
            &mut picker,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)]
        )
        .0,
        Some(Action::Cancel)
    ));
}
#[test]
fn background_picker_does_not_consume_foreground_typing_or_focus() {
    let ctx = egui::Context::default();
    let mut background = items(&ctx);
    let mut foreground = items(&ctx);
    let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
    let floating = egui::Rect::from_min_size(egui::pos2(200.0, 100.0), egui::vec2(600.0, 500.0));
    for events in [vec![], vec![], vec![egui::Event::Text("x".into())]] {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(bounds),
                events,
                ..Default::default()
            },
            |ui| {
                background.show_background_in_bounds(ui.ctx(), bounds);
                foreground.show_in_bounds(ui.ctx(), floating);
            },
        );
        output.textures_delta.clear();
    }
    assert_eq!(background.shared.query(), "café");
    assert_eq!(foreground.shared.query(), "caféx");
    assert!(ctx.memory(|m| {
        m.has_focus(egui::Id::new(("ghostty-picker", foreground.session_id)).with("query"))
    }));
    let panel = ctx
        .memory(|m| m.area_rect(egui::Id::new(("ghostty-picker", foreground.session_id))))
        .unwrap();
    assert!(floating.contains_rect(panel));
}

#[test]
fn pane_picker_focus_returns_after_floating_pane_is_shown_and_hidden() {
    let ctx = egui::Context::default();
    let mut main = items(&ctx);
    let mut floating = items(&ctx);
    let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
    for (floating_active, text) in [
        (false, ""),
        (false, "a"),
        (true, ""),
        (true, "b"),
        (false, "c"),
        (true, "d"),
        (false, "e"),
    ] {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(bounds),
                events: if text.is_empty() {
                    vec![]
                } else {
                    vec![egui::Event::Text(text.into())]
                },
                ..Default::default()
            },
            |ui| {
                if floating_active {
                    main.show_background_in_bounds(ui.ctx(), bounds);
                    floating.show_in_bounds(ui.ctx(), bounds);
                } else {
                    main.show_in_bounds(ui.ctx(), bounds);
                }
            },
        );
        output.drop_without_applying_deltas();
        let active = if floating_active { &floating } else { &main };
        assert!(ctx.memory(|m| {
            m.has_focus(egui::Id::new(("ghostty-picker", active.session_id)).with("query"))
        }));
    }
    assert_eq!(main.shared.query(), "caféace");
    assert_eq!(floating.shared.query(), "cafébd");
}
#[cfg(windows)]
#[test]
fn filesystem_picker_accepts_path_without_terminal_snapshot() {
    let root = std::env::temp_dir().join(format!("ghostty-shared-picker-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("file with spaces.txt");
    std::fs::write(&file, "fixture").unwrap();
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::FileSystem(config::FileOptions {
                roots: vec![root.clone()],
                max_depth: 1,
                files_only: true,
                directories_only: false,
            }),
            params: [("query".into(), "file with spaces".into())].into(),
            enter_action: config::EnterAction::SendInput,
            ..Default::default()
        },
    );
    wait_selection(&ctx, &mut picker);
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(
        matches!(action, Some(Action::SendInput(value)) if std::path::Path::new(&value) == file)
    );
    drop(picker);
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(root).unwrap();
}

use super::*;
use nfm_search_core::{
    snapshot_store::SnapshotStore, store::PublishingStreamingItemStoreWithPayload,
};
use std::time::{Duration, Instant};

#[test]
fn workspace_scope_expands_picker_without_changing_cursor_origin() {
    for scope in [config::Scope::Pane, config::Scope::Workspace] {
        let ctx = egui::Context::default();
        let mut picker = items(&ctx);
        picker.scope = scope;
        picker.position = config::Position::FullPane;
        let workspace = egui::Rect::from_min_size(egui::pos2(0.0, 24.0), egui::vec2(1000.0, 700.0));
        let pane = egui::Rect::from_min_size(egui::pos2(510.0, 40.0), egui::vec2(460.0, 310.0));
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 750.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    picker.show_scoped(ui.ctx(), pane, Some(workspace), true);
                },
            );
            output.textures_delta.clear();
        }
        let panel = ctx
            .memory(|m| m.area_rect(egui::Id::new(("nterm-picker", picker.session_id))))
            .unwrap();
        let expected = if scope == config::Scope::Workspace {
            workspace
        } else {
            pane
        };
        assert!(
            expected.expand(1.0).contains_rect(panel),
            "{scope:?}: {panel:?}"
        );
        assert!(panel.width() > expected.width() * 0.9);
        picker.position = config::Position::Cursor;
        picker.cursor_row = Some(3);
        let placement = picker.placement(workspace);
        assert_eq!(
            placement.anchor,
            Some(egui::pos2(
                pane.center().x,
                pane.top() + 4.0 * picker.typography.row_height
            ))
        );
    }
}

#[test]
fn scrollback_resize_retains_loaded_document_query_and_selection() {
    let ctx = egui::Context::default();
    let store = Arc::new(SnapshotStore::new());
    let range = SnapshotCellRange {
        start: SnapshotCell { x: 2, y: 50 },
        end: SnapshotCell { x: 7, y: 51 },
    };
    let mut publisher = PublishingStreamingItemStoreWithPayload::new(store.clone());
    publisher.add_item(b"chosen", range);
    publisher.complete();
    let mut picker = items(&ctx);
    picker.source = PickerSource::test_scrollback(store);
    let document = nfm_egui::styled_document::DocumentBuilder::default().finish();
    assert!(picker.source.document().set(document.clone()).is_ok());
    picker.shared = nfm_egui::Picker::new(
        "resize-reset-test",
        &ctx,
        picker.source.provider(),
        nfm_egui::PickerConfig::default(),
    );
    assert_eq!(wait_selection(&ctx, &mut picker).item.range, Some(range));
    picker.shared.set_query("chosen");
    wait_selection(&ctx, &mut picker);
    assert!(!picker.prepare_snapshot_resize());
    assert_eq!(picker.shared.query(), "chosen");
    assert_eq!(picker.shared.selection().unwrap().item.range, Some(range));
    assert!(Arc::ptr_eq(
        picker.source.document().get().unwrap(),
        &document
    ));
}

#[test]
fn scrollback_resize_closes_an_unfinished_capture() {
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    picker.source = PickerSource::test_scrollback(Arc::new(SnapshotStore::new()));
    assert!(picker.prepare_snapshot_resize());
}

#[test]
fn full_pane_pipe_options_set_placement_and_result_rows() {
    let stream: pipe::StreamOptions = serde_json::from_value(serde_json::json!({
        "position":"FullPane", "result_rows":5, "preview_rows":1,
        "preview_visible":true, "preview":{"type":"Text", "text":"preview", "wrap":true}
    }))
    .unwrap();
    let options = config::Bindings::pipe_stream(&stream)
        .unwrap()
        .resolve(&Default::default())
        .unwrap();
    let ctx = egui::Context::default();
    let mut picker = Picker::start(std::ptr::null_mut(), None, false, 0, ctx.clone(), options);
    assert_eq!(picker.result_rows, 5);
    assert_eq!(picker.position, config::Position::FullPane);
    let bounds = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 600.0));
    assert_eq!(
        picker.placement(bounds).position,
        nfm_egui::Position::FullPane
    );
    let (session, _replies) = pipe::Session::new("full-pane".into());
    picker.attach_pipe(&ctx, session, Some(&stream));
    assert_eq!(picker.result_rows, 5);
    assert_eq!(picker.position, config::Position::FullPane);
}

#[test]
fn text_preview_wrap_pipe_options_validate() {
    for (field, expected) in [(None, false), (Some(false), false), (Some(true), true)] {
        let mut preview = serde_json::json!({"type":"Text", "text":"long line"});
        if let Some(wrap) = field {
            preview["wrap"] = wrap.into();
        }
        let options = config::Bindings::pipe_stream(&pipe::StreamOptions {
            preview: Some(preview),
            ..Default::default()
        })
        .unwrap()
        .resolve(&Default::default())
        .unwrap();
        assert!(
            matches!(options.preview, Some(config::Preview::Text { wrap, .. }) if wrap == expected)
        );
    }
    assert!(
        config::Bindings::pipe_stream(&pipe::StreamOptions {
            preview: Some(serde_json::json!({"type":"Text", "text":"long line", "wrap":"true"})),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn text_preview_wrap_copy_preserves_hard_breaks_and_unicode() {
    for wrap in [false, true] {
        let ctx = egui::Context::default();
        let text = "a猫e\u{301}".repeat(80) + "\nsecond";
        let mut picker = Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            config::Options {
                source: config::Source::Items(config::ItemsOptions {
                    values: vec![wire::Item {
                        text: "item".into(),
                        value: "item".into(),
                    }],
                    provider: None,
                }),
                preview: Some(config::Preview::Text {
                    text: text.clone(),
                    wrap,
                }),
                ..Default::default()
            },
        );
        wait_selection(&ctx, &mut picker);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !picker.shared.preview_focused() {
                frame(
                    &ctx,
                    &mut picker,
                    vec![key(egui::Key::W, egui::Modifiers::CTRL)],
                );
            }
            let (_, commands) = frame(
                &ctx,
                &mut picker,
                vec![key(
                    egui::Key::C,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                )],
            );
            if let Some(copied) = commands.iter().find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text),
                _ => None,
            }) {
                assert_eq!(copied.trim_end_matches('\n'), text, "wrap={wrap}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "text preview was not copied: wrap={wrap}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn worker_resolver(callback: &str) -> std::rc::Rc<crate::lua_callbacks::PreviewResolver> {
    let lua = std::rc::Rc::new(mlua::Lua::new());
    let worker = crate::preview_worker::Worker::new(
        crate::preview_worker::ConfigSource(
            "return {pickers={test={preview={resolve=".to_owned() + callback + "}}}}",
            "@preview-test".into(),
            String::new(),
        ),
        "test".into(),
        None,
    );
    std::rc::Rc::new(crate::lua_callbacks::PreviewResolver {
        key: lua.create_registry_value(mlua::Value::Nil).unwrap(),
        lua,
        name: "test".into(),
        worker: Some(worker),
    })
}

/// The rendered preview of `value` in the bundled `nterm.git` picker built
/// by the Lua expression `picker` (e.g. `branches()`), in a scratch repository
/// prepared by `setup`, once its first line is `first_line`. None when git is
/// not installed.
fn git_module_preview(
    picker: &str,
    setup: impl FnOnce(&std::path::Path, &dyn Fn(&[&str])),
    value: &str,
    first_line: &str,
) -> Option<String> {
    let label: String = picker.chars().filter(char::is_ascii_alphanumeric).collect();
    let root =
        std::env::temp_dir().join(format!("ghostty-worker-git-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(&root)
            .output()
    };
    if git(&["init", "-q", "-b", "main"]).is_err() {
        eprintln!("git is not installed; skipping");
        return None;
    }
    setup(&root, &|args| {
        let output = git(args).unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    });
    // Preview workers evaluate the config again, so they need the module path too.
    let lua = std::rc::Rc::new(mlua::Lua::new());
    let resolver = std::rc::Rc::new(crate::lua_callbacks::PreviewResolver {
        key: lua.create_registry_value(mlua::Value::Nil).unwrap(),
        lua,
        name: "test".into(),
        worker: Some(crate::preview_worker::Worker::new(
            crate::preview_worker::ConfigSource(
                format!("return {{ pickers = {{ test = require('nterm.git').{picker} }} }}"),
                "@git-module-test".into(),
                crate::preview_worker::module_path(&root.join("nterm.lua")),
            ),
            "test".into(),
            Some(crate::preview_worker::FactoryContext(
                serde_json::json!({"line": "", "line_before_cursor": ""}),
            )),
        )),
    });
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![wire::Item {
                    text: value.into(),
                    value: value.into(),
                }],
                provider: None,
            }),
            preview: Some(config::Preview::Dynamic {
                resolve: Some(resolver),
                max_bytes: None,
                max_scrollback_lines: None,
            }),
            params: [("cwd".into(), root.to_string_lossy().into_owned())].into(),
            ..Default::default()
        },
    );
    let text = wait_preview_rendered_text(&ctx, &mut picker, first_line);
    drop(picker);
    std::fs::remove_dir_all(&root).unwrap();
    Some(text)
}

#[test]
fn bundled_git_branch_preview_loads_in_workers_and_runs_git_directly() {
    let Some(text) = git_module_preview(
        "branches()",
        |_, git| {
            git(&["commit", "-q", "--allow-empty", "-m", "first"]);
            git(&["branch", "feature/x"]);
            git(&["commit", "-q", "--allow-empty", "-m", "second"]);
        },
        " \tfeature/x\tlocal\t2026-01-01\tTest\tfirst",
        "feature/x: 0 ahead, 1 behind main",
    ) else {
        return;
    };
    assert!(text.contains("first") && !text.contains("second"), "{text}");
}

#[test]
fn bundled_git_status_preview_shows_staged_unstaged_and_untracked_files() {
    let Some(text) = git_module_preview(
        "status()",
        |root, git| {
            std::fs::write(root.join("a.txt"), "one\n").unwrap();
            git(&["add", "a.txt"]);
            git(&["commit", "-q", "-m", "first"]);
            std::fs::write(root.join("a.txt"), "two\n").unwrap();
            git(&["add", "a.txt"]);
            std::fs::write(root.join("a.txt"), "three\n").unwrap();
        },
        "MM a.txt",
        "Staged",
    ) else {
        return;
    };
    // The unstaged diff follows, partly below the visible preview rows.
    assert!(
        text.contains("+two") && text.contains("Not staged"),
        "{text}"
    );

    // An untracked name with a space is quoted by git and shown as file text.
    git_module_preview(
        "status()",
        |root, _| std::fs::write(root.join("new file.txt"), "hello untracked\n").unwrap(),
        "?? \"new file.txt\"",
        "hello untracked",
    );
}

#[test]
fn lua_preview_worker_cancels_busy_lua_and_retains_helpers_and_closures() {
    let root = std::env::temp_dir().join(format!("ghostty-lua-worker-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("text.txt"), "worker file content").unwrap();
    std::fs::write(
        root.join("image.png"),
        [
            137u8, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 244, 34, 127, 138, 0, 0, 0, 14, 73, 68, 65, 84, 120, 156, 99, 16, 145,
            211, 248, 15, 194, 0, 9, 95, 2, 179, 214, 64, 22, 134, 0, 0, 0, 0, 73, 69, 78, 68, 174,
            66, 96, 130,
        ],
    )
    .unwrap();
    let source = r#"
        local function read_bytes(path, cwd)
            local f = assert(io.open(cwd .. '/' .. path, 'rb'))
            local data = f:read('*a'); f:close(); return data
        end
        local function read(path, cwd) return {read_bytes(path, cwd)} end
        local run = preview.run
        return {pickers={x=function(context)
          local prefix = context.line
          return {preview={resolve=function(item, params)
            if item.value == 'slow' then
                local f = assert(io.open(params.cwd .. '/started', 'w')); f:write('yes'); f:close()
                -- Unlike a Lua loop, this native call cannot observe the hook.
                os.execute('cmd.exe /d /c ping -n 4 127.0.0.1 >nul')
            elseif item.value == 'file' then
                return {type='Text',lines=read('text.txt', params.cwd)}
            elseif item.value == 'image' then
                return {type='Image',bytes=read_bytes('image.png', params.cwd)}
            elseif item.value == 'timeout' then
                local ok = pcall(run, {args={'pwsh.exe','-NoProfile','-Command','Start-Sleep -Seconds 30'},timeout_ms=100})
                assert(not ok)
                return {type='Text',text='timeout handled'}
            else
                local result = run {args={'cmd.exe','/d','/c','echo command'},cwd=params.cwd}
                assert(result.exit_code == 0)
                return {type='Text',text=prefix .. result.stdout}
            end
          end}}
        end}}
    "#;
    let worker = crate::preview_worker::Worker::new(
        crate::preview_worker::ConfigSource(source.into(), "@worker-test".into(), String::new()),
        "x".into(),
        Some(crate::preview_worker::FactoryContext(
            serde_json::json!({"line":"captured "}),
        )),
    );
    let lua = std::rc::Rc::new(mlua::Lua::new());
    let resolver = std::rc::Rc::new(crate::lua_callbacks::PreviewResolver {
        key: lua.create_registry_value(mlua::Value::Nil).unwrap(),
        lua,
        name: "x".into(),
        worker: Some(worker),
    });
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: ["slow", "file", "command", "timeout", "image"]
                    .into_iter()
                    .map(|v| wire::Item {
                        text: v.into(),
                        value: v.into(),
                    })
                    .collect(),
                provider: None,
            }),
            preview: Some(config::Preview::Dynamic {
                resolve: Some(resolver),
                max_bytes: Some(4096),
                max_scrollback_lines: Some(100),
            }),
            params: [
                ("cwd".into(), root.to_string_lossy().into_owned()),
                ("query".into(), "slow".into()),
            ]
            .into(),
            ..Default::default()
        },
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("started").exists() {
        frame(&ctx, &mut picker, vec![]);
        assert!(Instant::now() < deadline, "worker did not start");
        std::thread::sleep(Duration::from_millis(2));
    }
    // A native Lua call is blocked on another thread; UI frames must still finish.
    let start = Instant::now();
    frame(&ctx, &mut picker, vec![]);
    assert!(start.elapsed() < Duration::from_secs(1));
    let handoff = Instant::now();
    picker.shared.set_query("file");
    wait_preview_rendered_text(&ctx, &mut picker, "worker file content");
    assert!(
        handoff.elapsed() < Duration::from_secs(2),
        "new selection waited for blocked Lua IO"
    );
    picker.shared.set_query("image");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                picker.show(ui.ctx(), 24.0);
            },
        );
        let rendered = output
            .textures_delta
            .set
            .iter()
            .any(|(_, deltas)| deltas.iter().any(|delta| delta.image.size() == [2, 1]));
        output.textures_delta.clear();
        if rendered {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Lua image bytes were not decoded and rendered"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    picker.shared.set_query("command");
    wait_preview_rendered_text(&ctx, &mut picker, "captured command");
    picker.shared.set_query("timeout");
    wait_preview_rendered_text(&ctx, &mut picker, "timeout handled");
    picker.shared.set_query("file");
    wait_preview_rendered_text(&ctx, &mut picker, "worker file content");
    drop(picker);
    // Lua can resume before a timed-out process finishes spawning and releases
    // its working-directory handle. Cleanup remains owned by the command job.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(error) if error.raw_os_error() == Some(32) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("command did not release its fixture: {error}"),
        }
    }
}

#[test]
fn dynamic_preview_survives_stream_publications_before_completion() {
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: Vec::new(),
                provider: None,
            }),
            preview: Some(config::Preview::Dynamic {
                resolve: Some(worker_resolver(
                    "function(item) calls=(calls or 0)+1; return {type='Text',text='preview call '..calls} end",
                )),
                max_bytes: None,
                max_scrollback_lines: None,
            }),
            sort: config::Sort::SourceOrder,
            ..Default::default()
        },
    );
    let (session, rx) = pipe::Session::new("preview-stream-test".into());
    picker.attach_pipe(
        &ctx,
        session.clone(),
        Some(&pipe::StreamOptions {
            preview_visible: true,
            ..Default::default()
        }),
    );
    for sequence in 0..4 {
        let previous_version = picker.shared.selection().map(|s| s.source_version);
        picker.pipe_event(
            &ctx,
            pipe::Event::Items {
                sequence,
                items: vec![pipe::Item {
                    id: format!("item-{sequence}"),
                    text: format!("item {sequence}"),
                    value: serde_json::json!(format!("value-{sequence}")),
                }],
                session: session.clone(),
            },
        );
        assert_eq!(rx.try_recv().unwrap()["type"], "ack");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            frame(&ctx, &mut picker, Vec::new());
            if picker
                .shared
                .selection()
                .is_some_and(|s| Some(s.source_version) != previous_version)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "stream publication was not displayed"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        wait_preview_rendered_text(&ctx, &mut picker, "preview call 1");
    }
}

#[test]
fn pipe_stream_publishes_before_end_and_returns_value_without_pasting() {
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    let (session, rx) = pipe::Session::new("stream-test".into());
    picker.attach_pipe(&ctx, session.clone(), Some(&pipe::StreamOptions::default()));
    picker.pipe_event(
        &ctx,
        pipe::Event::Items {
            sequence: 0,
            items: vec![pipe::Item {
                id: "original".into(),
                text: "streamed item".into(),
                value: serde_json::json!({"path":"selected-value"}),
            }],
            session: session.clone(),
        },
    );
    assert_eq!(rx.try_recv().unwrap()["type"], "ack");
    wait_selection(&ctx, &mut picker);
    let (action, output) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(matches!(action, Some(Action::Cancel)));
    assert!(output.is_empty());
    let result = rx.try_recv().unwrap();
    assert_eq!(result["status"], "accepted");
    assert_eq!(result["selected"][0]["id"], "original");
    assert_eq!(result["selected"][0]["value"]["path"], "selected-value");
}

#[test]
fn preview_appearance_receives_configured_copy_colors() {
    let ctx = egui::Context::default();
    let picker = items(&ctx);
    let mut frame = ctx.run_ui(egui::RawInput::default(), |_| {});
    frame.textures_delta.clear();
    let appearance = picker.appearance(&ctx);
    let palette = crate::colors::palette();
    assert_eq!(
        appearance.palette.search_active_background,
        Some(palette.match_highlight)
    );
    assert_eq!(
        appearance.palette.search_active_text,
        Some(palette.background)
    );
    assert_eq!(
        appearance.palette.search_match_background,
        Some(palette.search_match_background)
    );
    assert_eq!(
        appearance.palette.search_match_text,
        Some(palette.search_match_text)
    );
    assert_eq!(
        appearance.palette.yank_background,
        Some(palette.yank_background)
    );
    assert_eq!(appearance.palette.yank_text, Some(palette.yank_text));
}

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

fn wait_preview_rendered_text(
    ctx: &egui::Context,
    picker: &mut Picker,
    expected_first_line: &str,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                picker.show(ui.ctx(), 24.0);
            },
        );
        output.textures_delta.clear();
        let mut glyphs: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => {
                    Some((text.pos.y, text.pos.x, text.galley.text().to_owned()))
                }
                _ => None,
            })
            .collect();
        glyphs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut lines = Vec::<String>::new();
        let mut row_y = f32::NEG_INFINITY;
        for (y, _, text) in glyphs {
            if (y - row_y).abs() > 1.0 {
                lines.push(String::new());
                row_y = y;
            }
            lines.last_mut().unwrap().push_str(&text);
        }
        if lines
            .first()
            .is_some_and(|line| line.trim() == expected_first_line)
        {
            return lines.join("\n");
        }
        assert!(
            Instant::now() < deadline,
            "Preview never showed {expected_first_line}; rendered lines: {lines:?}"
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
                values: vec![wire::Item {
                    text: "display".into(),
                    value: "inserted".into(),
                }],
                provider: None,
            }),
            preview: Some(config::Preview::Command {
                max_bytes: None,
                max_scrollback_lines: None,
                args: vec![
                    "cmd.exe".into(),
                    "/d".into(),
                    "/c".into(),
                    "echo {item}& cd".into(),
                ],
                cwd: Some(cwd.clone()),
            }),
            ..Default::default()
        },
    );
    let text = wait_preview_rendered_text(&ctx, &mut picker, "inserted");
    assert!(
        text.to_lowercase()
            .contains(&cwd.trim_end_matches(['/', '\\']).to_lowercase())
    );
}

#[cfg(windows)]
#[test]
fn lua_command_preview_resolves_once_and_nfm_owns_centering_paging_and_selection_changes() {
    let ctx = egui::Context::default();
    let command = worker_resolver(
        r#"function(item, params)
        assert(params.marker == 'passed')
        local result = preview.run {args={'cmd.exe','/d','/c','for /L %i in (0,1,29) do @echo row%i'}}
        assert(result.exit_code == 0)
        return {type='Ansi',bytes=result.stdout,center_line=tonumber(item.value)}
    end"#,
    );
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![
                    wire::Item {
                        text: "first".into(),
                        value: "20".into(),
                    },
                    wire::Item {
                        text: "second".into(),
                        value: "1".into(),
                    },
                ],
                provider: None,
            }),
            preview: Some(config::Preview::Dynamic {
                resolve: Some(command),
                max_bytes: None,
                max_scrollback_lines: None,
            }),
            sort: config::Sort::SourceOrder,
            params: [("marker".into(), "passed".into())].into(),
            ..Default::default()
        },
    );
    wait_preview_rendered_text(&ctx, &mut picker, "row14");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::PageUp, egui::Modifiers::CTRL)],
    );
    wait_preview_rendered_text(&ctx, &mut picker, "row3");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::PageDown, egui::Modifiers::CTRL)],
    );
    wait_preview_rendered_text(&ctx, &mut picker, "row14");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::N, egui::Modifiers::CTRL)],
    );
    wait_preview_rendered_text(&ctx, &mut picker, "row0");
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
    wait_preview_rendered_text(&ctx, &mut picker, "row0");
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
                values: vec![wire::Item {
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
                values: vec![wire::Item {
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
        if !picker.shared.preview_focused() {
            frame(
                &ctx,
                &mut picker,
                vec![key(egui::Key::W, egui::Modifiers::CTRL)],
            );
        }
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
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Space, egui::Modifiers::ALT)],
    );
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    assert!(picker.shared.preview_focused());
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::W, egui::Modifiers::CTRL)],
    );
    assert!(!picker.shared.preview_focused());
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Space, egui::Modifiers::ALT)],
    );
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    assert!(!picker.shared.preview_focused());
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Space, egui::Modifiers::ALT)],
    );
    let (action, _) = frame(&ctx, &mut picker, vec![egui::Event::Text("a".into())]);
    assert!(action.is_none());
    assert!(picker.shared.preview_focused());
    let (_, commands) = frame(
        &ctx,
        &mut picker,
        vec![
            key(egui::Key::Y, egui::Modifiers::NONE),
            key(egui::Key::Y, egui::Modifiers::NONE),
        ],
    );
    assert!(commands.contains(&egui::OutputCommand::CopyText("second\n".into())));
}

#[test]
fn native_file_preview_callback_centers_pages_caches_and_copies_original_text() {
    let root = std::env::temp_dir().join(format!(
        "ghostty-native-file-preview-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("preview.rs");
    let text = (0..40)
        .map(|i| format!("row{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, &text).unwrap();
    let file = worker_resolver(
        r#"function(item, params)
        assert(item.text == 'display')
        local f = assert(io.open(params.cwd .. '/' .. item.value, 'rb')); local text = f:read('*a'); f:close(); return {type='Text',text=text,center_line=20,highlight_line=20}
    end"#,
    );
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![wire::Item {
                    text: "display".into(),
                    value: "preview.rs".into(),
                }],
                provider: None,
            }),
            preview: Some(config::Preview::Dynamic {
                resolve: Some(file),
                max_bytes: None,
                max_scrollback_lines: None,
            }),
            params: [("cwd".into(), root.to_string_lossy().into_owned())].into(),
            ..Default::default()
        },
    );
    wait_preview_rendered_text(&ctx, &mut picker, "row13");
    std::fs::write(&path, "changed after preview").unwrap();
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::PageUp, egui::Modifiers::CTRL)],
    );
    wait_preview_rendered_text(&ctx, &mut picker, "row2");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::W, egui::Modifiers::CTRL)],
    );
    assert!(picker.shared.preview_focused());
    let (_, commands) = frame(
        &ctx,
        &mut picker,
        vec![key(
            egui::Key::C,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        )],
    );
    assert!(commands.contains(&egui::OutputCommand::CopyText(text)));
    drop(picker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn dynamic_preview_switches_between_native_file_image_and_text_and_caches_commands() {
    let root = std::env::temp_dir().join(format!("ghostty-dynamic-preview-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("native.txt"), "native dynamic text").unwrap();
    std::fs::write(
        root.join("image.png"),
        [
            137u8, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 244, 34, 127, 138, 0, 0, 0, 14, 73, 68, 65, 84, 120, 156, 99, 16, 145,
            211, 248, 15, 194, 0, 9, 95, 2, 179, 214, 64, 22, 134, 0, 0, 0, 0, 73, 69, 78, 68, 174,
            66, 96, 130,
        ],
    )
    .unwrap();
    let resolve = worker_resolver(
        r#"function(item, params)
            calls = (calls or 0) + 1
            if item.value == 'file' then
                local f = assert(io.open(params.cwd .. '/native.txt', 'rb')); local text = f:read('*a'); f:close(); return {type='Text',text=text}
            elseif item.value == 'image' then
                return {type='Command',output='Image',args={'pwsh.exe','-NoProfile','-Command',
                    '[IO.File]::AppendAllText("runs.txt", "x"); [Console]::OpenStandardOutput().Write([IO.File]::ReadAllBytes("image.png"))'}}
            else
                return {type='Command',output='Text',args={'cmd.exe','/d','/c','echo dynamic command text'}}
            end
        end"#,
    );
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: ["file", "image", "text"]
                    .into_iter()
                    .map(|v| wire::Item {
                        text: v.into(),
                        value: v.into(),
                    })
                    .collect(),
                provider: None,
            }),
            preview: Some(config::Preview::Dynamic {
                resolve: Some(resolve),
                max_bytes: Some(1024 * 1024),
                max_scrollback_lines: Some(100),
            }),
            params: [
                ("cwd".into(), root.to_string_lossy().into_owned()),
                ("query".into(), "file".into()),
            ]
            .into(),
            ..Default::default()
        },
    );
    wait_preview_rendered_text(&ctx, &mut picker, "native dynamic text");
    picker.shared.set_query("image");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                picker.show(ui.ctx(), 24.0);
            },
        );
        let texture = output.textures_delta.set.iter().find_map(|(id, deltas)| {
            deltas
                .iter()
                .any(|delta| delta.image.size() == [2, 1])
                .then_some(*id)
        });
        let rendered = texture.is_some_and(|id| {
            output.shapes.iter().any(
                |shape| matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.texture_id == id),
            )
        });
        output.textures_delta.clear();
        if rendered {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Dynamic image preview was not rendered"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..5 {
        frame(
            &ctx,
            &mut picker,
            vec![key(egui::Key::PageDown, egui::Modifiers::CTRL)],
        );
    }
    assert_eq!(std::fs::read_to_string(root.join("runs.txt")).unwrap(), "x");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::W, egui::Modifiers::CTRL)],
    );
    assert!(
        !picker.shared.preview_focused(),
        "Images must not enter text copy mode"
    );
    picker.shared.set_query("text");
    wait_preview_rendered_text(&ctx, &mut picker, "dynamic command text");
    picker.shared.set_query("file");
    wait_preview_rendered_text(&ctx, &mut picker, "native dynamic text");
    frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::W, egui::Modifiers::CTRL)],
    );
    let (_, commands) = frame(
        &ctx,
        &mut picker,
        vec![key(
            egui::Key::C,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        )],
    );
    assert!(commands.contains(&egui::OutputCommand::CopyText("native dynamic text".into())));
    drop(picker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn floating_palette_selection_returns_id_instead_of_display_label() {
    let ctx = egui::Context::default();
    let mut picker = Picker::start(
        std::ptr::null_mut(),
        None,
        false,
        0,
        ctx.clone(),
        config::Options {
            source: config::Source::CommandPalette(vec![
                crate::actions::floating_pane_entry(7, Some("tools"), "PowerShell", false),
                crate::actions::floating_pane_entry(12, None, "nvim", true),
            ]),
            params: [("query".into(), "tools".into())].into(),
            ..Default::default()
        },
    );
    let selected = wait_selection(&ctx, &mut picker);
    assert!(selected.text.contains("tools"));
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(matches!(
        action,
        Some(Action::Execute(
            crate::actions::AppAction::RestoreFloatingPane(7)
        ))
    ));
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
fn workspace_picker_uses_workspace_bottom_and_returns_stable_targets() {
    use crate::actions::AppAction;
    use crate::terminal::{workspace::Workspace, workspace_ui::workspace_picker_options};
    let first = Workspace::new(41, 1);
    let second = Workspace::new(97, 2);
    use crate::terminal::PaneRef;
    for pane in [None, Some(PaneRef::Tiled(123)), Some(PaneRef::Floating(5))] {
        let ctx = egui::Context::default();
        let mut options = workspace_picker_options(&[first.clone(), second.clone()], 41, pane);
        assert_eq!(options.scope, config::Scope::Workspace);
        assert!(matches!(options.position, config::Position::Bottom));
        options.params.insert("query".into(), "2".into());
        let mut picker = Picker::start(std::ptr::null_mut(), None, false, 0, ctx.clone(), options);
        let selected = wait_selection(&ctx, &mut picker);
        assert!(selected.text.contains("2"));
        assert_eq!(
            picker.source.label(),
            if pane.is_some() {
                "MOVE PANE TO WORKSPACE"
            } else {
                "WORKSPACES"
            }
        );
        let (action, _) = frame(
            &ctx,
            &mut picker,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        let expected = match pane {
            Some(pane) => AppAction::MovePaneToWorkspaceId {
                pane,
                workspace: 97,
            },
            None => AppAction::SelectWorkspaceId(97),
        };
        assert!(matches!(action, Some(Action::Execute(action)) if action == expected));
    }
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
    assert!(picker.shared.is_closed());
    assert_eq!(text, "café shown");
    assert_eq!(value, "inserted value");
    assert!(!filesystem && range.is_none());
    assert_eq!(params["fixture"], "kept");
}

#[test]
fn picker_local_lua_key_receives_selection_without_closing() {
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    let lua = std::rc::Rc::new(mlua::Lua::new());
    let callback = std::rc::Rc::new(crate::lua_callbacks::Callback {
        key: lua
            .create_registry_value(
                lua.load("return function(app,item) app:close_picker(); app:paste(item.value) end")
                    .eval::<mlua::Function>()
                    .unwrap(),
            )
            .unwrap(),
        lua,
        name: "key fixture".into(),
    });
    picker.keys.push(config::PickerKey {
        description: "Paste selected item".into(),
        trigger: config::BindingTrigger::Key(egui::Key::Enter),
        mods: egui::Modifiers::CTRL,
        action: callback,
    });
    wait_selection(&ctx, &mut picker);
    let query = picker.shared.query().to_owned();
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![
            key(egui::Key::Enter, egui::Modifiers::CTRL),
            egui::Event::Text("x".into()),
        ],
    );
    let Some(Action::Custom(callback, text, filesystem, range, value, params)) = action else {
        panic!("missing callback");
    };
    assert_eq!(picker.shared.query(), query);
    assert!(!picker.shared.is_closed());
    let commands = callback
        .call_with_selection(
            &text,
            &value,
            filesystem,
            None,
            range,
            &params,
            String::new(),
        )
        .unwrap();
    assert!(
        matches!(&commands[..], [crate::lua_callbacks::Command::ClosePicker, crate::lua_callbacks::Command::Paste(text)] if text == "inserted value")
    );
    let mut repeat = key(egui::Key::Enter, egui::Modifiers::CTRL);
    if let egui::Event::Key { repeat, .. } = &mut repeat {
        *repeat = true;
    }
    assert!(
        frame(
            &ctx,
            &mut picker,
            vec![repeat, egui::Event::Text("x".into())]
        )
        .0
        .is_none()
    );
    assert_eq!(picker.shared.query(), query);
    let mut release = key(egui::Key::Enter, egui::Modifiers::CTRL);
    if let egui::Event::Key { pressed, .. } = &mut release {
        *pressed = false;
    }
    assert!(frame(&ctx, &mut picker, vec![release]).0.is_none());
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(matches!(action, Some(Action::SendInput(value)) if value == "inserted value"));
}

#[test]
fn picker_local_key_cannot_use_selection_after_query_edit() {
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    let lua = std::rc::Rc::new(mlua::Lua::new());
    picker.keys.push(config::PickerKey {
        description: "Stale fixture".into(),
        trigger: config::BindingTrigger::Key(egui::Key::Enter),
        mods: egui::Modifiers::CTRL,
        action: std::rc::Rc::new(crate::lua_callbacks::Callback {
            key: lua
                .create_registry_value(
                    lua.load("return function() error('stale') end")
                        .eval::<mlua::Function>()
                        .unwrap(),
                )
                .unwrap(),
            lua,
            name: "stale fixture".into(),
        }),
    });
    wait_selection(&ctx, &mut picker);
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![
            egui::Event::Text("no match".into()),
            key(egui::Key::Enter, egui::Modifiers::CTRL),
        ],
    );
    assert!(action.is_none());
    assert!(!picker.shared.is_closed());
}

#[test]
fn picker_enter_override_replaces_default_acceptance() {
    let ctx = egui::Context::default();
    let mut picker = items(&ctx);
    let lua = std::rc::Rc::new(mlua::Lua::new());
    let table = lua
        .load("return {{key='Enter', action=function(app,item) app:paste(item.value) end}}")
        .eval::<mlua::Table>()
        .unwrap();
    picker.keys =
        crate::config::keybindings::parse_picker_keys(&lua, "fixture", Some(table)).unwrap();
    wait_selection(&ctx, &mut picker);
    let (action, _) = frame(
        &ctx,
        &mut picker,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
    );
    assert!(matches!(action, Some(Action::Custom(..))));
    assert!(!picker.shared.is_closed());
}

#[test]
fn replace_picker_from_accept_and_local_key_resets_source_and_callbacks() {
    for local_key in [false, true] {
        let ctx = egui::Context::default();
        let mut previous = items(&ctx);
        let lua = std::rc::Rc::new(mlua::Lua::new());
        let callback: mlua::Function = lua.load("return function(app,item) app:replace_picker('Details', {selected=item.value, query='target'}) end").eval().unwrap();
        previous.callback = Some(std::rc::Rc::new(crate::lua_callbacks::Callback {
            key: lua.create_registry_value(callback).unwrap(),
            lua: lua.clone(),
            name: "source".into(),
        }));
        if local_key {
            let keys = lua.load("return {{key='d', action=function(app,item) app:replace_picker('Details', {selected=item.value, query='target'}) end}}").eval().unwrap();
            previous.keys =
                crate::config::keybindings::parse_picker_keys(&lua, "source", Some(keys)).unwrap();
        }
        wait_selection(&ctx, &mut previous);
        let (action, _) = frame(
            &ctx,
            &mut previous,
            vec![key(
                if local_key {
                    egui::Key::D
                } else {
                    egui::Key::Enter
                },
                egui::Modifiers::NONE,
            )],
        );
        let Some(Action::Custom(callback, text, filesystem, range, value, params)) = action else {
            panic!("expected callback")
        };
        assert_eq!(previous.is_closed(), !local_key);
        let commands = callback
            .call(&text, &value, filesystem, None, range, &params)
            .unwrap();
        let crate::lua_callbacks::Command::ReplacePicker { name, params } = &commands[0] else {
            panic!("expected replacement")
        };
        assert_eq!(name, "Details");
        assert_eq!(params["selected"], "inserted value");
        let target = config::Options {
            source: config::Source::Items(config::ItemsOptions {
                values: vec![wire::Item {
                    text: "target item".into(),
                    value: "target value".into(),
                }],
                provider: None,
            }),
            enter_action: config::EnterAction::SendInput,
            preview: Some(config::Preview::Text {
                text: "new preview".into(),
                wrap: true,
            }),
            ..Default::default()
        }
        .with_request(params.clone())
        .unwrap();
        let mut replacement = Picker::start_replacing(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            target,
            Some(previous),
        );
        assert!(!replacement.is_closed());
        assert!(replacement.callback.is_none());
        assert!(replacement.keys.is_empty());
        assert_eq!(replacement.shared.query(), "target");
        assert_eq!(wait_selection(&ctx, &mut replacement).text, "target item");
        let (action, _) = frame(
            &ctx,
            &mut replacement,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        assert!(matches!(action, Some(Action::SendInput(value)) if value == "target value"));
    }
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
                columns: None,
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
    assert_eq!(selection.item.range, Some(range));
    // Native navigation now requires a retained logical document and a valid
    // snapshot; an old physical payload alone must never be accepted.
    assert!(picker.accept(selection).is_none());
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
                values: vec![wire::Item {
                    text: "one".into(),
                    value: "one".into(),
                }],
                provider: None,
            }),
            preview: Some(config::Preview::Text {
                text: "Preview {item}".into(),
                wrap: false,
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
        m.has_focus(egui::Id::new(("nterm-picker", foreground.session_id)).with("query"))
    }));
    let panel = ctx
        .memory(|m| m.area_rect(egui::Id::new(("nterm-picker", foreground.session_id))))
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
            m.has_focus(egui::Id::new(("nterm-picker", active.session_id)).with("query"))
        }));
    }
    assert_eq!(main.shared.query(), "caféace");
    assert_eq!(floating.shared.query(), "cafébd");
}
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
                relative: false,
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

#[test]
fn picker_double_click_override_receives_item_and_stays_open() {
    for overridden in [false, true] {
        let ctx = egui::Context::default();
        let mut picker = items(&ctx);
        if overridden {
            let lua = std::rc::Rc::new(mlua::Lua::new());
            picker.keys.push(config::PickerKey {
                trigger: config::BindingTrigger::DoubleClick, mods: egui::Modifiers::NONE, description: "Paste clicked item".into(),
                action: std::rc::Rc::new(crate::lua_callbacks::Callback {
                    key: lua.create_registry_value(lua.load("return function(app,item) app:close_picker(); app:paste(item.value) end").eval::<mlua::Function>().unwrap()).unwrap(),
                    lua, name: "double click fixture".into(),
                }),
            });
        }
        wait_selection(&ctx, &mut picker);
        frame(&ctx, &mut picker, vec![]);
        let row = egui::Id::new(("nterm-picker", picker.session_id)).with(("row", 0usize));
        let pos = ctx.read_response(row).expect("result row").rect.center();
        frame(&ctx, &mut picker, vec![egui::Event::PointerMoved(pos)]);
        let mut accepted = None;
        for _ in 0..2 {
            frame(
                &ctx,
                &mut picker,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            let (action, _) = frame(
                &ctx,
                &mut picker,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            if action.is_some() {
                accepted = action;
            }
        }
        if overridden {
            let Some(Action::Custom(_, text, _, _, value, _)) = accepted else {
                panic!("missing clicked item");
            };
            assert_eq!(text, "café shown");
            assert_eq!(value, "inserted value");
            assert!(!picker.shared.is_closed());
        } else {
            assert!(
                matches!(accepted, Some(Action::SendInput(value)) if value == "inserted value")
            );
            assert!(picker.shared.is_closed());
        }
    }
}

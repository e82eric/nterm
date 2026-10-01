use std::ffi::{CStr, c_char, c_void};
use std::path::PathBuf;

extern "C" fn collect_directory(userdata: *mut c_void, path: *const c_char) {
    crate::native_error::ffi_boundary("read native theme directories", || {
        if path.is_null() {
            return;
        }
        let paths = unsafe { &mut *userdata.cast::<Vec<PathBuf>>() };
        paths.push(PathBuf::from(
            unsafe { CStr::from_ptr(path) }.to_string_lossy().as_ref(),
        ));
    });
}

pub(crate) fn installed_themes() -> Result<Vec<String>, String> {
    let mut paths = Vec::<PathBuf>::new();
    if !unsafe {
        crate::ghostty_ffi::ghostty_config_theme_directories(
            (&mut paths as *mut Vec<PathBuf>).cast(),
            collect_directory,
        )
    } {
        return Err("Unable to discover Ghostty theme directories".into());
    }
    names_in_directories(&paths)
}

fn names_in_directories(paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut names = std::collections::BTreeMap::new();
    for path in paths {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|error| format!("{}: {error}", path.display()))?;
            if !entry.path().is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let key = if cfg!(windows) {
                name.to_lowercase()
            } else {
                name.clone()
            };
            names.entry(key).or_insert(name);
        }
    }
    let mut names: Vec<_> = names.into_values().collect();
    names.sort_by_key(|name| name.to_lowercase());
    Ok(names)
}

pub(crate) fn picker_options(names: Vec<String>) -> crate::picker::config::Options {
    use crate::picker::config::*;
    Options {
        position: Position::Center,
        sort: Sort::Score,
        callback: None,
        source: Source::CommandPalette(
            names
                .into_iter()
                .map(|name| crate::actions::Entry {
                    id: format!("theme.{name}"),
                    label: name.clone(),
                    shortcut: None,
                    action: crate::actions::AppAction::ApplyTheme(name),
                })
                .collect(),
        ),
        enter_action: EnterAction::Navigate,
        params: Default::default(),
        preview: None,
        preview_visible: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clicking_theme_row_returns_apply_action() {
        fn text_position(shape: &egui::epaint::Shape) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.text().trim().starts_with("Dracula") =>
                {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(text_position),
                _ => None,
            }
        }
        let ctx = egui::Context::default();
        let mut picker = crate::picker::Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            picker_options(vec!["Dracula".into()]),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut point = None;
        for _ in 0..200 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                picker.show(ui.ctx(), 24.0);
            });
            point = output
                .shapes
                .iter()
                .find_map(|shape| text_position(&shape.shape))
                .or(point);
            output.textures_delta.clear();
            if point.is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let point = point.expect("theme row should render");
        let mut selected = None;
        for pressed in [true, false] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| {
                    selected = picker.show(ui.ctx(), 24.0);
                },
            );
            output.textures_delta.clear();
        }
        assert!(
            matches!(selected, Some(crate::picker::Action::Execute(crate::actions::AppAction::ApplyTheme(name))) if name == "Dracula")
        );
    }
    #[test]
    fn shared_picker_filters_and_returns_theme_action() {
        let ctx = egui::Context::default();
        let mut options = picker_options(vec!["Dracula".into(), "Gruvbox Dark".into()]);
        options.params.insert("query".into(), "Dracula".into());
        let mut picker = crate::picker::Picker::start(
            std::ptr::null_mut(),
            None,
            false,
            0,
            ctx.clone(),
            options,
        );
        assert_eq!(picker.label(), "GHOSTTY THEMES");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut selected = None;
        while selected.is_none() {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: Some(egui::Key::Enter),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    selected = picker.show(ui.ctx(), 24.0);
                },
            );
            output.textures_delta.clear();
            assert!(std::time::Instant::now() < deadline);
            if selected.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        assert!(
            matches!(selected, Some(crate::picker::Action::Execute(crate::actions::AppAction::ApplyTheme(name))) if name == "Dracula")
        );
    }
    #[test]
    fn theme_discovery_sorts_deduplicates_and_skips_directories() {
        let root = std::env::temp_dir().join(format!("ghostty-theme-list-{}", std::process::id()));
        let user = root.join("user");
        let bundled = root.join("bundled");
        std::fs::create_dir_all(user.join("ignored-directory")).unwrap();
        std::fs::create_dir_all(&bundled).unwrap();
        for path in [
            user.join("Zulu"),
            user.join("Alpha"),
            bundled.join("Alpha"),
            bundled.join("Beta"),
        ] {
            std::fs::write(path, "background = #123456").unwrap();
        }
        assert_eq!(
            names_in_directories(&[user, bundled, root.join("missing")]).unwrap(),
            ["Alpha", "Beta", "Zulu"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

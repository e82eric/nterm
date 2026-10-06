use crate::ghostty_ffi::{
    Host, ghostty_egui_host_key, ghostty_egui_host_mouse_button, ghostty_egui_host_mouse_pos,
    ghostty_egui_host_mouse_scroll, ghostty_egui_host_preedit, ghostty_egui_host_text,
};
use std::ffi::CString;

pub(crate) fn forward(
    host: *mut Host,
    terminal_size: [u32; 2],
    ui: &egui::Ui,
    rect: egui::Rect,
    scale: f32,
    modal_input_blocked: bool,
) -> bool {
    let events = ui.input(|input| input.events.clone());
    let pointer_owned = ui.ctx().egui_wants_pointer_input();
    let keyboard_owned = modal_input_blocked || ui.ctx().egui_wants_keyboard_input();
    let mut scrolled = false;

    for (index, event) in events.iter().cloned().enumerate() {
        match event {
            egui::Event::Copy if !keyboard_owned => {
                // egui-winit consumes Ctrl+C as a clipboard shortcut before it
                // can emit a Key event. In the terminal it must reach the PTY.
                let (usb, unshifted) = key_to_usb(egui::Key::C).expect("C has a USB keycode");
                unsafe {
                    ghostty_egui_host_key(
                        host,
                        1,
                        mods(egui::Modifiers::CTRL),
                        usb,
                        unshifted,
                        std::ptr::null(),
                    );
                    ghostty_egui_host_key(
                        host,
                        0,
                        mods(egui::Modifiers::CTRL),
                        usb,
                        unshifted,
                        std::ptr::null(),
                    );
                }
            }
            egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
                ..
            } if !keyboard_owned => {
                // egui-winit emits translated Text immediately after its Key.
                // Keep them in one Ghostty key event; sending Text through the
                // paste API makes each typed character a bracketed paste.
                let text = if pressed {
                    paired_key_text(&events, index).and_then(|text| CString::new(text).ok())
                } else {
                    None
                };
                let native_key = physical_key.unwrap_or(key);
                #[cfg(windows)]
                if let Some(sequence) = win32_picker_key(native_key, modifiers, pressed) {
                    // ConPTY accepts Win32 key events as CSI ... _. This preserves
                    // Ctrl+Shift for PSReadLine, which asks for mode 9001; the
                    // legacy Ctrl+G byte loses Shift before it reaches it.
                    unsafe {
                        ghostty_egui_host_key(host, 1, 0, 0, 0, sequence.as_ptr());
                    }
                    continue;
                }
                let Some((usb, unshifted)) =
                    key_to_usb(native_key).or_else(|| text.as_ref().map(|_| (0, 0)))
                else {
                    continue;
                };
                let action = if !pressed {
                    0
                } else if repeat {
                    2
                } else {
                    1
                };
                unsafe {
                    ghostty_egui_host_key(
                        host,
                        action,
                        mods(modifiers),
                        usb,
                        unshifted,
                        text.as_ref().map_or(std::ptr::null(), |text| text.as_ptr()),
                    );
                }
            }
            egui::Event::Text(text) if !keyboard_owned && !text_follows_key(&events, index) => {
                if let Ok(text) = CString::new(text.as_str()) {
                    unsafe {
                        ghostty_egui_host_key(
                            host,
                            1,
                            mods(ui.input(|input| input.modifiers)),
                            0,
                            0,
                            text.as_ptr(),
                        );
                    }
                }
            }
            egui::Event::Paste(text) if !keyboard_owned => unsafe {
                ghostty_egui_host_text(host, text.as_ptr().cast(), text.len());
            },
            egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) if !keyboard_owned => unsafe {
                ghostty_egui_host_preedit(host, text.as_ptr().cast(), text.len());
            },
            egui::Event::Ime(egui::ImeEvent::Commit(text)) if !keyboard_owned => {
                unsafe { ghostty_egui_host_preedit(host, "".as_ptr().cast(), 0) };
                if let Ok(text) = CString::new(text.as_str()) {
                    unsafe { ghostty_egui_host_key(host, 1, 0, 0, 0, text.as_ptr()) };
                }
            }
            egui::Event::PointerMoved(pos)
                if !pointer_owned && !modal_input_blocked && rect.contains(pos) =>
            unsafe {
                let (x, y) = surface_pointer_position(pos, rect);
                ghostty_egui_host_mouse_pos(host, x, y, mods(ui.input(|i| i.modifiers)));
            },
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } if !pointer_owned && !modal_input_blocked && rect.contains(pos) => {
                let button = match button {
                    egui::PointerButton::Primary => 1,
                    egui::PointerButton::Secondary => 2,
                    egui::PointerButton::Middle => 3,
                    _ => continue,
                };
                unsafe {
                    let (x, y) = surface_pointer_position(pos, rect);
                    ghostty_egui_host_mouse_pos(host, x, y, mods(modifiers));
                    ghostty_egui_host_mouse_button(
                        host,
                        if pressed { 1 } else { 0 },
                        button,
                        mods(modifiers),
                    );
                }
            }
            egui::Event::MouseWheel { unit, delta, .. }
                if !pointer_owned
                    && ui.input(|input| {
                        input
                            .pointer
                            .hover_pos()
                            .is_some_and(|pos| rect.contains(pos))
                    }) =>
            {
                let (factor, scroll_mods) = match unit {
                    egui::MouseWheelUnit::Point => (scale as f64, 1),
                    egui::MouseWheelUnit::Line => (1.0, 0),
                    egui::MouseWheelUnit::Page => (terminal_size[1] as f64, 1),
                };
                unsafe {
                    ghostty_egui_host_mouse_scroll(
                        host,
                        delta.x as f64 * factor,
                        delta.y as f64 * factor,
                        scroll_mods,
                    );
                }
                scrolled = true;
            }
            _ => {}
        }
    }
    scrolled
}

fn surface_pointer_position(pos: egui::Pos2, rect: egui::Rect) -> (f64, f64) {
    // Ghostty's embedded surface converts these logical coordinates to pixels
    // using its content scale. Scaling here would apply the factor twice.
    ((pos.x - rect.left()) as f64, (pos.y - rect.top()) as f64)
}

fn mods(value: egui::Modifiers) -> i32 {
    (if value.shift { 1 } else { 0 })
        | (if value.ctrl { 2 } else { 0 })
        | (if value.alt { 4 } else { 0 })
}

#[cfg(windows)]
fn win32_picker_key(key: egui::Key, modifiers: egui::Modifiers, pressed: bool) -> Option<CString> {
    if !modifiers.ctrl || !modifiers.shift || modifiers.alt {
        return None;
    }
    let (virtual_key, scan_code, unicode) = match key {
        egui::Key::B => (66, 48, 2),
        egui::Key::G => (71, 34, 7),
        egui::Key::H => (72, 35, 8),
        egui::Key::P => (80, 25, 16),
        _ => return None,
    };
    // CSI Vk;Sc;Uc;Kd;Cs;Rc _, with SHIFT_PRESSED | LEFT_CTRL_PRESSED.
    CString::new(format!(
        "\x1b[{virtual_key};{scan_code};{unicode};{};24;1_",
        u8::from(pressed)
    ))
    .ok()
}

fn paired_key_text(events: &[egui::Event], index: usize) -> Option<&str> {
    match events.get(index + 1) {
        Some(egui::Event::Text(text)) => Some(text),
        _ => None,
    }
}

fn text_follows_key(events: &[egui::Event], index: usize) -> bool {
    matches!(
        index
            .checked_sub(1)
            .and_then(|previous| events.get(previous)),
        Some(egui::Event::Key { pressed: true, .. })
    )
}

pub(crate) fn key_to_usb(key: egui::Key) -> Option<(u32, u32)> {
    use egui::Key;
    let pair = match key {
        Key::A => (0x04, 'a'),
        Key::B => (0x05, 'b'),
        Key::C => (0x06, 'c'),
        Key::D => (0x07, 'd'),
        Key::E => (0x08, 'e'),
        Key::F => (0x09, 'f'),
        Key::G => (0x0a, 'g'),
        Key::H => (0x0b, 'h'),
        Key::I => (0x0c, 'i'),
        Key::J => (0x0d, 'j'),
        Key::K => (0x0e, 'k'),
        Key::L => (0x0f, 'l'),
        Key::M => (0x10, 'm'),
        Key::N => (0x11, 'n'),
        Key::O => (0x12, 'o'),
        Key::P => (0x13, 'p'),
        Key::Q => (0x14, 'q'),
        Key::R => (0x15, 'r'),
        Key::S => (0x16, 's'),
        Key::T => (0x17, 't'),
        Key::U => (0x18, 'u'),
        Key::V => (0x19, 'v'),
        Key::W => (0x1a, 'w'),
        Key::X => (0x1b, 'x'),
        Key::Y => (0x1c, 'y'),
        Key::Z => (0x1d, 'z'),
        Key::Num1 => (0x1e, '1'),
        Key::Num2 => (0x1f, '2'),
        Key::Num3 => (0x20, '3'),
        Key::Num4 => (0x21, '4'),
        Key::Num5 => (0x22, '5'),
        Key::Num6 => (0x23, '6'),
        Key::Num7 => (0x24, '7'),
        Key::Num8 => (0x25, '8'),
        Key::Num9 => (0x26, '9'),
        Key::Num0 => (0x27, '0'),
        Key::Enter => (0x28, '\r'),
        Key::Escape => (0x29, '\0'),
        Key::Backspace => (0x2a, '\0'),
        Key::Tab => (0x2b, '\t'),
        Key::Space => (0x2c, ' '),
        Key::Minus => (0x2d, '-'),
        Key::Equals | Key::Plus => (0x2e, '='),
        Key::OpenBracket | Key::OpenCurlyBracket => (0x2f, '['),
        Key::CloseBracket | Key::CloseCurlyBracket => (0x30, ']'),
        Key::Backslash | Key::Pipe => (0x31, '\\'),
        Key::Semicolon | Key::Colon => (0x33, ';'),
        Key::Quote => (0x34, '\''),
        Key::Backtick => (0x35, '`'),
        Key::Comma => (0x36, ','),
        Key::Period => (0x37, '.'),
        Key::Slash | Key::Questionmark => (0x38, '/'),
        Key::F1 => (0x3a, '\0'),
        Key::F2 => (0x3b, '\0'),
        Key::F3 => (0x3c, '\0'),
        Key::F4 => (0x3d, '\0'),
        Key::F5 => (0x3e, '\0'),
        Key::F6 => (0x3f, '\0'),
        Key::F7 => (0x40, '\0'),
        Key::F8 => (0x41, '\0'),
        Key::F9 => (0x42, '\0'),
        Key::F10 => (0x43, '\0'),
        Key::F11 => (0x44, '\0'),
        Key::F12 => (0x45, '\0'),
        Key::Home => (0x4a, '\0'),
        Key::PageUp => (0x4b, '\0'),
        Key::Delete => (0x4c, '\0'),
        Key::End => (0x4d, '\0'),
        Key::PageDown => (0x4e, '\0'),
        Key::ArrowRight => (0x4f, '\0'),
        Key::ArrowLeft => (0x50, '\0'),
        Key::ArrowDown => (0x51, '\0'),
        Key::ArrowUp => (0x52, '\0'),
        _ => return None,
    };
    // Ghostty stores USB keys as the HID usage page in the high 16 bits and
    // the usage ID in the low 16 bits. Keyboard/keypad is usage page 0x07.
    Some((0x0007_0000 | pair.0, pair.1 as u32))
}

#[cfg(test)]
mod tests {
    use super::{key_to_usb, paired_key_text, surface_pointer_position, text_follows_key};

    #[test]
    fn pointer_coordinates_are_local_and_unscaled_for_ghostty() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 500.0));
        assert_eq!(
            surface_pointer_position(egui::pos2(300.0, 175.0), rect),
            (200.0, 125.0)
        );
    }

    #[test]
    #[cfg(windows)]
    fn powershell_picker_chords_preserve_control_and_shift() {
        use super::win32_picker_key;
        let modifiers = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        assert_eq!(
            win32_picker_key(egui::Key::B, modifiers, true)
                .unwrap()
                .to_bytes(),
            b"\x1b[66;48;2;1;24;1_"
        );
        assert_eq!(
            win32_picker_key(egui::Key::G, modifiers, true)
                .unwrap()
                .to_bytes(),
            b"\x1b[71;34;7;1;24;1_"
        );
        assert_eq!(
            win32_picker_key(egui::Key::H, modifiers, false)
                .unwrap()
                .to_bytes(),
            b"\x1b[72;35;8;0;24;1_"
        );
        assert_eq!(
            win32_picker_key(egui::Key::P, modifiers, true)
                .unwrap()
                .to_bytes(),
            b"\x1b[80;25;16;1;24;1_"
        );
        assert!(win32_picker_key(egui::Key::G, egui::Modifiers::CTRL, true).is_none());
    }

    #[test]
    fn keys_include_the_usb_keyboard_usage_page() {
        assert_eq!(
            key_to_usb(egui::Key::Enter),
            Some((0x0007_0028, '\r' as u32))
        );
        assert_eq!(key_to_usb(egui::Key::A), Some((0x0007_0004, 'a' as u32)));
    }

    #[test]
    fn translated_text_is_attached_to_its_key_once() {
        let events = [
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Text("a".into()),
            egui::Event::Paste("pasted".into()),
        ];
        assert_eq!(paired_key_text(&events, 0), Some("a"));
        assert!(text_follows_key(&events, 1));
        assert_eq!(paired_key_text(&events, 1), None);
        assert!(!text_follows_key(&events, 2));
    }
}

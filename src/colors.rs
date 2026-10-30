//! Shared semantic colors for app chrome and UI-owned terminal decorations.
//! ANSI-colored terminal/preview content keeps its own colors.
use egui::Color32;
use serde::Deserialize;
use std::sync::RwLock;

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UiOverrides {
    pub background: Option<UiColor>,
    pub text: Option<UiColor>,
    pub panel_background: Option<UiColor>,
    pub muted_text: Option<UiColor>,
    pub border: Option<UiColor>,
    pub selection_background: Option<UiColor>,
    pub selection_text: Option<UiColor>,
    pub accent: Option<UiColor>,
    pub copy_cursor_background: Option<UiColor>,
    pub success: Option<UiColor>,
    pub warning: Option<UiColor>,
    pub error: Option<UiColor>,
    pub match_highlight: Option<UiColor>,
    pub picker_match_highlight: Option<UiColor>,
    pub search_match_background: Option<UiColor>,
    pub search_match_text: Option<UiColor>,
    pub yank_background: Option<UiColor>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UiColor(Color32);

impl<'de> Deserialize<'de> for UiColor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        parse_color(&value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

fn parse_color(value: &str) -> Result<Color32, String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid UI color {value:?}: expected #RRGGBB or RRGGBB"
        ));
    }
    let rgb = u32::from_str_radix(hex, 16).unwrap();
    Ok(Color32::from_rgb(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ))
}

impl UiOverrides {
    pub(crate) fn apply(self, mut palette: UiPalette) -> UiPalette {
        if let Some(color) = self.background {
            palette.background = color.0;
        }
        if let Some(color) = self.text {
            palette.text = color.0;
        }
        if let Some(color) = self.panel_background {
            palette.panel_background = color.0;
        }
        if let Some(color) = self.muted_text {
            palette.muted_text = color.0;
        }
        if let Some(color) = self.border {
            palette.border = color.0;
        }
        if let Some(color) = self.selection_background {
            palette.selection_background = color.0;
        }
        if let Some(color) = self.selection_text {
            palette.selection_text = color.0;
        }
        if let Some(color) = self.accent {
            palette.accent = color.0;
        }
        if let Some(color) = self.success {
            palette.success = color.0;
        }
        if let Some(color) = self.warning {
            palette.warning = color.0;
        }
        if let Some(color) = self.error {
            palette.error = color.0;
        }
        if let Some(color) = self.match_highlight {
            palette.match_highlight = color.0;
        }
        if let Some(color) = self.copy_cursor_background {
            palette.copy_cursor_background = Some(color.0);
        }
        if let Some(color) = self.picker_match_highlight {
            palette.picker_match_highlight = Some(color.0);
        }
        if let Some(color) = self.search_match_background {
            palette.search_match_background = color.0;
        }
        if let Some(color) = self.search_match_text {
            palette.search_match_text = color.0;
        }
        if let Some(color) = self.yank_background {
            palette.yank_background = Some(color.0);
        }
        palette
    }
}

/// Colors copied from Ghostty after configuration finalization.
#[repr(C)]
pub(crate) struct ThemeColors {
    pub background: u32,
    pub foreground: u32,
    pub selection_background: u32,
    pub selection_foreground: u32,
    pub selection_flags: u32,
    pub ansi: [u32; 16],
}

fn unpack(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub(crate) fn blend(a: Color32, b: Color32, amount: f32) -> Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) * (1.0 - amount) + f32::from(b) * amount).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

fn luminance(color: Color32) -> f32 {
    let channel = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
    let a = luminance(a);
    let b = luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn readable(color: Color32, background: Color32) -> Color32 {
    if contrast(color, background) >= 4.5 {
        return color;
    }
    let target = if contrast(Color32::WHITE, background) > contrast(Color32::BLACK, background) {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    for step in 1..=20 {
        let candidate = blend(color, target, step as f32 / 20.0);
        if contrast(candidate, background) >= 4.5 {
            return candidate;
        }
    }
    target
}

impl UiPalette {
    pub(crate) fn from_theme(theme: &ThemeColors) -> Self {
        let background = unpack(theme.background);
        let text = unpack(theme.foreground);
        let panel_background = blend(background, text, 0.06);
        let search_match_background = blend(background, unpack(theme.ansi[3]), 0.30);
        let selection_background = if theme.selection_flags & 1 != 0 {
            unpack(theme.selection_background)
        } else {
            blend(background, text, 0.25)
        };
        let selection_text = if theme.selection_flags & 2 != 0 {
            unpack(theme.selection_foreground)
        } else {
            readable(text, selection_background)
        };
        Self {
            background,
            text,
            panel_background,
            selection_background,
            selection_text,
            muted_text: readable(blend(background, text, 0.60), panel_background),
            border: blend(background, text, 0.20),
            accent: readable(unpack(theme.ansi[6]), panel_background),
            success: readable(unpack(theme.ansi[2]), panel_background),
            warning: readable(unpack(theme.ansi[3]), panel_background),
            error: readable(unpack(theme.ansi[1]), panel_background),
            match_highlight: readable(unpack(theme.ansi[3]), panel_background),
            picker_match_highlight: None,
            copy_cursor_background: None,
            search_match_background,
            search_match_text: readable(text, search_match_background),
            yank_background: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UiPalette {
    pub background: Color32,
    pub text: Color32,
    pub panel_background: Color32,
    pub muted_text: Color32,
    pub border: Color32,
    pub selection_background: Color32,
    pub selection_text: Color32,
    pub accent: Color32,
    pub copy_cursor_background: Option<Color32>,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    pub match_highlight: Color32,
    pub picker_match_highlight: Option<Color32>,
    pub search_match_background: Color32,
    pub search_match_text: Color32,
    /// Peak of the picker preview yank pulse; None follows its selection.
    pub yank_background: Option<Color32>,
}

impl UiPalette {
    pub const DEFAULT: Self = Self {
        background: Color32::from_rgb(0x28, 0x28, 0x28),
        text: Color32::from_rgb(0xeb, 0xdb, 0xb2),
        panel_background: Color32::from_rgb(0x1d, 0x20, 0x21),
        muted_text: Color32::from_rgb(0xa8, 0x99, 0x84),
        border: Color32::from_rgb(0x92, 0x83, 0x74),
        selection_background: Color32::from_rgb(0x3d, 0x39, 0x37),
        selection_text: Color32::from_rgb(0xeb, 0xdb, 0xb2),
        accent: Color32::from_rgb(0xfa, 0xbd, 0x2f),
        copy_cursor_background: None,
        success: Color32::from_rgb(0xb8, 0xbb, 0x26),
        warning: Color32::from_rgb(0xfa, 0xbd, 0x2f),
        error: Color32::from_rgb(0xfb, 0x49, 0x34),
        match_highlight: Color32::from_rgb(0xfb, 0x49, 0x34),
        picker_match_highlight: None,
        search_match_background: Color32::from_rgb(0x67, 0x55, 0x2a),
        search_match_text: Color32::from_rgb(0xeb, 0xdb, 0xb2),
        yank_background: None,
    };
}

impl Default for UiPalette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

pub(crate) struct ColorManager {
    base: RwLock<(UiPalette, Option<String>)>,
    palette: RwLock<UiPalette>,
    overrides: RwLock<ColorOverrides>,
}

#[derive(Default)]
pub(crate) struct ColorOverrides {
    pub(crate) global: UiOverrides,
    pub(crate) themes: std::collections::BTreeMap<String, UiOverrides>,
}

impl ColorOverrides {
    fn apply(&self, palette: UiPalette, theme_name: Option<&str>) -> UiPalette {
        let palette = self.global.apply(palette);
        theme_name
            .and_then(|name| self.themes.get(name))
            .map_or(palette, |overrides| overrides.apply(palette))
    }
}

pub(crate) static COLORS: ColorManager = ColorManager {
    base: RwLock::new((UiPalette::DEFAULT, None)),
    palette: RwLock::new(UiPalette::DEFAULT),
    overrides: RwLock::new(ColorOverrides {
        themes: std::collections::BTreeMap::new(),
        global: UiOverrides {
            background: None,
            text: None,
            panel_background: None,
            muted_text: None,
            border: None,
            selection_background: None,
            selection_text: None,
            accent: None,
            copy_cursor_background: None,
            success: None,
            warning: None,
            error: None,
            match_highlight: None,
            picker_match_highlight: None,
            search_match_background: None,
            search_match_text: None,
            yank_background: None,
        },
    }),
};

impl ColorManager {
    pub(crate) fn reload(&self, ctx: &egui::Context, overrides: ColorOverrides) {
        self.configure(overrides);
        let base = self.base.read().unwrap_or_else(|error| error.into_inner());
        let overrides = self
            .overrides
            .read()
            .unwrap_or_else(|error| error.into_inner());
        self.set(ctx, overrides.apply(base.0, base.1.as_deref()));
    }
    pub(crate) fn configure(&self, overrides: ColorOverrides) {
        *self
            .overrides
            .write()
            .unwrap_or_else(|error| error.into_inner()) = overrides;
    }

    pub(crate) fn apply_theme(&self, ctx: &egui::Context, theme: &ThemeColors, name: Option<&str>) {
        *self.base.write().unwrap_or_else(|error| error.into_inner()) =
            (UiPalette::from_theme(theme), name.map(str::to_owned));
        let overrides = self
            .overrides
            .read()
            .unwrap_or_else(|error| error.into_inner());
        self.set(ctx, overrides.apply(UiPalette::from_theme(theme), name));
    }
    pub(crate) fn set(&self, ctx: &egui::Context, palette: UiPalette) {
        *self
            .palette
            .write()
            .unwrap_or_else(|error| error.into_inner()) = palette;
        ctx.set_visuals(visuals(palette));
        ctx.request_repaint();
    }
}

pub(crate) fn palette() -> UiPalette {
    *COLORS
        .palette
        .read()
        .unwrap_or_else(|error| error.into_inner())
}

/// Packed RGB expected by Ghostty's decoration API (not RGBA).
pub(crate) fn rgb(color: Color32) -> u32 {
    (u32::from(color.r()) << 16) | (u32::from(color.g()) << 8) | u32::from(color.b())
}

fn visuals(p: UiPalette) -> egui::Visuals {
    let mut v = if p.background.intensity() < 0.5 {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.panel_background;
    v.window_fill = p.panel_background;
    v.extreme_bg_color = p.background;
    v.faint_bg_color = p.panel_background;
    v.window_stroke.color = p.border;
    v.selection.bg_fill = p.selection_background;
    v.selection.stroke.color = p.selection_text;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.error;
    v.text_cursor.stroke.color = p.accent;
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.bg_fill = p.panel_background;
        widget.weak_bg_fill = p.panel_background;
        widget.bg_stroke.color = p.border;
        widget.fg_stroke.color = p.text;
    }
    v.widgets.noninteractive.fg_stroke.color = p.muted_text;
    v.widgets.hovered.bg_fill = p.selection_background;
    v.widgets.active.bg_fill = p.selection_background;
    v.widgets.active.fg_stroke.color = p.selection_text;
    v.widgets.open.bg_fill = p.selection_background;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_cursor_color_is_independent_and_inherits_accent_when_omitted() {
        let base = UiPalette::DEFAULT;
        let overrides: UiOverrides =
            serde_json::from_str(r##"{"accent":"#fabd2f","copy_cursor_background":"#83a598"}"##)
                .unwrap();
        let configured = overrides.apply(base);
        assert_eq!(rgb(configured.copy_cursor_background.unwrap()), 0x83a598);
        assert_eq!(rgb(configured.accent), 0xfabd2f);
        assert_eq!(configured.match_highlight, base.match_highlight);
        let fallback = UiOverrides::default().apply(base);
        assert_eq!(
            fallback.copy_cursor_background.unwrap_or(fallback.accent),
            base.accent
        );
    }

    #[test]
    fn secondary_search_colors_are_independent_of_picker_selection() {
        let mut theme = theme(0x282828, 0xebdbb2);
        theme.ansi[3] = 0xfabd2f;
        let base = UiPalette::from_theme(&theme);
        assert!(contrast(base.search_match_text, base.search_match_background) >= 4.5);
        let overrides = ColorOverrides {
            global: serde_json::from_str(
                r##"{"selection_background":"#3d3937","selection_text":"#ebdbb2","search_match_background":"#504945","search_match_text":"#ffffff"}"##,
            ).unwrap(),
            themes: std::collections::BTreeMap::from([(
                "Gruvbox Dark".into(),
                serde_json::from_str(r##"{"search_match_background":"#665c54"}"##).unwrap(),
            )]),
        };
        let selected = overrides.apply(base, Some("Gruvbox Dark"));
        assert_eq!(rgb(selected.selection_background), 0x3d3937);
        assert_eq!(rgb(selected.selection_text), 0xebdbb2);
        assert_eq!(rgb(selected.search_match_background), 0x665c54);
        assert_eq!(rgb(selected.search_match_text), 0xffffff);
        assert_eq!(selected.match_highlight, base.match_highlight);
        assert_eq!(
            rgb(overrides.apply(base, Some("Other")).search_match_background),
            0x504945
        );
        let picker_only: UiOverrides = serde_json::from_str(
            r##"{"selection_background":"#3d3937","selection_text":"#ebdbb2"}"##,
        )
        .unwrap();
        let picker = picker_only.apply(base);
        assert_eq!(picker.search_match_background, base.search_match_background);
        assert_eq!(picker.search_match_text, base.search_match_text);
    }

    #[test]
    fn yank_colors_are_independent_and_theme_overrides_do_not_leak() {
        let base = UiPalette::from_theme(&theme(0x1d2021, 0xebdbb2));
        // Without an override the preview pulse follows its selection.
        assert_eq!(base.yank_background, None);
        let overrides = ColorOverrides {
            global: serde_json::from_str(r##"{"accent":"#fabd2f","yank_background":"#83a598"}"##)
                .unwrap(),
            themes: std::collections::BTreeMap::from([(
                "Gruvbox Dark".into(),
                serde_json::from_str(r##"{"yank_background":"#d3869b"}"##).unwrap(),
            )]),
        };
        let selected = overrides.apply(base, Some("Gruvbox Dark"));
        assert_eq!(rgb(selected.accent), 0xfabd2f);
        assert_eq!(selected.yank_background.map(rgb), Some(0xd3869b));
        assert_eq!(
            overrides
                .apply(base, Some("Other"))
                .yank_background
                .map(rgb),
            Some(0x83a598)
        );
        assert_eq!(selected.selection_background, base.selection_background);
    }

    #[test]
    fn picker_match_color_is_independent_and_inherits_when_omitted() {
        let base = UiPalette::DEFAULT;
        let overrides = ColorOverrides {
            global: serde_json::from_str(
                r##"{"match_highlight":"#fabd2f","picker_match_highlight":"#ffdd55"}"##,
            )
            .unwrap(),
            themes: std::collections::BTreeMap::from([(
                "Gruvbox Dark".into(),
                serde_json::from_str(r##"{"picker_match_highlight":"#ffffff"}"##).unwrap(),
            )]),
        };
        let selected = overrides.apply(base, Some("Gruvbox Dark"));
        assert_eq!(rgb(selected.match_highlight), 0xfabd2f);
        assert_eq!(rgb(selected.picker_match_highlight.unwrap()), 0xffffff);
        assert_eq!(
            rgb(overrides
                .apply(base, Some("Other"))
                .picker_match_highlight
                .unwrap()),
            0xffdd55
        );
        let legacy = serde_json::from_str::<UiOverrides>(r##"{"match_highlight":"#aabbcc"}"##)
            .unwrap()
            .apply(base);
        assert!(legacy.picker_match_highlight.is_none());
        assert_eq!(
            rgb(legacy
                .picker_match_highlight
                .unwrap_or(legacy.match_highlight)),
            0xaabbcc
        );
        assert!(
            ColorOverrides::default()
                .apply(base, None)
                .picker_match_highlight
                .is_none()
        );
    }

    fn theme(background: u32, foreground: u32) -> ThemeColors {
        ThemeColors {
            background,
            foreground,
            selection_background: 0x345678,
            selection_foreground: 0xfedcba,
            selection_flags: 3,
            ansi: [background; 16],
        }
    }

    #[test]
    fn theme_overrides_follow_selected_theme_and_do_not_leak() {
        let overrides = ColorOverrides {
            global: serde_json::from_str(r##"{"accent":"#123456","warning":"#abcdef"}"##).unwrap(),
            themes: std::collections::BTreeMap::from([
                (
                    "Gruvbox Dark".into(),
                    serde_json::from_str(r##"{"accent":"#fabd2f"}"##).unwrap(),
                ),
                (
                    "Rose Pine".into(),
                    serde_json::from_str(r##"{"accent":"#ebbcba"}"##).unwrap(),
                ),
            ]),
        };
        let manager = ColorManager {
            base: RwLock::new((UiPalette::DEFAULT, None)),
            palette: RwLock::new(UiPalette::DEFAULT),
            overrides: RwLock::new(overrides),
        };
        let context = egui::Context::default();
        let theme = theme(0x282828, 0xebdbb2);
        for (name, accent) in [
            (Some("Gruvbox Dark"), 0xfabd2f),
            (Some("Rose Pine"), 0xebbcba),
            (Some("Other"), 0x123456),
            (None, 0x123456),
        ] {
            manager.apply_theme(&context, &theme, name);
            let palette = *manager.palette.read().unwrap();
            assert_eq!(rgb(palette.accent), accent);
            assert_eq!(rgb(palette.warning), 0xabcdef);
            assert_eq!(rgb(palette.background), theme.background);
        }
        manager.apply_theme(&context, &theme, Some("Gruvbox Dark"));
        manager.reload(&context, ColorOverrides::default());
        let palette = *manager.palette.read().unwrap();
        assert_eq!(palette.accent, UiPalette::from_theme(&theme).accent);
        assert_eq!(palette.warning, UiPalette::from_theme(&theme).warning);
    }

    #[test]
    fn theme_colors_are_inherited_and_overrides_win() {
        let t = theme(0x102030, 0xf0e0d0);
        let derived = UiPalette::from_theme(&t);
        assert_eq!(rgb(derived.background), t.background);
        assert_eq!(rgb(derived.text), t.foreground);
        assert_eq!(rgb(derived.selection_background), t.selection_background);
        assert_eq!(rgb(derived.selection_text), t.selection_foreground);
        let overrides: UiOverrides =
            serde_json::from_str(r##"{"accent":"#123456","text":"abcdef"}"##).unwrap();
        let p = overrides.apply(derived);
        assert_eq!(rgb(p.accent), 0x123456);
        assert_eq!(rgb(p.text), 0xabcdef);
        assert_eq!(p.panel_background, derived.panel_background);
    }

    #[test]
    fn derived_colors_are_readable_for_light_and_dark_themes() {
        for (background, foreground) in [(0x000000, 0xffffff), (0xffffff, 0x000000)] {
            let mut t = theme(background, foreground);
            t.selection_flags = 0;
            let p = UiPalette::from_theme(&t);
            for color in [
                p.accent,
                p.warning,
                p.error,
                p.success,
                p.muted_text,
                p.match_highlight,
            ] {
                assert!(contrast(color, p.panel_background) >= 4.5);
            }
            assert!(contrast(p.selection_text, p.selection_background) >= 4.5);
        }
    }

    #[test]
    fn invalid_overrides_are_rejected() {
        for value in [
            r##"{"accent":"#123"}"##,
            r##"{"text":"red"}"##,
            r##"{"border":"#zzzzzz"}"##,
            r##"{"unknown":"#123456"}"##,
            r##"{"error":123}"##,
        ] {
            assert!(serde_json::from_str::<UiOverrides>(value).is_err());
        }
    }

    #[test]
    fn custom_palette_reaches_egui_and_native_decoration_colors() {
        let p = UiPalette {
            text: Color32::from_rgb(1, 2, 3),
            selection_text: Color32::from_rgb(4, 5, 6),
            error: Color32::from_rgb(7, 8, 9),
            ..UiPalette::default()
        };
        let v = visuals(p);
        assert_eq!(v.override_text_color, Some(p.text));
        assert_eq!(v.selection.stroke.color, p.selection_text);
        assert_eq!(v.error_fg_color, p.error);
        assert_eq!(rgb(p.text), 0x010203);
    }
}

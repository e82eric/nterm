use crate::ghostty_ffi::{self, Host};

#[derive(Clone)]
pub(crate) struct Typography {
    pub(crate) font: egui::FontId,
    pub(crate) matched_font: egui::FontId,
    pub(crate) row_height: f32,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            font: egui::FontId::monospace(14.0),
            matched_font: egui::FontId::monospace(14.0),
            row_height: 18.0,
        }
    }
}

pub(crate) struct PickerFont {
    family: egui::FontFamily,
    bold_family: egui::FontFamily,
    cached: Option<((u32, u32, u32), Typography)>,
}

impl PickerFont {
    pub(crate) fn new(ctx: &egui::Context, host: *mut Host) -> Self {
        let mut definitions = egui::FontDefinitions::default();
        let mut load = |bold, name: &str| {
            let mut data = ghostty_ffi::FontData::default();
            if !unsafe { ghostty_ffi::ghostty_egui_host_read_font(host, bold, &mut data) } {
                eprintln!(
                    "picker: terminal font cannot be exported; using egui monospace fallback"
                );
                return egui::FontFamily::Monospace;
            }
            let bytes = unsafe { std::slice::from_raw_parts(data.data, data.len) }.to_vec();
            let mut font = egui::FontData::from_owned(bytes);
            font.index = data.face_index;
            unsafe { ghostty_ffi::ghostty_font_data_free(&mut data) };
            let family = egui::FontFamily::Name(name.into());
            definitions
                .font_data
                .insert(name.into(), std::sync::Arc::new(font));
            let mut fallback = definitions.families[&egui::FontFamily::Monospace].clone();
            fallback.insert(0, name.into());
            definitions.families.insert(family.clone(), fallback);
            family
        };
        let family = load(false, "ghostty-terminal");
        let bold_family = load(true, "ghostty-terminal-bold");
        ctx.set_fonts(definitions);
        Self {
            family,
            bold_family,
            cached: None,
        }
    }

    pub(crate) fn typography(&mut self, ctx: &egui::Context, host: *mut Host) -> Typography {
        let size = unsafe { ghostty_ffi::ghostty_egui_host_surface_size(host) };
        self.for_cells(ctx, size.cell_width_px, size.cell_height_px)
    }

    fn for_cells(&mut self, ctx: &egui::Context, width: u32, height: u32) -> Typography {
        let scale = ctx.pixels_per_point();
        let key = (width, height, scale.to_bits());
        if let Some((previous, typography)) = &self.cached
            && *previous == key
        {
            return typography.clone();
        }
        let mut font = egui::FontId::new(14.0, self.family.clone());
        let advance = ctx.fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
        if advance > 0.0 && width > 0 {
            font.size *= width as f32 / scale / advance;
        }
        let typography = Typography {
            matched_font: egui::FontId::new(font.size, self.bold_family.clone()),
            font,
            row_height: (height as f32 / scale).max(1.0),
        };
        self.cached = Some((key, typography.clone()));
        typography
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizing_tracks_cell_width_height_zoom_and_dpi() {
        let ctx = egui::Context::default();
        let mut font = PickerFont {
            family: egui::FontFamily::Monospace,
            bold_family: egui::FontFamily::Monospace,
            cached: None,
        };
        for (scale, width, height) in [(1.0, 10, 20), (1.0, 15, 30), (2.0, 30, 60)] {
            ctx.set_pixels_per_point(scale);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let typography = font.for_cells(ui.ctx(), width, height);
                let advance = ui
                    .ctx()
                    .fonts_mut(|fonts| fonts.glyph_width(&typography.font, 'M'));
                assert!((advance - width as f32 / scale).abs() < 0.1);
                assert_eq!(typography.row_height, height as f32 / scale);
                assert_eq!(
                    font.for_cells(ui.ctx(), width, height).font,
                    typography.font
                );
            });
            output.textures_delta.clear();
        }
    }
}

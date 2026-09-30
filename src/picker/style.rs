use egui::{Color32, Stroke};

pub(crate) const BACKGROUND: Color32 = Color32::from_rgb(0x1d, 0x20, 0x21);
pub(crate) const BORDER: Color32 = Color32::from_rgb(0x92, 0x83, 0x74);
pub(crate) const SELECTION: Color32 = Color32::from_rgb(0x3d, 0x39, 0x37);
pub(crate) const TEXT: Color32 = Color32::from_rgb(0xeb, 0xdb, 0xb2);
pub(crate) const MUTED: Color32 = Color32::from_rgb(0xa8, 0x99, 0x84);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(0xfa, 0xbd, 0x2f);
pub(crate) const ERROR: Color32 = Color32::from_rgb(0xfb, 0x49, 0x34);
pub(crate) const PANEL_RADIUS: u8 = 8;
pub(crate) const PANEL_CONTENT_INSET: f32 = 58.0;
pub(crate) const DIVIDER_EXTRA: f32 = 6.0;

pub(crate) fn divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 9.0 + DIVIDER_EXTRA),
        egui::Sense::hover(),
    );
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.5, BORDER.gamma_multiply(0.8)),
    );
}

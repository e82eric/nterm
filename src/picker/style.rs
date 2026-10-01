use egui::Stroke;

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
        Stroke::new(1.5, crate::colors::palette().border.gamma_multiply(0.8)),
    );
}

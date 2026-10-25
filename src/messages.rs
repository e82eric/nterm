/// Session diagnostics remain available until the application restarts. They
/// are browsed in the messages picker; see `error_log::picker_options`.
#[derive(Default)]
pub(crate) struct Messages {
    config_issues: Vec<String>,
}

impl Messages {
    pub(crate) fn new(config_issues: Vec<String>) -> Self {
        Self { config_issues }
    }

    pub(crate) fn config_issues(&self) -> &[String] {
        &self.config_issues
    }

    /// Paints the configuration issue count at the bar's right end. Returns
    /// the space left of it and whether it was clicked to show the messages.
    pub(crate) fn status(&self, ui: &egui::Ui, rect: egui::Rect) -> (f32, bool) {
        let right = rect.right() - 10.0;
        if self.config_issues.is_empty() {
            return (right, false);
        }
        let color = crate::colors::palette().warning;
        let count = self.config_issues.len();
        let text = format!(
            "Config: {count} {}",
            if count == 1 { "issue" } else { "issues" }
        );
        let galley = ui
            .painter()
            .layout_no_wrap(text, egui::FontId::monospace(13.0), color);
        let left = (right - galley.size().x).max(rect.left());
        let hit = egui::Rect::from_min_max(egui::pos2(left, rect.top()), rect.right_bottom());
        let response = ui.interact(hit, ui.id().with("config-messages"), egui::Sense::CLICK);
        let clicked = response.clicked();
        response.on_hover_text(
            "View configuration messages. Fix the configuration and restart to clear these issues.",
        );
        ui.painter().with_clip_rect(rect).galley(
            egui::pos2(left, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        (left - 12.0, clicked)
    }
}

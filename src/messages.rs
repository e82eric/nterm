/// Session diagnostics remain available until the application restarts.
#[derive(Default)]
pub(crate) struct Messages {
    config_issues: Vec<String>,
    open: bool,
    selected_error: Option<crate::error_log::Record>,
}

impl Messages {
    pub(crate) fn open(&mut self) {
        self.selected_error = None;
        self.open = true;
    }
    pub(crate) fn open_record(&mut self, record: crate::error_log::Record) {
        self.selected_error = Some(record);
        self.open = true;
    }
    pub(crate) fn new(config_issues: Vec<String>) -> Self {
        Self {
            config_issues,
            open: false,
            selected_error: None,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn status(&mut self, ui: &egui::Ui, rect: egui::Rect) -> f32 {
        let right = rect.right() - 10.0;
        if self.config_issues.is_empty() {
            return right;
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
        let response = ui.interact(hit, ui.id().with("config-messages"), egui::Sense::click());
        if response.clicked() {
            self.open = !self.open;
        }
        response.on_hover_text(
            "View configuration messages. Fix the configuration and restart to clear these issues.",
        );
        ui.painter().with_clip_rect(rect).galley(
            egui::pos2(left, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        left - 12.0
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
            return;
        }
        egui::Window::new("Messages")
            .id(egui::Id::new("configuration-messages"))
            .open(&mut self.open)
            .default_width(650.0)
            .default_height(300.0)
            .default_pos(egui::pos2(16.0, (ctx.content_rect().bottom() - 350.0).max(16.0)))
            .show(ctx, |ui| {
                if let Some(record) = &self.selected_error {
                    ui.strong(record.label());
                    if ui.button("Copy error details").clicked() { ctx.copy_text(record.text()); }
                    egui::ScrollArea::vertical().show(ui, |ui| { ui.add(egui::Label::new(&record.details).selectable(true).wrap()); });
                    return;
                }
                let history = crate::error_log::snapshot();
                ui.label("Configuration issues remain active until you fix the file and restart the app.");
                if ui.button("Copy all messages").clicked() {
                    ctx.copy_text(self.config_issues.iter().cloned().chain(history.iter().map(|record| record.text())).collect::<Vec<_>>().join("\n\n---\n\n"));
                }
                ui.separator();
                if self.config_issues.is_empty() && history.is_empty() { ui.label("No messages."); }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (index, message) in self.config_issues.iter().enumerate() {
                        ui.strong(format!("Configuration issue {}", index + 1));
                        ui.add(egui::Label::new(message).selectable(true).wrap());
                        ui.separator();
                    }
                    for record in &history {
                        ui.strong(record.label());
                        ui.add(egui::Label::new(&record.details).selectable(true).wrap());
                        ui.separator();
                    }
                });
            });
    }
}

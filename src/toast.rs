use std::collections::VecDeque;
use std::time::{Duration, Instant};

struct Toast {
    title: String,
    message: String,
    expires: Instant,
}

#[derive(Default)]
pub(crate) struct Toasts {
    entries: VecDeque<Toast>,
}

impl Toasts {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn push(&mut self, title: String, message: String, duration_ms: u64) {
        if self.entries.len() == 3 {
            self.entries.pop_front();
        }
        self.entries.push_back(Toast {
            title,
            message,
            expires: Instant::now() + Duration::from_millis(duration_ms),
        });
    }

    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        bounds: egui::Rect,
        id: egui::Id,
        cursor_row: Option<u32>,
        row_height: f32,
    ) {
        let now = Instant::now();
        self.entries.retain(|entry| entry.expires > now);
        if self.entries.is_empty() {
            return;
        }
        if let Some(expires) = self.entries.iter().map(|entry| entry.expires).min() {
            ctx.request_repaint_after(expires.saturating_duration_since(now));
        }
        let max_text_width = (bounds.width() - 32.0 - 26.0).max(1.0);
        let galleys = ctx.fonts_mut(|fonts| {
            self.entries
                .iter()
                .map(|toast| {
                    let mut job = egui::text::LayoutJob::default();
                    job.append(
                        &toast.title,
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(20.0),
                            color: crate::picker::style::TEXT,
                            ..Default::default()
                        },
                    );
                    job.append(
                        &format!("  {}", toast.message),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(18.0),
                            color: crate::picker::style::TEXT,
                            ..Default::default()
                        },
                    );
                    job.wrap.max_width = max_text_width;
                    fonts.layout_job(job)
                })
                .collect::<Vec<_>>()
        });
        let width = galleys
            .iter()
            .map(|galley| galley.size().x)
            .fold(0.0_f32, f32::max)
            .ceil()
            + 26.0;
        let height = galleys
            .iter()
            .map(|galley| galley.size().y + 26.0 + 8.0)
            .sum();
        let measured_size = Some(egui::vec2(width, height));
        let (align, offset) = crate::picker::picker_anchor(
            crate::picker::config::Position::Cursor,
            cursor_row,
            row_height,
            bounds,
            ctx.content_rect(),
            measured_size,
        );
        egui::Area::new(id)
            .fade_in(false)
            .anchor(align, offset)
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.set_max_height((bounds.height() - 32.0).max(1.0));
                ui.set_clip_rect(bounds.intersect(ui.clip_rect()));
                for galley in &galleys {
                    egui::Frame::new()
                        .fill(crate::picker::style::BACKGROUND)
                        .stroke(egui::Stroke::new(1.0, crate::picker::style::BORDER))
                        .corner_radius(8)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(galley.size(), egui::Sense::hover());
                            ui.painter().galley(
                                rect.min,
                                galley.clone(),
                                crate::picker::style::TEXT,
                            );
                        });
                    ui.add_space(8.0);
                }
            });
    }
}

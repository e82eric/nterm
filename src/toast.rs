use std::collections::VecDeque;
use std::time::{Duration, Instant};

struct Toast {
    title: String,
    message: String,
    expires: Instant,
    error: bool,
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
        self.push_kind(title, message, duration_ms, false);
    }

    pub(crate) fn push_error(&mut self, title: String, message: String, duration_ms: u64) {
        self.push_kind(title, message, duration_ms, true);
    }

    fn push_kind(&mut self, title: String, message: String, duration_ms: u64, error: bool) {
        // Repeated failures refresh the existing notification instead of
        // covering the pane with copies of the same message.
        self.entries.retain(|entry| {
            entry.title != title || entry.message != message || entry.error != error
        });
        if self.entries.len() == 3 {
            self.entries.pop_front();
        }
        self.entries.push_back(Toast {
            title,
            message,
            expires: Instant::now() + Duration::from_millis(duration_ms),
            error,
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
        let max_text_width = (bounds.width() - 32.0 - 26.0).clamp(1.0, 420.0);
        let max_rows = (((bounds.height() - 32.0) / self.entries.len() as f32 - 32.0) / 20.0)
            .floor()
            .clamp(1.0, 7.0) as usize;
        let galleys = ctx.fonts_mut(|fonts| {
            let normal = egui::FontFamily::Name("ghostty-terminal".into());
            let body_family = if fonts.families().contains(&normal) {
                normal
            } else {
                egui::FontFamily::Monospace
            };
            let bold = egui::FontFamily::Name("ghostty-terminal-bold".into());
            let header_family = if fonts.families().contains(&bold) {
                bold
            } else {
                body_family.clone()
            };
            self.entries
                .iter()
                .map(|toast| {
                    let mut job = toast_layout(
                        &toast.title,
                        &toast.message,
                        max_text_width,
                        header_family.clone(),
                        body_family.clone(),
                    );
                    job.wrap.max_rows = max_rows;
                    if toast.error && !toast.title.trim().is_empty() {
                        job.sections[0].format.color = crate::colors::palette().error;
                    }
                    fonts.layout_job(job)
                })
                .collect::<Vec<_>>()
        });
        let width = galleys
            .iter()
            .map(|galley| galley.size().x)
            .fold(210.0_f32.min(max_text_width), f32::max)
            .ceil()
            + 26.0;
        let height = galleys
            .iter()
            .map(|galley| galley.size().y + 22.0 + 6.0)
            .sum::<f32>()
            - 6.0;
        let (align, offset) =
            toast_anchor(cursor_row, row_height, bounds, ctx.content_rect(), height);
        egui::Area::new(id)
            .fade_in(false)
            .anchor(align, offset)
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.set_max_height((bounds.height() - 32.0).max(1.0));
                ui.set_clip_rect(bounds.intersect(ui.clip_rect()));
                for galley in &galleys {
                    egui::Frame::new()
                        .fill(crate::colors::palette().panel_background)
                        .stroke(egui::Stroke::new(1.0, crate::colors::palette().border))
                        .corner_radius(5)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 3],
                            blur: 10,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(70),
                        })
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(width - 26.0, galley.size().y),
                                egui::Sense::hover(),
                            );
                            ui.painter().galley(
                                rect.min,
                                galley.clone(),
                                crate::colors::palette().text,
                            );
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

fn toast_layout(
    title: &str,
    message: &str,
    max_width: f32,
    header_family: egui::FontFamily,
    body_family: egui::FontFamily,
) -> egui::text::LayoutJob {
    let palette = crate::colors::palette();
    let mut job = egui::text::LayoutJob::default();
    if !title.trim().is_empty() {
        job.append(
            &format!("{title}\n"),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::new(14.0, header_family),
                color: palette.accent,
                line_height: Some(24.0),
                ..Default::default()
            },
        );
    }
    job.append(
        message,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::new(14.0, body_family),
            color: palette.text,
            line_height: Some(20.0),
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width;
    job.wrap.break_anywhere = true;
    job
}

fn toast_anchor(
    cursor_row: Option<u32>,
    row_height: f32,
    bounds: egui::Rect,
    screen: egui::Rect,
    height: f32,
) -> (egui::Align2, egui::Vec2) {
    let x = bounds.center().x - screen.center().x;
    let Some(row) = cursor_row else {
        return (
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(x, bounds.bottom() - screen.bottom() - 10.0),
        );
    };
    let below = bounds.top() + (row as f32 + 1.0) * row_height + 10.0;
    let above = below - 10.0;
    let min = bounds.top() + 10.0;
    let max = bounds.bottom() - 10.0;
    if below + height <= max {
        (
            egui::Align2::CENTER_TOP,
            egui::vec2(x, below - screen.top()),
        )
    } else if above - height >= min && above <= max {
        (
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(x, above - screen.bottom()),
        )
    } else if max - below >= above - min {
        (
            egui::Align2::CENTER_TOP,
            egui::vec2(x, below.clamp(min, (max - height).max(min)) - screen.top()),
        )
    } else {
        (
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(
                x,
                above.clamp((min + height).min(max), max) - screen.bottom(),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_toasts_refresh_instead_of_stacking() {
        let mut toasts = Toasts::default();
        toasts.push_error("Search unavailable".into(), "Try again.".into(), 1);
        let previous = toasts.entries[0].expires;
        toasts.push_error("Search unavailable".into(), "Try again.".into(), 10000);
        assert_eq!(toasts.entries.len(), 1);
        assert!(toasts.entries[0].expires > previous);
        assert!(toasts.entries[0].error);
        toasts.push("".into(), "Configuration reloaded".into(), 2000);
        assert_eq!(toasts.entries.len(), 2);
        assert!(!toasts.entries[1].error);
    }

    #[test]
    fn compact_toast_wraps_body_and_omits_empty_header_line() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            for header in ["", "Reload failed"] {
                let body = "A configuration message with enough words to wrap. ".repeat(12);
                let job = toast_layout(
                    header,
                    &body,
                    420.0,
                    egui::FontFamily::Monospace,
                    egui::FontFamily::Monospace,
                );
                assert_eq!(job.text.starts_with('\n'), false);
                assert_eq!(job.text.contains("Reload failed\n"), !header.is_empty());
                assert!(
                    job.sections
                        .iter()
                        .all(|section| section.format.font_id.size == 14.0)
                );
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                assert!(galley.size().x <= 421.0);
                assert!(galley.rows.len() > 1);
            }
        });
        output.textures_delta.clear();
    }
}

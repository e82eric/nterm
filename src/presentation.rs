use std::time::{Duration, Instant};

/// Input updates run immediately. Only presentation is paced.
pub(crate) struct Presentation {
    pub(crate) frame_pending: bool,
    pub(crate) ui_dirty: bool,
    pub(crate) force: bool,
    pub(crate) interval: Duration,
    last_present: Option<Instant>,
    pub(crate) repaint_deadline: Option<Instant>,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            frame_pending: false,
            ui_dirty: false,
            force: true,
            interval: Duration::from_nanos(16_666_667),
            last_present: None,
            repaint_deadline: None,
        }
    }
}

impl Presentation {
    pub(crate) fn present_deadline(&self, now: Instant) -> Option<Instant> {
        if self.force {
            return Some(now);
        }
        if !self.frame_pending && !self.ui_dirty {
            return None;
        }
        Some(
            self.last_present
                .map_or(now, |last| (last + self.interval).max(now)),
        )
    }

    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.present_deadline(now).is_some_and(|due| due <= now)
    }

    pub(crate) fn presented(&mut self, started: Instant) {
        self.last_present = Some(started);
        self.frame_pending = false;
        self.ui_dirty = false;
        self.force = false;
    }

    pub(crate) fn repaint_at(&mut self, deadline: Instant) {
        self.repaint_deadline = Some(
            self.repaint_deadline
                .map_or(deadline, |old| old.min(deadline)),
        );
    }

    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.present_deadline(now)
            .into_iter()
            .chain(self.repaint_deadline)
            .min()
    }
}

/// Observe UI output rather than treating every egui update as a visual change.
/// Terminal callbacks have a stable identity; their texture changes are tracked
/// separately by frame notifications. No terminal pixels are copied or compared.
#[derive(Default)]
pub(crate) struct UiChanges {
    shapes: Vec<egui::epaint::ClippedShape>,
    pixels_per_point: f32,
    pub(crate) changed: bool,
}

impl egui::Plugin for UiChanges {
    fn debug_name(&self) -> &'static str {
        "presentation changes"
    }

    fn output_hook(&mut self, _: &egui::Context, output: &mut egui::FullOutput) {
        self.changed |= self.shapes != output.shapes
            || self.pixels_per_point != output.pixels_per_point
            || !output.textures_delta.is_empty();
        self.shapes.clone_from(&output.shapes);
        self.pixels_per_point = output.pixels_per_point;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Plugin as _;

    #[test]
    fn unchanged_terminal_callback_does_not_hide_ui_changes() {
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
        let callback = egui::epaint::ClippedShape {
            clip_rect: rect,
            shape: egui::Shape::Callback(egui::PaintCallback {
                rect,
                callback: std::sync::Arc::new(()),
            }),
        };
        let mut output = egui::FullOutput {
            shapes: vec![callback],
            pixels_per_point: 1.0,
            ..Default::default()
        };
        let mut changes = UiChanges::default();
        changes.output_hook(&ctx, &mut output);
        assert!(std::mem::take(&mut changes.changed));
        changes.output_hook(&ctx, &mut output);
        assert!(!changes.changed);

        // An input caret appears without any change to the terminal texture.
        output.shapes.push(egui::epaint::ClippedShape {
            clip_rect: rect,
            shape: egui::Shape::line_segment(
                [egui::pos2(5.0, 5.0), egui::pos2(5.0, 20.0)],
                egui::Stroke::new(1.0, egui::Color32::WHITE),
            ),
        });
        changes.output_hook(&ctx, &mut output);
        assert!(std::mem::take(&mut changes.changed));
        output.shapes.pop();
        changes.output_hook(&ctx, &mut output);
        assert!(changes.changed);
    }

    #[test]
    fn notifications_do_not_postpone_presentation() {
        let now = Instant::now();
        let mut state = Presentation::default();
        state.presented(now);
        state.frame_pending = true;
        state.ui_dirty = true;
        assert!(!state.ready(now + state.interval / 2));
        assert_eq!(state.present_deadline(now), Some(now + state.interval));
        assert!(state.ready(now + state.interval));
        state.presented(now + state.interval);
        assert_eq!(state.next_deadline(now + state.interval), None);
    }

    #[test]
    fn ui_changes_remain_pending_until_presented() {
        let now = Instant::now();
        let mut state = Presentation::default();
        state.presented(now);
        state.ui_dirty = true;
        state.frame_pending = false; // A stale frame event must not discard UI work.
        assert!(state.ready(now + state.interval));
        state.force = true;
        assert!(state.ready(now));
    }
}

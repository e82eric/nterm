use crate::painter::PaintTrace;
use std::io::{BufWriter, Write as _};
use std::time::Instant;

pub(crate) struct FrameSample {
    pub(crate) started: Instant,
    pub(crate) ran_ui: Instant,
    pub(crate) painted: Instant,
    pub(crate) swapped: Instant,
    pub(crate) generation: Option<u64>,
    pub(crate) new_frame: bool,
    pub(crate) presented: bool,
    pub(crate) paced: bool,
    pub(crate) retired_before_paint: bool,
    pub(crate) paint_trace: PaintTrace,
}

#[derive(Default)]
pub(crate) struct FrameTrace {
    writer: Option<RenderTrace>,
    pub(crate) pending_frame_ready: Option<Instant>,
    pub(crate) frame_event_deferred: bool,
    pub(crate) ready_events: u32,
    pub(crate) tick_events: u32,
    pub(crate) tick_time: std::time::Duration,
    pub(crate) repaint_events: u32,
    pub(crate) timer_events: u32,
    pub(crate) window_repaints: u32,
    pub(crate) input_events: u32,
    pub(crate) pending_input: Option<Instant>,
}

impl FrameTrace {
    pub(crate) fn new() -> Self {
        Self {
            writer: RenderTrace::from_env(),
            ..Self::default()
        }
    }

    pub(crate) fn record(&mut self, ctx: &egui::Context, sample: FrameSample) {
        let FrameSample {
            started,
            ran_ui,
            painted,
            swapped,
            generation,
            new_frame,
            presented,
            paced,
            retired_before_paint,
            paint_trace,
        } = sample;
        if let Some(trace) = self.writer.as_mut() {
            let repaint_causes = ctx
                .repaint_causes()
                .into_iter()
                .map(|cause| format!("{}:{}", cause.file, cause.line))
                .collect::<Vec<_>>()
                .join("|");
            let ready_to_redraw = self
                .pending_frame_ready
                .map(|ready| started.saturating_duration_since(ready).as_micros());
            let _ = writeln!(
                trace.file,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                started.saturating_duration_since(trace.start).as_micros(),
                generation.unwrap_or(0),
                u8::from(new_frame),
                u8::from(presented),
                u8::from(paced || self.frame_event_deferred),
                self.ready_events,
                ready_to_redraw.unwrap_or(0),
                self.tick_events,
                self.tick_time.as_micros(),
                ran_ui.duration_since(started).as_micros(),
                painted.duration_since(ran_ui).as_micros(),
                swapped.duration_since(painted).as_micros(),
                swapped.duration_since(started).as_micros(),
                self.repaint_events,
                self.timer_events,
                self.window_repaints,
                u8::from(retired_before_paint),
                paint_trace.retired_count,
                paint_trace.retired_us,
                paint_trace.setup_us,
                paint_trace.draw_us,
                paint_trace.fence_us,
                paint_trace.fence_wait_failed,
                self.input_events,
                self.pending_input.map_or(0, |input| started
                    .saturating_duration_since(input)
                    .as_micros()),
                if new_frame {
                    self.pending_input.map_or(0, |input| {
                        started.saturating_duration_since(input).as_micros()
                    })
                } else {
                    0
                },
                repaint_causes,
            );
        }
        if new_frame {
            self.pending_input = None;
        }
        if presented {
            self.frame_event_deferred = false;
        }
        self.pending_frame_ready = None;
        self.ready_events = 0;
        self.tick_events = 0;
        self.tick_time = std::time::Duration::ZERO;
        self.repaint_events = 0;
        self.timer_events = 0;
        self.window_repaints = 0;
        self.input_events = 0;
    }
}

struct RenderTrace {
    start: Instant,
    file: BufWriter<std::fs::File>,
}

impl RenderTrace {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os("GHOSTTY_EGUI_TRACE")?;
        let file = match std::fs::File::create(&path) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("cannot create egui frame trace at {path:?}: {err}");
                return None;
            }
        };
        let mut trace = Self {
            start: Instant::now(),
            file: BufWriter::new(file),
        };
        if writeln!(
            trace.file,
            "time_us,generation,new_frame,presented,paced,ready_events,ready_to_redraw_us,tick_events,tick_us,egui_run_us,egui_paint_us,swap_us,total_us,repaint_events,timer_events,window_repaints,retired_before_paint,retired_count,retire_poll_us,terminal_setup_us,terminal_draw_us,terminal_fence_us,fence_wait_failed,input_events,input_to_redraw_us,input_to_new_frame_us,repaint_causes"
        )
        .is_err()
        {
            return None;
        }
        Some(trace)
    }
}

//! A hidden frame meter, for chasing stutter: a three-finger tap (or F12) shows how evenly
//! frames are arriving while something moves.

use std::collections::VecDeque;
use std::time::Duration;

use eframe::egui::{self, Color32, CornerRadius, FontId, Id, Key, Modifiers, Order, pos2, vec2};

use crate::platform;

/// How far back the figures look, in seconds.
const SPAN: f64 = 2.0;
/// Gaps longer than this are the app resting, not stutter.
const IDLE: f32 = 0.25;

#[derive(Default)]
pub struct Meter {
    shown: bool,
    /// (frame start time, time since the previous frame, time our own code took).
    frames: VecDeque<(f64, f32, Duration)>,
    refresh: Option<f32>,
    fingers: usize,
}

impl Meter {
    /// Note a frame; `work` is how long the app's own update took.
    pub fn record(&mut self, ctx: &egui::Context, work: Duration) {
        let (now, dt, fingers) =
            ctx.input(|i| (i.time, i.unstable_dt, i.multi_touch().map_or(0, |m| m.num_touches)));
        // Three fingers landing together toggle it.
        let three = fingers >= 3 && self.fingers < 3;
        self.fingers = fingers;
        if three || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F12)) {
            self.shown = !self.shown;
            self.frames.clear();
            self.refresh = platform::refresh_rate();
        }
        if !self.shown {
            return;
        }
        self.frames.push_back((now, dt, work));
        while self.frames.front().is_some_and(|&(t, _, _)| now - t > SPAN) {
            self.frames.pop_front();
        }
        self.show(ctx);
    }

    fn show(&self, ctx: &egui::Context) {
        let moving: Vec<_> = self.frames.iter().filter(|&&(_, dt, _)| dt < IDLE).collect();
        let lines = if moving.is_empty() {
            "Frame meter: move something".to_owned()
        } else {
            let mut dts: Vec<f32> = moving.iter().map(|&&(_, dt, _)| dt).collect();
            dts.sort_by(f32::total_cmp);
            let median = dts[dts.len() / 2];
            let period = self.refresh.map_or(median, |hz| 1.0 / hz);
            let late = dts.iter().filter(|&&dt| dt > period * 1.5).count();
            let worst = dts.last().copied().unwrap_or(0.0);
            let work = moving.iter().map(|&&(_, _, w)| w).max().unwrap_or_default();
            let mean_work = moving.iter().map(|&&(_, _, w)| w).sum::<Duration>() / moving.len() as u32;
            let hz = self.refresh.map_or(String::new(), |hz| format!("{hz:.0} Hz display\n"));
            format!(
                "{hz}{:.0} fps, frames {:.1} ms (worst {:.1})\nlate frames: {late} of {}\nupdate {:.1} ms (worst {:.1})",
                1.0 / median,
                median * 1000.0,
                worst * 1000.0,
                dts.len(),
                mean_work.as_secs_f32() * 1000.0,
                work.as_secs_f32() * 1000.0,
            )
        };
        let at = ctx.content_rect().left_top() + vec2(12.0, 12.0);
        egui::Area::new(Id::unique("frame-meter"))
            .order(Order::Debug)
            .fixed_pos(at)
            .interactable(false)
            .show(ctx, |ui| {
                let galley = ui.painter().layout_no_wrap(lines, FontId::monospace(12.0), Color32::WHITE);
                let rect = egui::Rect::from_min_size(pos2(at.x, at.y), galley.size() + vec2(16.0, 12.0));
                ui.painter().rect_filled(rect, CornerRadius::same(6), Color32::from_black_alpha(200));
                ui.painter().galley(rect.min + vec2(8.0, 6.0), galley, Color32::WHITE);
            });
    }
}

//! Quiet, hand-drawn controls that sit comfortably next to book typography.

use std::sync::Arc;

use eframe::egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Galley, Id, Pos2, Rect, Response, Sense, Shape, Stroke, Ui,
    pos2, vec2,
};

use crate::fonts;
use crate::theme::Palette;

#[derive(Clone, Copy)]
pub enum Icon {
    Back,
    Contents,
    Type,
    Fullscreen,
    Close,
    List,
    Grid,
}

pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    let at = |x: f32, y: f32| pos2(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    match icon {
        Icon::Back => {
            painter.add(Shape::line(vec![at(0.56, 0.3), at(0.38, 0.5), at(0.56, 0.7)], stroke));
        }
        Icon::Contents => {
            for y in [0.33, 0.5, 0.67] {
                painter.circle_filled(at(0.3, y), 1.4, color);
                painter.line_segment([at(0.4, y), at(0.72, y)], stroke);
            }
        }
        Icon::Type => {
            painter.text(
                at(0.5, 0.53),
                Align2::CENTER_CENTER,
                "Aa",
                fonts::serif(rect.height() * 0.42, false),
                color,
            );
        }
        Icon::Fullscreen => {
            let (a, b) = (0.3, 0.7);
            let l = 0.13;
            for (cx, cy, dx, dy) in
                [(a, a, 1.0, 1.0), (b, a, -1.0, 1.0), (a, b, 1.0, -1.0), (b, b, -1.0, -1.0)]
            {
                painter.add(Shape::line(vec![at(cx, cy + dy * l), at(cx, cy), at(cx + dx * l, cy)], stroke));
            }
        }
        Icon::Close => {
            painter.line_segment([at(0.35, 0.35), at(0.65, 0.65)], stroke);
            painter.line_segment([at(0.65, 0.35), at(0.35, 0.65)], stroke);
        }
        Icon::List => {
            for y in [0.34, 0.5, 0.66] {
                painter.line_segment([at(0.3, y), at(0.7, y)], stroke);
            }
        }
        Icon::Grid => {
            let r = CornerRadius::same(1);
            for (x, y) in [(0.3, 0.3), (0.53, 0.3), (0.3, 0.53), (0.53, 0.53)] {
                let cell = Rect::from_min_max(at(x, y), at(x + 0.17, y + 0.17));
                painter.rect_stroke(cell, r, stroke, egui::StrokeKind::Middle);
            }
        }
    }
}

/// Whether to show hover feedback. A touchscreen has no hover: the finger that starts a
/// scroll would otherwise light up whatever happens to be under it.
pub fn hovering(ui: &Ui, response: &Response) -> bool {
    response.hovered() && !ui.input(|i| i.has_touch_screen())
}

/// A round, borderless icon button.
pub fn icon_button(ui: &mut Ui, rect: Rect, id: Id, icon: Icon, pal: &Palette) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), hovering(ui, &response), 0.12);
    if hover > 0.0 {
        ui.painter().circle_filled(rect.center(), rect.width() * 0.5, pal.faint.gamma_multiply(hover));
    }
    let color = lerp_color(pal.muted, pal.text, hover);
    paint_icon(ui.painter(), rect, icon, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row of mutually exclusive options. Returns the newly chosen index, if any.
pub fn segmented(
    ui: &mut Ui,
    id: Id,
    labels: &[&str],
    fonts: &[FontId],
    selected: usize,
    pal: &Palette,
) -> Option<usize> {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), pal.faint.gamma_multiply(0.6));
    let n = labels.len() as f32;
    let cell = rect.width() / n;

    let target = selected as f32;
    let animated = ui.ctx().animate_value_with_time(id.with("pill"), target, 0.16);
    let pill =
        Rect::from_min_size(pos2(rect.left() + cell * animated, rect.top()), vec2(cell, rect.height()))
            .shrink(3.0);
    painter.rect_filled(pill, CornerRadius::same(8), pal.surface);
    painter.rect_stroke(pill, CornerRadius::same(8), Stroke::new(1.0, pal.faint), egui::StrokeKind::Inside);

    let mut chosen = None;
    for (i, label) in labels.iter().enumerate() {
        let cell_rect =
            Rect::from_min_size(pos2(rect.left() + cell * i as f32, rect.top()), vec2(cell, rect.height()));
        let response = ui
            .interact(cell_rect, id.with(i), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() && i != selected {
            chosen = Some(i);
        }
        let color = if i == selected { pal.text } else { pal.muted };
        let font = fonts.get(i).cloned().unwrap_or_else(|| fonts::ui(13.0));
        ui.painter().text(cell_rect.center(), Align2::CENTER_CENTER, *label, font, color);
    }
    chosen
}

/// A labelled switch on a full-width row.
pub fn toggle_row(ui: &mut Ui, id: Id, label: &str, value: &mut bool, pal: &Palette) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        *value = !*value;
    }
    let painter = ui.painter();
    painter.text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, label, fonts::ui(14.0), pal.text);

    let t = ui.ctx().animate_bool_with_time(id, *value, 0.14);
    let track = Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(36.0, 20.0));
    painter.rect_filled(track, CornerRadius::same(10), lerp_color(pal.faint, pal.accent, t));
    let knob = pos2(egui::lerp(track.left() + 10.0..=track.right() - 10.0, t), track.center().y);
    painter.circle_filled(
        knob,
        7.5,
        if pal.dark { lerp_color(pal.muted, pal.bg, t) } else { Color32::WHITE },
    );
    response.clicked()
}

pub fn section_label(ui: &mut Ui, text: &str, pal: &Palette) {
    ui.add_space(14.0);
    let galley = tracked(ui, &text.to_uppercase(), fonts::ui(10.5), pal.muted, 1.2);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y + 6.0), Sense::hover());
    ui.painter().galley(rect.left_top(), galley, pal.muted);
}

/// Text with letter-spacing, for small-caps style labels.
pub fn tracked(ui: &Ui, text: &str, font: FontId, color: Color32, tracking: f32) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat { font_id: font, color, extra_letter_spacing: tracking, ..Default::default() },
    );
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Single-line text, elided with an ellipsis if wider than `width`.
pub fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(text, 0.0, TextFormat { font_id: font, color, ..Default::default() });
    job.wrap =
        TextWrapping { max_width: width.max(10.0), max_rows: 1, break_anywhere: true, ..Default::default() };
    ui.fonts_mut(|f| f.layout_job(job))
}

/// A pill-shaped text button.
pub fn pill_button(ui: &mut Ui, id: Id, label: &str, pal: &Palette) -> Response {
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), fonts::ui(14.0), pal.text));
    let size = vec2(galley.size().x + 36.0, 40.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let response = ui.interact(rect, id, Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), hovering(ui, &response), 0.12);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(20), pal.faint.gamma_multiply(0.4 + 0.6 * hover));
    painter.galley(
        pos2(rect.center().x - galley.size().x * 0.5, rect.center().y - galley.size().y * 0.5),
        galley,
        pal.text,
    );
    response
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

pub fn hairline(ui: &Ui, y: f32, x: impl Into<egui::Rangef>, color: Color32) {
    ui.painter().hline(x, y, Stroke::new(1.0, color));
}

/// Snap a position to the physical pixel grid for crisp text.
pub fn snap(ctx: &egui::Context, p: Pos2) -> Pos2 {
    let ppp = ctx.pixels_per_point();
    pos2((p.x * ppp).round() / ppp, (p.y * ppp).round() / ppp)
}

/// Flick-scrolling, for vertical lists on touchscreens.
///
/// egui judges a flick by the finger's average speed over its last 0.1 s, which a flick (that
/// accelerates until the finger leaves the glass) always undershoots, and a second flick
/// replaces the glide rather than adding to it. Here the release speed comes from just the
/// last few frames, flicks in the same direction stack, and a touch stops the glide dead.
#[derive(Clone, Default)]
struct Flick {
    /// Glide speed, in points per second; positive scrolls down.
    vel: f32,
    /// The glide's speed when a finger landed on it, for a flick to build on.
    carried: f32,
    /// Recent (time, pointer y) while a finger drags the list.
    samples: Vec<(f64, f32)>,
    tracking: bool,
    offset: f32,
    max_offset: f32,
}

/// Decay time constant of a glide, in seconds.
const FLICK_DECAY: f32 = 0.6;
/// Only the last this-many seconds of a drag count towards its release speed.
const FLICK_WINDOW: f64 = 0.05;
const FLICK_MIN_SPEED: f32 = 150.0;
const FLICK_MAX_SPEED: f32 = 9000.0;

pub fn flick_scroll<R>(
    ui: &mut Ui,
    id: Id,
    area: egui::ScrollArea,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let mut flick: Flick = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let (time, frame_dt, pressed, down, released, pos) = ui.input(|i| {
        (
            i.time,
            i.stable_dt,
            i.pointer.any_pressed(),
            i.pointer.primary_down(),
            i.pointer.any_released(),
            i.pointer.latest_pos(),
        )
    });

    let dt = frame_dt.min(0.05);

    // Glide, unless a finger is on the glass.
    let mut area = area;
    if pressed {
        flick.carried = flick.vel;
    }
    if pressed || down {
        flick.vel = 0.0;
    } else if flick.vel != 0.0 {
        flick.offset += flick.vel * dt;
        flick.vel *= (-dt / FLICK_DECAY).exp();
        if flick.offset <= 0.0 || flick.offset >= flick.max_offset || flick.vel.abs() < 10.0 {
            flick.offset = flick.offset.clamp(0.0, flick.max_offset.max(0.0));
            flick.vel = 0.0;
        }
        area = area.vertical_scroll_offset(flick.offset);
        ui.ctx().request_repaint();
    }

    // The glide is ours; egui's own would run on top of it.
    let output = ui.scope(|ui| {
        ui.spacing_mut().scroll.kinetic.decay_time = 0.0;
        area.show(ui, add_contents)
    });
    let output = output.inner;
    flick.offset = output.state.clamped_offset().y;

    flick.max_offset = output.content_size.y - output.inner_rect.height();

    // Follow the finger, and measure it as it lets go.
    let touch = ui.input(|i| i.has_touch_screen());
    if touch && pressed && pos.is_some_and(|p| output.inner_rect.contains(p)) {
        flick.tracking = true;
        flick.samples.clear();
    }
    if flick.tracking {
        // Only real movement counts: frames redrawn under a still finger would dilute the speed.
        let window = FLICK_WINDOW.max(frame_dt as f64 * 2.5);
        if let Some(p) = pos
            && flick.samples.last().is_none_or(|&(_, y)| y != p.y)
        {
            flick.samples.push((time, p.y));
            flick.samples.retain(|&(t, _)| time - t <= 2.0 * window);
        }
        if released || !down {
            flick.tracking = false;
            let carried = std::mem::take(&mut flick.carried);
            // A finger that stopped before lifting doesn't flick.
            let last = flick.samples.last().copied().filter(|&(t, _)| time - t <= window);
            let first =
                last.and_then(|(t1, _)| flick.samples.iter().copied().find(|&(t, _)| t1 - t <= window));
            if let (Some((t0, y0)), Some((t1, y1))) = (first, last)
                && t1 > t0
            {
                // Offset grows as the finger moves up.
                let speed = -(y1 - y0) / (t1 - t0) as f32;
                if speed.abs() >= FLICK_MIN_SPEED {
                    let stacked = if speed.signum() == carried.signum() { carried } else { 0.0 };
                    flick.vel = (speed + stacked).clamp(-FLICK_MAX_SPEED, FLICK_MAX_SPEED);
                    ui.ctx().request_repaint();
                }
            }
        }
    }
    ui.data_mut(|d| d.insert_temp(id, flick));
    output.inner
}

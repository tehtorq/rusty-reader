//! A closer look at an illustration: it grows out of the page, and can be pinched, wheeled,
//! dragged and double-tapped like any image viewer.

use eframe::egui::{self, Color32, Id, Key, Modifiers, Order, Pos2, Rect, Sense, Vec2, vec2};

use crate::picture::Picture;
use crate::theme::Palette;
use crate::widgets::{self, Icon};

const OPEN_SECS: f32 = 0.24;
const CLOSE_SECS: f32 = 0.2;
const ZOOM_SECS: f32 = 0.22;
const MAX_ZOOM: f32 = 6.0;
const DOUBLE_TAP_ZOOM: f32 = 2.5;

/// A zoom level and pan, together.
type View = (f32, Vec2);

pub struct Viewer {
    key: String,
    /// Where the image sat on the page; it opens out of, and closes back into, this.
    from: Rect,
    opened: f64,
    /// When closing began, and from where.
    closing: Option<(f64, Rect)>,
    /// 1.0 fits the image to the screen.
    zoom: f32,
    /// The image centre's offset from the middle of the screen.
    pan: Vec2,
    /// A double-tap zoom on its way: from (zoom, pan) to (zoom, pan), since when.
    glide: Option<(View, View, f64)>,
}

impl Viewer {
    pub fn new(key: String, from: Rect, now: f64) -> Viewer {
        Viewer { key, from, opened: now, closing: None, zoom: 1.0, pan: Vec2::ZERO, glide: None }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Show the viewer; returns `false` once it has finished closing.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        screen: Rect,
        picture: &Picture,
        pal: &Palette,
        now: f64,
    ) -> bool {
        let opening = ease(((now - self.opened) as f32 / OPEN_SECS).clamp(0.0, 1.0));
        let closing = self.closing.map(|(at, _)| ease(((now - at) as f32 / CLOSE_SECS).clamp(0.0, 1.0)));
        if closing.is_some_and(|t| t >= 1.0) {
            return false;
        }
        if opening < 1.0 || closing.is_some() || self.glide.is_some() {
            ctx.request_repaint();
        }
        let presence = opening * (1.0 - closing.unwrap_or(0.0));

        let bounds = screen.shrink(12.0);
        let image = picture.size();
        let fit = image * (bounds.width() / image.x).min(bounds.height() / image.y);
        let center = bounds.center();

        let viewport = ctx.viewport_rect();
        egui::Area::new(Id::unique("image-viewer")).order(Order::Foreground).fixed_pos(viewport.min).show(
            ctx,
            |ui| {
                let response =
                    ui.interact(viewport, Id::unique("image-viewer-surface"), Sense::click_and_drag());
                if self.closing.is_none() {
                    self.handle_input(ui, &response, fit, center, screen, now);
                }
                self.advance_glide(now);

                let shown = Rect::from_center_size(center + self.pan, fit * self.zoom);
                let rect = match (self.closing, closing) {
                    (Some((_, last)), Some(t)) => lerp_rect(last, self.from, t),
                    _ => lerp_rect(self.from, shown, opening),
                };

                let painter = ui.painter();
                painter.rect_filled(viewport, 0.0, pal.bg.gamma_multiply(presence));
                let tint = if pal.dark { Color32::from_gray(205) } else { Color32::WHITE };
                picture.paint(painter, rect, tint.gamma_multiply(opening.max(0.001)));

                if presence > 0.0 {
                    ui.set_opacity(presence);
                    let close =
                        Rect::from_center_size(screen.right_top() + vec2(-30.0, 30.0), vec2(40.0, 40.0));
                    ui.painter().circle_filled(close.center(), 20.0, pal.bg.gamma_multiply(0.85));
                    if widgets::icon_button(ui, close, Id::unique("image-viewer-close"), Icon::Close, pal)
                        .clicked()
                    {
                        self.close(shown, now);
                    }
                }
                if self.closing.is_none()
                    && response.clicked()
                    && !shown.contains(response.interact_pointer_pos().unwrap_or(center))
                {
                    // A tap beside the image puts it back.
                    self.close(shown, now);
                }
            },
        );
        true
    }

    fn close(&mut self, shown: Rect, now: f64) {
        if self.closing.is_none() {
            self.closing = Some((now, shown));
            self.glide = None;
        }
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        fit: Vec2,
        center: Pos2,
        screen: Rect,
        now: f64,
    ) {
        let shown = Rect::from_center_size(center + self.pan, fit * self.zoom);
        if ui.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Escape) || i.consume_key(Modifiers::NONE, Key::BrowserBack)
        }) {
            self.close(shown, now);
            return;
        }

        let (pinch, multi, hover, wheel, touch) = ui.input(|i| {
            (i.zoom_delta(), i.multi_touch(), i.pointer.hover_pos(), i.smooth_scroll_delta.y, i.any_touches())
        });
        let mut factor = pinch;
        // A mouse wheel zooms too, the way image viewers do.
        if multi.is_none() && !touch && wheel != 0.0 {
            factor *= (wheel * 0.004).exp();
        }
        let focus = multi.map(|m| m.center_pos).or(hover).unwrap_or(center);
        if factor != 1.0 {
            self.glide = None;
            (self.zoom, self.pan) = zoomed(self.zoom, self.pan, factor, focus - center);
        }

        if let Some(m) = multi {
            self.glide = None;
            self.pan += m.translation_delta;
        } else if response.dragged() {
            self.glide = None;
            self.pan += response.drag_delta();
        }

        if response.double_clicked() {
            let at = response.interact_pointer_pos().unwrap_or(center);
            let to = if self.zoom > 1.05 {
                (1.0, Vec2::ZERO)
            } else {
                zoomed(self.zoom, self.pan, DOUBLE_TAP_ZOOM / self.zoom, at - center)
            };
            self.glide = Some(((self.zoom, self.pan), (to.0, clamp_pan(to.1, fit * to.0, screen)), now));
        }

        if self.glide.is_none() {
            self.pan = clamp_pan(self.pan, fit * self.zoom, screen);
        }
    }

    fn advance_glide(&mut self, now: f64) {
        let Some(((z0, p0), (z1, p1), start)) = self.glide else { return };
        let t = ease(((now - start) as f32 / ZOOM_SECS).clamp(0.0, 1.0));
        self.zoom = egui::lerp(z0..=z1, t);
        self.pan = p0 + (p1 - p0) * t;
        if t >= 1.0 {
            self.glide = None;
        }
    }
}

/// Zoom by `factor` keeping the point `focus` (relative to the screen centre) where it is.
fn zoomed(zoom: f32, pan: Vec2, factor: f32, focus: Vec2) -> (f32, Vec2) {
    let new = (zoom * factor).clamp(1.0, MAX_ZOOM);
    let k = new / zoom;
    (new, focus - (focus - pan) * k)
}

/// An image larger than the screen may be dragged until its edges meet the screen's; a smaller
/// one stays centred.
fn clamp_pan(pan: Vec2, size: Vec2, screen: Rect) -> Vec2 {
    let limit = ((size - screen.size()) * 0.5).max(Vec2::ZERO);
    vec2(pan.x.clamp(-limit.x, limit.x), pan.y.clamp(-limit.y, limit.y))
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t))
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_keeps_the_focus_still() {
        // A point 100 to the right of centre, on an unpanned image, stays put at any zoom.
        let (zoom, pan) = zoomed(1.0, Vec2::ZERO, 2.0, vec2(100.0, 0.0));
        assert_eq!(zoom, 2.0);
        assert_eq!(pan, vec2(-100.0, 0.0));
        // Zooming back out undoes it.
        let (zoom, pan) = zoomed(zoom, pan, 0.5, vec2(100.0, 0.0));
        assert_eq!((zoom, pan), (1.0, Vec2::ZERO));
    }

    #[test]
    fn small_images_stay_centred() {
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 800.0));
        assert_eq!(clamp_pan(vec2(50.0, -20.0), vec2(300.0, 300.0), screen), Vec2::ZERO);
        assert_eq!(clamp_pan(vec2(500.0, 0.0), vec2(800.0, 300.0), screen), vec2(200.0, 0.0));
    }
}

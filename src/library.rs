//! The shelf: recently read books, and a way to open another.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, Id, Rect, Sense, Shadow, Stroke, TextureHandle,
    TextureOptions, Ui, pos2, vec2,
};

use crate::book;
use crate::fonts;
use crate::store::RecentBook;
use crate::theme::{Palette, ShelfView, ThemeKind};
use crate::widgets::{self, Icon};

pub enum Action {
    Open(PathBuf),
    Pick,
    Forget(usize),
    Theme(ThemeKind),
    View(ShelfView),
}

enum Cover {
    Pending,
    Ready(TextureHandle),
    Missing,
}

/// Cover thumbnails, pulled out of each book on a background thread as the shelf asks for them.
pub struct Covers {
    slots: HashMap<PathBuf, Cover>,
    requests: Sender<PathBuf>,
    results: Receiver<(PathBuf, Option<ColorImage>)>,
}

impl Covers {
    pub fn new(ctx: &egui::Context) -> Covers {
        let (requests, inbox) = channel::<PathBuf>();
        let (outbox, results) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for path in inbox {
                let image = book::cover_thumbnail(&path, 480);
                if outbox.send((path, image)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Covers { slots: HashMap::new(), requests, results }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((path, image)) = self.results.try_recv() {
            let cover = match image {
                Some(image) => {
                    let name = format!("cover:{}", path.display());
                    Cover::Ready(ctx.load_texture(name, image, TextureOptions::LINEAR))
                }
                None => Cover::Missing,
            };
            self.slots.insert(path, cover);
        }
    }

    fn get(&mut self, path: &Path) -> &Cover {
        self.slots.entry(path.to_owned()).or_insert_with(|| {
            let _ = self.requests.send(path.to_owned());
            Cover::Pending
        })
    }
}

pub fn ui(
    ui: &mut Ui,
    recents: &[RecentBook],
    view: ShelfView,
    covers: &mut Covers,
    theme: ThemeKind,
    status: Option<(&str, bool)>,
) -> Option<Action> {
    let pal = theme.palette();
    covers.poll(ui.ctx());
    let armed_id = Id::unique("shelf-armed");
    let mut shelf = Shelf { armed: ui.data(|d| d.get_temp(armed_id)), now: ui.input(|i| i.time) };
    let armed_before = shelf.armed;
    let screen = ui.ctx().content_rect();
    ui.painter().rect_filled(screen, 0.0, pal.bg);
    let mut action = None;

    let area = egui::ScrollArea::vertical().auto_shrink(false);
    widgets::flick_scroll(ui, Id::unique("shelf-scroll"), area, |ui| {
        let width = (screen.width() - 48.0).min(560.0);
        let left = screen.center().x - width * 0.5;
        ui.add_space((screen.height() * 0.12).clamp(32.0, 120.0));

        // Title and invitation.
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 128.0), Sense::hover());
        let mark = widgets::tracked(ui, "RUSTY READER", fonts::ui(11.0), pal.muted, 2.2);
        ui.painter().galley(pos2(left, head.top()), mark, pal.muted);
        ui.painter().text(
            pos2(left - 2.0, head.top() + 24.0),
            Align2::LEFT_TOP,
            "Library",
            fonts::serif(42.0, false),
            pal.text,
        );
        ui.painter().text(
            pos2(left, head.top() + 100.0),
            Align2::LEFT_TOP,
            if cfg!(target_os = "android") {
                "Open an EPUB, or share one to Rusty Reader."
            } else {
                "Drop an EPUB anywhere on this window, or open one."
            },
            fonts::ui(13.5),
            pal.muted,
        );

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(left - ui.cursor().left());
            if widgets::pill_button(ui, Id::unique("open-book"), "Open a book…", &pal).clicked() {
                action = Some(Action::Pick);
            }
            if let Some((message, is_error)) = status {
                ui.add_space(14.0);
                let color = if is_error { pal.accent } else { pal.muted };
                let galley = widgets::elided(ui, message, fonts::serif(14.0, true), color, width - 180.0);
                let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x, 40.0), Sense::hover());
                ui.painter().galley(
                    pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
                    galley,
                    color,
                );
            }
        });
        ui.add_space(36.0);

        if recents.is_empty() {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
            widgets::hairline(ui, rect.top(), left..=left + width, pal.faint);
            ui.painter().text(
                pos2(left, rect.top() + 24.0),
                Align2::LEFT_TOP,
                "Your shelf is empty, for now.",
                fonts::serif(16.0, true),
                pal.muted,
            );
        } else {
            let (label, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
            let galley = widgets::tracked(ui, "CONTINUE READING", fonts::ui(10.5), pal.muted, 1.2);
            let label_center = label.top() + galley.size().y * 0.5;
            ui.painter().galley(pos2(left, label.top()), galley, pal.muted);
            if let Some(choice) = view_switch(ui, pos2(left + width, label_center), view, &pal) {
                action = Some(Action::View(choice));
            }
            let chosen = match view {
                ShelfView::List => list(ui, recents, covers, &mut shelf, left, width, &pal),
                ShelfView::Covers => {
                    ui.add_space(16.0);
                    grid(ui, recents, covers, &mut shelf, left, width, &pal)
                }
            };
            if chosen.is_some() {
                action = chosen;
            }
        }

        // A discreet theme switch at the foot of the page.
        ui.add_space(40.0);
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        for (i, kind) in ThemeKind::ALL.into_iter().enumerate() {
            let center = pos2(left + 10.0 + i as f32 * 30.0, row.center().y);
            let tp = kind.palette();
            let rect = Rect::from_center_size(center, vec2(26.0, 26.0));
            let response =
                ui.interact(rect, Id::unique(("lib-theme", i)), Sense::click()).on_hover_text(kind.name());
            if response.clicked() {
                action = Some(Action::Theme(kind));
            }
            let painter = ui.painter();
            painter.circle_filled(center, 9.0, tp.bg);
            painter.circle_stroke(center, 9.0, Stroke::new(1.0, pal.faint));
            if kind == theme {
                painter.circle_stroke(center, 12.5, Stroke::new(1.2, pal.accent));
            }
        }
        ui.add_space(24.0);
    });

    // A tap anywhere disarms a book armed for removal (but not the end of the press that armed it).
    if shelf.armed == armed_before
        && shelf.armed.is_some_and(|(_, at)| shelf.now - at >= ARM_GRACE)
        && ui.input(|i| i.pointer.any_click())
    {
        shelf.armed = None;
    }
    if matches!(action, Some(Action::Forget(_) | Action::Open(_))) {
        shelf.armed = None;
    }
    ui.data_mut(|d| match shelf.armed {
        Some(armed) => {
            d.insert_temp(armed_id, armed);
        }
        None => d.remove::<(usize, f64)>(armed_id),
    });
    action
}

/// Seconds after arming during which a click is just the long press ending.
const ARM_GRACE: f64 = 0.4;

/// Which book, if any, is armed for removal by a long press (or right-click), and since when.
struct Shelf {
    armed: Option<(usize, f64)>,
    now: f64,
}

impl Shelf {
    fn is_armed(&self, i: usize) -> bool {
        self.armed.is_some_and(|(a, _)| a == i)
    }

    /// Whether a press on book `i` should open it: long presses arm it instead, and while a
    /// book is armed the next tap only disarms.
    fn opens(&mut self, response: &egui::Response, i: usize) -> bool {
        if response.secondary_clicked() {
            self.armed = Some((i, self.now));
            return false;
        }
        response.clicked() && self.armed.is_none()
    }
}

/// Two small glyphs, right-aligned at `right_center`, for choosing between rows and covers.
fn view_switch(ui: &mut Ui, right_center: egui::Pos2, view: ShelfView, pal: &Palette) -> Option<ShelfView> {
    let mut chosen = None;
    let options = [(ShelfView::List, Icon::List, "List"), (ShelfView::Covers, Icon::Grid, "Covers")];
    for (i, (kind, icon, name)) in options.into_iter().enumerate() {
        let center = pos2(right_center.x - 13.0 - (1 - i) as f32 * 30.0, right_center.y);
        let rect = Rect::from_center_size(center, vec2(26.0, 26.0));
        let id = Id::unique(("shelf-view", i));
        let response = ui
            .interact(rect, id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(name);
        let hover = ui.ctx().animate_bool_with_time(id.with("hover"), widgets::hovering(ui, &response), 0.12);
        let on = ui.ctx().animate_bool_with_time(id.with("on"), view == kind, 0.16);
        let painter = ui.painter();
        painter.circle_filled(center, 13.0, pal.faint.gamma_multiply((0.7 * on).max(0.4 * hover)));
        widgets::paint_icon(painter, rect, icon, widgets::lerp_color(pal.muted, pal.text, on.max(hover)));
        if response.clicked() && view != kind {
            chosen = Some(kind);
        }
    }
    chosen
}

fn list(
    ui: &mut Ui,
    recents: &[RecentBook],
    covers: &mut Covers,
    shelf: &mut Shelf,
    left: f32,
    width: f32,
    pal: &Palette,
) -> Option<Action> {
    let mut action = None;
    for (i, book) in recents.iter().enumerate() {
        let (row, response) = ui.allocate_exact_size(vec2(ui.available_width(), 86.0), Sense::click());
        let row = Rect::from_x_y_ranges(left - 14.0..=left + width + 14.0, row.y_range());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = ui.ctx().animate_bool_with_time(
            Id::unique(("shelf-hover", i)),
            widgets::hovering(ui, &response),
            0.12,
        );
        let painter = ui.painter();
        if hover > 0.0 {
            painter.rect_filled(
                row.shrink2(vec2(0.0, 4.0)),
                CornerRadius::same(12),
                pal.faint.gamma_multiply(0.55 * hover),
            );
        }
        if i > 0 {
            widgets::hairline(ui, row.top(), left..=left + width, pal.faint.gamma_multiply(1.0 - hover));
        }

        // A small cover, sitting on the row like a spine-out book turned face-on.
        let thumb = Rect::from_min_size(pos2(left, row.center().y - 30.0), vec2(40.0, 60.0));
        let radius = CornerRadius::same(2);
        let shadow = Shadow {
            offset: [0, 2],
            blur: 6,
            spread: 0,
            color: Color32::from_black_alpha(if pal.dark { 80 } else { 28 }),
        };
        ui.painter().add(shadow.as_shape(thumb, radius));
        paint_cover(ui, covers, book, thumb, radius, pal);

        let text_left = thumb.right() + 18.0;
        let text_width = left + width - 90.0 - text_left;
        let title = widgets::elided(ui, &book.title, fonts::serif(19.0, false), pal.text, text_width);
        let title_h = title.size().y;
        let author_h = if book.author.is_some() { 20.0 } else { 0.0 };
        let top = row.center().y - (title_h + author_h) * 0.5;
        ui.painter().galley(pos2(text_left, top), title, pal.text);
        if let Some(author) = &book.author {
            let galley = widgets::elided(ui, author, fonts::ui(13.0), pal.muted, text_width);
            ui.painter().galley(pos2(text_left, top + title_h + 4.0), galley, pal.muted);
        }

        let right = left + width;
        if shelf.is_armed(i) {
            // Long-pressed: the progress makes way for a way off the shelf.
            let button = Rect::from_min_max(
                pos2(right - 92.0, row.center().y - 18.0),
                pos2(right, row.center().y + 18.0),
            );
            let resp = ui
                .interact(button, Id::unique(("remove", i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let painter = ui.painter();
            painter.rect_filled(
                button,
                CornerRadius::same(18),
                pal.faint.gamma_multiply(if widgets::hovering(ui, &resp) { 1.0 } else { 0.7 }),
            );
            painter.text(button.center(), Align2::CENTER_CENTER, "Remove", fonts::ui(13.5), pal.accent);
            if resp.clicked() {
                action = Some(Action::Forget(i));
                continue;
            }
        } else {
            // Progress, as a short rule and a number.
            let bar_w = 64.0;
            let y = row.center().y + 9.0;
            progress_rule(ui, right - bar_w..=right, y, book.progress, pal);
            ui.painter().text(
                pos2(right, y - 10.0),
                Align2::RIGHT_BOTTOM,
                progress_label(book.progress),
                fonts::ui(12.0),
                pal.muted,
            );
        }

        // Forget, on hover, in the margin (when the window leaves one).
        let forget = Rect::from_center_size(pos2(right + 34.0, row.center().y), vec2(30.0, 30.0));
        if !shelf.is_armed(i) && (hover > 0.5 || ui.rect_contains_pointer(forget)) {
            let resp = widgets::icon_button(ui, forget, Id::unique(("forget", i)), Icon::Close, pal)
                .on_hover_text("Remove from shelf");
            if resp.clicked() {
                action = Some(Action::Forget(i));
                continue;
            }
        }
        if shelf.opens(&response, i) {
            action = Some(Action::Open(book.path.clone()));
        }
    }
    action
}

fn grid(
    ui: &mut Ui,
    recents: &[RecentBook],
    covers: &mut Covers,
    shelf: &mut Shelf,
    left: f32,
    width: f32,
    pal: &Palette,
) -> Option<Action> {
    const GAP: f32 = 22.0;
    const MIN_CELL: f32 = 112.0;
    let columns = ((width + GAP) / (MIN_CELL + GAP)).floor().clamp(2.0, 5.0) as usize;
    let cell_w = (width - GAP * (columns - 1) as f32) / columns as f32;
    let cover_h = (cell_w * 1.5).round();
    let cell_h = cover_h + 82.0;

    let mut action = None;
    for (r, row) in recents.chunks(columns).enumerate() {
        let (band, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell_h), Sense::hover());
        for (c, book) in row.iter().enumerate() {
            let i = r * columns + c;
            let x = left + c as f32 * (cell_w + GAP);
            let cell = Rect::from_min_size(pos2(x, band.top()), vec2(cell_w, cell_h));
            let response = ui
                .interact(cell, Id::unique(("shelf-cell", i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let hover = ui.ctx().animate_bool_with_time(
                Id::unique(("shelf-hover", i)),
                widgets::hovering(ui, &response),
                0.14,
            );

            // The cover lifts a little towards the reader on hover.
            let lift = 3.0 * hover;
            let cover = Rect::from_min_size(pos2(x, band.top() + 4.0 - lift), vec2(cell_w, cover_h));
            let radius = CornerRadius::same(3);
            let shadow = Shadow {
                offset: [0, (3.0 + 4.0 * hover) as i8],
                blur: (10.0 + 10.0 * hover) as u8,
                spread: 0,
                color: Color32::from_black_alpha(if pal.dark { 90 } else { 34 }),
            };
            ui.painter().add(shadow.as_shape(cover, radius));
            paint_cover(ui, covers, book, cover, radius, pal);

            let y = cover.bottom() + lift + 12.0;
            progress_rule(ui, x..=x + cell_w, y, book.progress, pal);
            let title = wrapped(ui, &book.title, fonts::serif(15.0, false), pal.text, cell_w, 2, Align::LEFT);
            let title_h = title.size().y;
            ui.painter().galley(pos2(x, y + 9.0), title, pal.text);
            let byline = match &book.author {
                Some(author) if book.progress > 0.0 => {
                    format!("{author} · {}", progress_label(book.progress))
                }
                Some(author) => author.clone(),
                None => progress_label(book.progress),
            };
            let byline = widgets::elided(ui, &byline, fonts::ui(12.0), pal.muted, cell_w);
            ui.painter().galley(pos2(x, y + 11.0 + title_h), byline, pal.muted);

            // Forget, in the cover's corner: on hover, or after a long press.
            let forget = Rect::from_center_size(cover.right_top() + vec2(-16.0, 16.0), vec2(24.0, 24.0));
            let armed = shelf.is_armed(i);
            if armed || hover > 0.5 || ui.rect_contains_pointer(forget) {
                let shown = if armed { 1.0 } else { hover };
                ui.painter().circle_filled(forget.center(), 12.0, pal.surface.gamma_multiply(0.92 * shown));
                let resp = widgets::icon_button(ui, forget, Id::unique(("forget", i)), Icon::Close, pal)
                    .on_hover_text("Remove from shelf");
                if resp.clicked() {
                    action = Some(Action::Forget(i));
                    continue;
                }
            }
            if shelf.opens(&response, i) {
                action = Some(Action::Open(book.path.clone()));
            }
        }
        ui.add_space(26.0);
    }
    action
}

fn paint_cover(
    ui: &Ui,
    covers: &mut Covers,
    book: &RecentBook,
    rect: Rect,
    radius: CornerRadius,
    pal: &Palette,
) {
    match covers.get(&book.path) {
        Cover::Ready(texture) => {
            let tint = if pal.dark { Color32::from_gray(215) } else { Color32::WHITE };
            egui::Image::from_texture(texture)
                .uv(fill_uv(texture.size_vec2(), rect.size()))
                .corner_radius(radius)
                .tint(tint)
                .paint_at(ui, rect);
        }
        Cover::Missing => plain_cover(ui, rect, book),
        Cover::Pending => {
            ui.painter().rect_filled(rect, radius, pal.faint.gamma_multiply(0.6));
        }
    }
}

/// Texture coordinates that fill `target` with the image, cropping whichever dimension overflows.
fn fill_uv(image: egui::Vec2, target: egui::Vec2) -> Rect {
    let (ia, ta) = (image.x / image.y.max(1.0), target.x / target.y.max(1.0));
    if ia > ta {
        let w = ta / ia;
        Rect::from_min_max(pos2(0.5 - w * 0.5, 0.0), pos2(0.5 + w * 0.5, 1.0))
    } else {
        let h = ia / ta;
        Rect::from_min_max(pos2(0.0, 0.5 - h * 0.5), pos2(1.0, 0.5 + h * 0.5))
    }
}

/// A cloth-bound stand-in for books that carry no cover of their own.
fn plain_cover(ui: &Ui, rect: Rect, book: &RecentBook) {
    const CLOTH: [Color32; 6] = [
        Color32::from_rgb(0x4f, 0x5e, 0x4c),
        Color32::from_rgb(0x6b, 0x4a, 0x3c),
        Color32::from_rgb(0x3d, 0x50, 0x63),
        Color32::from_rgb(0x7a, 0x5c, 0x37),
        Color32::from_rgb(0x58, 0x47, 0x5f),
        Color32::from_rgb(0x5c, 0x3b, 0x40),
    ];
    let hash = book.title.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    let cloth = CLOTH[hash as usize % CLOTH.len()];
    let ink = Color32::from_rgb(0xee, 0xe4, 0xcf);
    let painter = ui.painter();
    let small = rect.width() < 80.0;
    painter.rect_filled(rect, CornerRadius::same(if small { 2 } else { 3 }), cloth);
    painter.rect_stroke(
        rect.shrink(if small { 3.0 } else { 6.0 }),
        CornerRadius::same(1),
        Stroke::new(if small { 0.6 } else { 0.8 }, ink.gamma_multiply(0.45)),
        egui::StrokeKind::Inside,
    );

    // Too small for a title page: a single initial will do.
    if small {
        let initial = book.title.trim_start_matches(|c: char| !c.is_alphanumeric()).chars().next();
        if let Some(initial) = initial {
            let font = fonts::serif(rect.width() * 0.45, false);
            painter.text(rect.center(), Align2::CENTER_CENTER, initial.to_uppercase(), font, ink);
        }
        return;
    }

    let inner = rect.width() - 28.0;
    // Subtitles rarely fit on a cover this size; keep the title proper.
    let short = book.title.split([':', ';']).next().unwrap_or(&book.title).trim();
    let title = wrapped(ui, short, fonts::serif(15.0, false), ink, inner, 5, Align::Center);
    painter.galley(pos2(rect.center().x, rect.top() + rect.height() * 0.24), title, ink);

    if let Some(author) = &book.author {
        let author = widgets::elided(ui, author, fonts::serif(11.0, true), ink, inner);
        let at = pos2(rect.center().x - author.size().x * 0.5, rect.bottom() - 18.0 - author.size().y);
        painter.galley(at, author, ink.gamma_multiply(0.85));
    }
}

fn wrapped(
    ui: &Ui,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
    rows: usize,
    halign: Align,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob {
        halign,
        wrap: TextWrapping { max_width: width, max_rows: rows, ..Default::default() },
        ..Default::default()
    };
    job.append(text, 0.0, TextFormat { font_id: font, color, ..Default::default() });
    ui.fonts_mut(|f| f.layout_job(job))
}

fn progress_rule(ui: &Ui, x: std::ops::RangeInclusive<f32>, y: f32, progress: f32, pal: &Palette) {
    let (a, b) = (*x.start(), *x.end());
    let painter = ui.painter();
    painter.line_segment([pos2(a, y), pos2(b, y)], Stroke::new(2.0, pal.faint));
    let done = a + (b - a) * progress.clamp(0.0, 1.0);
    if done > a {
        painter.line_segment([pos2(a, y), pos2(done, y)], Stroke::new(2.0, pal.muted));
    }
}

fn progress_label(progress: f32) -> String {
    if progress >= 0.995 { "Finished".to_owned() } else { format!("{}%", (progress * 100.0).round()) }
}

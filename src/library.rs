//! The shelf: recently read books, and a way to open another.

use eframe::egui::{self, Align2, CornerRadius, Id, Sense, Stroke, Ui, pos2, vec2};

use crate::fonts;
use crate::store::RecentBook;
use crate::theme::ThemeKind;
use crate::widgets;

pub enum Action {
    Open(std::path::PathBuf),
    Pick,
    Forget(usize),
    Theme(ThemeKind),
}

pub fn ui(
    ui: &mut Ui,
    recents: &[RecentBook],
    theme: ThemeKind,
    status: Option<(&str, bool)>,
) -> Option<Action> {
    let pal = theme.palette();
    let screen = ui.ctx().content_rect();
    ui.painter().rect_filled(screen, 0.0, pal.bg);
    let mut action = None;

    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
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
            "Drop an EPUB anywhere on this window, or open one.",
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
            ui.painter().galley(pos2(left, label.top()), galley, pal.muted);
        }

        for (i, book) in recents.iter().enumerate() {
            let (row, response) = ui.allocate_exact_size(vec2(ui.available_width(), 78.0), Sense::click());
            let row = egui::Rect::from_x_y_ranges(left - 14.0..=left + width + 14.0, row.y_range());
            let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
            let hover =
                ui.ctx().animate_bool_with_time(Id::unique(("shelf-hover", i)), response.hovered(), 0.12);
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

            let text_width = width - 110.0;
            let title = widgets::elided(ui, &book.title, fonts::serif(19.0, false), pal.text, text_width);
            let title_h = title.size().y;
            ui.painter().galley(pos2(left, row.top() + 16.0), title, pal.text);
            if let Some(author) = &book.author {
                let galley = widgets::elided(ui, author, fonts::ui(13.0), pal.muted, text_width);
                ui.painter().galley(pos2(left, row.top() + 20.0 + title_h), galley, pal.muted);
            }

            // Progress, as a short rule and a number.
            let right = left + width;
            let bar_w = 64.0;
            let y = row.center().y + 9.0;
            let painter = ui.painter();
            painter.line_segment([pos2(right - bar_w, y), pos2(right, y)], Stroke::new(2.0, pal.faint));
            painter.line_segment(
                [pos2(right - bar_w, y), pos2(right - bar_w + bar_w * book.progress.clamp(0.0, 1.0), y)],
                Stroke::new(2.0, pal.muted),
            );
            let pct = if book.progress >= 0.995 {
                "Finished".to_owned()
            } else {
                format!("{}%", (book.progress * 100.0).round())
            };
            painter.text(pos2(right, y - 10.0), Align2::RIGHT_BOTTOM, pct, fonts::ui(12.0), pal.muted);

            // Forget, on hover.
            let forget = egui::Rect::from_center_size(pos2(right + 34.0, row.center().y), vec2(30.0, 30.0));
            if hover > 0.5 || ui.rect_contains_pointer(forget) {
                let resp =
                    widgets::icon_button(ui, forget, Id::unique(("forget", i)), widgets::Icon::Close, &pal)
                        .on_hover_text("Remove from shelf");
                if resp.clicked() {
                    action = Some(Action::Forget(i));
                    continue;
                }
            }
            if response.clicked() {
                action = Some(Action::Open(book.path.clone()));
            }
        }

        // A discreet theme switch at the foot of the page.
        ui.add_space(40.0);
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        for (i, kind) in ThemeKind::ALL.into_iter().enumerate() {
            let center = pos2(left + 10.0 + i as f32 * 30.0, row.center().y);
            let tp = kind.palette();
            let rect = egui::Rect::from_center_size(center, vec2(26.0, 26.0));
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
    action
}

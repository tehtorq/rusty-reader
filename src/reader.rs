//! The reading surface: one quiet page, with everything else a tap away.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, Event, Id, Key, LayerId, MouseWheelUnit, Order,
    Painter, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, TouchPhase, Ui, pos2, vec2,
};
use hyphenation::Standard;

use crate::book::Book;
use crate::fonts;
use crate::layout::{self, ChapterLayout, Content, Params, Shaper};
use crate::theme::{Measure, Palette, Settings, Spacing, ThemeKind, Typeface};
use crate::viewer::Viewer;
use crate::widgets::{self, Icon, lerp_color};

/// Offset meaning "the last page of the chapter", resolved once the chapter is laid out.
const LAST: usize = usize::MAX;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sheet {
    None,
    Settings,
    Contents,
}

/// Illustrations are decoded on a background thread the first time they're needed.
enum ImageSlot {
    Loading(Receiver<Option<ColorImage>>),
    Ready(TextureHandle),
    Failed,
}

/// A page sliding home after a swipe or tap.
struct Slide {
    base: f32,
    start: f64,
    duration: f32,
}

pub struct Reader {
    pub book: Arc<Book>,
    pub path: PathBuf,
    pub chapter: usize,
    pub offset: usize,
    layouts: HashMap<usize, ChapterLayout>,
    params: Option<Params>,
    pending: Option<(Params, f64)>,
    shaper: Shaper,
    images: HashMap<String, ImageSlot>,
    /// Where each illustration on the page at rest was drawn, last frame: taps there open it.
    page_images: Vec<(Rect, String)>,
    viewer: Option<Viewer>,
    chrome: bool,
    sheet: Sheet,
    slide: Option<Slide>,
    drag: Option<f32>,
    wheel_acc: f32,
    wheel_locked: bool,
    wheel_last: f64,
    scrub: Option<f32>,
    pointer_moved: f64,
    opened_at: Option<f64>,
    scroll_toc: bool,
    /// Set whenever the reading position changes, so the app can remember it.
    pub moved: bool,
    /// Set when the reader wants to return to the library.
    pub close: bool,
}

impl Reader {
    pub fn new(book: Arc<Book>, path: PathBuf, chapter: usize, offset: usize) -> Reader {
        let chapter = chapter.min(book.chapters.len() - 1);
        let offset = offset.min(book.chapters[chapter].chars.saturating_sub(1));
        Reader {
            book,
            path,
            chapter,
            offset,
            layouts: HashMap::new(),
            params: None,
            pending: None,
            shaper: Shaper::new(Typeface::Literata),
            images: HashMap::new(),
            page_images: Vec::new(),
            viewer: None,
            chrome: false,
            sheet: Sheet::None,
            slide: None,
            drag: None,
            wheel_acc: 0.0,
            wheel_locked: false,
            wheel_last: 0.0,
            scrub: None,
            pointer_moved: 0.0,
            opened_at: None,
            scroll_toc: false,
            moved: false,
            close: false,
        }
    }

    pub fn progress(&self) -> f32 {
        self.book.progress(self.chapter, self.offset.min(self.book.chapters[self.chapter].chars))
    }

    pub fn ui(&mut self, ui: &mut Ui, settings: &mut Settings, hyph: Option<&Standard>) {
        let ctx = ui.ctx().clone();
        // Our hyphenation patterns are English; leave other languages unbroken.
        let english = self.book.language.as_deref().is_none_or(|l| l.starts_with("en"));
        let hyph = hyph.filter(|_| english);
        let pal = settings.theme.palette();
        let screen = ui.ctx().content_rect();
        let now = ctx.input(|i| i.time);
        let opened_at = *self.opened_at.get_or_insert(now);
        ui.painter().rect_filled(screen, 0.0, pal.bg);

        let target = target_params(screen, settings);
        self.update_params(target, now, &ctx);
        let Some(params) = self.params else { return };

        self.ensure(&ctx, hyph, self.chapter);
        let pages = &self.layouts[&self.chapter].pages;
        if self.offset == LAST {
            self.offset = pages.last().map_or(0, |p| p.offset);
        }

        let column = Rect::from_min_size(
            pos2(
                (screen.center().x - params.width * 0.5).round(),
                screen.top() + column_top(screen, &params),
            ),
            vec2(params.width, params.height),
        );

        if self.viewer.is_none() {
            self.handle_input(ui, &ctx, hyph, settings, screen, now);
        }

        // Pages, sliding if a turn is under way.
        let offset = self.visual_offset(now);
        if let Some(slide) = &self.slide {
            if (now - slide.start) as f32 >= slide.duration {
                self.slide = None;
            } else {
                ctx.request_repaint();
            }
        }
        let fade = ease_out(((now - opened_at) / 0.5).clamp(0.0, 1.0) as f32);
        if fade < 1.0 {
            ctx.request_repaint();
        }
        let page = self.layouts[&self.chapter].page_at(self.offset);
        let neighbour = if offset < -0.5 {
            self.neighbour(&ctx, hyph, 1).map(|n| (n, screen.width()))
        } else if offset > 0.5 {
            self.neighbour(&ctx, hyph, -1).map(|n| (n, -screen.width()))
        } else {
            None
        };
        let painter = ui.painter().clone();
        self.page_images.clear();
        let at_rest = offset.abs() < 0.5;
        let origin = column.min + vec2(offset, 0.0);
        self.paint_page(&painter, &ctx, self.chapter, page, origin, &pal, fade, at_rest);
        if let Some(((chapter, page), shift)) = neighbour {
            let origin = column.min + vec2(offset + shift, 0.0);
            self.paint_page(&painter, &ctx, chapter, page, origin, &pal, fade, false);
        }

        let chrome_t = ctx.animate_bool_with_time(Id::unique("chrome"), self.chrome, 0.2);
        if settings.show_progress {
            self.paint_footer(ui, screen, column, &pal, (1.0 - chrome_t) * fade);
        }
        if chrome_t > 0.0 {
            self.top_bar(&ctx, screen, &pal, chrome_t);
            let bottom_t = chrome_t
                * (1.0 - ctx.animate_bool_with_time(Id::unique("any-sheet"), self.sheet != Sheet::None, 0.2));
            if bottom_t > 0.0 {
                self.bottom_bar(&ctx, screen, &pal, bottom_t);
            }
        }
        self.settings_sheet(&ctx, screen, settings, &pal);
        self.contents_sheet(&ctx, screen, &pal);
        if let Some(viewer) = &mut self.viewer {
            let open = match self.images.get(viewer.key()) {
                Some(ImageSlot::Ready(texture)) => viewer.ui(&ctx, screen, texture, &pal, now),
                _ => false,
            };
            if !open {
                self.viewer = None;
            }
        }

        // Hide the pointer while reading undisturbed.
        if !self.chrome
            && self.sheet == Sheet::None
            && self.viewer.is_none()
            && now - self.pointer_moved > 2.5
        {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        } else if !self.chrome {
            ctx.request_repaint_after_secs(2.6);
        }

        // Quietly prepare the neighbouring chapters so turning into them is instant.
        if self.slide.is_none() && self.drag.is_none() {
            for chapter in [self.chapter + 1, self.chapter.wrapping_sub(1)] {
                if chapter < self.book.chapters.len() && !self.layouts.contains_key(&chapter) {
                    self.ensure(&ctx, hyph, chapter);
                    ctx.request_repaint();
                    break;
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // Layout bookkeeping

    fn update_params(&mut self, target: Params, now: f64, ctx: &egui::Context) {
        match self.params {
            Some(current) if current == target => self.pending = None,
            Some(current) if Params { width: target.width, height: target.height, ..current } == target => {
                // Only the window size changed: wait for it to settle before re-setting the book.
                match self.pending {
                    Some((pending, since)) if pending == target => {
                        if now - since > 0.15 {
                            self.apply_params(target);
                        } else {
                            ctx.request_repaint_after_secs(0.05);
                        }
                    }
                    _ => {
                        self.pending = Some((target, now));
                        ctx.request_repaint_after_secs(0.16);
                    }
                }
            }
            _ => self.apply_params(target),
        }
    }

    fn apply_params(&mut self, target: Params) {
        let font_changed = self.params.is_none_or(|p| p.typeface != target.typeface || p.size != target.size);
        if font_changed {
            self.shaper = Shaper::new(target.typeface);
        }
        self.layouts.clear();
        self.params = Some(target);
        self.pending = None;
    }

    fn ensure(&mut self, ctx: &egui::Context, hyph: Option<&Standard>, chapter: usize) {
        let Some(params) = self.params else { return };
        if self.layouts.contains_key(&chapter) {
            return;
        }
        let book = &self.book;
        let shaper = &mut self.shaper;
        let started = std::time::Instant::now();
        let layout =
            ctx.fonts_mut(|fonts| layout::layout_chapter(fonts, shaper, hyph, book, chapter, params));
        if std::env::var_os("RUSTY_READER_TIMING").is_some() {
            eprintln!("chapter {chapter}: {} pages in {:?}", layout.pages.len(), started.elapsed());
        }
        for element in &layout.elements {
            if let Content::Image { key, .. } = &element.content {
                request_image(&mut self.images, &self.book, ctx, key);
            }
        }
        self.layouts.insert(chapter, layout);
    }

    fn neighbour(
        &mut self,
        ctx: &egui::Context,
        hyph: Option<&Standard>,
        dir: i32,
    ) -> Option<(usize, usize)> {
        let layout = &self.layouts[&self.chapter];
        let page = layout.page_at(self.offset);
        if dir > 0 {
            if page + 1 < layout.pages.len() {
                Some((self.chapter, page + 1))
            } else if self.chapter + 1 < self.book.chapters.len() {
                self.ensure(ctx, hyph, self.chapter + 1);
                Some((self.chapter + 1, 0))
            } else {
                None
            }
        } else if page > 0 {
            Some((self.chapter, page - 1))
        } else if self.chapter > 0 {
            self.ensure(ctx, hyph, self.chapter - 1);
            Some((self.chapter - 1, self.layouts[&(self.chapter - 1)].pages.len() - 1))
        } else {
            None
        }
    }

    fn go(&mut self, ctx: &egui::Context, hyph: Option<&Standard>, dir: i32) -> bool {
        let Some((chapter, page)) = self.neighbour(ctx, hyph, dir) else { return false };
        self.chapter = chapter;
        self.offset = self.layouts[&chapter].pages[page].offset;
        self.moved = true;
        true
    }

    fn jump(&mut self, chapter: usize, offset: usize) {
        self.chapter = chapter.min(self.book.chapters.len() - 1);
        self.offset = offset;
        self.slide = None;
        self.moved = true;
    }

    // -----------------------------------------------------------------------------------
    // Motion and input

    fn visual_offset(&self, now: f64) -> f32 {
        if let Some(drag) = self.drag {
            drag
        } else if let Some(slide) = &self.slide {
            let t = ((now - slide.start) as f32 / slide.duration).clamp(0.0, 1.0);
            slide.base * (1.0 - ease_out(t))
        } else {
            0.0
        }
    }

    fn turn(
        &mut self,
        ctx: &egui::Context,
        hyph: Option<&Standard>,
        dir: i32,
        width: f32,
        animate: bool,
        now: f64,
    ) {
        let current = self.visual_offset(now);
        if self.go(ctx, hyph, dir) {
            self.slide =
                animate.then_some(Slide { base: current + dir as f32 * width, start: now, duration: 0.34 });
        } else if animate {
            // A gentle nudge at either end of the book.
            self.slide = Some(Slide { base: -dir as f32 * 28.0, start: now, duration: 0.3 });
        }
    }

    fn handle_input(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        hyph: Option<&Standard>,
        settings: &mut Settings,
        screen: Rect,
        now: f64,
    ) {
        let width = screen.width();
        let animate = settings.animate;
        let surface = ui.interact(screen, Id::unique("page-surface"), Sense::click_and_drag());

        if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_pressed()) {
            self.pointer_moved = now;
        }

        if self.sheet != Sheet::None {
            if surface.clicked() || surface.drag_started() {
                self.sheet = Sheet::None;
            }
        } else {
            if surface.drag_started() {
                self.drag = Some(self.visual_offset(now));
                self.slide = None;
            }
            if self.drag.is_some() && surface.dragged() {
                let dx = surface.drag_delta().x;
                let current = self.drag.unwrap_or(0.0);
                let heading = current + dx;
                let free = if heading < 0.0 {
                    self.neighbour(ctx, hyph, 1).is_some()
                } else {
                    self.neighbour(ctx, hyph, -1).is_some()
                };
                self.drag = Some(current + if free { dx } else { dx * 0.3 });
            }
            if surface.drag_stopped() {
                let o = self.drag.take().unwrap_or(0.0);
                let v = ctx.input(|i| i.pointer.velocity().x);
                let dir = if o < 0.0 && (o < -width * 0.15 || v < -350.0) {
                    1
                } else if o > 0.0 && (o > width * 0.15 || v > 350.0) {
                    -1
                } else {
                    0
                };
                if dir != 0 && self.go(ctx, hyph, dir) {
                    let base = o + dir as f32 * width;
                    let duration = 0.14 + 0.22 * (base.abs() / width);
                    self.slide = animate.then_some(Slide { base, start: now, duration });
                } else {
                    self.slide = Some(Slide { base: o, start: now, duration: 0.28 });
                }
            }
            if surface.clicked() {
                let pos = surface.interact_pointer_pos();
                let x = pos.map_or(0.5, |p| (p.x - screen.left()) / width);
                // Taps on an illustration (away from the page-turning edges) take a closer look.
                let image = pos.and_then(|p| self.page_images.iter().find(|(r, _)| r.contains(p)));
                if let (false, Some((rect, key))) = (self.chrome || !(0.28..=0.72).contains(&x), image) {
                    self.viewer = Some(Viewer::new(key.clone(), *rect, now));
                } else if self.chrome {
                    self.chrome = false;
                } else if pos.is_some() {
                    if x < 0.28 {
                        self.turn(ctx, hyph, -1, width, animate, now);
                    } else if x > 0.72 {
                        self.turn(ctx, hyph, 1, width, animate, now);
                    } else {
                        self.chrome = true;
                    }
                }
            }
        }

        let events = ctx.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::MouseWheel { unit, delta, phase, .. } if self.sheet == Sheet::None => {
                    let d = if delta.x.abs() > delta.y.abs() { delta.x } else { delta.y };
                    match unit {
                        MouseWheelUnit::Point => {
                            // Trackpads: one page per gesture.
                            if phase == TouchPhase::Start || now - self.wheel_last > 0.3 {
                                self.wheel_acc = 0.0;
                                self.wheel_locked = false;
                            }
                            self.wheel_last = now;
                            if !self.wheel_locked {
                                self.wheel_acc += d;
                                if self.wheel_acc.abs() > 70.0 {
                                    let dir = if self.wheel_acc < 0.0 { 1 } else { -1 };
                                    self.turn(ctx, hyph, dir, width, animate, now);
                                    self.wheel_locked = true;
                                }
                            }
                            if matches!(phase, TouchPhase::End | TouchPhase::Cancel) {
                                self.wheel_locked = false;
                                self.wheel_acc = 0.0;
                            }
                        }
                        _ => {
                            if d != 0.0 && now - self.wheel_last > 0.12 {
                                self.turn(ctx, hyph, if d < 0.0 { 1 } else { -1 }, width, animate, now);
                            }
                            self.wheel_last = now;
                        }
                    }
                }
                Event::Key { key, pressed: true, modifiers, .. } => {
                    if modifiers.ctrl || modifiers.command || modifiers.alt {
                        continue;
                    }
                    match key {
                        Key::ArrowRight | Key::ArrowDown | Key::PageDown | Key::J | Key::L
                            if self.sheet == Sheet::None =>
                        {
                            self.turn(ctx, hyph, 1, width, animate, now)
                        }
                        Key::Space if self.sheet == Sheet::None => {
                            let dir = if modifiers.shift { -1 } else { 1 };
                            self.turn(ctx, hyph, dir, width, animate, now)
                        }
                        Key::ArrowLeft | Key::ArrowUp | Key::PageUp | Key::Backspace | Key::K | Key::H
                            if self.sheet == Sheet::None =>
                        {
                            self.turn(ctx, hyph, -1, width, animate, now)
                        }
                        Key::Escape => {
                            if self.sheet != Sheet::None {
                                self.sheet = Sheet::None;
                            } else {
                                self.chrome = !self.chrome;
                            }
                        }
                        // Android's back gesture: peel away one layer at a time, then leave the book.
                        Key::BrowserBack => {
                            if self.sheet != Sheet::None {
                                self.sheet = Sheet::None;
                            } else if self.chrome {
                                self.chrome = false;
                            } else {
                                self.close = true;
                            }
                        }
                        Key::Enter | Key::M => self.chrome = !self.chrome,
                        Key::T => self.toggle_sheet(Sheet::Contents),
                        Key::S => self.toggle_sheet(Sheet::Settings),
                        Key::F11 | Key::F => toggle_fullscreen(ctx),
                        Key::Equals | Key::Plus => {
                            settings.size = (settings.size + 1.0).min(Settings::MAX_SIZE)
                        }
                        Key::Minus => settings.size = (settings.size - 1.0).max(Settings::MIN_SIZE),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn toggle_sheet(&mut self, sheet: Sheet) {
        if self.sheet == sheet {
            self.sheet = Sheet::None;
        } else {
            self.sheet = sheet;
            self.chrome = true;
            self.scroll_toc = sheet == Sheet::Contents;
        }
    }

    // -----------------------------------------------------------------------------------
    // Painting

    #[allow(clippy::too_many_arguments)]
    fn paint_page(
        &mut self,
        painter: &Painter,
        ctx: &egui::Context,
        chapter: usize,
        page: usize,
        origin: Pos2,
        pal: &Palette,
        alpha: f32,
        record: bool,
    ) {
        let Some(layout) = self.layouts.get(&chapter) else { return };
        let Some(page) = layout.pages.get(page) else { return };
        let text = pal.text.gamma_multiply(alpha);
        for element in &layout.elements[page.start..page.end] {
            let y = origin.y + element.y - page.top + page.shift;
            match &element.content {
                Content::Line(frags) => {
                    for frag in frags {
                        let pos = widgets::snap(ctx, pos2(origin.x + frag.pos.x, y + frag.pos.y));
                        painter.galley(pos, painter.layout_job(frag.job.clone()), text);
                    }
                }
                Content::Image { key, x, size } => {
                    request_image(&mut self.images, &self.book, ctx, key);
                    let slot = self.images.get_mut(key).expect("requested");
                    if let ImageSlot::Loading(rx) = slot {
                        match rx.try_recv() {
                            Ok(Some(image)) => {
                                *slot = ImageSlot::Ready(ctx.load_texture(key, image, TextureOptions::LINEAR))
                            }
                            Ok(None) | Err(TryRecvError::Disconnected) => *slot = ImageSlot::Failed,
                            Err(TryRecvError::Empty) => {}
                        }
                    }
                    let ImageSlot::Ready(texture) = slot else { continue };
                    let rect = Rect::from_min_size(widgets::snap(ctx, pos2(origin.x + x, y)), *size);
                    if record {
                        self.page_images.push((rect, key.clone()));
                    }
                    let tint = if pal.dark { Color32::from_gray(205) } else { Color32::WHITE };
                    painter.image(
                        texture.id(),
                        rect,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        tint.gamma_multiply(alpha),
                    );
                }
                Content::Ornament => {
                    let center = pos2(origin.x + layout.params.width * 0.5, y + element.height * 0.5);
                    let gap = layout.params.size * 1.1;
                    let r = (layout.params.size * 0.085).max(1.4);
                    for dx in [-gap, 0.0, gap] {
                        painter.circle_filled(center + vec2(dx, 0.0), r, pal.muted.gamma_multiply(alpha));
                    }
                }
            }
        }
    }

    /// Pages remaining before the next table-of-contents entry (or the chapter's end).
    /// `None` just after a jump, until the new chapter has been laid out.
    fn pages_left_in_section(&self) -> Option<usize> {
        let layout = self.layouts.get(&self.chapter)?;
        let page = layout.page_at(self.offset);
        let here = (self.chapter, layout.pages[page].offset);
        let next = self.book.toc.iter().find(|e| (e.chapter, e.offset) > here && e.chapter == self.chapter);
        let last = match next {
            Some(entry) => {
                let starts = layout.page_at(entry.offset);
                if layout.pages[starts].offset == entry.offset { starts.saturating_sub(1) } else { starts }
            }
            None => layout.pages.len() - 1,
        };
        Some(last.saturating_sub(page))
    }

    fn section_title(&self) -> Option<&str> {
        self.book.section_at(self.chapter, self.offset).map(|i| self.book.toc[i].label.as_str())
    }

    fn paint_footer(&self, ui: &Ui, screen: Rect, column: Rect, pal: &Palette, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let y = column.bottom() + (top_margin(screen) + 10.0) * 0.5;
        let color = pal.muted.gamma_multiply(0.75 * alpha);
        let font = fonts::ui(11.5);
        let pct = format!("{}%", (self.progress() * 100.0).round());
        let pct_galley = widgets::tracked(ui, &pct, font.clone(), color, 0.4);
        let painter = ui.painter();
        painter.galley(
            pos2(column.right() - pct_galley.size().x, y - pct_galley.size().y * 0.5),
            pct_galley.clone(),
            color,
        );
        if let Some(title) = self.section_title() {
            let galley = widgets::elided(ui, title, font, color, column.width() - pct_galley.size().x - 32.0);
            painter.galley(pos2(column.left(), y - galley.size().y * 0.5), galley, color);
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context, screen: Rect, pal: &Palette, t: f32) {
        let bar = Rect::from_min_size(screen.min - vec2(0.0, (1.0 - t) * 12.0), vec2(screen.width(), 56.0));
        egui::Area::new(Id::unique("top-bar"))
            .order(Order::Foreground)
            .fixed_pos(bar.min)
            .interactable(t > 0.6)
            .show(ctx, |ui| {
                ui.set_opacity(t);
                ui.set_min_size(bar.size());
                ui.interact(bar, Id::unique("top-bar-bg"), Sense::click());
                ui.painter().rect_filled(bar, 0.0, pal.bg);
                widgets::hairline(ui, bar.bottom(), bar.x_range(), pal.faint);

                let button = |x: f32| Rect::from_center_size(pos2(x, bar.center().y), vec2(40.0, 40.0));
                if widgets::icon_button(ui, button(bar.left() + 30.0), Id::unique("back"), Icon::Back, pal)
                    .clicked()
                {
                    self.close = true;
                }
                // Phones are always fullscreen, so there the button would do nothing.
                let fullscreen = !cfg!(target_os = "android");
                let mut x = bar.right() - 30.0;
                if fullscreen {
                    if widgets::icon_button(ui, button(x), Id::unique("fullscreen"), Icon::Fullscreen, pal)
                        .clicked()
                    {
                        toggle_fullscreen(ctx);
                    }
                    x -= 44.0;
                }
                if widgets::icon_button(ui, button(x), Id::unique("type"), Icon::Type, pal).clicked() {
                    self.toggle_sheet(Sheet::Settings);
                }
                x -= 44.0;
                if widgets::icon_button(ui, button(x), Id::unique("contents"), Icon::Contents, pal).clicked()
                {
                    self.toggle_sheet(Sheet::Contents);
                }

                let title_width = (bar.width() - 2.0 * 150.0).max(40.0);
                let galley =
                    widgets::elided(ui, &self.book.title, fonts::serif(15.0, true), pal.text, title_width);
                ui.painter().galley(bar.center() - galley.size() * 0.5, galley, pal.text);
            });
    }

    fn bottom_bar(&mut self, ctx: &egui::Context, screen: Rect, pal: &Palette, t: f32) {
        let height = 100.0;
        let bar = Rect::from_min_size(
            pos2(screen.left(), screen.bottom() - height + (1.0 - t) * 12.0),
            vec2(screen.width(), height),
        );
        egui::Area::new(Id::unique("bottom-bar"))
            .order(Order::Foreground)
            .fixed_pos(bar.min)
            .interactable(t > 0.6)
            .show(ctx, |ui| {
                ui.set_opacity(t);
                ui.set_min_size(bar.size());
                ui.interact(bar, Id::unique("bottom-bar-bg"), Sense::click());
                ui.painter().rect_filled(bar, 0.0, pal.bg);
                widgets::hairline(ui, bar.top(), bar.x_range(), pal.faint);

                let track_w = (screen.width() - 64.0).min(560.0);
                let x0 = bar.center().x - track_w * 0.5;
                let track_y = bar.top() + 54.0;
                let track = Rect::from_min_size(pos2(x0, track_y - 14.0), vec2(track_w, 28.0));
                let response = ui
                    .interact(track, Id::unique("scrubber"), Sense::click_and_drag())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if let Some(pos) = response.interact_pointer_pos()
                    && (response.dragged() || response.clicked() || response.drag_started())
                {
                    self.scrub = Some(((pos.x - x0) / track_w).clamp(0.0, 1.0));
                }
                if (response.drag_stopped() || response.clicked())
                    && let Some(fraction) = self.scrub.take()
                {
                    let (chapter, offset) = self.book.locate(fraction);
                    self.jump(chapter, offset);
                }

                let fraction = self.scrub.unwrap_or_else(|| self.progress());
                let painter = ui.painter();
                painter.line_segment(
                    [pos2(x0, track_y), pos2(x0 + track_w, track_y)],
                    Stroke::new(2.0, pal.faint),
                );
                if self.book.chapters.len() <= 80 {
                    for &start in &self.book.chapter_starts[1..] {
                        let x = x0 + track_w * start as f32 / self.book.total_chars as f32;
                        painter.line_segment(
                            [pos2(x, track_y - 3.0), pos2(x, track_y + 3.0)],
                            Stroke::new(1.0, pal.faint),
                        );
                    }
                }
                let knob_x = x0 + track_w * fraction;
                painter.line_segment([pos2(x0, track_y), pos2(knob_x, track_y)], Stroke::new(2.0, pal.muted));
                let knob_r = if response.hovered() || self.scrub.is_some() { 7.0 } else { 5.5 };
                painter.circle_filled(pos2(knob_x, track_y), knob_r, pal.text);

                // Where you are (or where you'd go).
                let (label, pct) = match self.scrub {
                    Some(f) => {
                        let (chapter, offset) = self.book.locate(f);
                        let title =
                            self.book.section_at(chapter, offset).map(|i| self.book.toc[i].label.clone());
                        (title, f)
                    }
                    None => (self.section_title().map(str::to_owned), self.progress()),
                };
                if let Some(label) = label {
                    let galley = widgets::elided(ui, &label, fonts::serif(15.0, true), pal.text, track_w);
                    ui.painter().galley(
                        pos2(bar.center().x - galley.size().x * 0.5, bar.top() + 18.0),
                        galley,
                        pal.text,
                    );
                }
                let small = fonts::ui(12.0);
                let pages_text = match self.pages_left_in_section() {
                    Some(0) => "Last page in chapter".to_owned(),
                    Some(1) => "1 page left in chapter".to_owned(),
                    Some(n) => format!("{n} pages left in chapter"),
                    None => String::new(),
                };
                let painter = ui.painter();
                painter.text(
                    pos2(x0, track_y + 22.0),
                    Align2::LEFT_CENTER,
                    pages_text,
                    small.clone(),
                    pal.muted,
                );
                painter.text(
                    pos2(x0 + track_w, track_y + 22.0),
                    Align2::RIGHT_CENTER,
                    format!("{}%", (pct * 100.0).round()),
                    small,
                    pal.muted,
                );
            });
    }

    fn settings_sheet(&mut self, ctx: &egui::Context, screen: Rect, settings: &mut Settings, pal: &Palette) {
        let t = ctx.animate_bool_with_time(Id::unique("settings-sheet"), self.sheet == Sheet::Settings, 0.22);
        if t <= 0.0 {
            return;
        }
        let width = (screen.width() - 24.0).min(420.0);
        egui::Area::new(Id::unique("settings-area"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -14.0 + (1.0 - ease_out(t)) * 40.0))
            .interactable(t > 0.6)
            .show(ctx, |ui| {
                ui.set_opacity(t);
                sheet_frame(pal).show(ui, |ui| {
                    ui.set_width(width - 44.0);
                    if screen.height() >= 780.0 {
                        settings_contents(ui, settings, pal);
                    } else {
                        let height = screen.height() - 90.0;
                        egui::ScrollArea::vertical()
                            .max_height(height)
                            .min_scrolled_height(height)
                            .show(ui, |ui| settings_contents(ui, settings, pal));
                    }
                });
            });
    }

    fn contents_sheet(&mut self, ctx: &egui::Context, screen: Rect, pal: &Palette) {
        let t = ctx.animate_bool_with_time(Id::unique("contents-sheet"), self.sheet == Sheet::Contents, 0.24);
        if t <= 0.0 {
            return;
        }
        let e = ease_out(t);
        let scrim = if pal.dark { 120.0 } else { 46.0 };
        ctx.layer_painter(LayerId::new(Order::Middle, Id::unique("scrim"))).rect_filled(
            screen,
            0.0,
            Color32::from_black_alpha((scrim * e) as u8),
        );

        let width = (screen.width() * 0.86).min(380.0);
        let current = self.book.section_at(self.chapter, self.offset);
        let mut target = None;
        egui::Area::new(Id::unique("contents-area"))
            .order(Order::Foreground)
            .fixed_pos(pos2(screen.left() - (1.0 - e) * (width + 20.0), screen.top()))
            .interactable(t > 0.6)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.surface)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 0],
                        blur: 30,
                        spread: 0,
                        color: Color32::from_black_alpha(if pal.dark { 120 } else { 40 }),
                    })
                    .inner_margin(egui::Margin { left: 10, right: 10, top: 18, bottom: 10 })
                    .show(ui, |ui| {
                        ui.set_width(width - 20.0);
                        ui.set_height(screen.height() - 28.0);

                        let (head, _) =
                            ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
                        ui.painter().text(
                            pos2(head.left() + 12.0, head.center().y),
                            Align2::LEFT_CENTER,
                            "Contents",
                            fonts::serif(24.0, false),
                            pal.text,
                        );
                        let close = Rect::from_center_size(
                            pos2(head.right() - 20.0, head.center().y),
                            vec2(36.0, 36.0),
                        );
                        if widgets::icon_button(ui, close, Id::unique("close-contents"), Icon::Close, pal)
                            .clicked()
                        {
                            self.sheet = Sheet::None;
                        }
                        let byline = match &self.book.author {
                            Some(author) => format!("{} · {}", self.book.title, author),
                            None => self.book.title.clone(),
                        };
                        let galley = widgets::elided(
                            ui,
                            &byline,
                            fonts::serif(14.0, true),
                            pal.muted,
                            ui.available_width() - 24.0,
                        );
                        let (line, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), galley.size().y + 14.0),
                            Sense::hover(),
                        );
                        ui.painter().galley(pos2(line.left() + 12.0, line.top()), galley, pal.muted);
                        widgets::hairline(
                            ui,
                            line.bottom(),
                            line.left() + 12.0..=line.right() - 12.0,
                            pal.faint,
                        );
                        ui.add_space(6.0);

                        let area = egui::ScrollArea::vertical().auto_shrink(false);
                        widgets::flick_scroll(ui, Id::unique("contents-scroll"), area, |ui| {
                            for (i, entry) in self.book.toc.iter().enumerate() {
                                let is_current = current == Some(i);
                                let indent = 12.0 + entry.depth as f32 * 16.0;
                                let font = fonts::serif(if entry.depth == 0 { 16.0 } else { 15.0 }, false);
                                let color = if is_current {
                                    pal.accent
                                } else if entry.depth == 0 {
                                    pal.text
                                } else {
                                    lerp_color(pal.text, pal.muted, 0.4)
                                };
                                let wrap = ui.available_width() - indent - 16.0;
                                let galley =
                                    ui.fonts_mut(|f| f.layout(entry.label.clone(), font, color, wrap));
                                let (rect, response) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), galley.size().y + 18.0),
                                    Sense::click(),
                                );
                                let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                                if widgets::hovering(ui, &response) {
                                    ui.painter().rect_filled(
                                        rect,
                                        CornerRadius::same(8),
                                        pal.faint.gamma_multiply(0.6),
                                    );
                                }
                                if is_current {
                                    ui.painter().circle_filled(
                                        pos2(rect.left() + indent - 7.0, rect.center().y),
                                        2.5,
                                        pal.accent,
                                    );
                                    if self.scroll_toc {
                                        response.scroll_to_me(Some(Align::Center));
                                    }
                                }
                                ui.painter().galley(
                                    pos2(rect.left() + indent, rect.top() + 9.0),
                                    galley,
                                    color,
                                );
                                if response.clicked() {
                                    target = Some((entry.chapter, entry.offset));
                                }
                            }
                        });
                    });
            });
        self.scroll_toc = false;
        if let Some((chapter, offset)) = target {
            self.jump(chapter, offset);
            self.sheet = Sheet::None;
            self.chrome = false;
        }
    }
}

fn request_image(images: &mut HashMap<String, ImageSlot>, book: &Book, ctx: &egui::Context, key: &str) {
    if images.contains_key(key) {
        return;
    }
    let Some(image) = book.images.get(key).cloned() else { return };
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(image.decode());
        ctx.request_repaint();
    });
    images.insert(key.to_owned(), ImageSlot::Loading(rx));
}

fn sheet_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.faint))
        .corner_radius(CornerRadius::same(18))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 36,
            spread: 0,
            color: Color32::from_black_alpha(if pal.dark { 140 } else { 36 }),
        })
        .inner_margin(egui::Margin::same(22))
}

fn settings_contents(ui: &mut Ui, settings: &mut Settings, pal: &Palette) {
    widgets::section_label(ui, "Theme", pal);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 74.0), Sense::hover());
    let cell = row.width() / ThemeKind::ALL.len() as f32;
    for (i, theme) in ThemeKind::ALL.into_iter().enumerate() {
        let tp = theme.palette();
        let center = pos2(row.left() + cell * (i as f32 + 0.5), row.top() + 26.0);
        let hit = Rect::from_center_size(center + vec2(0.0, 10.0), vec2(cell, 64.0));
        let response = ui
            .interact(hit, Id::unique(("theme", i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            settings.theme = theme;
        }
        let painter = ui.painter();
        if settings.theme == theme {
            painter.circle_stroke(center, 24.0, Stroke::new(1.5, pal.accent));
        }
        painter.circle_filled(center, 20.0, tp.bg);
        painter.circle_stroke(center, 20.0, Stroke::new(1.0, pal.faint));
        painter.text(center, Align2::CENTER_CENTER, "Aa", fonts::serif(15.0, false), tp.text);
        let label_color = if settings.theme == theme { pal.text } else { pal.muted };
        painter.text(
            center + vec2(0.0, 36.0),
            Align2::CENTER_CENTER,
            theme.name(),
            fonts::ui(11.5),
            label_color,
        );
    }

    widgets::section_label(ui, "Typeface", pal);
    let names: Vec<&str> = Typeface::ALL.iter().map(|t| t.name()).collect();
    let faces: Vec<_> = Typeface::ALL.iter().map(|t| t.font(15.0 * t.size_scale(), false)).collect();
    let selected = Typeface::ALL.iter().position(|&t| t == settings.typeface).unwrap_or(0);
    if let Some(i) = widgets::segmented(ui, Id::unique("typeface"), &names, &faces, selected, pal) {
        settings.typeface = Typeface::ALL[i];
    }

    widgets::section_label(ui, "Size", pal);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
    ui.painter().rect_filled(row, CornerRadius::same(10), pal.faint.gamma_multiply(0.6));
    let third = row.width() / 3.0;
    let smaller = Rect::from_min_size(row.min, vec2(third, row.height()));
    let larger = Rect::from_min_size(pos2(row.right() - third, row.top()), vec2(third, row.height()));
    for (rect, delta, size) in [(smaller, -1.0, 13.0), (larger, 1.0, 19.0)] {
        let response = ui
            .interact(rect, Id::unique(("size", delta as i32)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            settings.size = (settings.size + delta).clamp(Settings::MIN_SIZE, Settings::MAX_SIZE);
        }
        let color = if widgets::hovering(ui, &response) { pal.text } else { pal.muted };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "A", fonts::serif(size, false), color);
    }
    ui.painter().text(
        row.center(),
        Align2::CENTER_CENTER,
        format!("{}", settings.size as i32),
        fonts::ui(14.0),
        pal.text,
    );

    widgets::section_label(ui, "Line spacing", pal);
    let names: Vec<&str> = Spacing::ALL.iter().map(|s| s.name()).collect();
    let selected = Spacing::ALL.iter().position(|&s| s == settings.spacing).unwrap_or(1);
    if let Some(i) = widgets::segmented(ui, Id::unique("spacing"), &names, &[], selected, pal) {
        settings.spacing = Spacing::ALL[i];
    }

    widgets::section_label(ui, "Line length", pal);
    let names: Vec<&str> = Measure::ALL.iter().map(|m| m.name()).collect();
    let selected = Measure::ALL.iter().position(|&m| m == settings.measure).unwrap_or(1);
    if let Some(i) = widgets::segmented(ui, Id::unique("measure"), &names, &[], selected, pal) {
        settings.measure = Measure::ALL[i];
    }

    ui.add_space(12.0);
    widgets::toggle_row(ui, Id::unique("justify"), "Justify text", &mut settings.justify, pal);
    widgets::toggle_row(ui, Id::unique("hyphenate"), "Hyphenation", &mut settings.hyphenate, pal);
    widgets::toggle_row(ui, Id::unique("indent"), "Indent paragraphs", &mut settings.indent, pal);
    widgets::toggle_row(
        ui,
        Id::unique("progress"),
        "Show progress while reading",
        &mut settings.show_progress,
        pal,
    );
    widgets::toggle_row(ui, Id::unique("animate"), "Animate page turns", &mut settings.animate, pal);
}

fn top_margin(screen: Rect) -> f32 {
    (screen.height() * 0.075).clamp(34.0, 80.0).round()
}

/// The tallest a page may be, relative to its width. Roughly a printed book's text block, so a
/// tall portrait screen shows a page rather than a scroll.
const PAGE_RATIO: f32 = 1.6;

/// Where the text column starts: below the top margin, or lower so that a page shorter than
/// the screen sits in the middle of it.
fn column_top(screen: Rect, params: &Params) -> f32 {
    ((screen.height() - params.height - 10.0) * 0.5).max(top_margin(screen)).round()
}

fn target_params(screen: Rect, s: &Settings) -> Params {
    let side = (screen.width() * 0.07).clamp(20.0, 120.0);
    let measure = s.size * s.measure.ems();
    let width = measure.min(screen.width() - 2.0 * side).max(160.0).floor();
    let top = top_margin(screen);
    let bottom = top + 10.0;
    // On a phone the screen already is the page; only shape it when the line length, not the
    // screen, decided the width.
    let max_height = if measure < screen.width() - 2.0 * side { width * PAGE_RATIO } else { f32::INFINITY };
    let height = (screen.height() - top - bottom).min(max_height).max(120.0).floor();
    Params {
        width,
        height,
        typeface: s.typeface,
        size: s.size,
        spacing: s.spacing.factor(),
        justify: s.justify,
        hyphenate: s.hyphenate,
        indent: s.indent,
    }
}

fn toggle_fullscreen(ctx: &egui::Context) {
    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{self, Key, Modifiers};
use hyphenation::{Language, Load, Standard};

use crate::book::Book;
use crate::fonts;
use crate::library::{self, Action, Covers};
use crate::meter::Meter;
use crate::platform;
use crate::reader::Reader;
use crate::store::{Admitted, Store};
use crate::theme::ThemeKind;

enum Screen {
    Library,
    Reading(Box<Reader>),
}

struct Loading {
    path: PathBuf,
    rx: Receiver<Result<Book, String>>,
}

pub struct App {
    store: Store,
    screen: Screen,
    loading: Option<Loading>,
    message: Option<(String, bool, f64)>,
    hyphenator: Option<Standard>,
    covers: Covers,
    applied_theme: Option<ThemeKind>,
    dirty_since: Option<f64>,
    meter: Meter,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> App {
        fonts::install(&cc.egui_ctx);
        if cfg!(target_os = "android") {
            // Scroll like an Android list rather than an iPhone one: stop at the ends instead of
            // rubber-banding, and coast a little less after a flick.
            cc.egui_ctx.all_styles_mut(|style| {
                style.spacing.scroll.kinetic.rubber_band = false;
                style.spacing.scroll.kinetic.decay_time = 0.35;
            });
        }
        let mut app = App {
            store: Store::load(),
            screen: Screen::Library,
            loading: None,
            message: None,
            hyphenator: Standard::from_embedded(Language::EnglishUS).ok(),
            covers: Covers::new(&cc.egui_ctx),
            applied_theme: None,
            dirty_since: None,
            meter: Meter::default(),
        };
        if app.store.fill_keys() {
            app.store.save();
        }
        app.shelve_found();
        if let Some(path) = initial {
            app.open(&cc.egui_ctx, path);
        }
        app
    }

    /// Pick up books copied into the books folder (on platforms that have one).
    fn shelve_found(&mut self) {
        let found = platform::found_books();
        let added = found.iter().filter(|p| matches!(self.store.admit(p, false), Admitted::Added(_))).count();
        if added > 0 {
            self.store.save();
        }
    }

    /// Books arriving together: a single one opens straight away (at your place, if it is
    /// already on the shelf), several go onto the shelf.
    fn import(&mut self, ctx: &egui::Context, books: Vec<PathBuf>, now: f64) {
        let books: Vec<PathBuf> = books.into_iter().filter(|p| platform::is_epub(p)).collect();
        if books.is_empty() {
            return;
        }
        let admitted: Vec<Admitted> = books.iter().map(|p| self.store.admit(p, true)).collect();
        self.store.save();
        if let [Admitted::Added(path) | Admitted::Already(path)] = &admitted[..] {
            self.open(ctx, path.clone());
            return;
        }
        let added = admitted.iter().filter(|a| matches!(a, Admitted::Added(_))).count();
        let already = admitted.len() - added;
        let books = |n: usize| if n == 1 { "1 book".to_owned() } else { format!("{n} books") };
        let message = match (added, already) {
            (_, 0) => format!("Added {} to your shelf", books(added)),
            (0, _) => "Those books are already on your shelf".to_owned(),
            (_, 1) => format!("Added {}; 1 was already on your shelf", books(added)),
            (_, _) => format!("Added {}; {already} were already on your shelf", books(added)),
        };
        self.message = Some((message, false, now));
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let (tx, rx) = channel();
        let repaint = ctx.clone();
        let load_path = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Book::open(&load_path));
            repaint.request_repaint();
        });
        self.loading = Some(Loading { path, rx });
        self.message = None;
    }

    fn pick(&mut self, ctx: &egui::Context, now: f64) {
        let books = platform::pick_books();
        self.import(ctx, books, now);
    }

    fn poll_loading(&mut self, ctx: &egui::Context, now: f64) {
        let Some(loading) = &self.loading else { return };
        let Ok(result) = loading.rx.try_recv() else { return };
        let path = self.loading.take().map(|l| l.path).unwrap_or_default();
        match result {
            Ok(book) => {
                let (chapter, offset) = self.store.recent(&path).map_or((0, 0), |r| (r.chapter, r.offset));
                self.store.touch(&path, &book.title, book.author.as_deref());
                self.store.save();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(book.title.clone()));
                self.screen = Screen::Reading(Box::new(Reader::new(Arc::new(book), path, chapter, offset)));
            }
            Err(error) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.message = Some((format!("{error}: {name}"), true, now));
            }
        }
    }

    fn remember(&mut self, reader: &Reader, now: f64) {
        if let Some(entry) = self.store.recent_mut(&reader.path) {
            entry.chapter = reader.chapter;
            entry.offset = reader.offset;
            entry.progress = reader.progress();
        }
        self.dirty_since.get_or_insert(now);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let started = std::time::Instant::now();
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);

        if self.applied_theme != Some(self.store.settings.theme) {
            self.store.settings.theme.palette().apply(&ctx);
            self.applied_theme = Some(self.store.settings.theme);
        }

        // Books dropped onto the window.
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        self.import(&ctx, dropped, now);
        let picked = platform::take_picked_books();
        self.import(&ctx, picked, now);
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.pick(&ctx, now);
        }
        self.poll_loading(&ctx, now);

        let settings_before = self.store.settings.clone();
        let mut back_to_library = false;
        match &mut self.screen {
            Screen::Reading(reader) => {
                reader.ui(ui, &mut self.store.settings, self.hyphenator.as_ref());
                if reader.moved {
                    reader.moved = false;
                    if let Some(entry) = self.store.recent_mut(&reader.path) {
                        entry.chapter = reader.chapter;
                        entry.offset = reader.offset;
                        entry.progress = reader.progress();
                    }
                    self.dirty_since.get_or_insert(now);
                }
                back_to_library = reader.close;
            }
            Screen::Library => {
                let status = match (&self.loading, &self.message) {
                    (Some(_), _) => Some(("Opening…".to_owned(), false)),
                    (None, Some((message, is_error, at))) if now - at < 8.0 => {
                        Some((message.clone(), *is_error))
                    }
                    _ => None,
                };
                let action = library::ui(
                    ui,
                    &self.store.recents,
                    self.store.settings.shelf,
                    &mut self.covers,
                    self.store.settings.theme,
                    status.as_ref().map(|(m, e)| (m.as_str(), *e)),
                );
                match action {
                    Some(Action::Open(path)) => self.open(&ctx, path),
                    Some(Action::Pick) => self.pick(&ctx, now),
                    Some(Action::Forget(i)) => {
                        self.store.forget(i);
                        self.store.save();
                    }
                    Some(Action::Theme(theme)) => self.store.settings.theme = theme,
                    Some(Action::View(view)) => self.store.settings.shelf = view,
                    None => {}
                }
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::BrowserBack)) {
                    platform::leave(&ctx);
                }
            }
        }

        if back_to_library {
            if let Screen::Reading(reader) = std::mem::replace(&mut self.screen, Screen::Library) {
                self.remember(&reader, now);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Title("Rusty Reader".into()));
        }

        if self.store.settings != settings_before {
            self.dirty_since.get_or_insert(now);
        }
        if let Some(since) = self.dirty_since {
            if now - since > 1.5 {
                self.store.save();
                self.dirty_since = None;
            } else {
                ctx.request_repaint_after_secs(1.6);
            }
        }
        self.meter.record(&ctx, started.elapsed());
    }

    /// The page colour reaches every edge, behind the camera cut-out and gesture bar too;
    /// only the content keeps clear of them.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(insets) = platform::safe_area(ctx) {
            raw_input.safe_area_insets = Some(insets);
        }
        // Phones rarely say goodbye: when the app loses focus it may never come back, so
        // write down the reading position now rather than when the debounce expires.
        let blurred = raw_input.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)));
        let refocused = raw_input.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(true)));
        if blurred && self.dirty_since.take().is_some() {
            self.store.save();
        }
        if refocused && matches!(self.screen, Screen::Library) {
            self.shelve_found();
        }
    }

    fn on_exit(&mut self) {
        if let Screen::Reading(reader) = &self.screen
            && let Some(entry) = self.store.recent_mut(&reader.path)
        {
            entry.chapter = reader.chapter;
            entry.offset = reader.offset;
            entry.progress = reader.progress();
        }
        self.store.save();
    }
}

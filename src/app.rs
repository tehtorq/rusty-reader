use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{self, Key, Modifiers};
use hyphenation::{Language, Load, Standard};

use crate::book::Book;
use crate::fonts;
use crate::library::{self, Action};
use crate::reader::Reader;
use crate::store::Store;
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
    applied_theme: Option<ThemeKind>,
    dirty_since: Option<f64>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> App {
        fonts::install(&cc.egui_ctx);
        let mut app = App {
            store: Store::load(),
            screen: Screen::Library,
            loading: None,
            message: None,
            hyphenator: Standard::from_embedded(Language::EnglishUS).ok(),
            applied_theme: None,
            dirty_since: None,
        };
        if let Some(path) = initial {
            app.open(&cc.egui_ctx, path);
        }
        app
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

    fn pick(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new().add_filter("EPUB books", &["epub"]).pick_file() {
            self.open(ctx, path);
        }
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
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);

        if self.applied_theme != Some(self.store.settings.theme) {
            self.store.settings.theme.palette().apply(&ctx);
            self.applied_theme = Some(self.store.settings.theme);
        }

        // Books dropped onto the window.
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if let Some(path) = dropped.into_iter().find(|p| is_epub(p)) {
            self.open(&ctx, path);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.pick(&ctx);
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
                    self.store.settings.theme,
                    status.as_ref().map(|(m, e)| (m.as_str(), *e)),
                );
                match action {
                    Some(Action::Open(path)) => self.open(&ctx, path),
                    Some(Action::Pick) => self.pick(&ctx),
                    Some(Action::Forget(i)) => {
                        self.store.recents.remove(i);
                        self.store.save();
                    }
                    Some(Action::Theme(theme)) => self.store.settings.theme = theme,
                    None => {}
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

fn is_epub(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub"))
}

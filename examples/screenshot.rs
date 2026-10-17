//! Render the reader headlessly, for checking typography without opening a window.
//!
//! cargo run --example screenshot -- BOOK.epub OUT_DIR [commands…]
//!
//! Commands: `ch:N` jump to chapter, `next`/`prev` turn a page, `key:NAME` press a key,
//! `theme:NAME`, `size:WxH`, `font:NAME`, `shot:NAME` save a PNG.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use egui_kittest::Harness;
use hyphenation::{Language, Load, Standard};
use rusty_reader::book::Book;
use rusty_reader::reader::Reader;
use rusty_reader::store::RecentBook;
use rusty_reader::theme::{Settings, ThemeKind, Typeface};
use rusty_reader::{fonts, library};

struct State {
    library: bool,
    reader: Reader,
    settings: Settings,
    hyph: Option<Standard>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("book path"));
    let out = PathBuf::from(args.next().expect("output dir"));
    std::fs::create_dir_all(&out).unwrap();

    let started = std::time::Instant::now();
    let book = Arc::new(Book::open(&path).expect("open book"));
    eprintln!(
        "{}: {} chapters, {} toc entries, {} images, {} chars, parsed in {:?}",
        book.title,
        book.chapters.len(),
        book.toc.len(),
        book.images.len(),
        book.total_chars,
        started.elapsed()
    );
    let state = State {
        library: false,
        reader: Reader::new(book.clone(), path.clone(), 0, 0),
        settings: Settings::default(),
        hyph: Standard::from_embedded(Language::EnglishUS).ok(),
    };
    let mut harness = Harness::builder()
        .with_size([780.0, 1000.0])
        .with_pixels_per_point(1.0)
        .with_context_setup(fonts::install)
        .wgpu()
        .build_ui_state(
            |ui, s: &mut State| {
                s.settings.theme.palette().apply(ui.ctx());
                if s.library {
                    let recents = demo_shelf();
                    library::ui(ui, &recents, s.settings.theme, None);
                } else {
                    s.reader.ui(ui, &mut s.settings, s.hyph.as_ref());
                }
            },
            state,
        );
    harness.run_steps(5);

    for cmd in args {
        let (name, value) = cmd.split_once(':').unwrap_or((cmd.as_str(), ""));
        match name {
            "ch" => {
                let ch: usize = value.parse().unwrap();
                let s = harness.state_mut();
                s.reader = Reader::new(book.clone(), path.clone(), ch, 0);
            }
            "next" => harness.key_press(egui::Key::ArrowRight),
            "prev" => harness.key_press(egui::Key::ArrowLeft),
            "key" => {
                let key = egui::Key::from_name(value).expect("key name");
                harness.key_press(key);
            }
            "theme" => {
                harness.state_mut().settings.theme =
                    ThemeKind::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(value)).unwrap()
            }
            "font" => {
                harness.state_mut().settings.typeface =
                    Typeface::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(value)).unwrap()
            }
            "fontsize" => harness.state_mut().settings.size = value.parse().unwrap(),
            "size" => {
                let (w, h) = value.split_once('x').unwrap();
                harness.set_size(egui::vec2(w.parse().unwrap(), h.parse().unwrap()));
            }
            "library" => harness.state_mut().library = true,
            "tap" => {
                let (x, y) = value.split_once(',').unwrap();
                let pos = egui::pos2(x.parse().unwrap(), y.parse().unwrap());
                harness.event(egui::Event::PointerMoved(pos));
                harness.run_steps(1);
                for pressed in [true, false] {
                    harness.event(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    });
                    harness.run_steps(1);
                }
            }
            "swipe" => {
                // swipe:FROM_X,TO_X at mid-height, over a few frames.
                let (a, b) = value.split_once(',').unwrap();
                let (a, b): (f32, f32) = (a.parse().unwrap(), b.parse().unwrap());
                let y = 500.0;
                harness.event(egui::Event::PointerMoved(egui::pos2(a, y)));
                harness.run_steps(1);
                harness.event(egui::Event::PointerButton {
                    pos: egui::pos2(a, y),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                harness.run_steps(1);
                for i in 1..=8 {
                    let x = a + (b - a) * i as f32 / 8.0;
                    harness.event(egui::Event::PointerMoved(egui::pos2(x, y)));
                    harness.run_steps(1);
                }
                harness.event(egui::Event::PointerButton {
                    pos: egui::pos2(b, y),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
                harness.run_steps(1);
            }
            "where" => {
                let r = &harness.state().reader;
                eprintln!("at chapter {} offset {} ({:.1}%)", r.chapter, r.offset, r.progress() * 100.0);
            }
            "shot" => {
                for _ in 0..12 {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    harness.run_steps(4);
                }
                let image = harness.render().expect("render");
                let file = out.join(format!("{value}.png"));
                image.save(&file).unwrap();
                eprintln!("saved {}", file.display());
                continue;
            }
            other => panic!("unknown command {other}"),
        }
        harness.run_steps(3);
    }
}

fn demo_shelf() -> Vec<RecentBook> {
    let book = |title: &str, author: &str, progress: f32| RecentBook {
        path: PathBuf::from(title),
        title: title.to_owned(),
        author: Some(author.to_owned()),
        chapter: 0,
        offset: 0,
        progress,
        opened: 0,
    };
    vec![
        book("Pride and Prejudice", "Jane Austen", 0.66),
        book("Frankenstein; Or, The Modern Prometheus", "Mary Wollstonecraft Shelley", 0.12),
        book("Alice's Adventures in Wonderland", "Lewis Carroll", 1.0),
    ]
}

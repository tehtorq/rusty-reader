//! Render the reader headlessly, for checking typography without opening a window.
//!
//! cargo run --example screenshot -- BOOK.epub OUT_DIR [commands…]
//!
//! Commands: `ch:N` jump to chapter, `next`/`prev` turn a page, `key:NAME` press a key,
//! `theme:NAME`, `size:WxH`, `font:NAME`, `library`, `view:list|covers`, `tap:X,Y`, `rclick:X,Y`, `flick:X,Y0,Y1`, `steps:N`, `wait:MS`, `shot:NAME` save a PNG.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use egui_kittest::Harness;
use hyphenation::{Language, Load, Standard};
use rusty_reader::book::Book;
use rusty_reader::reader::Reader;
use rusty_reader::store::RecentBook;
use rusty_reader::theme::{Settings, ShelfView, ThemeKind, Typeface};
use rusty_reader::{fonts, library};

struct State {
    library: bool,
    reader: Reader,
    settings: Settings,
    hyph: Option<Standard>,
    covers: Option<library::Covers>,
    shelf: Vec<RecentBook>,
    meter: rusty_reader::meter::Meter,
    narrator: rusty_reader::speech::Narrator,
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
        covers: None,
        shelf: demo_shelf(&path, &book.title, book.author.as_deref()),
        meter: Default::default(),
        narrator: Default::default(),
    };
    // RR_STEP_DT=0.0166 simulates real frame times (for gestures); the default jumps ahead.
    let step_dt = std::env::var("RR_STEP_DT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.25);
    let mut harness = Harness::builder()
        .with_step_dt(step_dt)
        .with_size([780.0, 1000.0])
        .with_pixels_per_point(1.0)
        .with_context_setup(fonts::install)
        .wgpu()
        .build_ui_state(
            |ui, s: &mut State| {
                s.settings.theme.palette().apply(ui.ctx());
                if s.library {
                    let covers = s.covers.get_or_insert_with(|| library::Covers::new(ui.ctx()));
                    library::ui(ui, &s.shelf, s.settings.shelf, covers, s.settings.theme, None);
                } else {
                    s.reader.ui(ui, &mut s.settings, s.hyph.as_ref(), &mut s.narrator);
                }
                s.meter.record(ui.ctx(), std::time::Duration::ZERO);
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
            "view" => {
                harness.state_mut().settings.shelf =
                    if value == "covers" { ShelfView::Covers } else { ShelfView::List };
                // Give the cover thread a moment.
                for _ in 0..20 {
                    harness.run_steps(1);
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
            "tap" | "rclick" => {
                let (x, y) = value.split_once(',').unwrap();
                let pos = egui::pos2(x.parse().unwrap(), y.parse().unwrap());
                let button =
                    if name == "tap" { egui::PointerButton::Primary } else { egui::PointerButton::Secondary };
                harness.event(egui::Event::PointerMoved(pos));
                harness.run_steps(1);
                for pressed in [true, false] {
                    harness.event(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: Default::default(),
                    });
                    harness.run_steps(1);
                }
            }
            "flick" => {
                // flick:X,FROM_Y,TO_Y — a touch drag over three frames, then let go.
                let v: Vec<f32> = value.split(',').map(|v| v.parse().unwrap()).collect();
                let (x, y0, y1) = (v[0], v[1], v[2]);
                let touch = |phase, y: f32| egui::Event::Touch {
                    device_id: egui::TouchDeviceId(1),
                    id: egui::TouchId(1),
                    phase,
                    pos: egui::pos2(x, y),
                    force: None,
                };
                let press = |pressed, y: f32| egui::Event::PointerButton {
                    pos: egui::pos2(x, y),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                };
                harness.event(touch(egui::TouchPhase::Start, y0));
                harness.event(egui::Event::PointerMoved(egui::pos2(x, y0)));
                harness.event(press(true, y0));
                harness.step();
                for k in 1..=3 {
                    let y = y0 + (y1 - y0) * k as f32 / 3.0;
                    harness.event(touch(egui::TouchPhase::Move, y));
                    harness.event(egui::Event::PointerMoved(egui::pos2(x, y)));
                    harness.step();
                }
                harness.event(touch(egui::TouchPhase::End, y1));
                harness.event(press(false, y1));
                harness.event(egui::Event::PointerGone);
                harness.step();
            }
            "steps" => harness.run_steps(value.parse().unwrap()),
            // wait:MS — real time, for background work such as image decoding.
            "wait" => {
                std::thread::sleep(std::time::Duration::from_millis(value.parse().unwrap()));
                harness.run_steps(2);
            }
            "swipe" => {
                // swipe:FROM_X,TO_X[,Y] (mid-height by default), over a few frames.
                let parts: Vec<f32> = value.split(',').map(|v| v.parse().unwrap()).collect();
                let (a, b) = (parts[0], parts[1]);
                let y = parts.get(2).copied().unwrap_or(500.0);
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

fn demo_shelf(real: &std::path::Path, title: &str, author: Option<&str>) -> Vec<RecentBook> {
    let book = |title: &str, author: &str, progress: f32| RecentBook {
        path: PathBuf::from(title),
        title: title.to_owned(),
        author: Some(author.to_owned()),
        chapter: 0,
        offset: 0,
        progress,
        opened: 0,
        key: None,
    };
    let mut first = book(title, author.unwrap_or(""), 0.31);
    first.path = real.to_owned();
    first.author = author.map(str::to_owned);
    vec![
        first,
        book("Pride and Prejudice", "Jane Austen", 0.66),
        book("Frankenstein; Or, The Modern Prometheus", "Mary Wollstonecraft Shelley", 0.12),
        book("Alice's Adventures in Wonderland", "Lewis Carroll", 1.0),
    ]
}

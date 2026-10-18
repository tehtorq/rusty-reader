#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rusty_reader::app;

use eframe::egui;

fn main() -> eframe::Result {
    let initial = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rusty Reader")
            .with_app_id("rusty-reader")
            .with_inner_size([780.0, 1000.0])
            .with_min_inner_size([360.0, 480.0])
            .with_drag_and_drop(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).expect("valid icon"),
            ),
        ..Default::default()
    };
    eframe::run_native("Rusty Reader", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, initial)))))
}

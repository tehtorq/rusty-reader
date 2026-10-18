//! Rusty Reader — a quiet EPUB reader.

pub mod app;
pub mod book;
pub mod fonts;
pub mod layout;
pub mod library;
pub mod meter;
pub mod platform;
pub mod reader;
pub mod store;
pub mod theme;
pub mod viewer;
pub mod widgets;

/// Entry point when running as an Android app.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));
    std::panic::set_hook(Box::new(|info| log::error!("{info}")));
    platform::android::set_app(app.clone());

    let mut options = eframe::NativeOptions { android_app: Some(app), ..Default::default() };
    // Let the GPU work on one frame while the next is prepared. With a single frame in flight,
    // any hiccup misses the (8 ms, at 120 Hz) vsync and motion visibly stutters.
    options.wgpu_options.surface = eframe::SurfaceConfig::HIGH_THROUGHPUT;
    let result =
        eframe::run_native("Rusty Reader", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, None)))));
    if let Err(error) = result {
        log::error!("{error}");
    }
    // Android keeps the process for a quick relaunch, but winit can only start once per
    // process, so a relaunch would close at once. End the process with the app instead.
    std::process::exit(0);
}

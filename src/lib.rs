//! Rusty Reader — a quiet EPUB reader.

pub mod app;
pub mod book;
pub mod fonts;
pub mod layout;
pub mod library;
pub mod meter;
pub mod picture;
pub mod platform;
pub mod reader;
pub mod speech;
pub mod store;
pub mod theme;
pub mod viewer;
pub mod widgets;

/// Entry point when running as an Android app.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    use std::sync::atomic::{AtomicBool, Ordering};

    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("RustyReader"),
    );
    std::panic::set_hook(Box::new(|info| log::error!("{info}")));
    platform::startup("android_main entered");
    // If the first frame hasn't appeared after a while, say how far start-up got.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        platform::startup_report();
    });

    // winit can only start once per process. Android sometimes reuses a process the app
    // already ran in (one kept alive by reading aloud, say), and starting again there would
    // hang on the launch screen: relaunch in a fresh process instead.
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        log::warn!("Started again in an old process: relaunching in a new one");
        platform::android::restart_fresh(&app);
        std::thread::sleep(std::time::Duration::from_secs(2));
        std::process::exit(0);
    }
    platform::android::set_app(app.clone());
    platform::startup("set_app done");

    let mut options = eframe::NativeOptions { android_app: Some(app), ..Default::default() };
    // Let the GPU work on one frame while the next is prepared. With a single frame in flight,
    // any hiccup misses the (8 ms, at 120 Hz) vsync and motion visibly stutters.
    options.wgpu_options.surface = eframe::SurfaceConfig::HIGH_THROUGHPUT;
    // A panic must end the process too: left alone, the app would sit on its launch screen.
    platform::startup("run_native starting");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native("Rusty Reader", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, None)))))
    }));
    match result {
        Ok(Err(error)) => log::error!("{error}"),
        Err(_) => log::error!("Rusty Reader panicked; closing"),
        Ok(Ok(())) => {}
    }
    // Android keeps the process for a quick relaunch, but winit can only start once per
    // process, so a relaunch would close at once. End the process with the app instead.
    std::process::exit(0);
}

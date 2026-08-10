#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod config;
mod game;
#[cfg(windows)]
mod guard;
mod input;
mod platform;
mod process;
mod seeder;
mod startup;
mod updater;

slint::include_modules!();

fn main() {
    // Debug-only: preview the squad-creation overlay without seeding. Esc or 20s to close.
    #[cfg(all(windows, debug_assertions))]
    if std::env::args().any(|a| a == "--preview-overlay") {
        unsafe { guard::preview(); }
        return;
    }

    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime init failed");

    // Keep the runtime active on the main thread so tokio::spawn works in Slint callbacks
    let _guard = rt.enter();

    let window = AppWindow::new().expect("Slint window init failed");

    app::setup(window.as_weak());

    window.run().expect("Slint event loop failed");
}

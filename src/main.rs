//! RustPlorer — a fast, lightweight file manager for Windows.
//!
//! Hides the console window on Windows release builds. Debug builds keep it so
//! `tracing` output is visible during development.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod core;
mod fs;
mod ui;

use core::logging;
use core::paths::AppPaths;

fn main() -> eframe::Result<()> {
    let paths = AppPaths::resolve();

    // Order matters: the panic hook must be installed before any worker starts,
    // so a panic during startup is still captured with its backtrace.
    let diagnostics = logging::init(paths.log_dir());
    logging::install_panic_hook();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "RustPlorer starting"
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([600.0, 400.0])
            .with_title("RustPlorer"),
        // Persist window geometry between sessions.
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "RustPlorer",
        options,
        Box::new(move |cc| {
            // Image loaders back the preview and thumbnail features added later.
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(ui::app::RustPlorer::new(paths, diagnostics)))
        }),
    )
}

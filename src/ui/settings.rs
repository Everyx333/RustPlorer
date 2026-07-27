//! The settings window.
//!
//! Tabbed so later phases (Appearance theming, Keybindings) drop into the
//! existing structure instead of forcing a redesign. Tabs that aren't
//! implemented yet are shown but marked, so the shape of the app is honest
//! about what exists.

use crate::core::config::{PerformanceProfile, SCHEMA_VERSION};
use crate::ui::app::RustPlorer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Performance,
    Behavior,
    Appearance,
    Keybindings,
    Archives,
    About,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 6] = [
        Self::Performance,
        Self::Behavior,
        Self::Appearance,
        Self::Keybindings,
        Self::Archives,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Performance => "⚡ Performance",
            Self::Behavior => "🔧 Behavior",
            Self::Appearance => "🎨 Appearance",
            Self::Keybindings => "⌨ Keybindings",
            Self::Archives => "📦 Archives",
            Self::About => "ℹ About",
        }
    }
}

/// Draw the settings window if open. Returns true if settings changed.
pub fn draw(app: &mut RustPlorer, ctx: &egui::Context) -> bool {
    if !app.settings_open {
        return false;
    }

    let mut open = true;
    let mut changed = false;

    egui::Window::new("Settings")
        .open(&mut open)
        .resizable(true)
        .default_size([620.0, 460.0])
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for tab in SettingsTab::ALL {
                    if ui
                        .selectable_label(app.settings_tab == tab, tab.label())
                        .clicked()
                    {
                        app.settings_tab = tab;
                    }
                }
            });

            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| match app.settings_tab {
                SettingsTab::Performance => changed |= performance_tab(app, ui),
                SettingsTab::Behavior => changed |= behavior_tab(app, ui),
                SettingsTab::Appearance => changed |= appearance_tab(app, ui),
                SettingsTab::Keybindings => keybindings_tab(ui),
                SettingsTab::Archives => changed |= archives_tab(app, ui),
                SettingsTab::About => about_tab(app, ui),
            });
        });

    app.settings_open = open;
    changed
}

fn performance_tab(app: &mut RustPlorer, ui: &mut egui::Ui) -> bool {
    let mut changed = false;

    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(format!("Detected {cpus} logical processors"))
            .weak(),
    );
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Performance profile").strong());
    ui.add_space(4.0);

    for profile in [
        PerformanceProfile::Conservative,
        PerformanceProfile::Balanced,
        PerformanceProfile::Aggressive,
        PerformanceProfile::Custom,
    ] {
        let selected = app.config.performance.profile == profile;
        if ui
            .radio(selected, profile.label())
            .on_hover_text(profile.description())
            .clicked()
            && !selected
        {
            app.config.performance.profile = profile;
            changed = true;
        }
    }

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(app.config.performance.profile.description())
            .weak()
            .italics(),
    );

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    // Show the resulting numbers for every profile, so the effect of the
    // choice is visible rather than mysterious.
    let workers = app.config.performance.effective_workers();
    let walks = app.config.performance.effective_size_walks();

    ui.label(egui::RichText::new("Effective settings").strong());
    ui.add_space(4.0);

    let is_custom = app.config.performance.profile == PerformanceProfile::Custom;

    ui.add_enabled_ui(is_custom, |ui| {
        let mut w = if is_custom {
            app.config.performance.worker_threads
        } else {
            workers
        };
        if ui
            .add(egui::Slider::new(&mut w, 1..=32).text("Worker threads"))
            .on_hover_text(
                "Threads for scanning and file operations.\nApplies after restart.",
            )
            .changed()
        {
            app.config.performance.worker_threads = w;
            changed = true;
        }

        let mut s = if is_custom {
            app.config.performance.concurrent_size_walks
        } else {
            walks
        };
        if ui
            .add(egui::Slider::new(&mut s, 1..=16).text("Concurrent folder-size scans"))
            .on_hover_text(
                "How many folders are measured at once.\n\
                 Higher is faster on SSDs, slower on spinning disks.\n\
                 Applies immediately.",
            )
            .changed()
        {
            app.config.performance.concurrent_size_walks = s;
            changed = true;
        }
    });

    if !is_custom {
        ui.label(
            egui::RichText::new(format!(
                "Using {workers} workers and {walks} concurrent size scans. \
                 Select Custom to change."
            ))
            .weak()
            .small(),
        );
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Folder sizes").strong());
    ui.add_space(4.0);

    if ui
        .checkbox(
            &mut app.config.performance.folder_sizes_enabled,
            "Calculate folder sizes",
        )
        .on_hover_text("Show real sizes for folders instead of a blank column.")
        .changed()
    {
        changed = true;
        if !app.config.performance.folder_sizes_enabled {
            app.sizer.clear();
        }
    }

    let mut look = app.config.performance.size_lookahead_rows;
    if ui
        .add(egui::Slider::new(&mut look, 0..=100).text("Pre-size rows ahead"))
        .on_hover_text(
            "Measure folders slightly beyond the visible area so sizes are\n\
             ready when you scroll to them.",
        )
        .changed()
    {
        app.config.performance.size_lookahead_rows = look;
        changed = true;
    }

    changed
}

fn behavior_tab(app: &mut RustPlorer, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    let b = &mut app.config.behavior;

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Browsing").strong());
    ui.add_space(4.0);

    changed |= ui.checkbox(&mut b.show_hidden, "Show hidden files").changed();
    changed |= ui
        .checkbox(&mut b.live_refresh, "Refresh automatically when files change")
        .on_hover_text("Watches the current folder and updates the listing.")
        .changed();
    changed |= ui
        .checkbox(&mut b.restore_last_path, "Reopen the last folder on startup")
        .changed();

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Deleting").strong());
    ui.add_space(4.0);

    changed |= ui
        .checkbox(&mut b.delete_to_recycle_bin, "Send deleted files to the Recycle Bin")
        .on_hover_text("Turn off to delete permanently. Not recommended.")
        .changed();
    changed |= ui
        .checkbox(&mut b.confirm_delete, "Ask before deleting")
        .changed();

    if !b.delete_to_recycle_bin {
        ui.add_space(4.0);
        ui.colored_label(
            egui::Color32::from_rgb(220, 140, 60),
            "⚠ Files will be deleted permanently and cannot be recovered.",
        );
    }

    changed
}

fn appearance_tab(app: &mut RustPlorer, ui: &mut egui::Ui) -> bool {
    let mut changed = false;

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Theme").strong());
    ui.add_space(4.0);

    let current = app.config.appearance.dark_mode;
    for (label, value) in [
        ("Follow system", None),
        ("Dark", Some(true)),
        ("Light", Some(false)),
    ] {
        if ui.radio(current == value, label).clicked() && current != value {
            app.config.appearance.dark_mode = value;
            changed = true;
        }
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Layout").strong());
    ui.add_space(4.0);

    let a = &mut app.config.appearance;

    changed |= ui
        .add(egui::Slider::new(&mut a.font_size, 10.0..=24.0).text("Font size"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut a.row_height, 18.0..=48.0).text("Row height"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut a.row_spacing, 0.5..=2.5).text("Spacing"))
        .on_hover_text("Multiplier on default widget spacing. Lower is denser.")
        .changed();
    changed |= ui.checkbox(&mut a.striped_rows, "Striped rows").changed();

    ui.add_space(10.0);
    if ui.button("Reset to defaults").clicked() {
        app.config.appearance = crate::core::config::AppearanceConfig::default();
        changed = true;
    }

    ui.add_space(12.0);
    ui.label(
        egui::RichText::new(
            "Full theming — custom colors and font families — arrives in a later update.",
        )
        .weak()
        .italics(),
    );

    changed
}

fn keybindings_tab(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Customizable keybindings are coming in a later update.")
            .weak()
            .italics(),
    );
    ui.add_space(10.0);

    ui.label(egui::RichText::new("Current shortcuts").strong());
    ui.add_space(6.0);

    egui::Grid::new("keybinds")
        .num_columns(2)
        .spacing([32.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (keys, action) in [
                ("Ctrl + F", "Focus the filter box"),
                ("F5", "Refresh"),
                ("Alt + ←", "Back"),
                ("Alt + →", "Forward"),
                ("Alt + ↑ / Backspace", "Up one folder"),
                ("Delete", "Send to Recycle Bin"),
                ("Ctrl + ,", "Open settings"),
                ("Escape", "Clear filter / dismiss message"),
                ("Enter / Double-click", "Open"),
            ] {
                ui.label(egui::RichText::new(keys).monospace().strong());
                ui.label(action);
                ui.end_row();
            }
        });
}

fn archives_tab(app: &mut RustPlorer, ui: &mut egui::Ui) -> bool {
    let mut changed = false;

    ui.add_space(6.0);
    ui.label(egui::RichText::new("External tools").strong());
    ui.add_space(4.0);

    changed |= ui
        .checkbox(
            &mut app.config.archive.prefer_external_tool,
            "Use installed 7-Zip or WinRAR when available",
        )
        .on_hover_text(
            "External tools are usually faster and more thoroughly tested.\n\
             RustPlorer falls back to its built-in support when none is found.",
        )
        .changed();

    ui.add_space(6.0);
    match &app.config.archive.external_tool_path {
        Some(p) => {
            ui.label(egui::RichText::new("Detected:").weak());
            ui.label(egui::RichText::new(p.display().to_string()).monospace().small());
        }
        None => {
            ui.label(
                egui::RichText::new("No external archive tool detected.")
                    .weak()
                    .italics(),
            );
        }
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Built-in support").strong());
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "All formats below work without any external tool installed:",
        )
        .weak()
        .small(),
    );
    ui.add_space(4.0);

    egui::Grid::new("formats")
        .num_columns(2)
        .spacing([32.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for (fmt, ops) in [
                (".zip", "read + write"),
                (".7z", "read + write"),
                (".rar", "read + write"),
                (".tar", "read + write"),
                (".gz / .tar.gz", "read + write"),
                (".xz / .tar.xz", "read + write"),
                (".zst / .tar.zst", "read"),
            ] {
                ui.label(egui::RichText::new(fmt).monospace());
                ui.label(egui::RichText::new(ops).weak());
                ui.end_row();
            }
        });

    changed
}

fn about_tab(app: &RustPlorer, ui: &mut egui::Ui) {
    ui.add_space(10.0);
    ui.vertical_centered(|ui| {
        ui.heading("RustPlorer");
        ui.label(
            egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).weak(),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("A fast, lightweight file manager for Windows")
                .weak()
                .italics(),
        );
    });

    ui.add_space(16.0);
    ui.separator();
    ui.add_space(8.0);

    egui::Grid::new("about")
        .num_columns(2)
        .spacing([24.0, 6.0])
        .show(ui, |ui| {
            ui.label("Config schema");
            ui.label(SCHEMA_VERSION.to_string());
            ui.end_row();

            ui.label("Worker threads");
            ui.label(app.pool.size().to_string());
            ui.end_row();

            ui.label("Logs");
            ui.label(
                egui::RichText::new(
                    app.diagnostics
                        .log_dir
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "unavailable".into()),
                )
                .monospace()
                .small(),
            );
            ui.end_row();

            ui.label("Config file");
            ui.label(
                egui::RichText::new(
                    app.paths
                        .config_file()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "unavailable".into()),
                )
                .monospace()
                .small(),
            );
            ui.end_row();
        });

    ui.add_space(12.0);
    if ui
        .button("Copy diagnostics")
        .on_hover_text("Copy version, environment, and recent logs to the clipboard")
        .clicked()
    {
        let report = app.diagnostics_report();
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(report) {
                    tracing::warn!(error = %e, "clipboard write failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "clipboard unavailable"),
        }
    }
}

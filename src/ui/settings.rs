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

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Thumbnails").strong());
    ui.add_space(4.0);

    if ui
        .checkbox(
            &mut app.config.performance.thumbnails_enabled,
            "Show image thumbnails",
        )
        .on_hover_text("Generate small previews for image files in the listing.")
        .changed()
    {
        changed = true;
    }

    let mut mb = app.config.performance.thumbnail_cache_mb;
    if ui
        .add(egui::Slider::new(&mut mb, 8..=512).text("Thumbnail memory (MB)"))
        .on_hover_text(
            "Hard ceiling on thumbnail memory.\n\
             Oldest thumbnails are discarded when the limit is reached.",
        )
        .changed()
    {
        app.config.performance.thumbnail_cache_mb = mb;
        changed = true;
    }

    ui.label(
        egui::RichText::new(format!(
            "Currently using {} across {} cached items",
            humansize::format_size(app.thumbs.bytes_used() as u64, humansize::DECIMAL),
            app.thumbs.len()
        ))
        .weak()
        .small(),
    );

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Folder sizes").strong());
    ui.add_space(4.0);

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
    use crate::core::config::{parse_hex, to_hex, Theme};

    let mut changed = false;

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Theme").strong());
    ui.add_space(4.0);

    // Built-in presets. Selecting one replaces the whole colour set.
    ui.horizontal_wrapped(|ui| {
        for theme in Theme::builtins() {
            let active = app.config.appearance.theme.name == theme.name;
            if ui.selectable_label(active, &theme.name).clicked() && !active {
                app.config.appearance.theme = theme;
                changed = true;
            }
        }
    });

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);

    ui.label(egui::RichText::new("Colors").strong());
    ui.label(
        egui::RichText::new("Editing a color switches the theme to Custom.")
            .weak()
            .small(),
    );
    ui.add_space(6.0);

    // Individual colour pickers. egui works in RGB bytes; the config stores
    // hex so the file stays readable and hand-editable.
    let mut edited = false;
    {
        let t = &mut app.config.appearance.theme;

        for (label, field) in [
            ("Background", &mut t.background),
            ("Panel", &mut t.panel),
            ("Text", &mut t.text),
            ("Accent", &mut t.accent),
            ("Stripe", &mut t.stripe),
            ("Warning", &mut t.warning),
            ("Error", &mut t.error),
        ] {
            ui.horizontal(|ui| {
                let (r, g, b) = parse_hex(field).unwrap_or((128, 128, 128));
                let mut rgb = [r, g, b];

                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    *field = to_hex(rgb[0], rgb[1], rgb[2]);
                    edited = true;
                }

                ui.label(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(field.as_str()).monospace().small().weak());
                });
            });
        }
    }

    if edited {
        // Mark the theme as no longer a stock preset, so the preset row does
        // not falsely claim the built-in is still active.
        if Theme::builtins()
            .iter()
            .any(|b| b.name == app.config.appearance.theme.name)
        {
            app.config.appearance.theme.name = "Custom".to_string();
        }
        changed = true;
    }

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);

    ui.label(egui::RichText::new("Typography and layout").strong());
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
    changed |= ui
        .checkbox(&mut a.monospace_listing, "Monospace font")
        .on_hover_text("Useful when names contain aligned numbers or hashes.")
        .changed();

    ui.add_space(10.0);
    if ui.button("Reset appearance to defaults").clicked() {
        app.config.appearance = crate::core::config::AppearanceConfig::default();
        changed = true;
    }

    changed
}

fn keybindings_tab(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new("Keyboard shortcuts").strong());
    ui.add_space(6.0);

    let groups: [(&str, &[(&str, &str)]); 4] = [
        (
            "Navigation",
            &[
                ("Alt + ⬅", "Back"),
                ("Alt + ➡", "Forward"),
                ("Alt + ⬆ / Backspace", "Up one folder"),
                ("F5", "Refresh"),
                ("Enter / Double-click", "Open"),
            ],
        ),
        (
            "Panes",
            &[
                ("Ctrl + F6", "Show or hide the second pane"),
                ("F6", "Switch focus between panes"),
                ("Ctrl + F5", "Copy selection to the other pane"),
                ("Shift + F6", "Move selection to the other pane"),
            ],
        ),
        (
            "Files",
            &[
                ("F2", "Batch rename"),
                ("Delete", "Send to Recycle Bin"),
                ("Space", "Quick preview"),
                ("Ctrl + F", "Filter files"),
            ],
        ),
        (
            "Application",
            &[
                ("Ctrl + Shift + P", "Command palette"),
                ("Ctrl + ,", "Settings"),
                ("Escape", "Clear filter / dismiss message"),
            ],
        ),
    ];

    for (group, binds) in groups {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(group).strong().small());
        ui.add_space(2.0);

        egui::Grid::new(format!("keys_{group}"))
            .num_columns(2)
            .spacing([28.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                for (keys, action) in binds {
                    ui.label(egui::RichText::new(*keys).monospace().small());
                    ui.label(egui::RichText::new(*action).small());
                    ui.end_row();
                }
            });
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Remappable shortcuts are not implemented yet. Every action above \n\
             is also reachable from the command palette (Ctrl + Shift + P), \n\
             which needs no binding.",
        )
        .weak()
        .small()
        .italics(),
    );
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

    ui.add_space(8.0);
    if ui
        .button("Install 7-Zip or WinRAR…")
        .on_hover_text("Opens the official download page in your browser")
        .clicked()
    {
        app.first_run_stage = crate::ui::first_run::FirstRunStage::Choose;
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

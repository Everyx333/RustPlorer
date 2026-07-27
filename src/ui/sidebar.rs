//! Left navigation: quick access folders and drive roots.

use crate::ui::app::RustPlorer;

pub fn draw(app: &mut RustPlorer, ctx: &egui::Context) {
    // Deferred so we don't mutate `app` while borrowing its lists.
    let mut navigate_to = None;
    let mut settings_changed = false;
    let mut save_workspace: Option<String> = None;
    let mut load_workspace: Option<String> = None;
    let mut delete_workspace: Option<String> = None;

    egui::SidePanel::left("sidebar")
        .resizable(true)
        .default_width(200.0)
        .width_range(140.0..=400.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Quick access").strong());
                ui.add_space(2.0);

                for (label, path) in &app.quick_access {
                    let active = &app.tabs[app.active_tab].path == path;
                    if ui.selectable_label(active, format!("📁 {label}")).clicked() {
                        navigate_to = Some(path.clone());
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);

                ui.label(egui::RichText::new("Drives").strong());
                ui.add_space(2.0);

                for drive in &app.drives {
                    let active = &app.tabs[app.active_tab].path == drive;
                    let label = drive.to_string_lossy().to_string();
                    if ui.selectable_label(active, format!("💾 {label}")).clicked() {
                        navigate_to = Some(drive.clone());
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);

                let mut show_hidden = app.show_hidden;
                if ui.checkbox(&mut show_hidden, "Show hidden").changed() {
                    app.toggle_hidden();
                }

                let mut sizes = app.config.performance.folder_sizes_enabled;
                if ui
                    .checkbox(&mut sizes, "Folder sizes")
                    .on_hover_text(
                        "Calculate folder sizes in the background.\n\
                         Configure concurrency in Settings > Performance.",
                    )
                    .changed()
                {
                    app.config.performance.folder_sizes_enabled = sizes;
                    if !sizes {
                        app.sizer.clear();
                    }
                    settings_changed = true;
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);

                ui.label(egui::RichText::new("Workspaces").strong());
                ui.add_space(2.0);

                let saved: Vec<String> =
                    app.config.workspaces.iter().map(|w| w.name.clone()).collect();

                if saved.is_empty() {
                    ui.label(
                        egui::RichText::new("None saved yet")
                            .weak()
                            .small()
                            .italics(),
                    );
                }

                for name in &saved {
                    ui.horizontal(|ui| {
                        if ui
                            .small_button(format!("▣ {name}"))
                            .on_hover_text("Restore this layout")
                            .clicked()
                        {
                            load_workspace = Some(name.clone());
                        }
                        if ui.small_button("✖").on_hover_text("Delete").clicked() {
                            delete_workspace = Some(name.clone());
                        }
                    });
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut app.workspace_name_input)
                            .desired_width(110.0)
                            .hint_text("Name…"),
                    );
                    let can_save = !app.workspace_name_input.trim().is_empty();
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save"))
                        .on_hover_text("Save the current pane layout")
                        .clicked()
                    {
                        save_workspace = Some(app.workspace_name_input.trim().to_string());
                    }
                });

                ui.add_space(8.0);

                if ui
                    .button("⚙ Settings")
                    .on_hover_text("Open settings (Ctrl+,)")
                    .clicked()
                {
                    app.settings_open = true;
                }

                if ui
                    .button("Copy diagnostics")
                    .on_hover_text("Copy version, environment, and recent logs to the clipboard")
                    .clicked()
                {
                    copy_diagnostics(app);
                }
            });
        });

    if let Some(name) = save_workspace {
        app.save_workspace(name);
        app.workspace_name_input.clear();
    }
    if let Some(name) = load_workspace {
        app.load_workspace(&name);
    }
    if let Some(name) = delete_workspace {
        app.delete_workspace(&name);
    }
    if settings_changed {
        app.config_changed();
    }
    if let Some(path) = navigate_to {
        app.navigate(path, true);
    }
}

/// Put a diagnostics report on the clipboard.
///
/// Clipboard access can fail (another process holding it, locked-down session),
/// and that must never take the app down — it is logged and ignored.
fn copy_diagnostics(app: &RustPlorer) {
    let report = app.diagnostics_report();

    match arboard::Clipboard::new() {
        Ok(mut cb) => {
            if let Err(e) = cb.set_text(report) {
                tracing::warn!(error = %e, "could not write diagnostics to clipboard");
            } else {
                tracing::info!("diagnostics copied to clipboard");
            }
        }
        Err(e) => tracing::warn!(error = %e, "clipboard unavailable"),
    }
}

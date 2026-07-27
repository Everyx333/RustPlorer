//! The main file listing.
//!
//! Rendering is **virtualized**: only rows intersecting the viewport are built.
//! A naive implementation that emits a widget per entry costs O(n) per frame
//! and stalls at a few thousand files. `TableBuilder::body::rows` gives O(visible)
//! instead, so a 200k-entry directory scrolls at the same speed as a 20-entry one.

use egui_extras::{Column, TableBuilder};

use crate::fs::entry::{EntryKind, SortKey};
use crate::ui::app::{LoadState, RustPlorer};

/// Row height in points. Fixed height is what makes virtualization possible:
/// egui can compute which rows are visible arithmetically instead of measuring
/// every row.
const ROW_HEIGHT: f32 = 24.0;

pub fn draw(app: &mut RustPlorer, ctx: &egui::Context) {
    // Deferred actions. We cannot mutate `app` while iterating its entries, so
    // UI interactions are recorded here and applied after the closure ends.
    let mut navigate_to = None;
    let mut sort_request = None;

    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
        draw_toolbar(app, ui, &mut navigate_to);
    });

    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        draw_status(app, ui);
    });

    egui::CentralPanel::default().show(ctx, |ui| {
        if let LoadState::Failed(err) = &app.active().state {
            ui.centered_and_justified(|ui| {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
            });
            return;
        }

        let available = ui.available_height();

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(200.0).clip(true)) // Name
            .column(Column::exact(110.0)) // Size
            .column(Column::exact(140.0)) // Modified
            .column(Column::exact(80.0)) // Kind
            .min_scrolled_height(available)
            .header(22.0, |mut header| {
                header.col(|ui| {
                    if sort_button(ui, app, "Name", SortKey::Name).clicked() {
                        sort_request = Some(SortKey::Name);
                    }
                });
                header.col(|ui| {
                    if sort_button(ui, app, "Size", SortKey::Size).clicked() {
                        sort_request = Some(SortKey::Size);
                    }
                });
                header.col(|ui| {
                    if sort_button(ui, app, "Modified", SortKey::Modified).clicked() {
                        sort_request = Some(SortKey::Modified);
                    }
                });
                header.col(|ui| {
                    if sort_button(ui, app, "Type", SortKey::Kind).clicked() {
                        sort_request = Some(SortKey::Kind);
                    }
                });
            })
            .body(|body| {
                let entries = &app.tabs[app.active_tab].entries;
                let selected = app.tabs[app.active_tab].selected;

                // The virtualization boundary: egui invokes this closure only
                // for rows actually on screen.
                body.rows(ROW_HEIGHT, entries.len(), |mut row| {
                    let idx = row.index();
                    let entry = &entries[idx];
                    row.set_selected(selected == Some(idx));

                    row.col(|ui| {
                        let icon = match entry.kind {
                            EntryKind::Directory => "📁",
                            EntryKind::Symlink => "🔗",
                            EntryKind::File => "📄",
                        };

                        let label = format!("{icon} {}", entry.name);
                        let text = if entry.is_hidden {
                            // Dim hidden files rather than hiding the fact
                            // they're hidden.
                            egui::RichText::new(label).weak()
                        } else {
                            egui::RichText::new(label)
                        };

                        let resp = ui.add(
                            egui::Label::new(text)
                                .sense(egui::Sense::click())
                                .truncate(),
                        );

                        if resp.clicked() {
                            sort_request = sort_request.take(); // no-op, keeps borrow simple
                        }

                        if resp.double_clicked() && entry.is_dir() {
                            navigate_to = Some(entry.path.clone());
                        }
                    });

                    row.col(|ui| {
                        ui.label(entry.size_display());
                    });
                    row.col(|ui| {
                        ui.label(entry.modified_display());
                    });
                    row.col(|ui| {
                        let kind = match entry.kind {
                            EntryKind::Directory => "Folder".to_string(),
                            EntryKind::Symlink => "Link".to_string(),
                            EntryKind::File => entry
                                .extension
                                .clone()
                                .unwrap_or_else(|| "File".to_string()),
                        };
                        ui.label(kind);
                    });
                });
            });
    });

    if let Some(key) = sort_request {
        app.set_sort(key);
    }
    if let Some(path) = navigate_to {
        app.navigate(path, true);
    }
}

/// A header cell that shows the active sort direction.
fn sort_button(
    ui: &mut egui::Ui,
    app: &RustPlorer,
    label: &str,
    key: SortKey,
) -> egui::Response {
    let arrow = if app.sort_key == key {
        match app.sort_order {
            crate::fs::entry::SortOrder::Ascending => " ▲",
            crate::fs::entry::SortOrder::Descending => " ▼",
        }
    } else {
        ""
    };

    ui.add(
        egui::Label::new(egui::RichText::new(format!("{label}{arrow}")).strong())
            .sense(egui::Sense::click()),
    )
}

fn draw_toolbar(
    app: &mut RustPlorer,
    ui: &mut egui::Ui,
    navigate_to: &mut Option<std::path::PathBuf>,
) {
    ui.horizontal(|ui| {
        ui.add_enabled_ui(app.active().can_go_back(), |ui| {
            if ui.button("◀").on_hover_text("Back (Alt+Left)").clicked() {
                app.go_back();
            }
        });

        ui.add_enabled_ui(app.active().can_go_forward(), |ui| {
            if ui.button("▶").on_hover_text("Forward (Alt+Right)").clicked() {
                app.go_forward();
            }
        });

        if ui.button("▲").on_hover_text("Up (Alt+Up)").clicked() {
            app.go_up();
        }

        if ui.button("⟳").on_hover_text("Refresh (F5)").clicked() {
            app.refresh();
        }

        ui.separator();

        // Breadcrumbs. Each ancestor is clickable, which is faster than
        // repeatedly pressing Up.
        let path = app.active().path.clone();
        let mut accumulated = std::path::PathBuf::new();

        egui::ScrollArea::horizontal()
            .id_salt("breadcrumbs")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for component in path.components() {
                        accumulated.push(component.as_os_str());
                        let text = component.as_os_str().to_string_lossy().to_string();
                        let text = if text.is_empty() { "/".to_string() } else { text };

                        if ui.small_button(text).clicked() {
                            *navigate_to = Some(accumulated.clone());
                        }
                        ui.label("›");
                    }
                });
            });
    });
}

fn draw_status(app: &RustPlorer, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let tab = app.active();

        match &tab.state {
            LoadState::Loading => {
                ui.spinner();
                ui.label(format!("Scanning… {} items", tab.entries.len()));
            }
            LoadState::Loaded => {
                let dirs = tab.entries.iter().filter(|e| e.is_dir()).count();
                let files = tab.entries.len() - dirs;
                ui.label(format!("{dirs} folders, {files} files"));
            }
            LoadState::Failed(_) => {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), "Failed to load");
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(format!("{} workers", app.pool.size()));
        });
    });
}

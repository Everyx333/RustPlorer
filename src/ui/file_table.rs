//! The main file listing.
//!
//! Rendering is **virtualized**: only rows intersecting the viewport are built.
//! A naive implementation that emits a widget per entry costs O(n) per frame
//! and stalls at a few thousand files. `TableBuilder::body::rows` gives
//! O(visible) instead, so a 200k-entry directory scrolls like a 20-entry one.
//!
//! Virtualization also drives folder sizing: the visible row range is exactly
//! the set of folders worth measuring.

use egui_extras::{Column, TableBuilder};

use crate::archive::format::ArchiveFormat;
use crate::fs::entry::{EntryKind, SortKey};
use crate::ui::app::{LoadState, RustPlorer};

pub fn draw(app: &mut RustPlorer, ctx: &egui::Context) {
    // Deferred actions: we cannot mutate `app` while borrowing its entries, so
    // interactions are recorded here and applied after the closures end.
    let mut navigate_to = None;
    let mut sort_request = None;
    let mut select_row = None;
    let mut visible_range: Option<std::ops::Range<usize>> = None;
    let mut open_archive: Option<std::path::PathBuf> = None;
    let mut enter_archive_dir: Option<String> = None;

    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
        draw_toolbar(app, ui, &mut navigate_to);
    });

    egui::TopBottomPanel::top("searchbar").show(ctx, |ui| {
        draw_searchbar(app, ui);
    });

    if let Some(err) = app.error_banner.clone() {
        egui::TopBottomPanel::top("error").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), "⚠");
                ui.label(err);
                if ui.small_button("Dismiss").clicked() {
                    app.error_banner = None;
                }
            });
        });
    }

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

        // When a filter is active we render an index map instead of the whole
        // listing, so filtering costs nothing extra at render time.
        let filtered: Option<Vec<usize>> =
            app.search.matched_indices().map(|s| s.to_vec());

        let row_count = match &filtered {
            Some(f) => f.len(),
            None => app.active().entries.len(),
        };

        if row_count == 0 && app.active().state == LoadState::Loaded {
            ui.centered_and_justified(|ui| {
                if app.search.is_active() {
                    ui.weak("No matches.");
                } else {
                    ui.weak("This folder is empty.");
                }
            });
            return;
        }

        let available = ui.available_height();
        let row_height = app.config.appearance.row_height;

        TableBuilder::new(ui)
            .striped(app.config.appearance.striped_rows)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(200.0).clip(true)) // Name
            .column(Column::exact(120.0)) // Size
            .column(Column::exact(140.0)) // Modified
            .column(Column::exact(80.0)) // Type
            .min_scrolled_height(available)
            .header(22.0, |mut header| {
                for (label, key) in [
                    ("Name", SortKey::Name),
                    ("Size", SortKey::Size),
                    ("Modified", SortKey::Modified),
                    ("Type", SortKey::Kind),
                ] {
                    header.col(|ui| {
                        if sort_header(ui, app, label, key).clicked() {
                            sort_request = Some(key);
                        }
                    });
                }
            })
            .body(|body| {
                let tab = &app.tabs[app.active_tab];
                let selected = tab.selected;
                let in_archive = app.archive_location.is_some();

                body.rows(row_height, row_count, |mut row| {
                    let row_idx = row.index();

                    // Map the visible row back to its index in the full listing.
                    let entry_idx = match &filtered {
                        Some(f) => f[row_idx],
                        None => row_idx,
                    };

                    let Some(entry) = tab.entries.get(entry_idx) else {
                        return;
                    };

                    // Track what's on screen so folder sizing can be scoped
                    // to just these rows.
                    visible_range = Some(match visible_range.take() {
                        Some(r) => r.start.min(entry_idx)..r.end.max(entry_idx + 1),
                        None => entry_idx..entry_idx + 1,
                    });

                    row.set_selected(selected == Some(entry_idx));

                    row.col(|ui| {
                        let icon = match entry.kind {
                            EntryKind::Directory => "📁",
                            EntryKind::Symlink => "🔗",
                            // Archives read as folders, so they get their own
                            // glyph to signal they can be entered.
                            EntryKind::File if ArchiveFormat::is_archive(&entry.path) => "📦",
                            EntryKind::File => "📄",
                        };
                        let text = egui::RichText::new(format!("{icon} {}", entry.name));
                        let text = if entry.is_hidden { text.weak() } else { text };
                        ui.add(egui::Label::new(text).truncate().selectable(false));
                    });

                    row.col(|ui| {
                        // The headline feature: directories show a real size
                        // instead of a blank cell.
                        //
                        // Archive folders are handled separately. Their sizes
                        // are already rolled up from the archive index when the
                        // listing is built, and their paths are relative to the
                        // archive — so consulting the filesystem sizer cache
                        // would never hit and the cell would read "…" forever.
                        let text = if in_archive {
                            entry.size_display()
                        } else if entry.is_dir() {
                            match app.folder_size(&entry.path) {
                                Some(state) => state.display(),
                                None if app.config.performance.folder_sizes_enabled => {
                                    "…".to_string()
                                }
                                None => "—".to_string(),
                            }
                        } else {
                            entry.size_display()
                        };

                        // Right-align sizes so magnitudes line up visually.
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.add(egui::Label::new(text).selectable(false));
                            },
                        );
                    });

                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(entry.modified_display()).selectable(false),
                        );
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
                        ui.add(egui::Label::new(kind).selectable(false));
                    });

                    let resp = row.response();
                    if resp.clicked() {
                        select_row = Some(entry_idx);
                    }
                    if resp.double_clicked() {
                        if in_archive {
                            // Inside an archive, paths are relative to the
                            // archive rather than the disk.
                            if entry.is_dir() {
                                enter_archive_dir = Some(entry.name.clone());
                            }
                            // Opening a file inside an archive requires
                            // extracting it first; deferred for now.
                        } else if entry.is_dir() {
                            navigate_to = Some(entry.path.clone());
                        } else if ArchiveFormat::is_archive(&entry.path) {
                            open_archive = Some(entry.path.clone());
                        } else if let Err(e) = open::that_detached(&entry.path) {
                            // Common failure (no registered handler); never
                            // fatal.
                            tracing::warn!(error = %e, "could not open file");
                        }
                    }
                });
            });
    });

    // Apply deferred actions.
    if let Some(idx) = select_row {
        app.tabs[app.active_tab].selected = Some(idx);
    }
    if let Some(key) = sort_request {
        app.set_sort(key);
    }
    if let Some(name) = enter_archive_dir {
        app.enter_archive_dir(&name);
    }
    if let Some(path) = open_archive {
        app.open_archive(path);
    }
    if let Some(range) = visible_range.filter(|_| !app.in_archive()) {
        // Widen slightly so sizes for rows just off-screen are ready by the
        // time they scroll into view.
        let look = app.config.performance.size_lookahead_rows;
        let start = range.start.saturating_sub(look);
        let end = range.end + look;
        app.request_visible_sizes(start..end);
    }
    if let Some(path) = navigate_to {
        app.navigate(path, true);
    }
}

/// A header cell showing the active sort direction.
fn sort_header(
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
            .sense(egui::Sense::click())
            .selectable(false),
    )
}

fn draw_searchbar(app: &mut RustPlorer, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("🔍");

        let mut query = app.search.query().to_string();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut query)
                .desired_width(f32::INFINITY)
                .hint_text("Filter files… (Ctrl+F)"),
        );

        if resp.changed() {
            app.search.set_query(query);
        }

        // Ctrl+F focuses the box.
        if app.focus_search {
            app.focus_search = false;
            resp.request_focus();
        }

        if app.search.is_active() && ui.small_button("✕").clicked() {
            app.search.clear();
        }
    });
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
            app.sizer.clear();
            app.refresh();
        }

        ui.separator();

        // Inside an archive the breadcrumb shows the archive path plus the
        // inner location, and is not clickable component-by-component.
        if let Some(loc) = app.archive_location.clone() {
            ui.label("📦");
            ui.label(egui::RichText::new(loc.display()).monospace().small());
            return;
        }

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

        // A running file operation takes priority in the status line.
        if let Some(status) = &app.op_status {
            ui.spinner();
            ui.label(status);
            return;
        }

        match &tab.state {
            LoadState::Loading => {
                ui.spinner();
                ui.label(format!("Scanning… {} items", tab.entries.len()));
            }
            LoadState::Loaded => {
                let dirs = tab.entries.iter().filter(|e| e.is_dir()).count();
                let files = tab.entries.len() - dirs;

                if app.search.is_active() {
                    ui.label(format!(
                        "{} of {} shown",
                        app.search.match_count(),
                        tab.entries.len()
                    ));
                } else {
                    ui.label(format!("{dirs} folders, {files} files"));
                }
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

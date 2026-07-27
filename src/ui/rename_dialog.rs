//! Batch rename dialog.
//!
//! The preview table is the feature. Renaming hundreds of files is
//! irreversible, so the user sees every resulting name — and every collision —
//! before the Apply button becomes usable.

use crate::fs::rename::{build_plan, CaseMode, RenamePlan, RenameRule};
use crate::ui::app::RustPlorer;

/// Which rule the user is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    FindReplace,
    Pattern,
    Prefix,
    Suffix,
    Case,
}

impl RuleKind {
    const ALL: [RuleKind; 5] = [
        Self::FindReplace,
        Self::Pattern,
        Self::Prefix,
        Self::Suffix,
        Self::Case,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::FindReplace => "Find & replace",
            Self::Pattern => "Pattern",
            Self::Prefix => "Add prefix",
            Self::Suffix => "Add suffix",
            Self::Case => "Change case",
        }
    }
}

/// Dialog state.
pub struct RenameDialog {
    pub open: bool,
    pub kind: RuleKind,

    pub find: String,
    pub replace: String,
    pub case_sensitive: bool,

    pub pattern: String,
    pub start: usize,
    pub padding: usize,

    pub prefix: String,
    pub suffix: String,
    pub case_mode: CaseMode,

    /// Recomputed whenever an input changes.
    plan: RenamePlan,
    dirty: bool,
}

impl Default for RenameDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl RenameDialog {
    pub fn new() -> Self {
        Self {
            open: false,
            kind: RuleKind::Pattern,
            find: String::new(),
            replace: String::new(),
            case_sensitive: false,
            pattern: "{name}_{n}".to_string(),
            start: 1,
            padding: 3,
            prefix: String::new(),
            suffix: String::new(),
            case_mode: CaseMode::Lower,
            plan: RenamePlan::default(),
            dirty: true,
        }
    }

    pub fn show(&mut self) {
        self.open = true;
        self.dirty = true;
    }

    /// Build the current rule from the form.
    fn rule(&self) -> RenameRule {
        match self.kind {
            RuleKind::FindReplace => RenameRule::FindReplace {
                find: self.find.clone(),
                replace: self.replace.clone(),
                case_sensitive: self.case_sensitive,
            },
            RuleKind::Pattern => RenameRule::Pattern {
                pattern: self.pattern.clone(),
                start: self.start,
                padding: self.padding,
            },
            RuleKind::Prefix => RenameRule::Prefix(self.prefix.clone()),
            RuleKind::Suffix => RenameRule::Suffix(self.suffix.clone()),
            RuleKind::Case => RenameRule::ChangeCase(self.case_mode),
        }
    }

    pub fn plan(&self) -> &RenamePlan {
        &self.plan
    }
}

/// Draw the dialog. Returns true if a rename was applied.
pub fn draw(app: &mut RustPlorer, ctx: &egui::Context) -> bool {
    if !app.rename_dialog.open {
        return false;
    }

    // Rename the whole visible listing; inside an archive nothing is editable.
    let targets: Vec<std::path::PathBuf> = if app.in_archive() {
        Vec::new()
    } else {
        app.active()
            .entries
            .iter()
            .filter(|e| !e.is_dir())
            .map(|e| e.path.clone())
            .collect()
    };

    let mut applied = false;
    let mut open = true;

    egui::Window::new("Batch rename")
        .open(&mut open)
        .resizable(true)
        .default_size([680.0, 520.0])
        .collapsible(false)
        .show(ctx, |ui| {
            if targets.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.weak(if app.in_archive() {
                        "Files inside an archive cannot be renamed."
                    } else {
                        "No files in this folder to rename."
                    });
                });
                return;
            }

            ui.horizontal(|ui| {
                for kind in RuleKind::ALL {
                    if ui
                        .selectable_label(app.rename_dialog.kind == kind, kind.label())
                        .clicked()
                    {
                        app.rename_dialog.kind = kind;
                        app.rename_dialog.dirty = true;
                    }
                }
            });

            ui.separator();
            ui.add_space(6.0);

            let d = &mut app.rename_dialog;
            let mut changed = false;

            match d.kind {
                RuleKind::FindReplace => {
                    ui.horizontal(|ui| {
                        ui.label("Find:");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut d.find)
                                    .desired_width(200.0)
                                    .hint_text("text to find"),
                            )
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Replace:");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut d.replace)
                                    .desired_width(200.0)
                                    .hint_text("replacement"),
                            )
                            .changed();
                    });
                    changed |= ui.checkbox(&mut d.case_sensitive, "Match case").changed();
                }

                RuleKind::Pattern => {
                    ui.horizontal(|ui| {
                        ui.label("Pattern:");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut d.pattern)
                                    .desired_width(280.0),
                            )
                            .changed();
                    });
                    ui.label(
                        egui::RichText::new(
                            "{name} original name   {ext} extension   \
                             {n} number   {parent} folder",
                        )
                        .weak()
                        .small(),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Start at:");
                        changed |= ui
                            .add(egui::DragValue::new(&mut d.start).range(0..=999_999))
                            .changed();
                        ui.add_space(12.0);
                        ui.label("Digits:");
                        changed |= ui
                            .add(egui::DragValue::new(&mut d.padding).range(0..=10))
                            .changed();
                    });
                }

                RuleKind::Prefix => {
                    ui.horizontal(|ui| {
                        ui.label("Prefix:");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut d.prefix)
                                    .desired_width(240.0)
                                    .hint_text("text to add at the start"),
                            )
                            .changed();
                    });
                }

                RuleKind::Suffix => {
                    ui.horizontal(|ui| {
                        ui.label("Suffix:");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut d.suffix)
                                    .desired_width(240.0)
                                    .hint_text("text to add before the extension"),
                            )
                            .changed();
                    });
                }

                RuleKind::Case => {
                    for (mode, label) in [
                        (CaseMode::Lower, "lowercase"),
                        (CaseMode::Upper, "UPPERCASE"),
                        (CaseMode::Title, "Title Case"),
                    ] {
                        if ui.radio(d.case_mode == mode, label).clicked()
                            && d.case_mode != mode
                        {
                            d.case_mode = mode;
                            changed = true;
                        }
                    }
                }
            }

            if changed {
                d.dirty = true;
            }

            // Recompute the preview when anything changed.
            if d.dirty {
                d.dirty = false;
                let rule = d.rule();
                d.plan = build_plan(&targets, &rule);
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            let valid = d.plan.valid_count();
            let problems = d.plan.problem_count();

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{valid} file(s) will be renamed"))
                        .strong(),
                );
                if problems > 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 140, 60),
                        format!("⚠ {problems} problem(s)"),
                    );
                }
            });

            ui.add_space(6.0);

            // The preview. Every resulting name, with problems called out.
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    egui::Grid::new("rename_preview")
                        .num_columns(3)
                        .spacing([12.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for item in &d.plan.items {
                                let old = item
                                    .from
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();

                                ui.label(egui::RichText::new(old).small());
                                ui.label(egui::RichText::new("➡").weak().small());

                                match &item.problem {
                                    Some(p) => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(220, 80, 80),
                                            egui::RichText::new(format!(
                                                "{} — {}",
                                                item.new_name,
                                                p.message()
                                            ))
                                            .small(),
                                        );
                                    }
                                    None if item.is_changed() => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(120, 200, 120),
                                            egui::RichText::new(&item.new_name).small(),
                                        );
                                    }
                                    None => {
                                        ui.label(
                                            egui::RichText::new(&item.new_name)
                                                .weak()
                                                .small(),
                                        );
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                // Apply stays disabled while anything would fail. A partial
                // batch rename is worse than none.
                let can_apply = valid > 0 && problems == 0;

                let btn = ui.add_enabled(
                    can_apply,
                    egui::Button::new(format!("Rename {valid} file(s)")),
                );

                let btn = if problems > 0 {
                    btn.on_disabled_hover_text(
                        "Resolve the problems above before renaming",
                    )
                } else {
                    btn
                };

                if btn.clicked() {
                    applied = true;
                }

                if ui.button("Cancel").clicked() {
                    d.open = false;
                }
            });
        });

    if !open {
        app.rename_dialog.open = false;
    }

    if applied {
        let plan = app.rename_dialog.plan.clone();
        match crate::fs::rename::apply_plan(&plan) {
            Ok(n) => {
                tracing::info!(renamed = n, "batch rename applied");
                app.rename_dialog.open = false;
                app.refresh();
            }
            Err(e) => {
                tracing::error!(error = %e, "batch rename failed");
                app.error_banner = Some(format!("Rename failed: {e}"));
            }
        }
    }

    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pattern_is_useful() {
        let d = RenameDialog::new();
        assert_eq!(d.kind, RuleKind::Pattern);
        assert!(d.pattern.contains("{name}"));
        assert!(d.pattern.contains("{n}"));
    }

    #[test]
    fn rule_reflects_selected_kind() {
        let mut d = RenameDialog::new();

        d.kind = RuleKind::Prefix;
        d.prefix = "x_".into();
        assert!(matches!(d.rule(), RenameRule::Prefix(p) if p == "x_"));

        d.kind = RuleKind::Case;
        d.case_mode = CaseMode::Upper;
        assert!(matches!(
            d.rule(),
            RenameRule::ChangeCase(CaseMode::Upper)
        ));
    }

    #[test]
    fn all_rule_kinds_have_labels() {
        for kind in RuleKind::ALL {
            assert!(!kind.label().is_empty());
        }
    }
}

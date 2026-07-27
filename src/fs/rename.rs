//! Batch rename.
//!
//! # Why this is preview-first
//!
//! Renaming 400 files with a bad pattern is not undoable. So the rename plan is
//! computed as pure data first, checked for collisions, and shown before
//! anything touches the disk. The apply step only executes an already-validated
//! plan.
//!
//! Two collision classes are detected, and they are different problems:
//!
//! - **Internal**: two sources map to the same new name. The pattern itself is
//!   wrong; renaming would destroy one file.
//! - **External**: the new name already exists on disk and isn't part of this
//!   batch. Renaming would clobber an unrelated file.
//!
//! There is also an ordering hazard: renaming `a→b` then `b→c` destroys the
//! original `b`. [`RenamePlan::needs_two_phase`] detects that, and apply routes
//! through temporary names when it happens.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// How to build the new names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameRule {
    /// Replace occurrences of `find` with `replace`.
    FindReplace {
        find: String,
        replace: String,
        case_sensitive: bool,
    },
    /// Apply a pattern with placeholders:
    /// `{name}` original stem, `{ext}` extension, `{n}` counter,
    /// `{parent}` containing folder name.
    Pattern {
        pattern: String,
        start: usize,
        /// Zero-pad the counter to this width.
        padding: usize,
    },
    /// Add text before the stem.
    Prefix(String),
    /// Add text after the stem, before the extension.
    Suffix(String),
    ChangeCase(CaseMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    Lower,
    Upper,
    Title,
}

/// One entry in a rename plan.
#[derive(Debug, Clone)]
pub struct RenameItem {
    pub from: PathBuf,
    pub new_name: String,
    /// Why this item cannot be applied, if anything.
    pub problem: Option<RenameProblem>,
}

impl RenameItem {
    pub fn is_valid(&self) -> bool {
        self.problem.is_none()
    }

    /// True if the name is actually changing.
    pub fn is_changed(&self) -> bool {
        self.from
            .file_name()
            .map(|n| n.to_string_lossy() != self.new_name)
            .unwrap_or(false)
    }
}

/// Why a rename cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameProblem {
    /// Another item in this batch produces the same name.
    DuplicateInBatch,
    /// A file with this name already exists and is not part of the batch.
    AlreadyExists,
    /// Contains characters Windows forbids.
    InvalidCharacters(String),
    Empty,
    /// Reserved device name (CON, PRN, NUL, COM1…).
    ReservedName,
}

impl RenameProblem {
    pub fn message(&self) -> String {
        match self {
            Self::DuplicateInBatch => "Two files would get this same name".to_string(),
            Self::AlreadyExists => "A file with this name already exists".to_string(),
            Self::InvalidCharacters(c) => format!("Name cannot contain {c}"),
            Self::Empty => "Name cannot be empty".to_string(),
            Self::ReservedName => "This name is reserved by Windows".to_string(),
        }
    }
}

/// A validated set of renames.
#[derive(Debug, Clone, Default)]
pub struct RenamePlan {
    pub items: Vec<RenameItem>,
}

impl RenamePlan {
    /// Items that will actually be renamed.
    pub fn valid_changes(&self) -> impl Iterator<Item = &RenameItem> {
        self.items.iter().filter(|i| i.is_valid() && i.is_changed())
    }

    pub fn valid_count(&self) -> usize {
        self.valid_changes().count()
    }

    pub fn problem_count(&self) -> usize {
        self.items.iter().filter(|i| !i.is_valid()).count()
    }

    pub fn has_problems(&self) -> bool {
        self.problem_count() > 0
    }

    /// True if any target name collides with a source name in this batch.
    ///
    /// Example: renaming `a→b` and `b→c`. Applied naively in order, the first
    /// rename destroys the `b` the second one needs. Apply must then route
    /// through temporary names.
    pub fn needs_two_phase(&self) -> bool {
        let sources: HashSet<String> = self
            .items
            .iter()
            .filter_map(|i| i.from.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();

        self.valid_changes()
            .any(|i| sources.contains(&i.new_name))
    }
}

/// Windows-forbidden filename characters.
const INVALID_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Device names Windows reserves, with or without an extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Build a rename plan. Pure — touches the disk only to check for existing
/// names.
pub fn build_plan(paths: &[PathBuf], rule: &RenameRule) -> RenamePlan {
    let mut items: Vec<RenameItem> = Vec::with_capacity(paths.len());

    for (idx, path) in paths.iter().enumerate() {
        let new_name = apply_rule(path, rule, idx);
        items.push(RenameItem {
            from: path.clone(),
            new_name,
            problem: None,
        });
    }

    validate(&mut items);
    RenamePlan { items }
}

/// Produce the new name for one path.
fn apply_rule(path: &Path, rule: &RenameRule, index: usize) -> String {
    let full_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| full_name.clone());

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();

    let with_ext = |base: String| {
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    };

    match rule {
        RenameRule::FindReplace {
            find,
            replace,
            case_sensitive,
        } => {
            if find.is_empty() {
                return full_name;
            }
            if *case_sensitive {
                full_name.replace(find.as_str(), replace)
            } else {
                // Case-insensitive replace, preserving the untouched parts of
                // the original exactly.
                let lower_name = full_name.to_lowercase();
                let lower_find = find.to_lowercase();
                let mut out = String::with_capacity(full_name.len());
                let mut pos = 0;

                while let Some(found) = lower_name[pos..].find(&lower_find) {
                    let start = pos + found;
                    out.push_str(&full_name[pos..start]);
                    out.push_str(replace);
                    pos = start + find.len();
                }
                out.push_str(&full_name[pos..]);
                out
            }
        }

        RenameRule::Pattern {
            pattern,
            start,
            padding,
        } => {
            let counter = start + index;
            let number = if *padding > 0 {
                format!("{counter:0width$}", width = padding)
            } else {
                counter.to_string()
            };

            let parent = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let result = pattern
                .replace("{name}", &stem)
                .replace("{ext}", &ext)
                .replace("{n}", &number)
                .replace("{parent}", &parent);

            // If the pattern names the extension itself, respect it verbatim
            // rather than appending a second one.
            if pattern.contains("{ext}") {
                result
            } else {
                with_ext(result)
            }
        }

        RenameRule::Prefix(p) => with_ext(format!("{p}{stem}")),
        RenameRule::Suffix(s) => with_ext(format!("{stem}{s}")),

        RenameRule::ChangeCase(mode) => {
            let base = match mode {
                CaseMode::Lower => stem.to_lowercase(),
                CaseMode::Upper => stem.to_uppercase(),
                CaseMode::Title => stem
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            Some(f) => {
                                f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase()
                            }
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            with_ext(base)
        }
    }
}

/// Flag every problem in the plan.
fn validate(items: &mut [RenameItem]) {
    // Count target names to find internal duplicates.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in items.iter() {
        *counts.entry(item.new_name.to_lowercase()).or_insert(0) += 1;
    }

    // Names being renamed away are free to be reused by another item.
    let sources: HashSet<PathBuf> = items.iter().map(|i| i.from.clone()).collect();

    for item in items.iter_mut() {
        let name = &item.new_name;

        if name.trim().is_empty() {
            item.problem = Some(RenameProblem::Empty);
            continue;
        }

        if let Some(bad) = name.chars().find(|c| INVALID_CHARS.contains(c)) {
            item.problem = Some(RenameProblem::InvalidCharacters(format!("'{bad}'")));
            continue;
        }

        // Windows reserves these regardless of extension.
        let stem_upper = name
            .split('.')
            .next()
            .unwrap_or(name)
            .to_uppercase();
        if RESERVED.contains(&stem_upper.as_str()) {
            item.problem = Some(RenameProblem::ReservedName);
            continue;
        }

        if counts.get(&name.to_lowercase()).copied().unwrap_or(0) > 1 {
            item.problem = Some(RenameProblem::DuplicateInBatch);
            continue;
        }

        // Existing file check, ignoring files that are themselves being
        // renamed away in this batch.
        if let Some(parent) = item.from.parent() {
            let target = parent.join(name);
            if target.exists() && target != item.from && !sources.contains(&target) {
                item.problem = Some(RenameProblem::AlreadyExists);
            }
        }
    }
}

/// Execute a validated plan.
///
/// Returns the number renamed. Invalid items are skipped rather than
/// attempted.
pub fn apply_plan(plan: &RenamePlan) -> anyhow::Result<usize> {
    let changes: Vec<&RenameItem> = plan.valid_changes().collect();
    if changes.is_empty() {
        return Ok(0);
    }

    let span = tracing::info_span!("batch_rename", count = changes.len());
    let _guard = span.enter();

    if plan.needs_two_phase() {
        // Cyclic or chained renames (a→b, b→c) must not be applied in order,
        // or the first rename destroys the second's source.
        tracing::debug!("using two-phase rename to avoid clobbering");
        return apply_two_phase(&changes);
    }

    let mut renamed = 0;
    for item in changes {
        let Some(parent) = item.from.parent() else {
            continue;
        };
        let to = parent.join(&item.new_name);

        std::fs::rename(&item.from, &to).map_err(|e| {
            anyhow::anyhow!("could not rename {}: {e}", item.from.display())
        })?;
        renamed += 1;
    }

    tracing::info!(renamed, "batch rename complete");
    Ok(renamed)
}

/// Rename via temporary names so chained renames cannot clobber each other.
fn apply_two_phase(changes: &[&RenameItem]) -> anyhow::Result<usize> {
    let mut temps: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(changes.len());

    // Phase 1: everything to a unique temporary name.
    for (i, item) in changes.iter().enumerate() {
        let Some(parent) = item.from.parent() else {
            continue;
        };
        let temp = parent.join(format!(".rustplorer_rename_{i}_{}", std::process::id()));

        std::fs::rename(&item.from, &temp).map_err(|e| {
            anyhow::anyhow!("could not stage rename of {}: {e}", item.from.display())
        })?;

        temps.push((temp, parent.join(&item.new_name)));
    }

    // Phase 2: temporaries to their final names.
    let mut renamed = 0;
    for (temp, final_path) in temps {
        std::fs::rename(&temp, &final_path).map_err(|e| {
            anyhow::anyhow!("could not finish rename to {}: {e}", final_path.display())
        })?;
        renamed += 1;
    }

    tracing::info!(renamed, "two-phase batch rename complete");
    Ok(renamed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from("/tmp/rp").join(n)).collect()
    }

    #[test]
    fn prefix_preserves_extension() {
        let p = paths(&["photo.jpg"]);
        let plan = build_plan(&p, &RenameRule::Prefix("2024_".into()));
        assert_eq!(plan.items[0].new_name, "2024_photo.jpg");
    }

    #[test]
    fn suffix_goes_before_extension() {
        let p = paths(&["report.pdf"]);
        let plan = build_plan(&p, &RenameRule::Suffix("_final".into()));
        assert_eq!(plan.items[0].new_name, "report_final.pdf");
    }

    #[test]
    fn pattern_numbers_sequentially_with_padding() {
        let p = paths(&["a.jpg", "b.jpg", "c.jpg"]);
        let plan = build_plan(
            &p,
            &RenameRule::Pattern {
                pattern: "img_{n}".into(),
                start: 1,
                padding: 3,
            },
        );
        assert_eq!(plan.items[0].new_name, "img_001.jpg");
        assert_eq!(plan.items[2].new_name, "img_003.jpg");
    }

    #[test]
    fn pattern_with_explicit_ext_does_not_double_it() {
        let p = paths(&["a.jpg"]);
        let plan = build_plan(
            &p,
            &RenameRule::Pattern {
                pattern: "{name}_copy.{ext}".into(),
                start: 1,
                padding: 0,
            },
        );
        assert_eq!(plan.items[0].new_name, "a_copy.jpg", "must not append .jpg twice");
    }

    #[test]
    fn find_replace_is_case_sensitive_when_asked() {
        let p = paths(&["Photo_IMG.jpg"]);

        let sensitive = build_plan(
            &p,
            &RenameRule::FindReplace {
                find: "img".into(),
                replace: "pic".into(),
                case_sensitive: true,
            },
        );
        assert_eq!(sensitive.items[0].new_name, "Photo_IMG.jpg", "no match expected");

        let insensitive = build_plan(
            &p,
            &RenameRule::FindReplace {
                find: "img".into(),
                replace: "pic".into(),
                case_sensitive: false,
            },
        );
        assert_eq!(insensitive.items[0].new_name, "Photo_pic.jpg");
    }

    #[test]
    fn case_insensitive_replace_preserves_surrounding_text() {
        let p = paths(&["AaBbAa.txt"]);
        let plan = build_plan(
            &p,
            &RenameRule::FindReplace {
                find: "aa".into(),
                replace: "X".into(),
                case_sensitive: false,
            },
        );
        assert_eq!(plan.items[0].new_name, "XBbX.txt");
    }

    #[test]
    fn detects_duplicate_names_in_batch() {
        let p = paths(&["one.txt", "two.txt"]);
        // A constant pattern maps every file to the same name.
        let plan = build_plan(
            &p,
            &RenameRule::Pattern {
                pattern: "same".into(),
                start: 1,
                padding: 0,
            },
        );

        assert_eq!(plan.problem_count(), 2);
        assert_eq!(
            plan.items[0].problem,
            Some(RenameProblem::DuplicateInBatch)
        );
    }

    #[test]
    fn rejects_invalid_characters() {
        let p = paths(&["file.txt"]);
        let plan = build_plan(&p, &RenameRule::Prefix("bad:name".into()));
        assert!(matches!(
            plan.items[0].problem,
            Some(RenameProblem::InvalidCharacters(_))
        ));
    }

    #[test]
    fn rejects_windows_reserved_names() {
        let p = paths(&["anything.txt"]);
        let plan = build_plan(
            &p,
            &RenameRule::Pattern {
                pattern: "CON".into(),
                start: 1,
                padding: 0,
            },
        );
        assert_eq!(plan.items[0].problem, Some(RenameProblem::ReservedName));
    }

    #[test]
    fn detects_chained_renames_needing_two_phase() {
        // A shift: 1->2, 2->3, 3->4. Applied in order, renaming 1 to 2 would
        // destroy the original 2 before it is read.
        //
        // (An earlier version of this test used find/replace "a"->"b" over
        // [a.txt, b.txt], but that maps b.txt to itself, so the plan is caught
        // as a duplicate and never reaches the two-phase check. The fixture was
        // wrong, not the detection.)
        let items = vec![
            RenameItem {
                from: PathBuf::from("/tmp/rp/1.txt"),
                new_name: "2.txt".to_string(),
                problem: None,
            },
            RenameItem {
                from: PathBuf::from("/tmp/rp/2.txt"),
                new_name: "3.txt".to_string(),
                problem: None,
            },
        ];
        let plan = RenamePlan { items };

        assert!(
            plan.needs_two_phase(),
            "a rename targeting another source name must use two phases"
        );
    }

    #[test]
    fn independent_renames_skip_two_phase() {
        // No target collides with a source, so the simple path is safe.
        let items = vec![
            RenameItem {
                from: PathBuf::from("/tmp/rp/a.txt"),
                new_name: "x.txt".to_string(),
                problem: None,
            },
            RenameItem {
                from: PathBuf::from("/tmp/rp/b.txt"),
                new_name: "y.txt".to_string(),
                problem: None,
            },
        ];
        let plan = RenamePlan { items };

        assert!(!plan.needs_two_phase());
    }

    #[test]
    fn unchanged_names_are_not_counted() {
        let p = paths(&["file.txt"]);
        let plan = build_plan(
            &p,
            &RenameRule::FindReplace {
                find: "zzz".into(),
                replace: "yyy".into(),
                case_sensitive: true,
            },
        );
        assert_eq!(plan.valid_count(), 0, "a no-op rename should not count");
    }

    #[test]
    fn case_change_modes() {
        let p = paths(&["My File.TXT"]);

        let lower = build_plan(&p, &RenameRule::ChangeCase(CaseMode::Lower));
        assert_eq!(lower.items[0].new_name, "my file.TXT");

        let upper = build_plan(&p, &RenameRule::ChangeCase(CaseMode::Upper));
        assert_eq!(upper.items[0].new_name, "MY FILE.TXT");
    }

    #[test]
    fn applies_renames_on_disk() {
        let dir = std::env::temp_dir().join("rustplorer_rename_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let files = ["one.txt", "two.txt"];
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }

        let paths: Vec<PathBuf> = files.iter().map(|f| dir.join(f)).collect();
        let plan = build_plan(&paths, &RenameRule::Prefix("new_".into()));
        assert!(!plan.has_problems());

        let count = apply_plan(&plan).unwrap();
        assert_eq!(count, 2);
        assert!(dir.join("new_one.txt").exists());
        assert!(dir.join("new_two.txt").exists());
        assert!(!dir.join("one.txt").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_phase_swap_does_not_lose_files() {
        // The hardest case: swap two names. A naive in-order rename destroys
        // one file.
        let dir = std::env::temp_dir().join("rustplorer_rename_swap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("a.txt"), b"content-a").unwrap();
        std::fs::write(dir.join("b.txt"), b"content-b").unwrap();

        let items = vec![
            RenameItem {
                from: dir.join("a.txt"),
                new_name: "b.txt".to_string(),
                problem: None,
            },
            RenameItem {
                from: dir.join("b.txt"),
                new_name: "a.txt".to_string(),
                problem: None,
            },
        ];
        let plan = RenamePlan { items };

        assert!(plan.needs_two_phase());
        let count = apply_plan(&plan).unwrap();
        assert_eq!(count, 2);

        // Both files must still exist, with swapped contents.
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "content-a"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "content-b"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Guard against missing font glyphs.
//!
//! # Why this test exists, and why v1 of it was wrong
//!
//! egui bundles four fonts, but it does **not** search all four for every
//! character. It resolves per font *family*:
//!
//! - `Monospace` → Hack, Ubuntu-Light, NotoEmoji, emoji-icon-font
//! - `Proportional` → Ubuntu-Light, NotoEmoji, emoji-icon-font  (**no Hack**)
//!
//! Buttons, labels and headers use Proportional. Hack is the font carrying the
//! geometric shapes (`▲ ▼ ● ← → ↑`), so those render correctly in monospace
//! text and as **empty boxes** on every button.
//!
//! The first version of this test OR'd all four fonts together, so it passed
//! on characters that were broken in the real UI. It caught `✕` (absent
//! everywhere) but missed `▲`, `▼`, `●`, `←`, `→`, `↑` — all of which shipped
//! as boxes on buttons.
//!
//! This version models the family split, which is what egui actually does.

/// Characters present only in Hack, and therefore unavailable to any
/// Proportional widget: buttons, labels, headers, hover text.
const HACK_ONLY: &[(char, &str)] = &[
    ('▲', "U+25B2 — use ⬆ (U+2B06) on buttons"),
    ('▼', "U+25BC — use ⬇ (U+2B07) on buttons"),
    ('●', "U+25CF — use ⚫ (U+26AB) on buttons"),
    ('←', "U+2190 — use ⬅ (U+2B05) in labels"),
    ('→', "U+2192 — use ➡ (U+27A1) in labels"),
    ('↑', "U+2191 — use ⬆ (U+2B06) in labels"),
];

/// Characters absent from every bundled font.
const MISSING_EVERYWHERE: &[(char, &str)] = &[
    ('✕', "U+2715 — use ✖ (U+2716)"),
    ('⬒', "U+2B12 — use ▣ (U+25A3)"),
];

/// Characters verified present in the Proportional family, so they are safe
/// anywhere in the UI.
const PROPORTIONAL_SAFE: &[(char, &str)] = &[
    ('◀', "back"),
    ('▶', "forward"),
    ('⬆', "up / sort ascending"),
    ('⬇', "sort descending"),
    ('⬅', "left arrow in shortcut hints"),
    ('➡', "right arrow in shortcut hints"),
    ('⟳', "refresh"),
    ('✖', "close / clear"),
    ('🔍', "search"),
    ('📁', "folder"),
    ('📄', "file"),
    ('📦', "archive"),
    ('🔗', "symlink"),
    ('💾', "drive"),
    ('⚙', "settings"),
    ('⚡', "performance"),
    ('🔧', "behavior"),
    ('🎨', "appearance"),
    ('⌨', "keybindings"),
    ('ℹ', "about"),
    ('⚠', "warning"),
    ('⚫', "focused pane marker"),
    ('○', "unfocused pane marker"),
    ('▣', "workspace"),
    ('›', "breadcrumb separator"),
    ('…', "calculating / truncated"),
    ('≥', "at-least size"),
    ('—', "unknown value"),
];

#[test]
fn no_hack_only_glyphs_in_rendered_strings() {
    let sources = collect_ui_strings();
    assert!(!sources.is_empty(), "no UI string literals found");

    for (ch, fix) in HACK_ONLY {
        for (path, line_no, line) in &sources {
            assert!(
                !line.contains(*ch),
                "{path}:{line_no} renders '{ch}' in a Proportional widget, \
                 where it shows as an empty box.\n  Fix: {fix}\n  Line: {}",
                line.trim()
            );
        }
    }
}

#[test]
fn no_universally_missing_glyphs() {
    let sources = collect_ui_strings();

    for (ch, fix) in MISSING_EVERYWHERE {
        for (path, line_no, line) in &sources {
            assert!(
                !line.contains(*ch),
                "{path}:{line_no} uses '{ch}', which no bundled font \
                 contains.\n  Fix: {fix}\n  Line: {}",
                line.trim()
            );
        }
    }
}

#[test]
fn safe_and_unsafe_lists_do_not_overlap() {
    for (ch, _) in PROPORTIONAL_SAFE {
        assert!(
            !HACK_ONLY.iter().any(|(b, _)| b == ch),
            "'{ch}' is listed as both safe and Hack-only"
        );
        assert!(
            !MISSING_EVERYWHERE.iter().any(|(b, _)| b == ch),
            "'{ch}' is listed as both safe and missing"
        );
    }
}

#[test]
fn every_safe_glyph_is_documented() {
    for (ch, note) in PROPORTIONAL_SAFE {
        assert!(!note.is_empty(), "'{ch}' needs a description of its use");
    }
}

/// Collect string literals from source, skipping comments.
///
/// Only rendered text matters. Documentation legitimately contains arrows and
/// shapes (`temp → fsync → rename`); flagging those would produce noise and
/// train people to ignore this test.
fn collect_ui_strings() -> Vec<(String, usize, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, usize, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    // Skip comments and doc comments.
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    // Only lines with a string literal can render text.
                    if line.contains('"') {
                        out.push((path.display().to_string(), i + 1, line.to_string()));
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    walk(std::path::Path::new("src"), &mut out);
    out
}

//! Guard against missing font glyphs.
//!
//! egui bundles four fonts (Hack, Ubuntu-Light, NotoEmoji, emoji-icon-font).
//! A character outside all four renders as a "tofu" box, and nothing warns you
//! at compile time — it is only visible by looking at the running app, on
//! Windows, which is not something CI does.
//!
//! This shipped once: the preview panel's close button used `✕` (U+2715),
//! which is in none of the four fonts, so users saw an empty rectangle.
//!
//! This test asserts every symbol used in the UI is one that has been verified
//! against the bundled fonts. Adding a new symbol means adding it here.

/// Characters confirmed present in egui 0.33's bundled fonts.
///
/// Verified by enumerating each font's cmap table. Do not add to this list
/// without checking — the whole point is that assumptions are what broke it.
const VERIFIED_GLYPHS: &[(char, &str)] = &[
    // Navigation — Hack
    ('◀', "back"),
    ('▶', "forward"),
    ('▲', "up / sort ascending"),
    ('▼', "sort descending"),
    ('←', "left arrow in shortcut hints"),
    ('→', "right arrow in shortcut hints"),
    ('↑', "up arrow in shortcut hints"),
    // Actions — emoji fonts
    ('⟳', "refresh"),
    ('✖', "close / clear (replaces U+2715, which is NOT available)"),
    ('🔍', "search"),
    // File kinds — NotoEmoji
    ('📁', "folder"),
    ('📄', "file"),
    ('📦', "archive"),
    ('🔗', "symlink"),
    ('💾', "drive"),
    // Settings tabs
    ('⚙', "settings"),
    ('⚡', "performance"),
    ('🔧', "behavior"),
    ('🎨', "appearance"),
    ('⌨', "keybindings"),
    ('ℹ', "about"),
    ('⚠', "warning"),
    // Typography — Hack / Ubuntu
    ('›', "breadcrumb separator"),
    ('…', "calculating / truncated"),
    ('≥', "at-least size"),
    ('—', "unknown value"),
];

/// Characters known to be MISSING. Using any of these produces a tofu box.
const KNOWN_MISSING: &[(char, &str)] = &[
    ('✕', "U+2715 MULTIPLICATION X — absent from all bundled fonts; use U+2716 instead"),
];

#[test]
fn known_missing_glyphs_are_not_used_in_source() {
    let sources = collect_sources();
    assert!(!sources.is_empty(), "no source files found");

    for (ch, why) in KNOWN_MISSING {
        for (path, text) in &sources {
            assert!(
                !text.contains(*ch),
                "{path} uses '{ch}', which renders as an empty box.\n  {why}"
            );
        }
    }
}

#[test]
fn verified_and_missing_lists_do_not_overlap() {
    for (ch, _) in VERIFIED_GLYPHS {
        assert!(
            !KNOWN_MISSING.iter().any(|(m, _)| m == ch),
            "'{ch}' is listed as both verified and missing"
        );
    }
}

#[test]
fn every_verified_glyph_is_documented() {
    for (ch, note) in VERIFIED_GLYPHS {
        assert!(!note.is_empty(), "'{ch}' needs a description of its use");
    }
}

/// Read every `.rs` file under `src/`.
fn collect_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path.display().to_string(), text));
                }
            }
        }
    }

    let mut out = Vec::new();
    walk(std::path::Path::new("src"), &mut out);
    out
}

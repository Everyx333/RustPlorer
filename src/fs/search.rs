//! Real-time fuzzy filtering of the current listing.
//!
//! Runs synchronously on the UI thread, which is safe because it operates on
//! the already-loaded in-memory listing — no I/O. `nucleo-matcher` is the
//! matcher behind Helix and Telescope; it scores in the microseconds per item
//! range, so even 200k entries filter within a frame budget.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::fs::entry::Entry;

/// Filters entries by a fuzzy query.
pub struct SearchFilter {
    matcher: Matcher,
    query: String,
    /// Indices into the source listing that matched, best score first.
    matches: Vec<usize>,
    dirty: bool,
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchFilter {
    pub fn new() -> Self {
        // `match_paths` tunes scoring for path-like input: separators are
        // treated as boundaries, so "sr/mn" sensibly matches "src/main.rs".
        Self {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            query: String::new(),
            matches: Vec::new(),
            dirty: false,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    /// Update the query. Marks the filter dirty if it changed.
    pub fn set_query(&mut self, q: impl Into<String>) {
        let q = q.into();
        if q != self.query {
            self.query = q;
            self.dirty = true;
        }
    }

    pub fn clear(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
            self.dirty = true;
        }
    }

    /// Mark the results stale — call when the underlying listing changes.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Recompute matches if needed. Cheap no-op when nothing changed, so it is
    /// safe to call every frame.
    pub fn refresh(&mut self, entries: &[Entry]) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.matches.clear();

        if self.query.is_empty() {
            return;
        }

        // Smart case: lowercase queries match case-insensitively, but typing a
        // capital signals intent and narrows the search.
        let pattern = Pattern::parse(
            &self.query,
            CaseMatching::Smart,
            Normalization::Smart,
        );

        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = Vec::new();

        for (idx, entry) in entries.iter().enumerate() {
            buf.clear();
            let haystack = nucleo_matcher::Utf32Str::new(&entry.name, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut self.matcher) {
                scored.push((score, idx));
            }
        }

        // Highest score first; ties broken by original order so results don't
        // jitter between frames.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().map(|(_, i)| i).collect();
    }

    /// Indices of matching entries, or `None` when the filter is inactive
    /// (meaning: show everything).
    pub fn matched_indices(&self) -> Option<&[usize]> {
        if self.query.is_empty() {
            None
        } else {
            Some(&self.matches)
        }
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::entry::EntryKind;
    use std::path::PathBuf;

    fn entries(names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|n| Entry {
                path: PathBuf::from(n),
                name: n.to_string(),
                kind: EntryKind::File,
                size: Some(0),
                modified: None,
                is_hidden: false,
                is_readonly: false,
                extension: None,
            })
            .collect()
    }

    #[test]
    fn empty_query_matches_everything() {
        let mut f = SearchFilter::new();
        let e = entries(&["alpha.txt", "beta.rs"]);
        f.refresh(&e);
        assert!(f.matched_indices().is_none());
    }

    #[test]
    fn fuzzy_matches_subsequence() {
        let mut f = SearchFilter::new();
        let e = entries(&["main.rs", "cargo.toml", "readme.md"]);

        f.set_query("mrs");
        f.refresh(&e);

        let m = f.matched_indices().unwrap();
        assert!(!m.is_empty(), "expected 'mrs' to fuzzy-match 'main.rs'");
        assert_eq!(e[m[0]].name, "main.rs");
    }

    #[test]
    fn non_matching_query_returns_nothing() {
        let mut f = SearchFilter::new();
        let e = entries(&["alpha.txt"]);

        f.set_query("zzzzzz");
        f.refresh(&e);

        assert_eq!(f.match_count(), 0);
    }

    #[test]
    fn clearing_restores_all() {
        let mut f = SearchFilter::new();
        let e = entries(&["alpha.txt", "beta.rs"]);

        f.set_query("alpha");
        f.refresh(&e);
        assert!(f.matched_indices().is_some());

        f.clear();
        f.refresh(&e);
        assert!(f.matched_indices().is_none());
    }

    #[test]
    fn exact_prefix_outranks_scattered_match() {
        let mut f = SearchFilter::new();
        let e = entries(&["xxtestxx.txt", "test.txt"]);

        f.set_query("test");
        f.refresh(&e);

        let m = f.matched_indices().unwrap();
        assert_eq!(e[m[0]].name, "test.txt", "cleanest match should rank first");
    }
}

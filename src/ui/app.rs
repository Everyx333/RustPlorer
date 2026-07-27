//! Application state and the eframe entry point.

use std::path::PathBuf;

use crate::core::logging::Diagnostics;
use crate::core::paths::AppPaths;
use crate::core::task::WorkerPool;
use crate::fs::entry::{sort_entries, Entry, SortKey, SortOrder};
use crate::fs::scanner::{quick_access, ScanUpdate, Scanner};

/// Status of the directory currently being displayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// A scan is running. Entries may already be partially populated —
    /// we show them as they stream in rather than blocking on completion.
    Loading,
    Loaded,
    Failed(String),
}

/// One browsing context. A tab owns its location, listing, and scroll state.
pub struct Tab {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub state: LoadState,
    /// Generation of the scan currently populating `entries`. Used to discard
    /// late-arriving batches from a superseded navigation.
    pub generation: u64,
    pub selected: Option<usize>,
    pub history: Vec<PathBuf>,
    pub history_pos: usize,
    /// Set when a tab is restored from a saved session but not yet visited.
    /// Deferring the scan until activation keeps startup fast and flat in
    /// memory regardless of how many tabs were saved.
    pub needs_scan: bool,
}

impl Tab {
    pub fn new(path: PathBuf) -> Self {
        Self {
            history: vec![path.clone()],
            history_pos: 0,
            path,
            entries: Vec::new(),
            state: LoadState::Loading,
            generation: 0,
            selected: None,
            needs_scan: true,
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.history_pos > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_pos + 1 < self.history.len()
    }

    /// Record a navigation, truncating any forward history.
    pub fn push_history(&mut self, path: PathBuf) {
        // Standard browser semantics: navigating from a back-position discards
        // the forward entries.
        self.history.truncate(self.history_pos + 1);
        self.history.push(path);
        self.history_pos = self.history.len() - 1;
    }
}

/// Top-level application state.
pub struct RustPlorer {
    pub pool: WorkerPool,
    pub scanner: Scanner,
    pub paths: AppPaths,
    pub diagnostics: Diagnostics,

    pub tabs: Vec<Tab>,
    pub active_tab: usize,

    pub drives: Vec<PathBuf>,
    pub quick_access: Vec<(String, PathBuf)>,

    pub sort_key: SortKey,
    pub sort_order: SortOrder,
    pub show_hidden: bool,

    /// Set once the first frame has painted, so startup work can be deferred
    /// until after the window is visible.
    first_frame_done: bool,
}

impl RustPlorer {
    pub fn new(paths: AppPaths, diagnostics: Diagnostics) -> Self {
        // Cap worker count. On a 32-core machine an uncapped pool would run 32
        // concurrent scans and thrash the disk queue rather than going faster.
        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4);

        let start_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            pool: WorkerPool::new(worker_count),
            scanner: Scanner::new(),
            drives: crate::fs::scanner::list_drives(),
            quick_access: quick_access(),
            tabs: vec![Tab::new(start_dir)],
            active_tab: 0,
            sort_key: SortKey::Name,
            sort_order: SortOrder::Ascending,
            show_hidden: false,
            first_frame_done: false,
            paths,
            diagnostics,
        }
    }

    pub fn active(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn active_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    /// Navigate the active tab to `path`.
    pub fn navigate(&mut self, path: PathBuf, record_history: bool) {
        let show_hidden = self.show_hidden;

        {
            let tab = self.active_mut();
            if record_history {
                tab.push_history(path.clone());
            }
            tab.path = path.clone();
            tab.entries.clear();
            tab.selected = None;
            tab.state = LoadState::Loading;
            tab.needs_scan = false;
        }

        // Bumping the generation here is what makes leaving a slow directory
        // instant — the outstanding scan is abandoned rather than awaited.
        self.scanner.scan_dir(&self.pool, path, show_hidden);
        self.tabs[self.active_tab].generation = self.pool.generation().current();
    }

    pub fn go_back(&mut self) {
        if !self.active().can_go_back() {
            return;
        }
        let tab = self.active_mut();
        tab.history_pos -= 1;
        let path = tab.history[tab.history_pos].clone();
        self.navigate(path, false);
    }

    pub fn go_forward(&mut self) {
        if !self.active().can_go_forward() {
            return;
        }
        let tab = self.active_mut();
        tab.history_pos += 1;
        let path = tab.history[tab.history_pos].clone();
        self.navigate(path, false);
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.active().path.parent().map(|p| p.to_path_buf()) {
            self.navigate(parent, true);
        }
    }

    pub fn refresh(&mut self) {
        let path = self.active().path.clone();
        self.navigate(path, false);
    }

    /// Drain scan results and fold them into the active tab.
    ///
    /// Called once per frame. Never blocks.
    fn apply_scan_updates(&mut self) {
        let updates = self.scanner.poll();
        if updates.is_empty() {
            return;
        }

        let mut dirty = false;

        for update in updates {
            match update {
                ScanUpdate::Batch {
                    generation,
                    path,
                    entries,
                } => {
                    if let Some(tab) = self.tab_for(&path, generation) {
                        tab.entries.extend(entries);
                        dirty = true;
                    }
                }
                ScanUpdate::Done {
                    generation,
                    path,
                    total,
                } => {
                    if let Some(tab) = self.tab_for(&path, generation) {
                        tab.state = LoadState::Loaded;
                        dirty = true;
                        tracing::debug!(?path, total, "listing ready");
                    }
                }
                ScanUpdate::Failed {
                    generation,
                    path,
                    error,
                } => {
                    if let Some(tab) = self.tab_for(&path, generation) {
                        tab.state = LoadState::Failed(error);
                    }
                }
                ScanUpdate::Cancelled { .. } => {
                    // Expected during fast navigation; nothing to do.
                }
            }
        }

        if dirty {
            let (key, order) = (self.sort_key, self.sort_order);
            // Re-sort after each batch so the partial listing is always
            // correctly ordered rather than jumping around as data arrives.
            sort_entries(&mut self.tabs[self.active_tab].entries, key, order);
        }
    }

    /// Find the tab a scan result belongs to, ignoring stale generations.
    fn tab_for(&mut self, path: &PathBuf, generation: u64) -> Option<&mut Tab> {
        let tab = &mut self.tabs[self.active_tab];
        if &tab.path == path && tab.generation == generation {
            Some(tab)
        } else {
            None
        }
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_order = self.sort_order.toggled();
        } else {
            self.sort_key = key;
            self.sort_order = SortOrder::Ascending;
        }
        let (k, o) = (self.sort_key, self.sort_order);
        sort_entries(&mut self.tabs[self.active_tab].entries, k, o);
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh();
    }

    /// Build a diagnostics report for the clipboard.
    ///
    /// This exists because bug reports arrive as prose. A one-click dump of
    /// version, environment, and recent logs turns "it froze" into something
    /// actionable.
    pub fn diagnostics_report(&self) -> String {
        let mut out = String::new();
        out.push_str("=== RustPlorer Diagnostics ===\n");
        out.push_str(&format!("version: {}\n", env!("CARGO_PKG_VERSION")));
        out.push_str(&format!("os: {}\n", std::env::consts::OS));
        out.push_str(&format!("arch: {}\n", std::env::consts::ARCH));
        out.push_str(&format!("workers: {}\n", self.pool.size()));
        out.push_str(&format!("log_dir: {:?}\n", self.diagnostics.log_dir));
        out.push_str(&format!("current_path: {:?}\n", self.active().path));
        out.push_str(&format!("entries_loaded: {}\n", self.active().entries.len()));
        out.push_str("\n=== Recent log ===\n");
        out.push_str(&self.diagnostics.ring.dump());
        out
    }
}

impl eframe::App for RustPlorer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Defer the initial scan until after the first paint, so the window
        // appears immediately instead of after the home directory is read.
        if !self.first_frame_done {
            self.first_frame_done = true;
            let path = self.active().path.clone();
            self.navigate(path, false);
        }

        self.apply_scan_updates();

        crate::ui::sidebar::draw(self, ctx);
        crate::ui::file_table::draw(self, ctx);

        // Repaint while a scan is streaming so incoming batches are shown
        // promptly. When idle, egui sleeps — which is what keeps a file manager
        // at ~0% CPU sitting in the background.
        if self.active().state == LoadState::Loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_truncates_forward_entries() {
        let mut tab = Tab::new(PathBuf::from("/a"));
        tab.push_history(PathBuf::from("/b"));
        tab.push_history(PathBuf::from("/c"));

        assert!(tab.can_go_back());
        assert!(!tab.can_go_forward());

        tab.history_pos = 0;
        assert!(tab.can_go_forward());

        // Navigating from a back-position must discard /b and /c.
        tab.push_history(PathBuf::from("/d"));
        assert_eq!(tab.history, vec![PathBuf::from("/a"), PathBuf::from("/d")]);
        assert!(!tab.can_go_forward());
    }

    #[test]
    fn new_tab_starts_at_history_root() {
        let tab = Tab::new(PathBuf::from("/start"));
        assert!(!tab.can_go_back());
        assert!(!tab.can_go_forward());
        assert!(tab.needs_scan);
    }
}

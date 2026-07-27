//! Application state and the eframe entry point.

use std::path::PathBuf;

use crate::archive::browser::{ArchiveBrowser, ArchiveLocation};
use crate::core::config::Config;
use crate::core::logging::Diagnostics;
use crate::core::paths::AppPaths;
use crate::core::task::WorkerPool;
use crate::fs::entry::{sort_entries, Entry, SortKey, SortOrder};
use crate::fs::ops::{FileOp, OpRunner, OpUpdate};
use crate::fs::scanner::{quick_access, ScanUpdate, Scanner};
use crate::fs::search::SearchFilter;
use crate::fs::sizer::{FolderSizer, SizeState};
use crate::fs::thumbs::{ThumbState, ThumbnailCache};
use crate::fs::watcher::DirWatcher;
use crate::ui::first_run::FirstRunStage;
use crate::ui::palette::{Command, CommandPalette};
use crate::ui::preview::Previewer;
use crate::ui::settings::SettingsTab;

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

    /// Second pane's tab. Owns its own location and history, so the two panes
    /// browse independently -- the point of a dual-pane manager.
    pub right_tab: Option<Tab>,
    /// Which pane has focus. Navigation and shortcuts apply to this one.
    pub right_focused: bool,

    pub drives: Vec<PathBuf>,
    pub quick_access: Vec<(String, PathBuf)>,

    /// Set when browsing inside an archive rather than the filesystem.
    pub archive_location: Option<ArchiveLocation>,
    pub archives: ArchiveBrowser,

    pub sizer: FolderSizer,
    pub thumbs: ThumbnailCache,
    pub search: SearchFilter,
    pub watcher: DirWatcher,
    pub ops: OpRunner,

    /// Progress line for a running file operation, if any.
    pub op_status: Option<String>,
    /// Last error, shown as a dismissible banner.
    pub error_banner: Option<String>,

    /// Persistent user settings.
    pub config: Config,
    /// Whether the second pane is visible.
    pub dual_pane: bool,
    /// Text box contents for naming a new workspace.
    pub workspace_name_input: String,
    pub settings_open: bool,
    pub settings_tab: SettingsTab,
    /// First-run prompt offering to install 7-Zip or WinRAR.
    pub first_run_stage: FirstRunStage,
    pub palette: CommandPalette,
    pub previewer: Previewer,
    /// Set when config changes, so we save once on a debounce rather than
    /// writing to disk on every slider tick.
    config_dirty: bool,
    last_config_save: std::time::Instant,

    /// Focus the search box on the next frame (set by Ctrl+F).
    pub focus_search: bool,

    pub sort_key: SortKey,
    pub sort_order: SortOrder,
    pub show_hidden: bool,

    /// Set once the first frame has painted, so startup work can be deferred
    /// until after the window is visible.
    first_frame_done: bool,
}

impl RustPlorer {
    pub fn new(paths: AppPaths, diagnostics: Diagnostics) -> Self {
        let config = Config::load(paths.config_file().as_deref());

        // Worker count comes from the performance profile, which scales with
        // the machine rather than a fixed cap.
        let worker_count = config.performance.effective_workers();

        // Honour "reopen last folder", falling back to home.
        let start_dir = config
            .behavior
            .restore_last_path
            .then(|| config.behavior.last_path.clone())
            .flatten()
            .filter(|p| p.is_dir())
            .or_else(|| directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));

        let sizer = FolderSizer::new();
        sizer.set_max_concurrent(config.performance.effective_size_walks());

        let thumbs =
            ThumbnailCache::new(config.performance.thumbnail_cache_mb * 1024 * 1024);

        Self {
            pool: WorkerPool::new(worker_count),
            scanner: Scanner::new(),
            drives: crate::fs::scanner::list_drives(),
            quick_access: quick_access(),
            tabs: vec![Tab::new(start_dir)],
            active_tab: 0,
            right_tab: None,
            right_focused: false,
            archive_location: None,
            archives: ArchiveBrowser::new(),
            sizer,
            thumbs,
            search: SearchFilter::new(),
            watcher: DirWatcher::new(),
            ops: OpRunner::new(),
            op_status: None,
            error_banner: None,
            dual_pane: false,
            workspace_name_input: String::new(),
            settings_open: false,
            settings_tab: SettingsTab::Performance,
            first_run_stage: FirstRunStage::Hidden,
            palette: CommandPalette::new(),
            previewer: Previewer::new(),
            config_dirty: false,
            last_config_save: std::time::Instant::now(),
            focus_search: false,
            sort_key: SortKey::Name,
            sort_order: SortOrder::Ascending,
            show_hidden: config.behavior.show_hidden,
            first_frame_done: false,
            config,
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

    /// Navigate the active tab to `path` on the real filesystem.
    pub fn navigate(&mut self, path: PathBuf, record_history: bool) {
        // Any filesystem navigation exits archive-browsing mode.
        if self.archive_location.is_some() {
            self.archive_location = None;
            self.archives.close();
        }

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
        self.scanner.scan_dir(&self.pool, path.clone(), show_hidden);
        self.tabs[self.active_tab].generation = self.pool.generation().current();

        // Watch the new location for live updates, and invalidate any active
        // filter since it refers to the previous listing.
        if self.config.behavior.live_refresh {
            self.watcher.watch(path);
        } else {
            self.watcher.stop();
        }
        self.search.invalidate();
    }

    /// Run a command from the palette or a keyboard shortcut.
    pub fn run_command(&mut self, cmd: Command) {
        tracing::debug!(?cmd, "running command");

        match cmd {
            Command::GoBack => self.go_back(),
            Command::GoForward => self.go_forward(),
            Command::GoUp => self.go_up(),
            Command::Refresh => {
                self.sizer.clear();
                self.refresh();
            }
            Command::ToggleHidden => self.toggle_hidden(),
            Command::ToggleFolderSizes => {
                let on = !self.config.performance.folder_sizes_enabled;
                self.config.performance.folder_sizes_enabled = on;
                if !on {
                    self.sizer.clear();
                }
                self.config_dirty = true;
            }
            Command::FocusSearch => self.focus_search = true,
            Command::OpenSettings => self.settings_open = true,
            Command::CopyDiagnostics => {
                let report = self.diagnostics_report();
                self.copy_to_clipboard(report, "Diagnostics copied");
            }
            Command::CopyPath => {
                let path = self.active().path.display().to_string();
                self.copy_to_clipboard(path, "Path copied");
            }
            Command::NewFolder => self.create_new_folder(),
            Command::DeleteSelected => self.trash_selected(),
            Command::OpenInExplorer => {
                let path = self.active().path.clone();
                if let Err(e) = open::that_detached(&path) {
                    tracing::warn!(error = %e, "could not open in shell");
                }
            }
            Command::TogglePane => self.toggle_second_pane(),
            Command::CloseArchive => {
                if let Some(loc) = self.archive_location.clone() {
                    let containing = loc
                        .archive
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| PathBuf::from("."));
                    self.navigate(containing, true);
                }
            }
        }
    }

    /// Put text on the clipboard, reporting failure rather than silently
    /// doing nothing.
    fn copy_to_clipboard(&mut self, text: String, success_msg: &str) {
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
            Ok(()) => tracing::info!("{success_msg}"),
            Err(e) => {
                tracing::warn!(error = %e, "clipboard unavailable");
                self.error_banner = Some("Could not access the clipboard.".to_string());
            }
        }
    }

    /// Create "New folder" in the current directory, numbering to avoid
    /// clobbering an existing one.
    fn create_new_folder(&mut self) {
        if self.in_archive() {
            self.error_banner =
                Some("Cannot create folders inside an archive.".to_string());
            return;
        }

        let base = self.active().path.clone();
        let mut candidate = base.join("New folder");
        let mut n = 2;
        while candidate.exists() && n < 1000 {
            candidate = base.join(format!("New folder ({n})"));
            n += 1;
        }

        self.submit_op(FileOp::CreateDir { path: candidate });
    }

    /// Show or hide the second pane.
    ///
    /// The new pane opens at the current location, which is what you almost
    /// always want: split, then navigate one side to the destination.
    pub fn toggle_second_pane(&mut self) {
        self.dual_pane = !self.dual_pane;

        if self.dual_pane && self.right_tab.is_none() {
            let start = self.active().path.clone();
            let mut tab = Tab::new(start.clone());
            tab.state = LoadState::Loaded;
            // Copy the current listing rather than rescanning: the data is
            // already in memory and identical.
            tab.entries = self.active().entries.clone();
            self.right_tab = Some(tab);
        }

        if !self.dual_pane {
            // Focus must return to the left pane, or shortcuts would target a
            // pane that is no longer visible.
            self.right_focused = false;
        }

        tracing::debug!(enabled = self.dual_pane, "dual pane toggled");
    }

    /// Navigate the second pane.
    ///
    /// Scans synchronously off a dedicated read rather than the shared
    /// scanner channel, which is keyed to the active tab. The second pane is a
    /// destination picker, so a simple bounded read is the right trade.
    pub fn navigate_right_pane(&mut self, path: PathBuf) {
        let show_hidden = self.show_hidden;

        let Some(tab) = &mut self.right_tab else { return };
        tab.push_history(path.clone());
        tab.path = path.clone();
        tab.selected = None;
        tab.entries.clear();
        tab.state = LoadState::Loading;

        // Read directly. Second-pane directories are browsed deliberately, and
        // this keeps the pane's state independent of the main scanner.
        let mut entries = Vec::new();
        if let Ok(read) = std::fs::read_dir(&path) {
            for dir_entry in read.flatten() {
                let Ok(meta) = dir_entry.metadata() else {
                    continue;
                };
                let entry = Entry::from_metadata(dir_entry.path(), &meta);
                if entry.is_hidden && !show_hidden {
                    continue;
                }
                entries.push(entry);
            }
        }

        let (key, order) = (self.sort_key, self.sort_order);
        sort_entries(&mut entries, key, order);

        if let Some(tab) = &mut self.right_tab {
            tab.entries = entries;
            tab.state = LoadState::Loaded;
        }
    }

    /// Save the current pane arrangement under a name.
    pub fn save_workspace(&mut self, name: String) {
        let ws = crate::core::config::Workspace {
            name: name.clone(),
            left_path: self.tabs[self.active_tab].path.clone(),
            right_path: self.right_tab.as_ref().map(|t| t.path.clone()),
            dual_pane: self.dual_pane,
        };

        // Overwrite an existing workspace with the same name rather than
        // silently creating a duplicate the user cannot tell apart.
        if let Some(existing) = self.config.workspaces.iter_mut().find(|w| w.name == name) {
            *existing = ws;
        } else {
            self.config.workspaces.push(ws);
        }

        self.config_dirty = true;
        tracing::info!(%name, "workspace saved");
    }

    /// Restore a saved arrangement.
    pub fn load_workspace(&mut self, name: &str) {
        let Some(ws) = self.config.workspaces.iter().find(|w| w.name == name).cloned() else {
            return;
        };

        self.dual_pane = ws.dual_pane;

        if let Some(right) = ws.right_path {
            // Create the pane but do NOT scan it yet -- it is populated when
            // navigated to, keeping restore cost flat regardless of how many
            // panes were saved.
            let mut tab = Tab::new(right);
            tab.state = LoadState::Loading;
            self.right_tab = Some(tab);
        } else {
            self.right_tab = None;
        }

        // Only the active pane scans eagerly.
        self.navigate(ws.left_path, true);

        if self.dual_pane {
            if let Some(path) = self.right_tab.as_ref().map(|t| t.path.clone()) {
                self.navigate_right_pane(path);
            }
        }

        tracing::info!(%name, "workspace restored");
    }

    /// Delete a saved workspace.
    pub fn delete_workspace(&mut self, name: &str) {
        self.config.workspaces.retain(|w| w.name != name);
        self.config_dirty = true;
    }

    /// Move focus between panes.
    pub fn focus_other_pane(&mut self) {
        if self.dual_pane {
            self.right_focused = !self.right_focused;
        }
    }

    /// Copy the selection from the focused pane to the other pane's folder.
    ///
    /// This is the core dual-pane workflow: two locations on screen, one key
    /// to move between them.
    pub fn transfer_to_other_pane(&mut self, move_files: bool) {
        if !self.dual_pane {
            return;
        }
        if self.in_archive() {
            self.error_banner =
                Some("Cannot copy out of an archive yet.".to_string());
            return;
        }

        let (source, dest) = if self.right_focused {
            let Some(right) = &self.right_tab else { return };
            (right.selected.and_then(|i| right.entries.get(i)).cloned(),
             self.tabs[self.active_tab].path.clone())
        } else {
            let left = &self.tabs[self.active_tab];
            let Some(right) = &self.right_tab else { return };
            (left.selected.and_then(|i| left.entries.get(i)).cloned(),
             right.path.clone())
        };

        let Some(entry) = source else {
            self.error_banner = Some("Nothing selected.".to_string());
            return;
        };

        let op = if move_files {
            FileOp::Move {
                sources: vec![entry.path.clone()],
                dest_dir: dest,
                policy: crate::fs::ops::ConflictPolicy::Rename,
            }
        } else {
            FileOp::Copy {
                sources: vec![entry.path.clone()],
                dest_dir: dest,
                policy: crate::fs::ops::ConflictPolicy::Rename,
            }
        };

        self.submit_op(op);
    }

    /// Preview the selected file.
    pub fn toggle_preview(&mut self) {
        if self.in_archive() {
            return;
        }
        let Some(idx) = self.active().selected else {
            return;
        };
        let Some(entry) = self.active().entries.get(idx) else {
            return;
        };
        if entry.is_dir() {
            return;
        }

        let path = entry.path.clone();
        self.previewer.toggle(&self.pool, path);
    }

    /// Look for an installed 7-Zip/WinRAR, and offer to install one if none
    /// is present.
    ///
    /// Runs once after the first paint so it never delays the window
    /// appearing. The prompt is shown only when there is genuinely nothing
    /// installed and the user has not already answered.
    fn detect_external_tool(&mut self) {
        match crate::archive::external::detect() {
            Some(tool) => {
                tracing::info!(tool = tool.name, "external archive tool available");
                self.config.archive.external_tool_path = Some(tool.path);
                self.config_dirty = true;
            }
            None => {
                self.config.archive.external_tool_path = None;

                // Only ask if they haven't answered before. "Ask me later"
                // deliberately leaves this flag unset so it returns.
                if !self.config.archive.external_tool_prompted {
                    self.first_run_stage = FirstRunStage::Ask;
                }
            }
        }
    }

    /// True when the listing shows archive contents rather than the disk.
    pub fn in_archive(&self) -> bool {
        self.archive_location.is_some()
    }

    /// Open an archive and browse its root.
    pub fn open_archive(&mut self, archive: PathBuf) {
        tracing::info!(?archive, "opening archive");
        self.archives.open(&self.pool, archive.clone());
        self.archive_location = Some(ArchiveLocation::root(archive));
        self.search.invalidate();
    }

    /// Move to a folder inside the open archive.
    pub fn enter_archive_dir(&mut self, name: &str) {
        if let Some(loc) = &self.archive_location {
            self.archive_location = Some(loc.child(name));
            self.search.invalidate();
        }
    }

    /// Go up inside the archive, leaving it entirely at the root.
    pub fn archive_go_up(&mut self) {
        let Some(loc) = self.archive_location.clone() else {
            return;
        };

        match loc.parent() {
            Some(parent) => {
                self.archive_location = Some(parent);
                self.search.invalidate();
            }
            None => {
                // At the archive root, "up" means back to the containing
                // folder on disk.
                let containing = loc
                    .archive
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
                self.navigate(containing, true);
            }
        }
    }

    /// Entries to display: archive contents when browsing one, else the disk
    /// listing. Converted to `Entry` so the table renders both identically.
    pub fn archive_entries_as_listing(&self) -> Vec<Entry> {
        let Some(loc) = &self.archive_location else {
            return Vec::new();
        };

        self.archives
            .entries_at(loc)
            .into_iter()
            .map(|a| Entry {
                path: PathBuf::from(&a.path),
                name: a.name().to_string(),
                kind: if a.is_dir {
                    crate::fs::entry::EntryKind::Directory
                } else {
                    crate::fs::entry::EntryKind::File
                },
                // Archives know their sizes up front, so unlike real folders
                // these need no background walk.
                size: Some(a.size),
                modified: a.modified,
                is_hidden: false,
                is_readonly: true,
                extension: std::path::Path::new(&a.path)
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase()),
            })
            .collect()
    }

    /// Ask for folder sizes for the directories currently on screen.
    ///
    /// Called with the visible row range each frame. Requesting only what is
    /// visible is what keeps this affordable in a folder with thousands of
    /// subdirectories.
    pub fn request_visible_sizes(&mut self, visible: std::ops::Range<usize>) {
        if !self.config.performance.folder_sizes_enabled {
            return;
        }

        let paths: Vec<PathBuf> = {
            let entries = &self.tabs[self.active_tab].entries;
            visible
                .filter_map(|i| entries.get(i))
                .filter(|e| e.is_dir())
                .map(|e| e.path.clone())
                .collect()
        };

        for p in paths {
            self.sizer.request(&self.pool, p);
        }
    }

    /// Size for a directory, if known.
    pub fn folder_size(&self, path: &std::path::Path) -> Option<SizeState> {
        self.sizer.get(path)
    }

    /// Fold in filesystem change notifications.
    fn apply_watch_events(&mut self) {
        let events = self.watcher.poll();
        if events.is_empty() {
            return;
        }

        // Only react to changes in the directory we are actually showing.
        let current = self.active().path.clone();
        if events.iter().any(|e| e.path == current) {
            tracing::debug!(?current, "directory changed on disk; refreshing");
            // Sizes may be stale now, so drop the cache for this listing.
            self.sizer.clear();
            self.refresh();
        }
    }

    /// Persist config, debounced.
    ///
    /// Dragging a slider fires `changed()` every frame; writing the file each
    /// time would mean hundreds of disk writes for one adjustment.
    fn maybe_save_config(&mut self) {
        if !self.config_dirty {
            return;
        }
        if self.last_config_save.elapsed() < std::time::Duration::from_millis(800) {
            return;
        }

        self.config_dirty = false;
        self.last_config_save = std::time::Instant::now();

        // Record the current folder so it can be restored next launch.
        if self.config.behavior.restore_last_path {
            self.config.behavior.last_path = Some(self.active().path.clone());
        }

        if let Err(e) = self.config.save(self.paths.config_file().as_deref()) {
            tracing::warn!(error = %e, "could not save config");
        }
    }

    /// Mark config as needing a save, and apply anything that takes effect
    /// immediately.
    pub fn config_changed(&mut self) {
        self.config_dirty = true;

        // Concurrency applies live; worker count needs a restart.
        self.sizer
            .set_max_concurrent(self.config.performance.effective_size_walks());
        self.thumbs
            .set_budget(self.config.performance.thumbnail_cache_mb * 1024 * 1024);
        if !self.config.performance.thumbnails_enabled {
            self.thumbs.clear();
        }

        if self.show_hidden != self.config.behavior.show_hidden {
            self.show_hidden = self.config.behavior.show_hidden;
            self.refresh();
        }
    }

    /// Apply appearance settings to the egui context.
    fn apply_appearance(&self, ctx: &egui::Context) {
        let a = &self.config.appearance;

        if let Some(dark) = a.dark_mode {
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }

        ctx.style_mut(|style| {
            for (_, font) in style.text_styles.iter_mut() {
                font.size = a.font_size;
            }
            style.spacing.item_spacing.y = 4.0 * a.row_spacing;
        });
    }

    /// Request a thumbnail for a visible row.
    pub fn thumbnail_for(&self, path: &std::path::Path) -> Option<ThumbState> {
        if !self.config.performance.thumbnails_enabled {
            return None;
        }
        Some(self.thumbs.get_or_request(&self.pool, path))
    }

    /// Fold in folder-size results.
    fn apply_size_updates(&mut self) {
        // Results are read from the sizer's cache during render; draining the
        // channel here just keeps it from growing unbounded and lets us log.
        let _ = self.sizer.poll();
    }

    /// Fold in file-operation progress.
    fn apply_op_updates(&mut self) {
        for update in self.ops.poll() {
            match update {
                OpUpdate::Progress {
                    label,
                    current_file,
                    files_done,
                    files_total,
                    bytes_done,
                    bytes_total,
                    ..
                } => {
                    let pct = if bytes_total > 0 {
                        (bytes_done as f64 / bytes_total as f64 * 100.0) as u32
                    } else {
                        0
                    };
                    self.op_status = Some(format!(
                        "{label} {current_file} — {files_done}/{files_total} files, {pct}%"
                    ));
                }
                OpUpdate::Finished { touched, skipped, .. } => {
                    self.op_status = None;
                    if skipped > 0 {
                        self.error_banner =
                            Some(format!("{skipped} item(s) skipped because they already exist."));
                    }
                    // Refresh if the operation touched what we are viewing.
                    let current = self.active().path.clone();
                    if touched.iter().any(|p| p == &current) {
                        self.sizer.clear();
                        self.refresh();
                    }
                }
                OpUpdate::Failed { error, .. } => {
                    self.op_status = None;
                    self.error_banner = Some(error);
                }
                OpUpdate::Cancelled { .. } => {
                    self.op_status = None;
                }
            }
        }
    }

    /// Queue a file operation.
    pub fn submit_op(&mut self, op: FileOp) {
        self.ops.submit(&self.pool, op);
    }

    /// Send the current selection to the Recycle Bin.
    pub fn trash_selected(&mut self) {
        let Some(idx) = self.active().selected else {
            return;
        };
        let Some(entry) = self.active().entries.get(idx) else {
            return;
        };
        let path = entry.path.clone();
        self.submit_op(FileOp::Trash { paths: vec![path] });
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
        if self.in_archive() {
            self.archive_go_up();
            return;
        }
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
        self.config.behavior.show_hidden = self.show_hidden;
        self.config_dirty = true;
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
        out.push_str(&format!(
            "thumbnail_cache: {} entries, {}\n",
            self.thumbs.len(),
            humansize::format_size(self.thumbs.bytes_used() as u64, humansize::DECIMAL)
        ));
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
            self.detect_external_tool();
        }

        self.apply_scan_updates();

        // Surface archive-loading failures as a banner.
        if let Some(err) = self.archives.poll() {
            self.error_banner = Some(err);
            self.archive_location = None;
        }

        // While browsing an archive the listing comes from memory, so refresh
        // it into the tab each frame.
        if self.in_archive() {
            let listing = self.archive_entries_as_listing();
            let (key, order) = (self.sort_key, self.sort_order);
            self.tabs[self.active_tab].entries = listing;
            sort_entries(&mut self.tabs[self.active_tab].entries, key, order);
            self.tabs[self.active_tab].state = if self.archives.is_loading() {
                LoadState::Loading
            } else {
                LoadState::Loaded
            };
        }

        self.apply_watch_events();
        self.apply_size_updates();

        // New thumbnails need a repaint to become visible.
        if self.thumbs.poll() > 0 {
            ctx.request_repaint();
        }
        self.apply_op_updates();

        // Recompute the filter if the query or listing changed. Cheap no-op
        // otherwise.
        let entries = std::mem::take(&mut self.tabs[self.active_tab].entries);
        self.search.refresh(&entries);
        self.tabs[self.active_tab].entries = entries;

        handle_shortcuts(self, ctx);
        self.apply_appearance(ctx);

        crate::ui::sidebar::draw(self, ctx);
        crate::ui::file_table::draw(self, ctx);

        if crate::ui::settings::draw(self, ctx) {
            self.config_changed();
        }

        if crate::ui::first_run::draw(self, ctx) {
            self.config_dirty = true;
        }

        self.previewer.poll();
        crate::ui::preview::draw(self, ctx);

        if let Some(cmd) = crate::ui::palette::draw(self, ctx) {
            self.run_command(cmd);
        }

        self.maybe_save_config();

        // Repaint while a scan is streaming so incoming batches are shown
        // promptly. When idle, egui sleeps — which is what keeps a file manager
        // at ~0% CPU sitting in the background.
        // Repaint while work is streaming in. When fully idle egui sleeps,
        // which is what keeps background CPU near zero.
        let busy = self.active().state == LoadState::Loading || self.op_status.is_some();
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        } else if self.config.performance.folder_sizes_enabled {
            // Slower tick while folder sizes count up.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

/// Global keyboard shortcuts.
fn handle_shortcuts(app: &mut RustPlorer, ctx: &egui::Context) {
    // The command palette must work from anywhere, including mid-typing --
    // that is the point of a palette. Its modifier combination is one no text
    // field consumes, so it is safe to check before the focus guard.
    if ctx.input(|i| i.key_pressed(egui::Key::P) && i.modifiers.ctrl && i.modifiers.shift) {
        app.palette.show();
        return;
    }

    // Everything below is a bare key, so it must not fire while a text field
    // has focus -- otherwise typing "f" in the filter box would navigate, and
    // Space would open a preview instead of inserting a space.
    if ctx.memory(|m| m.focused().is_some()) {
        // Escape still needs to work, to leave the search box.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.search.clear();
            ctx.memory_mut(|m| m.request_focus(egui::Id::NULL));
        }
        return;
    }

    let input = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::F5),
            i.key_pressed(egui::Key::Backspace),
            i.key_pressed(egui::Key::Delete),
            i.key_pressed(egui::Key::F) && i.modifiers.ctrl,
            i.key_pressed(egui::Key::ArrowLeft) && i.modifiers.alt,
            i.key_pressed(egui::Key::ArrowRight) && i.modifiers.alt,
            i.key_pressed(egui::Key::ArrowUp) && i.modifiers.alt,
            i.key_pressed(egui::Key::Escape),
        )
    });

    let (f5, backspace, delete, ctrl_f, alt_left, alt_right, alt_up, escape) = input;

    if f5 {
        app.sizer.clear();
        app.refresh();
    }
    if backspace || alt_up {
        app.go_up();
    }
    if alt_left {
        app.go_back();
    }
    if alt_right {
        app.go_forward();
    }
    if ctrl_f {
        app.focus_search = true;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Comma) && i.modifiers.ctrl) {
        app.settings_open = !app.settings_open;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
        app.toggle_preview();
    }
    // F6 switches panes (matching Explorer and most dual-pane managers);
    // Ctrl+F6 shows or hides the second pane.
    if ctx.input(|i| i.key_pressed(egui::Key::F6) && i.modifiers.ctrl) {
        app.toggle_second_pane();
    } else if ctx.input(|i| i.key_pressed(egui::Key::F6)) {
        app.focus_other_pane();
    }
    if ctx.input(|i| i.key_pressed(egui::Key::F5) && i.modifiers.ctrl) {
        app.transfer_to_other_pane(false);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::F6) && i.modifiers.shift) {
        app.transfer_to_other_pane(true);
    }
    if delete {
        app.trash_selected();
    }
    if escape {
        app.search.clear();
        app.error_banner = None;
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

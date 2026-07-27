//! Persistent user settings.
//!
//! # Durability rules
//!
//! A file manager that loses your settings — or worse, refuses to start
//! because of a malformed config — is a bad file manager. So:
//!
//! - **Atomic writes.** Write to a temp file, fsync, then rename over the
//!   target. A crash or power loss mid-save leaves the old config intact
//!   rather than a half-written one.
//! - **Every field defaults.** `#[serde(default)]` throughout, so a config
//!   written by an older version still loads when new fields appear.
//! - **Corrupt configs are quarantined, not fatal.** An unparseable file is
//!   renamed aside and defaults are used. The app always starts.
//! - **`schema_version`** is carried for future migrations.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bumped when a breaking change requires migration logic.
pub const SCHEMA_VERSION: u32 = 1;

/// How aggressively to use the machine's parallelism.
///
/// The right value depends on the *storage*, not the CPU. NVMe SSDs love deep
/// queues; spinning disks thrash when several walks seek at once. We cannot
/// reliably detect drive type across all Windows configurations, so this is
/// exposed rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerformanceProfile {
    /// Minimal parallelism. For spinning disks or slow network shares.
    Conservative,
    /// Scales with core count. Good default for SSDs.
    Balanced,
    /// Maximum parallelism. For fast NVMe on a many-core machine.
    Aggressive,
    /// Use the explicit worker/walk counts below.
    Custom,
}

impl Default for PerformanceProfile {
    fn default() -> Self {
        Self::Balanced
    }
}

impl PerformanceProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Conservative => "Conservative",
            Self::Balanced => "Balanced",
            Self::Aggressive => "Aggressive",
            Self::Custom => "Custom",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Conservative => "Fewer parallel operations. Best for spinning disks or network drives.",
            Self::Balanced => "Scales with your CPU. Good default for SSDs.",
            Self::Aggressive => "Maximum parallelism. Best for fast NVMe drives.",
            Self::Custom => "Use the values set below.",
        }
    }
}

/// Threading and I/O concurrency.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceConfig {
    pub profile: PerformanceProfile,
    /// Worker threads for scanning and file operations.
    /// Applies on restart — the pool is built at startup.
    pub worker_threads: usize,
    /// Concurrent recursive folder-size walks. Applies immediately.
    pub concurrent_size_walks: usize,
    /// Compute folder sizes at all.
    pub folder_sizes_enabled: bool,
    /// Rows beyond the viewport to pre-size, so sizes are ready when they
    /// scroll into view.
    pub size_lookahead_rows: usize,
    /// Show image thumbnails instead of a generic file icon.
    pub thumbnails_enabled: bool,
    /// Hard ceiling on thumbnail memory, in megabytes.
    pub thumbnail_cache_mb: usize,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        let cpus = cpu_count();
        Self {
            profile: PerformanceProfile::Balanced,
            worker_threads: balanced_workers(cpus),
            concurrent_size_walks: balanced_walks(cpus),
            folder_sizes_enabled: true,
            size_lookahead_rows: 20,
            thumbnails_enabled: true,
            thumbnail_cache_mb: 64,
        }
    }
}

impl PerformanceConfig {
    /// Effective worker count for the current profile.
    pub fn effective_workers(&self) -> usize {
        let cpus = cpu_count();
        match self.profile {
            PerformanceProfile::Conservative => 2,
            PerformanceProfile::Balanced => balanced_workers(cpus),
            PerformanceProfile::Aggressive => aggressive_workers(cpus),
            PerformanceProfile::Custom => self.worker_threads.clamp(1, 64),
        }
    }

    /// Effective concurrent size walks for the current profile.
    pub fn effective_size_walks(&self) -> usize {
        let cpus = cpu_count();
        match self.profile {
            PerformanceProfile::Conservative => 1,
            PerformanceProfile::Balanced => balanced_walks(cpus),
            PerformanceProfile::Aggressive => aggressive_walks(cpus),
            PerformanceProfile::Custom => self.concurrent_size_walks.clamp(1, 32),
        }
    }
}

fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Balanced: most logical cores, capped so we never monopolise the machine.
/// On a 12-thread CPU (e.g. Ryzen 5 5600) this yields 10.
fn balanced_workers(cpus: usize) -> usize {
    cpus.saturating_sub(2).clamp(2, 12)
}

/// Balanced size walks: half the cores. 12 threads → 6 concurrent walks.
/// Enough to saturate an SSD without turning an HDD into a seek storm.
fn balanced_walks(cpus: usize) -> usize {
    (cpus / 2).clamp(2, 8)
}

fn aggressive_workers(cpus: usize) -> usize {
    cpus.clamp(4, 32)
}

fn aggressive_walks(cpus: usize) -> usize {
    cpus.clamp(4, 16)
}

/// Appearance settings. Extended with full theming in a later phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Dark mode. `None` follows the system.
    pub dark_mode: Option<bool>,
    pub font_size: f32,
    /// Multiplier on default widget spacing. Lower is denser.
    pub row_spacing: f32,
    pub row_height: f32,
    pub striped_rows: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            dark_mode: None,
            font_size: 14.0,
            row_spacing: 1.0,
            row_height: 24.0,
            striped_rows: true,
        }
    }
}

/// Browsing behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    pub show_hidden: bool,
    /// Watch the current directory for changes.
    pub live_refresh: bool,
    /// Send deletions to the Recycle Bin instead of removing them outright.
    pub delete_to_recycle_bin: bool,
    /// Confirm before deleting.
    pub confirm_delete: bool,
    /// Restore the previous location at startup.
    pub restore_last_path: bool,
    pub last_path: Option<PathBuf>,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            show_hidden: false,
            live_refresh: true,
            delete_to_recycle_bin: true,
            confirm_delete: true,
            restore_last_path: true,
            last_path: None,
        }
    }
}

/// Archive handling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveConfig {
    /// Prefer an installed 7-Zip / WinRAR over the built-in Rust
    /// implementations when one is detected.
    pub prefer_external_tool: bool,
    /// Cached path to a detected external tool.
    pub external_tool_path: Option<PathBuf>,
    /// Set once the user has answered the "use your installed 7-Zip?" prompt,
    /// so it is never asked twice.
    pub external_tool_prompted: bool,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            prefer_external_tool: true,
            external_tool_path: None,
            external_tool_prompted: false,
        }
    }
}

/// A saved arrangement of panes and locations.
///
/// Restoring a workspace does NOT eagerly scan every location: only the active
/// pane is read on load. Scanning several directories at startup is exactly
/// the spike this app exists to avoid.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub name: String,
    pub left_path: PathBuf,
    /// `None` means the workspace was saved with a single pane.
    pub right_path: Option<PathBuf>,
    pub dual_pane: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            name: "Untitled".to_string(),
            left_path: PathBuf::from("."),
            right_path: None,
            dual_pane: false,
        }
    }
}

/// The complete configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub performance: PerformanceConfig,
    pub appearance: AppearanceConfig,
    pub behavior: BehaviorConfig,
    pub archive: ArchiveConfig,
    /// Saved pane arrangements, restorable by name.
    pub workspaces: Vec<Workspace>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            performance: PerformanceConfig::default(),
            appearance: AppearanceConfig::default(),
            behavior: BehaviorConfig::default(),
            archive: ArchiveConfig::default(),
            workspaces: Vec::new(),
        }
    }
}

impl Config {
    /// Load from disk, falling back to defaults.
    ///
    /// A missing file is normal (first run). A corrupt file is quarantined and
    /// reported, never fatal.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            tracing::warn!("no config path available; using defaults");
            return Self::default();
        };

        if !path.exists() {
            tracing::info!(?path, "no config file yet; using defaults");
            return Self::default();
        }

        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(?path, error = %e, "could not read config; using defaults");
                return Self::default();
            }
        };

        match serde_json::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                if cfg.schema_version != SCHEMA_VERSION {
                    tracing::info!(
                        from = cfg.schema_version,
                        to = SCHEMA_VERSION,
                        "migrating config"
                    );
                    cfg.migrate();
                }
                tracing::info!(?path, "config loaded");
                cfg
            }
            Err(e) => {
                // Quarantine rather than overwrite: the user may want to
                // recover hand-edited values.
                tracing::error!(?path, error = %e, "config is corrupt; quarantining");
                quarantine(path);
                Self::default()
            }
        }
    }

    /// Apply migrations between schema versions.
    fn migrate(&mut self) {
        // Only version 1 exists so far. Future migrations chain here:
        //   if self.schema_version < 2 { ... }
        self.schema_version = SCHEMA_VERSION;
    }

    /// Save atomically.
    ///
    /// temp file → flush → fsync → rename. On Windows `rename` maps to
    /// `MoveFileExW` with `REPLACE_EXISTING`, which is atomic within a volume.
    pub fn save(&self, path: Option<&Path>) -> anyhow::Result<()> {
        let Some(path) = path else {
            anyhow::bail!("no config path available");
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");

        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.flush()?;
            // Without this the rename can land before the data does, so a
            // power loss yields a zero-length config.
            f.sync_all()?;
        }

        std::fs::rename(&tmp, path)?;
        tracing::debug!(?path, "config saved");
        Ok(())
    }
}

/// Move an unparseable config aside so the user can inspect it.
fn quarantine(path: &Path) {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let bad = path.with_extension(format!("bad.{stamp}.json"));

    match std::fs::rename(path, &bad) {
        Ok(()) => tracing::warn!(?bad, "corrupt config moved aside"),
        Err(e) => tracing::error!(error = %e, "could not quarantine corrupt config"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("rustplorer_cfg_tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn round_trips() {
        let p = tmp_path("round_trip.json");
        let _ = std::fs::remove_file(&p);

        let mut cfg = Config::default();
        cfg.performance.concurrent_size_walks = 7;
        cfg.appearance.font_size = 18.0;
        cfg.save(Some(&p)).unwrap();

        let loaded = Config::load(Some(&p));
        assert_eq!(loaded.performance.concurrent_size_walks, 7);
        assert_eq!(loaded.appearance.font_size, 18.0);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let p = tmp_path("does_not_exist.json");
        let _ = std::fs::remove_file(&p);

        let cfg = Config::load(Some(&p));
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn corrupt_file_is_quarantined_and_defaults_used() {
        let p = tmp_path("corrupt.json");
        std::fs::write(&p, b"{ this is not valid json ][").unwrap();

        let cfg = Config::load(Some(&p));

        // Must not panic, must return defaults, and must move the bad file.
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
        assert!(!p.exists(), "corrupt config should have been renamed away");

        // Clean up quarantined files.
        if let Some(dir) = p.parent() {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("corrupt.bad.") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    #[test]
    fn partial_config_fills_in_defaults() {
        let p = tmp_path("partial.json");
        // Simulates a config written by an older version that lacked most
        // fields. Every one must fall back to its default.
        std::fs::write(&p, br#"{"schema_version":1,"appearance":{"font_size":20.0}}"#).unwrap();

        let cfg = Config::load(Some(&p));
        assert_eq!(cfg.appearance.font_size, 20.0);
        assert!(cfg.behavior.live_refresh, "missing field should default");
        assert!(cfg.performance.folder_sizes_enabled);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn workspaces_round_trip() {
        let p = tmp_path("workspaces.json");
        let _ = std::fs::remove_file(&p);

        let mut cfg = Config::default();
        cfg.workspaces.push(Workspace {
            name: "Project".to_string(),
            left_path: PathBuf::from("/a"),
            right_path: Some(PathBuf::from("/b")),
            dual_pane: true,
        });
        cfg.save(Some(&p)).unwrap();

        let loaded = Config::load(Some(&p));
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].name, "Project");
        assert!(loaded.workspaces[0].dual_pane);
        assert_eq!(loaded.workspaces[0].right_path, Some(PathBuf::from("/b")));

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn config_without_workspaces_still_loads() {
        // Older configs predate the workspaces field entirely.
        let p = tmp_path("no_workspaces.json");
        std::fs::write(&p, br#"{"schema_version":1}"#).unwrap();

        let cfg = Config::load(Some(&p));
        assert!(cfg.workspaces.is_empty());

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn profiles_scale_with_cores() {
        let mut cfg = PerformanceConfig::default();

        cfg.profile = PerformanceProfile::Conservative;
        let conservative = cfg.effective_size_walks();

        cfg.profile = PerformanceProfile::Aggressive;
        let aggressive = cfg.effective_size_walks();

        assert!(
            aggressive > conservative,
            "aggressive ({aggressive}) should exceed conservative ({conservative})"
        );
    }

    #[test]
    fn custom_profile_is_clamped() {
        let mut cfg = PerformanceConfig {
            profile: PerformanceProfile::Custom,
            concurrent_size_walks: 9999,
            ..Default::default()
        };
        assert!(cfg.effective_size_walks() <= 32);

        cfg.concurrent_size_walks = 0;
        assert!(cfg.effective_size_walks() >= 1, "must never be zero");
    }

    #[test]
    fn atomic_save_leaves_no_temp_file() {
        let p = tmp_path("atomic.json");
        let _ = std::fs::remove_file(&p);

        Config::default().save(Some(&p)).unwrap();

        assert!(p.exists());
        assert!(!p.with_extension("json.tmp").exists(), "temp file left behind");

        let _ = std::fs::remove_file(&p);
    }
}

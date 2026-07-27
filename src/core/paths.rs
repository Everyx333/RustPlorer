//! Where RustPlorer keeps its files on disk.
//!
//! Windows distinguishes roaming from local app data, and the distinction is
//! not cosmetic. On a domain-joined machine `%APPDATA%` follows the user
//! between machines; `%LOCALAPPDATA%` does not.
//!
//! So:
//! - **Config → roaming.** Your theme and keybindings should follow you.
//! - **Logs and thumbnail cache → local.** Machine-specific and potentially
//!   large; syncing them across a network profile would be a bug, not a feature.

use std::path::PathBuf;

use directories::ProjectDirs;

/// Resolved application directories.
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// Roaming config directory (`%APPDATA%\RustPlorer`).
    pub config_dir: Option<PathBuf>,
    /// Local data directory (`%LOCALAPPDATA%\RustPlorer`).
    pub data_dir: Option<PathBuf>,
}

impl AppPaths {
    /// Resolve platform directories.
    ///
    /// Every field is optional: a user with a broken or redirected profile
    /// should still get a working file manager, just without persistence.
    pub fn resolve() -> Self {
        match ProjectDirs::from("", "", "RustPlorer") {
            Some(dirs) => Self {
                config_dir: Some(dirs.config_dir().to_path_buf()),
                data_dir: Some(dirs.data_local_dir().to_path_buf()),
            },
            None => {
                eprintln!("rustplorer: could not resolve app directories; persistence disabled");
                Self {
                    config_dir: None,
                    data_dir: None,
                }
            }
        }
    }

    /// Path to `config.json`.
    pub fn config_file(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|d| d.join("config.json"))
    }

    /// Directory for rolling log files.
    pub fn log_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("logs"))
    }

    /// Directory for the on-disk thumbnail cache.
    pub fn thumbnail_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("thumbnails"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_without_panicking() {
        let paths = AppPaths::resolve();
        // On CI the profile may be unusual; we only require that derived paths
        // are consistent with their parents.
        if paths.config_dir.is_some() {
            assert!(paths.config_file().is_some());
        }
        if paths.data_dir.is_some() {
            assert!(paths.log_dir().is_some());
            assert!(paths.thumbnail_dir().is_some());
        }
    }
}

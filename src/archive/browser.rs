//! Browsing archives as if they were folders.
//!
//! Double-clicking a `.zip` navigates *into* it rather than launching another
//! application. Internally an archive location is `(archive_path, inner_path)`;
//! the listing is read once on a worker and then navigated in memory, so
//! moving between folders inside an archive costs nothing.

use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::archive::format::ArchiveEntry;
use crate::archive::reader;
use crate::core::task::WorkerPool;

/// A location inside an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveLocation {
    /// The archive file on disk.
    pub archive: PathBuf,
    /// Path within the archive. Empty means the archive root.
    pub inner: String,
}

impl ArchiveLocation {
    pub fn root(archive: PathBuf) -> Self {
        Self {
            archive,
            inner: String::new(),
        }
    }

    /// Descend into a subdirectory.
    pub fn child(&self, name: &str) -> Self {
        let inner = if self.inner.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", self.inner, name)
        };
        Self {
            archive: self.archive.clone(),
            inner,
        }
    }

    /// Go up one level. `None` at the archive root, meaning "leave the
    /// archive" — the caller navigates back to the containing folder.
    pub fn parent(&self) -> Option<Self> {
        if self.inner.is_empty() {
            return None;
        }
        let inner = match self.inner.rfind('/') {
            Some(i) => self.inner[..i].to_string(),
            None => String::new(),
        };
        Some(Self {
            archive: self.archive.clone(),
            inner,
        })
    }

    /// Display path, e.g. `C:\d\pkg.zip\src`.
    pub fn display(&self) -> String {
        if self.inner.is_empty() {
            self.archive.display().to_string()
        } else {
            format!("{}\\{}", self.archive.display(), self.inner.replace('/', "\\"))
        }
    }
}

/// Result of loading an archive's index.
#[derive(Debug)]
pub enum ArchiveUpdate {
    Loaded {
        archive: PathBuf,
        entries: Vec<ArchiveEntry>,
    },
    Failed {
        archive: PathBuf,
        error: String,
    },
}

/// Loads and caches archive listings.
pub struct ArchiveBrowser {
    /// The currently open archive and its full entry list.
    loaded: Option<(PathBuf, Vec<ArchiveEntry>)>,
    tx: Sender<ArchiveUpdate>,
    rx: Receiver<ArchiveUpdate>,
    loading: bool,
}

impl Default for ArchiveBrowser {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveBrowser {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            loaded: None,
            tx,
            rx,
            loading: false,
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// True if this archive's index is already in memory.
    pub fn has(&self, archive: &Path) -> bool {
        self.loaded
            .as_ref()
            .is_some_and(|(p, _)| p.as_path() == archive)
    }

    /// Read an archive's index on the worker pool.
    ///
    /// Parsing runs inside the pool's `catch_unwind`, so a malformed archive
    /// that panics a decoder fails this one operation instead of the app —
    /// which is exactly why archives are the highest-value place for it.
    pub fn open(&mut self, pool: &WorkerPool, archive: PathBuf) {
        if self.has(&archive) {
            return;
        }

        self.loading = true;
        let tx = self.tx.clone();

        pool.submit("archive_list", move |_token| {
            match reader::list_entries(&archive) {
                Ok(entries) => {
                    let _ = tx.send(ArchiveUpdate::Loaded { archive, entries });
                }
                Err(e) => {
                    tracing::warn!(?archive, error = %e, "could not read archive");
                    let _ = tx.send(ArchiveUpdate::Failed {
                        archive,
                        error: format!("Could not read archive: {e}"),
                    });
                }
            }
        });
    }

    /// Drain pending results. Returns an error message if loading failed.
    pub fn poll(&mut self) -> Option<String> {
        let mut error = None;

        for update in self.rx.try_iter() {
            self.loading = false;
            match update {
                ArchiveUpdate::Loaded { archive, entries } => {
                    tracing::debug!(?archive, count = entries.len(), "archive index ready");
                    self.loaded = Some((archive, entries));
                }
                ArchiveUpdate::Failed { error: e, .. } => {
                    self.loaded = None;
                    error = Some(e);
                }
            }
        }

        error
    }

    /// Entries directly inside `location`, or an empty list if not loaded.
    pub fn entries_at(&self, location: &ArchiveLocation) -> Vec<ArchiveEntry> {
        match &self.loaded {
            Some((path, all)) if path == &location.archive => {
                reader::entries_in_dir(all, &location.inner)
            }
            _ => Vec::new(),
        }
    }

    /// Total entries in the open archive.
    pub fn total_entries(&self) -> usize {
        self.loaded.as_ref().map(|(_, e)| e.len()).unwrap_or(0)
    }

    /// Forget the open archive.
    pub fn close(&mut self) {
        self.loaded = None;
        self.loading = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_location_has_no_parent() {
        let loc = ArchiveLocation::root(PathBuf::from("a.zip"));
        assert!(loc.parent().is_none(), "root should signal 'leave archive'");
    }

    #[test]
    fn child_and_parent_round_trip() {
        let root = ArchiveLocation::root(PathBuf::from("a.zip"));
        let src = root.child("src");
        let core = src.child("core");

        assert_eq!(core.inner, "src/core");
        assert_eq!(core.parent().unwrap().inner, "src");
        assert_eq!(src.parent().unwrap().inner, "");
    }

    #[test]
    fn display_joins_with_backslashes() {
        let loc = ArchiveLocation {
            archive: PathBuf::from("pkg.zip"),
            inner: "src/core".to_string(),
        };
        assert!(loc.display().ends_with("src\\core"));
    }

    #[test]
    fn empty_browser_returns_nothing() {
        let b = ArchiveBrowser::new();
        let loc = ArchiveLocation::root(PathBuf::from("a.zip"));
        assert!(b.entries_at(&loc).is_empty());
        assert_eq!(b.total_entries(), 0);
    }
}

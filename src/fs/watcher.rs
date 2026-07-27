//! Live directory watching.
//!
//! Keeps the listing current when files change underneath us — a download
//! completing, an installer writing files, a build emitting artifacts.
//!
//! Events are **debounced**. Saving a file in an editor can emit several raw
//! events in milliseconds, and a build can emit thousands. Reacting to each one
//! would rescan the directory continuously and defeat the point of the async
//! core. `notify-debouncer-full` coalesces bursts into one notification.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};

/// Debounce window.
///
/// Long enough to coalesce an editor's save burst or an extract operation,
/// short enough that a completed download appears promptly.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Signal that the watched directory changed and should be rescanned.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub path: PathBuf,
}

/// Watches a single directory (non-recursively) and reports coalesced changes.
///
/// Non-recursive is deliberate: the listing only shows one directory's
/// contents, so watching the whole subtree would burn resources on events that
/// cannot affect what is displayed.
pub struct DirWatcher {
    debouncer: Option<Debouncer<notify::RecommendedWatcher, RecommendedCache>>,
    watched: Option<PathBuf>,
    tx: Sender<ChangeEvent>,
    rx: Receiver<ChangeEvent>,
}

impl Default for DirWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl DirWatcher {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            debouncer: None,
            watched: None,
            tx,
            rx,
        }
    }

    /// Currently watched path, if any.
    pub fn watched(&self) -> Option<&Path> {
        self.watched.as_deref()
    }

    /// Start watching `path`, replacing any previous watch.
    ///
    /// Failures are logged and ignored: watching is an enhancement, and a
    /// directory that cannot be watched (network share, restricted folder)
    /// must still be browsable — just without live updates.
    pub fn watch(&mut self, path: PathBuf) {
        if self.watched.as_deref() == Some(path.as_path()) {
            return;
        }

        // Dropping the old debouncer stops the previous watch.
        self.debouncer = None;
        self.watched = None;

        let tx = self.tx.clone();
        let watch_path = path.clone();

        let result = new_debouncer(
            DEBOUNCE,
            None,
            move |res: DebounceEventResult| match res {
                Ok(events) => {
                    if events.is_empty() {
                        return;
                    }
                    // Collapse the whole burst into a single rescan request.
                    // Applying individual events would mean reimplementing
                    // filesystem semantics (renames, atomic replaces) in the
                    // UI layer; a rescan is cheap and always correct.
                    let _ = tx.send(ChangeEvent {
                        path: watch_path.clone(),
                    });
                }
                Err(errors) => {
                    for e in errors {
                        tracing::debug!(error = %e, "watch error");
                    }
                }
            },
        );

        match result {
            Ok(mut debouncer) => {
                match debouncer.watch(&path, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        tracing::debug!(?path, "watching directory");
                        self.debouncer = Some(debouncer);
                        self.watched = Some(path);
                    }
                    Err(e) => {
                        tracing::debug!(?path, error = %e, "could not watch directory");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not create watcher");
            }
        }
    }

    /// Stop watching.
    pub fn stop(&mut self) {
        self.debouncer = None;
        self.watched = None;
    }

    /// Drain pending change notifications. Non-blocking.
    pub fn poll(&self) -> Vec<ChangeEvent> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn detects_a_new_file() {
        let dir = std::env::temp_dir().join("rustplorer_watch_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut w = DirWatcher::new();
        w.watch(dir.clone());

        // Watchers need a moment to register with the OS before changes
        // are observed.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(dir.join("new.txt"), b"hi").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = false;
        while Instant::now() < deadline && !got {
            if !w.poll().is_empty() {
                got = true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(got, "expected a change event after creating a file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watching_missing_path_is_survivable() {
        let mut w = DirWatcher::new();
        w.watch(PathBuf::from("/definitely/not/real/xyz"));
        // Must not panic, and must not claim to be watching.
        assert!(w.watched().is_none());
    }

    #[test]
    fn rewatching_same_path_is_a_noop() {
        let dir = std::env::temp_dir().join("rustplorer_watch_noop");
        std::fs::create_dir_all(&dir).unwrap();

        let mut w = DirWatcher::new();
        w.watch(dir.clone());
        w.watch(dir.clone());

        assert_eq!(w.watched(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

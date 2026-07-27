//! Asynchronous directory scanning.
//!
//! Scans run on the worker pool and stream results back over a channel. The UI
//! polls that channel without blocking, so a slow network share degrades to
//! "results arrive late" rather than "the window stops painting".

use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::core::task::{CancelToken, WorkerPool};
use crate::fs::entry::Entry;

/// Emit partial results every N entries.
///
/// This is what makes a 200k-file directory feel instant: the first rows appear
/// after ~500 entries instead of after the whole scan. Too small and we spam
/// the channel and repaint constantly; too large and the first paint is late.
const BATCH_SIZE: usize = 500;

/// A message from a scan job back to the UI.
#[derive(Debug)]
pub enum ScanUpdate {
    /// A batch of entries. `generation` lets the UI discard results from a
    /// navigation the user has already moved on from.
    Batch {
        generation: u64,
        path: PathBuf,
        entries: Vec<Entry>,
    },
    /// Scan finished normally.
    Done {
        generation: u64,
        path: PathBuf,
        total: usize,
    },
    /// Scan failed. Carries a display-ready message — permission denied and
    /// disconnected network drives are routine, not exceptional.
    Failed {
        generation: u64,
        path: PathBuf,
        error: String,
    },
    /// Scan was superseded before completing.
    Cancelled { generation: u64, path: PathBuf },
}

/// Owns the channel that scan results arrive on.
pub struct Scanner {
    tx: Sender<ScanUpdate>,
    rx: Receiver<ScanUpdate>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }

    /// Non-blocking drain of pending updates. Called once per frame.
    ///
    /// Never blocks: if nothing is ready the UI just paints the previous
    /// snapshot again.
    pub fn poll(&self) -> Vec<ScanUpdate> {
        self.rx.try_iter().collect()
    }

    /// Queue a directory scan.
    ///
    /// Bumps the pool generation first, which invalidates any in-flight scan —
    /// the mechanism that makes navigating away from a slow directory instant.
    pub fn scan_dir(&self, pool: &WorkerPool, path: PathBuf, show_hidden: bool) {
        let generation = pool.cancel_all();
        let tx = self.tx.clone();

        tracing::debug!(path = ?path, generation, "queueing directory scan");

        pool.submit("scan_dir", move |token| {
            scan_directory_job(&tx, token, generation, path, show_hidden);
        });
    }
}

/// The body of a scan job. Runs on a worker thread.
fn scan_directory_job(
    tx: &Sender<ScanUpdate>,
    token: &CancelToken,
    generation: u64,
    path: PathBuf,
    show_hidden: bool,
) {
    let span = tracing::debug_span!("scan", path = ?path);
    let _guard = span.enter();

    let started = std::time::Instant::now();

    let read_dir = match std::fs::read_dir(&path) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(error = %e, "scan failed");
            let _ = tx.send(ScanUpdate::Failed {
                generation,
                path,
                error: friendly_io_error(&e),
            });
            return;
        }
    };

    let mut batch = Vec::with_capacity(BATCH_SIZE);
    let mut total = 0usize;

    for dir_entry in read_dir {
        // Poll cancellation each iteration. On a slow network share this is the
        // difference between abandoning instantly and waiting out the scan.
        if token.is_cancelled() {
            tracing::debug!(scanned = total, "scan cancelled");
            let _ = tx.send(ScanUpdate::Cancelled { generation, path });
            return;
        }

        let dir_entry = match dir_entry {
            Ok(d) => d,
            // One unreadable entry must not abort the listing. This happens
            // routinely in system folders.
            Err(e) => {
                tracing::trace!(error = %e, "skipping unreadable entry");
                continue;
            }
        };

        let entry_path = dir_entry.path();

        // `symlink_metadata` does not follow links, so a symlink pointing at a
        // dead network path costs nothing instead of stalling the scan.
        let meta = match dir_entry
            .metadata()
            .or_else(|_| std::fs::symlink_metadata(&entry_path))
        {
            Ok(m) => m,
            Err(e) => {
                tracing::trace!(path = ?entry_path, error = %e, "skipping entry without metadata");
                continue;
            }
        };

        let entry = Entry::from_metadata(entry_path, &meta);

        if entry.is_hidden && !show_hidden {
            continue;
        }

        batch.push(entry);
        total += 1;

        if batch.len() >= BATCH_SIZE {
            let chunk = std::mem::replace(&mut batch, Vec::with_capacity(BATCH_SIZE));
            if tx
                .send(ScanUpdate::Batch {
                    generation,
                    path: path.clone(),
                    entries: chunk,
                })
                .is_err()
            {
                // Receiver gone — the app is shutting down.
                return;
            }
        }
    }

    if !batch.is_empty() {
        let _ = tx.send(ScanUpdate::Batch {
            generation,
            path: path.clone(),
            entries: batch,
        });
    }

    tracing::debug!(total, elapsed_ms = started.elapsed().as_millis(), "scan done");

    let _ = tx.send(ScanUpdate::Done {
        generation,
        path,
        total,
    });
}

/// Turn an `io::Error` into something worth showing a user.
///
/// "The system cannot find the path specified. (os error 3)" is noise; the
/// point is to say what to do about it.
fn friendly_io_error(e: &std::io::Error) -> String {
    use std::io::ErrorKind;

    match e.kind() {
        ErrorKind::NotFound => "This folder no longer exists.".to_string(),
        ErrorKind::PermissionDenied => {
            "Access denied. You may need administrator rights to view this folder.".to_string()
        }
        _ => format!("Could not open folder: {e}"),
    }
}

/// List the drive roots available on this machine.
///
/// On Windows this probes `A:\` through `Z:\`. `std::fs::metadata` on a drive
/// root is cheap and, importantly, does not spin up sleeping external drives
/// the way enumerating their contents would.
#[cfg(windows)]
pub fn list_drives() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .map(|c| PathBuf::from(format!("{}:\\", c as char)))
        .filter(|p| std::fs::metadata(p).is_ok())
        .collect()
}

#[cfg(not(windows))]
pub fn list_drives() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

/// Well-known user folders for the sidebar.
pub fn quick_access() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();

    if let Some(dirs) = directories::UserDirs::new() {
        out.push(("Home".to_string(), dirs.home_dir().to_path_buf()));

        let candidates: [(&str, Option<&Path>); 5] = [
            ("Desktop", dirs.desktop_dir()),
            ("Documents", dirs.document_dir()),
            ("Downloads", dirs.download_dir()),
            ("Pictures", dirs.picture_dir()),
            ("Videos", dirs.video_dir()),
        ];

        for (label, dir) in candidates {
            if let Some(d) = dir {
                out.push((label.to_string(), d.to_path_buf()));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task::WorkerPool;
    use std::time::{Duration, Instant};

    #[test]
    fn scans_a_real_directory() {
        let tmp = std::env::temp_dir().join("rustplorer_scan_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), b"hello").unwrap();
        std::fs::write(tmp.join("b.txt"), b"world").unwrap();
        std::fs::create_dir_all(tmp.join("subdir")).unwrap();

        let pool = WorkerPool::new(2);
        let scanner = Scanner::new();
        scanner.scan_dir(&pool, tmp.clone(), false);

        let mut found = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut done = false;

        while Instant::now() < deadline && !done {
            for update in scanner.poll() {
                match update {
                    ScanUpdate::Batch { entries, .. } => found.extend(entries),
                    ScanUpdate::Done { .. } => done = true,
                    ScanUpdate::Failed { error, .. } => panic!("scan failed: {error}"),
                    ScanUpdate::Cancelled { .. } => panic!("unexpected cancellation"),
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(done, "scan did not complete in time");
        assert_eq!(found.len(), 3);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_directory_reports_failure() {
        let pool = WorkerPool::new(1);
        let scanner = Scanner::new();
        scanner.scan_dir(&pool, PathBuf::from("/definitely/not/a/real/path"), false);

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failed = false;

        while Instant::now() < deadline && !failed {
            for update in scanner.poll() {
                if let ScanUpdate::Failed { .. } = update {
                    failed = true;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(failed, "expected a Failed update");
    }

    #[test]
    fn drive_listing_is_not_empty() {
        assert!(!list_drives().is_empty());
    }
}

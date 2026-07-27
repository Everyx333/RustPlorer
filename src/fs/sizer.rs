//! Recursive folder size calculation.
//!
//! # Why Explorer doesn't do this
//!
//! Showing a folder's size means walking its entire subtree. Explorer avoids it
//! because doing it naively is ruinous: land in `C:\Windows`, kick off a
//! recursive walk for all ~80 subfolders at once, and you have thousands of
//! threads fighting over the disk queue.
//!
//! So the work is bounded four ways:
//!
//! 1. **Viewport-only.** Sizes are requested for visible rows, not the whole
//!    listing.
//! 2. **Capped concurrency.** A semaphore limits simultaneous walks regardless
//!    of how many folders are on screen.
//! 3. **Cached.** Results are memoized by path, so scrolling back is free.
//! 4. **Cancellable and capped.** Walks honour the generation counter and stop
//!    at a depth/entry ceiling, so a junction loop can't hang a worker.
//!
//! Sizes also stream: a partial total is published as the walk proceeds, so a
//! large folder shows a rising number rather than a frozen placeholder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use dashmap::DashMap;

use crate::core::task::{CancelToken, WorkerPool};

/// Maximum directory depth to descend.
///
/// Guards against symlink/junction cycles. Real trees are rarely deeper than
/// ~20; NTFS junctions can be infinite.
const MAX_DEPTH: usize = 40;

/// Maximum entries to visit in a single folder-size job.
///
/// A hard ceiling on worst-case work. Beyond this the result is reported as
/// partial rather than running unbounded.
const MAX_ENTRIES: usize = 2_000_000;

/// Publish a running total every N entries.
const PROGRESS_INTERVAL: usize = 20_000;

/// Concurrent size walks allowed at once.
///
/// Deliberately small and independent of the scan pool: the bottleneck is the
/// disk, not the CPU, and more parallel walks make it slower, not faster.
const MAX_CONCURRENT_WALKS: usize = 2;

/// State of a folder's size calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeState {
    /// Walk in progress. Carries the running total so the UI can count up.
    Calculating(u64),
    /// Fully walked.
    Done(u64),
    /// Stopped at the entry cap — the real size is at least this much.
    Partial(u64),
    /// Could not be read (permissions, disconnected drive).
    Failed,
}

impl SizeState {
    /// Bytes known so far, if any.
    pub fn bytes(self) -> Option<u64> {
        match self {
            Self::Calculating(b) | Self::Done(b) | Self::Partial(b) => Some(b),
            Self::Failed => None,
        }
    }

    /// Display string for the size column.
    pub fn display(self) -> String {
        match self {
            // Trailing ellipsis signals "still counting" without a spinner per
            // row, which would be visually noisy in a dense table.
            Self::Calculating(b) => format!("{}…", humansize::format_size(b, humansize::DECIMAL)),
            Self::Done(b) => humansize::format_size(b, humansize::DECIMAL),
            Self::Partial(b) => format!("≥{}", humansize::format_size(b, humansize::DECIMAL)),
            Self::Failed => "—".to_string(),
        }
    }
}

/// A size update from a worker.
#[derive(Debug)]
pub struct SizeUpdate {
    pub path: PathBuf,
    pub state: SizeState,
    pub generation: u64,
}

/// Computes and caches folder sizes.
pub struct FolderSizer {
    /// Memoized results. `DashMap` so workers can write while the UI reads,
    /// without a global lock on the render path.
    cache: Arc<DashMap<PathBuf, SizeState>>,
    /// Number of walks currently running, capped at `MAX_CONCURRENT_WALKS`.
    active: Arc<AtomicUsize>,
    tx: Sender<SizeUpdate>,
    rx: Receiver<SizeUpdate>,
}

impl Default for FolderSizer {
    fn default() -> Self {
        Self::new()
    }
}

impl FolderSizer {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            cache: Arc::new(DashMap::new()),
            active: Arc::new(AtomicUsize::new(0)),
            tx,
            rx,
        }
    }

    /// Look up a cached size, if present.
    pub fn get(&self, path: &Path) -> Option<SizeState> {
        self.cache.get(path).map(|e| *e.value())
    }

    /// Discard all cached sizes. Called when the tree may have changed
    /// underneath us (e.g. a filesystem event).
    pub fn clear(&self) {
        self.cache.clear();
    }

    /// Forget one path, so the next request recomputes it.
    pub fn invalidate(&self, path: &Path) {
        self.cache.remove(path);
    }

    /// Request a size for `path` if it isn't already known or running.
    ///
    /// Cheap and idempotent — safe to call every frame for every visible row.
    pub fn request(&self, pool: &WorkerPool, path: PathBuf) {
        // Already cached, or already being walked.
        if self.cache.contains_key(&path) {
            return;
        }

        // Respect the concurrency cap. Rows that miss out here simply get
        // picked up on a later frame, which is why `request` must be cheap.
        let running = self.active.load(Ordering::Acquire);
        if running >= MAX_CONCURRENT_WALKS {
            return;
        }

        // Claim a slot before marking the cache, so two frames can't both
        // start the same walk.
        self.active.fetch_add(1, Ordering::AcqRel);
        self.cache.insert(path.clone(), SizeState::Calculating(0));

        let tx = self.tx.clone();
        let cache = Arc::clone(&self.cache);
        let active = Arc::clone(&self.active);
        let generation = pool.generation().current();

        // Clone for the closure; the original is retained so we can still
        // clean up if the job is rejected below.
        let job_path = path.clone();

        let submitted = pool.submit("folder_size", move |token| {
            let final_state = walk_size(&job_path, token, &tx, generation);

            cache.insert(job_path.clone(), final_state);
            let _ = tx.send(SizeUpdate {
                path: job_path,
                state: final_state,
                generation,
            });

            active.fetch_sub(1, Ordering::AcqRel);
        });

        // If the queue rejected the job, release the slot and the placeholder —
        // otherwise the folder would show "calculating" forever.
        if submitted.is_none() {
            self.active.fetch_sub(1, Ordering::AcqRel);
            self.cache.remove(&path);
        }
    }

    /// Drain pending size updates. Non-blocking; called once per frame.
    pub fn poll(&self) -> Vec<SizeUpdate> {
        self.rx.try_iter().collect()
    }
}

/// Walk a directory tree, accumulating file sizes.
///
/// Uses an explicit stack rather than recursion: a deeply nested tree would
/// otherwise risk blowing the worker's stack.
fn walk_size(
    root: &Path,
    token: &CancelToken,
    tx: &Sender<SizeUpdate>,
    generation: u64,
) -> SizeState {
    let span = tracing::debug_span!("folder_size", path = ?root);
    let _guard = span.enter();
    let started = std::time::Instant::now();

    let mut total: u64 = 0;
    let mut visited: usize = 0;
    let mut last_published: usize = 0;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    // Track visited directories to break junction/symlink cycles that the
    // depth cap alone would not catch quickly.
    let mut seen: HashMap<PathBuf, ()> = HashMap::new();

    while let Some((dir, depth)) = stack.pop() {
        if token.is_cancelled() {
            tracing::trace!("size walk cancelled");
            // Return what we have; the UI discards it via the generation check.
            return SizeState::Partial(total);
        }

        if depth > MAX_DEPTH {
            tracing::debug!(?dir, "depth cap reached");
            continue;
        }

        if visited >= MAX_ENTRIES {
            tracing::warn!(visited, "entry cap reached; reporting partial size");
            return SizeState::Partial(total);
        }

        // Canonicalize to detect cycles. Failure is fine — we just skip the
        // cycle check for that path rather than aborting.
        if let Ok(real) = dir.canonicalize() {
            if seen.insert(real, ()).is_some() {
                continue;
            }
        }

        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                // Unreadable subdirectories are routine (permissions). Skip
                // them and keep counting rather than failing the whole walk.
                tracing::trace!(?dir, error = %e, "skipping unreadable directory");
                continue;
            }
        };

        for entry in read.flatten() {
            visited += 1;

            // `symlink_metadata` does not follow links: a link's target is
            // counted where it actually lives, not double-counted here.
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            if meta.is_dir() {
                stack.push((entry.path(), depth + 1));
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }

        // Publish a running total periodically so large folders count up
        // visibly instead of appearing frozen.
        if visited - last_published >= PROGRESS_INTERVAL {
            last_published = visited;
            let _ = tx.send(SizeUpdate {
                path: root.to_path_buf(),
                state: SizeState::Calculating(total),
                generation,
            });
        }
    }

    tracing::debug!(
        total,
        visited,
        elapsed_ms = started.elapsed().as_millis(),
        "size walk complete"
    );

    SizeState::Done(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task::WorkerPool;
    use std::time::{Duration, Instant};

    fn temp_tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::write(root.join("a.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(root.join("sub/b.bin"), vec![0u8; 2000]).unwrap();
        std::fs::write(root.join("sub/deeper/c.bin"), vec![0u8; 3000]).unwrap();
        root
    }

    #[test]
    fn sums_nested_files() {
        let root = temp_tree("rustplorer_size_test");
        let pool = WorkerPool::new(2);
        let sizer = FolderSizer::new();

        sizer.request(&pool, root.clone());

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut done = None;

        while Instant::now() < deadline && done.is_none() {
            for u in sizer.poll() {
                if let SizeState::Done(b) = u.state {
                    done = Some(b);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(done, Some(6000), "expected 1000+2000+3000 bytes");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn caches_results() {
        let root = temp_tree("rustplorer_size_cache");
        let pool = WorkerPool::new(2);
        let sizer = FolderSizer::new();

        sizer.request(&pool, root.clone());

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let _ = sizer.poll();
            if matches!(sizer.get(&root), Some(SizeState::Done(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(matches!(sizer.get(&root), Some(SizeState::Done(6000))));

        // A second request must not restart the walk.
        sizer.request(&pool, root.clone());
        assert!(matches!(sizer.get(&root), Some(SizeState::Done(6000))));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn display_marks_partial_and_calculating() {
        assert!(SizeState::Calculating(1000).display().ends_with('…'));
        assert!(SizeState::Partial(1000).display().starts_with('≥'));
        assert_eq!(SizeState::Failed.display(), "—");
    }

    #[test]
    fn invalidate_forces_recompute() {
        let sizer = FolderSizer::new();
        let p = PathBuf::from("/some/path");
        sizer.cache.insert(p.clone(), SizeState::Done(42));

        assert!(sizer.get(&p).is_some());
        sizer.invalidate(&p);
        assert!(sizer.get(&p).is_none());
    }
}

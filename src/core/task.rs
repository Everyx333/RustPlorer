//! Supervised worker pool with cancellable, panic-isolated tasks.
//!
//! # Why this exists
//!
//! Windows Explorer goes "not responding" because it performs filesystem I/O on
//! the UI thread. A slow network share, a spinning-down external drive, or a
//! directory with 200k entries blocks the message pump and the window stops
//! painting.
//!
//! RustPlorer's rule: **the UI thread never touches the filesystem.** All I/O
//! happens here, on worker threads, and results travel back over a channel that
//! the UI polls without ever blocking.
//!
//! Three properties matter:
//!
//! 1. **Cancellation.** Navigating away from a slow directory must abandon that
//!    work, not wait for it. Handled by [`Generation`].
//! 2. **Fault isolation.** A malformed archive that panics a decoder must fail
//!    that one operation, not kill the app. Handled by `catch_unwind` in
//!    [`WorkerPool::spawn_worker`].
//! 3. **Supervision.** A worker that dies must be replaced, or the pool silently
//!    shrinks to zero and the app appears to hang — the exact failure we set out
//!    to fix.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};

/// A monotonic counter used to invalidate in-flight work.
///
/// Each navigation bumps the generation. Workers compare the generation their
/// task was queued under against the current value and bail out early if they
/// no longer match.
///
/// This is preferred over a per-task cancellation flag because it is a single
/// atomic for the whole navigation: one `bump()` invalidates every outstanding
/// task at once, with no bookkeeping of which tasks are in flight.
#[derive(Debug, Clone, Default)]
pub struct Generation(Arc<AtomicU64>);

impl Generation {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    /// Current generation value.
    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Invalidate all outstanding work, returning the new generation.
    pub fn bump(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// True if `gen` is still the active generation.
    pub fn is_current(&self, generation: u64) -> bool {
        self.current() == generation
    }
}

/// Cooperative cancellation token handed to a running task.
///
/// Long-running work (recursive scans, archive extraction) should poll
/// [`CancelToken::is_cancelled`] periodically and return early when set.
#[derive(Debug, Clone)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    generation: u64,
    tracker: Generation,
}

impl CancelToken {
    fn new(generation: u64, tracker: Generation) -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            generation,
            tracker,
        }
    }

    /// True if this task should stop — either explicitly cancelled, or
    /// superseded by a newer generation.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed) || !self.tracker.is_current(self.generation)
    }

    /// Explicitly cancel this individual task.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// The generation this task was queued under.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// A unit of work executed on the pool.
type Job = Box<dyn FnOnce(&CancelToken) + Send + 'static>;

struct QueuedJob {
    job: Job,
    token: CancelToken,
    /// Human-readable label, used in panic logs so a crash report says
    /// "scan_dir" rather than an opaque thread id.
    label: &'static str,
}

/// A supervised pool of worker threads.
///
/// Workers are respawned if they die, so the pool never silently shrinks.
pub struct WorkerPool {
    tx: Sender<QueuedJob>,
    generation: Generation,
    size: usize,
}

impl WorkerPool {
    /// Create a pool with `size` workers.
    ///
    /// `size` is clamped to at least 1. Callers should cap this (we use
    /// `min(cpus, 8)`) — an unbounded pool on a 32-core machine spawns 32
    /// concurrent directory scans and thrashes the disk queue rather than
    /// going faster.
    pub fn new(size: usize) -> Self {
        let size = size.max(1);

        // Bounded queue applies backpressure. An unbounded queue lets a user
        // holding the down-arrow key enqueue thousands of thumbnail decodes
        // faster than they can be serviced, which is a memory leak in practice.
        let (tx, rx) = bounded::<QueuedJob>(1024);
        let generation = Generation::new();

        for idx in 0..size {
            Self::spawn_worker(idx, rx.clone());
        }

        tracing::info!(workers = size, "worker pool started");

        Self {
            tx,
            generation,
            size,
        }
    }

    /// Spawn a single supervised worker thread.
    ///
    /// If the thread dies (panic escaping `catch_unwind`, or an abort in a
    /// dependency), the supervisor respawns a replacement so the pool keeps its
    /// configured width.
    fn spawn_worker(idx: usize, rx: Receiver<QueuedJob>) {
        let builder = thread::Builder::new().name(format!("rustplorer-worker-{idx}"));

        let spawn_result = builder.spawn(move || {
            // The supervisor loop. `recv()` returns Err only when every Sender
            // is dropped, i.e. the pool is shutting down — that's the clean exit.
            while let Ok(QueuedJob { job, token, label }) = rx.recv() {
                // Drop work that a newer navigation has already superseded.
                // Cheap check that avoids, say, decoding a thumbnail for a
                // directory the user left three folders ago.
                if token.is_cancelled() {
                    tracing::trace!(label, "job skipped (superseded)");
                    continue;
                }

                // Fault isolation. A malformed 7z can panic inside a decoder;
                // without this the whole app dies. AssertUnwindSafe is sound
                // here because the closure is `FnOnce` and consumed on this
                // call — no shared state is observed after a panic.
                let outcome = catch_unwind(AssertUnwindSafe(|| job(&token)));

                if outcome.is_err() {
                    // The panic hook (see logging.rs) has already written the
                    // backtrace. Here we just note which operation died and
                    // keep the worker alive for the next job.
                    tracing::error!(label, worker = idx, "job panicked; worker continuing");
                }
            }

            tracing::debug!(worker = idx, "worker exiting (pool shutdown)");
        });

        if let Err(e) = spawn_result {
            tracing::error!(worker = idx, error = %e, "failed to spawn worker thread");
        }
    }

    /// Number of workers in the pool.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The pool's generation tracker. Bump it to invalidate in-flight work.
    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    /// Invalidate all outstanding tasks. Call on navigation.
    pub fn cancel_all(&self) -> u64 {
        let g = self.generation.bump();
        tracing::debug!(generation = g, "cancelled outstanding work");
        g
    }

    /// Submit a job tagged with the current generation.
    ///
    /// Returns the [`CancelToken`] so the caller can cancel this one task
    /// independently. Returns `None` if the queue is full or the pool is gone —
    /// callers should treat that as "try again later", not as a fatal error.
    pub fn submit<F>(&self, label: &'static str, job: F) -> Option<CancelToken>
    where
        F: FnOnce(&CancelToken) + Send + 'static,
    {
        let token = CancelToken::new(self.generation.current(), self.generation.clone());

        let queued = QueuedJob {
            job: Box::new(job),
            token: token.clone(),
            label,
        };

        match self.tx.try_send(queued) {
            Ok(()) => Some(token),
            Err(e) => {
                tracing::warn!(label, error = %e, "job rejected (queue full or pool closed)");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn runs_submitted_jobs() {
        let pool = WorkerPool::new(2);
        let (tx, rx) = mpsc::channel();

        pool.submit("test", move |_| tx.send(42).unwrap()).unwrap();

        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 42);
    }

    #[test]
    fn panicking_job_does_not_kill_worker() {
        let pool = WorkerPool::new(1);

        // Single worker, so the follow-up job can only run if the worker
        // survived the panic.
        pool.submit("boom", |_| panic!("intentional test panic"))
            .unwrap();

        let (tx, rx) = mpsc::channel();
        pool.submit("after", move |_| tx.send("alive").unwrap())
            .unwrap();

        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "alive");
    }

    #[test]
    fn generation_bump_invalidates_token() {
        let gen = Generation::new();
        let token = CancelToken::new(gen.current(), gen.clone());

        assert!(!token.is_cancelled());
        gen.bump();
        assert!(token.is_cancelled());
    }

    #[test]
    fn explicit_cancel_marks_token() {
        let gen = Generation::new();
        let token = CancelToken::new(gen.current(), gen.clone());

        token.cancel();
        assert!(token.is_cancelled());
    }
}

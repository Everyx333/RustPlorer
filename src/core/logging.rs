//! Diagnostics: rolling file logs, an in-memory ring buffer, and a panic hook.
//!
//! # Why this is more than "add a logger"
//!
//! RustPlorer is developed without access to the machine it runs on. Nobody can
//! watch the window while it misbehaves. So the logs *are* the debugging
//! interface, and they need to answer "what happened?" from a copy-paste.
//!
//! Three pieces:
//!
//! - A **rolling file** in `%LOCALAPPDATA%` for post-mortem analysis.
//! - An **in-memory ring buffer** of recent lines, so the app can surface its
//!   own recent history via the "Copy diagnostics" command without asking the
//!   user to find a file on disk.
//! - A **panic hook** that captures the backtrace before the stack unwinds.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// How many recent log lines to keep in memory for diagnostics.
///
/// 500 lines is enough to cover a navigation plus the failure that followed,
/// while staying trivially small in memory (a few hundred KB at most).
const RING_CAPACITY: usize = 500;

/// A fixed-size ring buffer of recent log lines.
///
/// Cloneable and internally synchronised, so it can be both a `tracing` writer
/// and read by the UI thread.
#[derive(Clone, Default)]
pub struct LogRing {
    inner: Arc<Mutex<RingInner>>,
}

#[derive(Default)]
struct RingInner {
    lines: std::collections::VecDeque<String>,
    /// Partial line accumulator. `tracing` may write a record in several
    /// `write()` calls, so we buffer until we see a newline.
    partial: String,
}

impl LogRing {
    pub fn new() -> Self {
        Self::default()
    }

    fn push_bytes(&self, buf: &[u8]) {
        let text = String::from_utf8_lossy(buf);
        let mut inner = self.inner.lock();

        for ch in text.chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut inner.partial);
                if inner.lines.len() >= RING_CAPACITY {
                    inner.lines.pop_front();
                }
                inner.lines.push_back(line);
            } else {
                inner.partial.push(ch);
            }
        }
    }

    /// Snapshot the most recent `n` lines, oldest first.
    pub fn recent(&self, n: usize) -> Vec<String> {
        let inner = self.inner.lock();
        let skip = inner.lines.len().saturating_sub(n);
        inner.lines.iter().skip(skip).cloned().collect()
    }

    /// All buffered lines joined with newlines — for "Copy diagnostics".
    pub fn dump(&self) -> String {
        self.recent(RING_CAPACITY).join("\n")
    }
}

impl io::Write for LogRing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.push_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogRing {
    type Writer = LogRing;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Handles returned by [`init`], kept alive for the process lifetime.
pub struct Diagnostics {
    /// Recent log lines, for the "Copy diagnostics" command.
    pub ring: LogRing,
    /// Directory the log files are written to.
    pub log_dir: Option<PathBuf>,
    /// Dropping this stops the background writer thread and flushes.
    _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Diagnostics")
            .field("log_dir", &self.log_dir)
            .finish_non_exhaustive()
    }
}

/// Initialise logging.
///
/// Level defaults to `info` and is overridable via the `RUSTPLORER_LOG`
/// environment variable (standard `EnvFilter` syntax, e.g.
/// `RUSTPLORER_LOG=rustplorer::core=trace`).
///
/// Logging to a file is best-effort: if the directory can't be created (locked
/// down profile, full disk), the app still runs with the in-memory ring only.
/// Diagnostics must never be the reason the app fails to start.
pub fn init(log_dir: Option<PathBuf>) -> Diagnostics {
    let filter = EnvFilter::try_from_env("RUSTPLORER_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let ring = LogRing::new();

    let ring_layer = tracing_subscriber::fmt::layer()
        .with_writer(ring.clone())
        .with_ansi(false)
        .with_target(true);

    // Try to attach a rolling file writer.
    let (file_layer, guard, resolved_dir) = match log_dir {
        Some(dir) => match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                let appender = tracing_appender::rolling::daily(&dir, "rustplorer.log");
                let (nb, guard) = tracing_appender::non_blocking(appender);
                let layer = tracing_subscriber::fmt::layer()
                    .with_writer(nb)
                    .with_ansi(false)
                    .with_target(true);
                (Some(layer), Some(guard), Some(dir))
            }
            Err(e) => {
                eprintln!("rustplorer: could not create log dir {dir:?}: {e}");
                (None, None, None)
            }
        },
        None => (None, None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(ring_layer)
        .with(file_layer)
        .init();

    if let Some(dir) = &resolved_dir {
        tracing::info!(dir = ?dir, "logging to file");
    } else {
        tracing::warn!("file logging unavailable; using in-memory log only");
    }

    Diagnostics {
        ring,
        log_dir: resolved_dir,
        _file_guard: guard,
    }
}

/// Install a panic hook that logs panics (with backtrace) before unwinding.
///
/// Ordering matters: worker threads catch panics with `catch_unwind`, but that
/// discards the backtrace. The hook runs *first*, at the panic site, while the
/// stack is intact — so the log gets the real origin, then `catch_unwind`
/// contains the damage.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());

        let message = extract_panic_message(info);
        let backtrace = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>").to_string();

        tracing::error!(
            location = %location,
            thread = %thread_name,
            message = %message,
            backtrace = %backtrace,
            "PANIC"
        );

        previous(info);
    }));
}

/// Pull a readable message out of a `PanicHookInfo` payload.
fn extract_panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();

    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn ring_buffer_collects_lines() {
        let mut ring = LogRing::new();
        ring.write_all(b"first\nsecond\n").unwrap();

        let lines = ring.recent(10);
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn ring_buffer_handles_split_writes() {
        let mut ring = LogRing::new();
        // tracing can emit one record across several write() calls.
        ring.write_all(b"par").unwrap();
        ring.write_all(b"tial\n").unwrap();

        assert_eq!(ring.recent(10), vec!["partial"]);
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut ring = LogRing::new();
        for i in 0..(RING_CAPACITY + 50) {
            writeln!(ring, "line{i}").unwrap();
        }

        let lines = ring.recent(RING_CAPACITY * 2);
        assert_eq!(lines.len(), RING_CAPACITY);
        // The first 50 should have been evicted.
        assert_eq!(lines[0], format!("line{}", 50));
    }
}

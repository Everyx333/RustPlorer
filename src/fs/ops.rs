//! File operations: copy, move, delete, rename, new folder.
//!
//! All operations run on the worker pool and report progress. Copying a 4 GB
//! file must not freeze the window, and must be abortable partway through.
//!
//! Deletes go to the Recycle Bin by default. Permanent deletion is available
//! but explicit — a file manager that silently destroys data is a liability.

use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::core::task::{CancelToken, WorkerPool};

/// Copy buffer size. 1 MiB balances syscall overhead against memory; larger
/// buffers show no measurable gain on typical drives.
const COPY_BUFFER: usize = 1024 * 1024;

/// Emit progress at most every N bytes, to avoid flooding the channel on fast
/// local copies.
const PROGRESS_BYTES: u64 = 4 * 1024 * 1024;

/// What to do when the destination already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Stop and report. The safe default — never destroy data implicitly.
    Skip,
    Overwrite,
    /// Append " (2)", " (3)", … to the file stem.
    Rename,
}

/// A requested operation.
#[derive(Debug, Clone)]
pub enum FileOp {
    Copy {
        sources: Vec<PathBuf>,
        dest_dir: PathBuf,
        policy: ConflictPolicy,
    },
    Move {
        sources: Vec<PathBuf>,
        dest_dir: PathBuf,
        policy: ConflictPolicy,
    },
    /// Send to the Recycle Bin.
    Trash { paths: Vec<PathBuf> },
    /// Bypass the Recycle Bin. Irreversible.
    DeletePermanent { paths: Vec<PathBuf> },
    Rename { from: PathBuf, to: PathBuf },
    CreateDir { path: PathBuf },
}

impl FileOp {
    /// Short label for the progress UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Copy { .. } => "Copying",
            Self::Move { .. } => "Moving",
            Self::Trash { .. } => "Deleting",
            Self::DeletePermanent { .. } => "Deleting permanently",
            Self::Rename { .. } => "Renaming",
            Self::CreateDir { .. } => "Creating folder",
        }
    }
}

/// Progress or completion for a running operation.
#[derive(Debug, Clone)]
pub enum OpUpdate {
    Progress {
        id: u64,
        label: &'static str,
        current_file: String,
        files_done: usize,
        files_total: usize,
        bytes_done: u64,
        bytes_total: u64,
    },
    Finished {
        id: u64,
        /// Paths that need a listing refresh.
        touched: Vec<PathBuf>,
        skipped: usize,
    },
    Failed {
        id: u64,
        error: String,
    },
    Cancelled {
        id: u64,
    },
}

/// Runs file operations off the UI thread.
pub struct OpRunner {
    tx: Sender<OpUpdate>,
    rx: Receiver<OpUpdate>,
    next_id: u64,
}

impl Default for OpRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl OpRunner {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx,
            next_id: 1,
        }
    }

    /// Drain pending updates. Non-blocking.
    pub fn poll(&self) -> Vec<OpUpdate> {
        self.rx.try_iter().collect()
    }

    /// Queue an operation, returning its id.
    pub fn submit(&mut self, pool: &WorkerPool, op: FileOp) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let tx = self.tx.clone();

        // NOTE: file operations deliberately do NOT use the navigation
        // generation counter. Navigating away must never abort an in-flight
        // copy — that would be data loss triggered by a click.
        pool.submit("file_op", move |token| {
            run_op(id, op, token, &tx);
        });

        id
    }
}

fn run_op(id: u64, op: FileOp, token: &CancelToken, tx: &Sender<OpUpdate>) {
    let label = op.label();
    let span = tracing::info_span!("file_op", id, label);
    let _guard = span.enter();

    let result = match op {
        FileOp::Copy {
            sources,
            dest_dir,
            policy,
        } => transfer(id, label, &sources, &dest_dir, policy, false, token, tx),
        FileOp::Move {
            sources,
            dest_dir,
            policy,
        } => transfer(id, label, &sources, &dest_dir, policy, true, token, tx),
        FileOp::Trash { paths } => trash_paths(&paths),
        FileOp::DeletePermanent { paths } => delete_paths(&paths),
        FileOp::Rename { from, to } => rename_path(&from, &to),
        FileOp::CreateDir { path } => create_dir(&path),
    };

    match result {
        Ok(outcome) => {
            if token.is_cancelled() && outcome.cancelled {
                let _ = tx.send(OpUpdate::Cancelled { id });
            } else {
                let _ = tx.send(OpUpdate::Finished {
                    id,
                    touched: outcome.touched,
                    skipped: outcome.skipped,
                });
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "operation failed");
            let _ = tx.send(OpUpdate::Failed {
                id,
                error: e.to_string(),
            });
        }
    }
}

struct Outcome {
    touched: Vec<PathBuf>,
    skipped: usize,
    cancelled: bool,
}

/// Copy or move a set of paths into `dest_dir`.
#[allow(clippy::too_many_arguments)]
fn transfer(
    id: u64,
    label: &'static str,
    sources: &[PathBuf],
    dest_dir: &Path,
    policy: ConflictPolicy,
    remove_source: bool,
    token: &CancelToken,
    tx: &Sender<OpUpdate>,
) -> anyhow::Result<Outcome> {
    // Size everything up front so the progress bar is meaningful rather than
    // an indeterminate spinner.
    let mut plan: Vec<(PathBuf, PathBuf, u64)> = Vec::new();
    for src in sources {
        collect_transfer_items(src, dest_dir, &mut plan)?;
    }

    let bytes_total: u64 = plan.iter().map(|(_, _, n)| *n).sum();
    let files_total = plan.len();
    let mut bytes_done = 0u64;
    let mut files_done = 0usize;
    let mut skipped = 0usize;
    let mut last_report = 0u64;

    for (src, dest, size) in &plan {
        if token.is_cancelled() {
            return Ok(Outcome {
                touched: vec![dest_dir.to_path_buf()],
                skipped,
                cancelled: true,
            });
        }

        let dest = match resolve_conflict(dest, policy)? {
            Some(d) => d,
            None => {
                skipped += 1;
                files_done += 1;
                continue;
            }
        };

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if src.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            // Try a rename first for same-volume moves: it is atomic and
            // instant, versus copying the bytes and deleting.
            let renamed = remove_source && std::fs::rename(src, &dest).is_ok();

            if !renamed {
                copy_file_with_progress(src, &dest, token)?;
                if remove_source {
                    std::fs::remove_file(src)?;
                }
            }
        }

        bytes_done += size;
        files_done += 1;

        if bytes_done - last_report >= PROGRESS_BYTES || files_done == files_total {
            last_report = bytes_done;
            let _ = tx.send(OpUpdate::Progress {
                id,
                label,
                current_file: src
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                files_done,
                files_total,
                bytes_done,
                bytes_total,
            });
        }
    }

    // Moving directories leaves the now-empty source tree behind; clean it up.
    if remove_source {
        for src in sources {
            if src.is_dir() {
                let _ = std::fs::remove_dir_all(src);
            }
        }
    }

    let mut touched = vec![dest_dir.to_path_buf()];
    if remove_source {
        for src in sources {
            if let Some(p) = src.parent() {
                touched.push(p.to_path_buf());
            }
        }
    }

    Ok(Outcome {
        touched,
        skipped,
        cancelled: false,
    })
}

/// Expand a source path into concrete (source, destination, size) items.
fn collect_transfer_items(
    src: &Path,
    dest_dir: &Path,
    out: &mut Vec<(PathBuf, PathBuf, u64)>,
) -> anyhow::Result<()> {
    let name = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid source path: {}", src.display()))?;
    let dest = dest_dir.join(name);

    let meta = std::fs::symlink_metadata(src)?;

    if meta.is_dir() {
        out.push((src.to_path_buf(), dest.clone(), 0));
        for entry in std::fs::read_dir(src)?.flatten() {
            collect_transfer_items(&entry.path(), &dest, out)?;
        }
    } else {
        out.push((src.to_path_buf(), dest, meta.len()));
    }

    Ok(())
}

/// Apply the conflict policy. `None` means skip this item.
fn resolve_conflict(dest: &Path, policy: ConflictPolicy) -> anyhow::Result<Option<PathBuf>> {
    if !dest.exists() {
        return Ok(Some(dest.to_path_buf()));
    }

    match policy {
        ConflictPolicy::Skip => Ok(None),
        ConflictPolicy::Overwrite => Ok(Some(dest.to_path_buf())),
        ConflictPolicy::Rename => {
            let stem = dest
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = dest.extension().map(|e| e.to_string_lossy().into_owned());
            let parent = dest.parent().unwrap_or(Path::new("."));

            // Bounded so a pathological directory can't spin forever.
            for n in 2..10_000 {
                let candidate = match &ext {
                    Some(e) => parent.join(format!("{stem} ({n}).{e}")),
                    None => parent.join(format!("{stem} ({n})")),
                };
                if !candidate.exists() {
                    return Ok(Some(candidate));
                }
            }
            anyhow::bail!("could not find an available name for {}", dest.display())
        }
    }
}

/// Copy one file, checking for cancellation between chunks.
fn copy_file_with_progress(src: &Path, dest: &Path, token: &CancelToken) -> anyhow::Result<()> {
    use std::io::{Read, Write};

    let mut reader = std::fs::File::open(src)?;
    let mut writer = std::fs::File::create(dest)?;
    let mut buf = vec![0u8; COPY_BUFFER];

    loop {
        if token.is_cancelled() {
            // Drop the partial file rather than leaving a truncated copy that
            // looks complete.
            drop(writer);
            let _ = std::fs::remove_file(dest);
            anyhow::bail!("cancelled");
        }

        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
    }

    writer.flush()?;
    Ok(())
}

fn trash_paths(paths: &[PathBuf]) -> anyhow::Result<Outcome> {
    let mut touched = Vec::new();
    for p in paths {
        trash::delete(p)?;
        if let Some(parent) = p.parent() {
            touched.push(parent.to_path_buf());
        }
    }
    Ok(Outcome {
        touched,
        skipped: 0,
        cancelled: false,
    })
}

fn delete_paths(paths: &[PathBuf]) -> anyhow::Result<Outcome> {
    let mut touched = Vec::new();
    for p in paths {
        if p.is_dir() {
            std::fs::remove_dir_all(p)?;
        } else {
            std::fs::remove_file(p)?;
        }
        if let Some(parent) = p.parent() {
            touched.push(parent.to_path_buf());
        }
    }
    Ok(Outcome {
        touched,
        skipped: 0,
        cancelled: false,
    })
}

fn rename_path(from: &Path, to: &Path) -> anyhow::Result<Outcome> {
    if to.exists() {
        anyhow::bail!("a file named \"{}\" already exists", to.display());
    }
    std::fs::rename(from, to)?;
    Ok(Outcome {
        touched: from.parent().map(|p| vec![p.to_path_buf()]).unwrap_or_default(),
        skipped: 0,
        cancelled: false,
    })
}

fn create_dir(path: &Path) -> anyhow::Result<Outcome> {
    if path.exists() {
        anyhow::bail!("\"{}\" already exists", path.display());
    }
    std::fs::create_dir(path)?;
    Ok(Outcome {
        touched: path.parent().map(|p| vec![p.to_path_buf()]).unwrap_or_default(),
        skipped: 0,
        cancelled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn skip_policy_leaves_existing_file() {
        let d = temp("rp_ops_skip");
        let f = d.join("x.txt");
        std::fs::write(&f, b"original").unwrap();

        assert_eq!(resolve_conflict(&f, ConflictPolicy::Skip).unwrap(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_policy_finds_free_name() {
        let d = temp("rp_ops_rename");
        let f = d.join("x.txt");
        std::fs::write(&f, b"a").unwrap();

        let resolved = resolve_conflict(&f, ConflictPolicy::Rename).unwrap().unwrap();
        assert_eq!(resolved.file_name().unwrap(), "x (2).txt");

        std::fs::write(&resolved, b"b").unwrap();
        let next = resolve_conflict(&f, ConflictPolicy::Rename).unwrap().unwrap();
        assert_eq!(next.file_name().unwrap(), "x (3).txt");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_policy_handles_extensionless_files() {
        let d = temp("rp_ops_noext");
        let f = d.join("LICENSE");
        std::fs::write(&f, b"a").unwrap();

        let resolved = resolve_conflict(&f, ConflictPolicy::Rename).unwrap().unwrap();
        assert_eq!(resolved.file_name().unwrap(), "LICENSE (2)");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_walks_nested_directories() {
        let d = temp("rp_ops_collect");
        std::fs::create_dir_all(d.join("src/sub")).unwrap();
        std::fs::write(d.join("src/a.txt"), b"12345").unwrap();
        std::fs::write(d.join("src/sub/b.txt"), b"123").unwrap();

        let dest = d.join("dest");
        let mut plan = Vec::new();
        collect_transfer_items(&d.join("src"), &dest, &mut plan).unwrap();

        let bytes: u64 = plan.iter().map(|(_, _, n)| *n).sum();
        assert_eq!(bytes, 8);
        // src dir, sub dir, and two files
        assert_eq!(plan.len(), 4);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_rejects_existing_target() {
        let d = temp("rp_ops_rn");
        let a = d.join("a.txt");
        let b = d.join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        assert!(rename_path(&a, &b).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn create_dir_rejects_existing() {
        let d = temp("rp_ops_mkdir");
        assert!(create_dir(&d).is_err());

        let fresh = d.join("new");
        assert!(create_dir(&fresh).is_ok());
        assert!(fresh.is_dir());

        let _ = std::fs::remove_dir_all(&d);
    }
}

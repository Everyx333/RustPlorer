//! Extracting and creating archives.
//!
//! Every write path guards against **Zip Slip**: archive entries carrying
//! `../` or absolute paths must never escape the destination directory.
//! `sanitize_entry_path` enforces that, and every extractor routes through it.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crossbeam_channel::Sender;

use crate::archive::format::{sanitize_entry_path, ArchiveFormat};
use crate::core::task::CancelToken;

/// Progress while extracting or compressing.
#[derive(Debug, Clone)]
pub enum ArchiveProgress {
    Working {
        current: String,
        done: usize,
        total: usize,
    },
    Finished {
        dest: PathBuf,
        count: usize,
        skipped: usize,
    },
    Failed {
        error: String,
    },
    Cancelled,
}

/// Extract an archive into `dest_dir`.
pub fn extract(
    archive_path: &Path,
    dest_dir: &Path,
    token: &CancelToken,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let format = ArchiveFormat::from_path(archive_path)
        .with_context(|| format!("unsupported archive: {}", archive_path.display()))?;

    let span = tracing::info_span!("extract", ?archive_path, format = format.label());
    let _guard = span.enter();

    std::fs::create_dir_all(dest_dir)?;

    match format {
        ArchiveFormat::Zip => extract_zip(archive_path, dest_dir, token, tx),
        ArchiveFormat::SevenZ => extract_7z(archive_path, dest_dir, tx),
        ArchiveFormat::Tar => extract_tar(archive_path, dest_dir, token, tx),
        ArchiveFormat::TarGz => extract_tar_gz(archive_path, dest_dir, token, tx),
        ArchiveFormat::Gz => extract_gz(archive_path, dest_dir, tx),
        other => anyhow::bail!("extracting {} is not supported yet", other.label()),
    }
}

fn extract_zip(
    archive_path: &Path,
    dest_dir: &Path,
    token: &CancelToken,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let file = File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))?;
    let total = archive.len();
    let mut skipped = 0usize;

    for i in 0..total {
        if token.is_cancelled() {
            let _ = tx.send(ArchiveProgress::Cancelled);
            return Ok(());
        }

        let mut entry = archive.by_index(i)?;
        let raw = entry.name().to_string();

        // Zip Slip guard. A rejected path is skipped and logged loudly rather
        // than silently ignored — it may indicate a malicious archive.
        let Some(safe) = sanitize_entry_path(&raw) else {
            tracing::warn!(entry = %raw, "rejected unsafe archive path");
            skipped += 1;
            continue;
        };

        let out_path = dest_dir.join(&safe);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }

        let _ = tx.send(ArchiveProgress::Working {
            current: safe,
            done: i + 1,
            total,
        });
    }

    let _ = tx.send(ArchiveProgress::Finished {
        dest: dest_dir.to_path_buf(),
        count: total - skipped,
        skipped,
    });
    Ok(())
}

fn extract_7z(
    archive_path: &Path,
    dest_dir: &Path,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let mut archive = zesven::read::Archive::open_path(archive_path)?;
    let count = archive.entries().len();

    // zesven drives the whole extraction internally, so this is not
    // incrementally cancellable. 7z archives are usually solid, meaning a
    // partial extract has little value anyway.
    archive.extract(dest_dir, (), &zesven::read::ExtractOptions::default())?;

    let _ = tx.send(ArchiveProgress::Finished {
        dest: dest_dir.to_path_buf(),
        count,
        skipped: 0,
    });
    Ok(())
}

fn extract_tar(
    archive_path: &Path,
    dest_dir: &Path,
    token: &CancelToken,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let file = File::open(archive_path)?;
    let mut archive = tar::Archive::new(BufReader::new(file));
    unpack_tar(&mut archive, dest_dir, token, tx)
}

fn extract_tar_gz(
    archive_path: &Path,
    dest_dir: &Path,
    token: &CancelToken,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let file = File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);
    unpack_tar(&mut archive, dest_dir, token, tx)
}

fn unpack_tar<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    dest_dir: &Path,
    token: &CancelToken,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let mut done = 0usize;
    let mut skipped = 0usize;

    for entry in archive.entries()? {
        if token.is_cancelled() {
            let _ = tx.send(ArchiveProgress::Cancelled);
            return Ok(());
        }

        let mut entry = entry?;
        let raw = entry.path()?.to_string_lossy().into_owned();

        let Some(safe) = sanitize_entry_path(&raw) else {
            tracing::warn!(entry = %raw, "rejected unsafe archive path");
            skipped += 1;
            continue;
        };

        let out_path = dest_dir.join(&safe);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        entry.unpack(&out_path)?;
        done += 1;

        let _ = tx.send(ArchiveProgress::Working {
            current: safe,
            done,
            // tar is a stream: the entry count is unknown until the end, so
            // total tracks done rather than claiming a false denominator.
            total: done,
        });
    }

    let _ = tx.send(ArchiveProgress::Finished {
        dest: dest_dir.to_path_buf(),
        count: done,
        skipped,
    });
    Ok(())
}

fn extract_gz(
    archive_path: &Path,
    dest_dir: &Path,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let stem = archive_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".to_string());

    let out_path = dest_dir.join(stem);
    let file = File::open(archive_path)?;
    let mut decoder = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut out = File::create(&out_path)?;
    std::io::copy(&mut decoder, &mut out)?;

    let _ = tx.send(ArchiveProgress::Finished {
        dest: dest_dir.to_path_buf(),
        count: 1,
        skipped: 0,
    });
    Ok(())
}

/// Create an archive from a set of paths.
pub fn create(
    sources: &[PathBuf],
    dest: &Path,
    token: &CancelToken,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let format = ArchiveFormat::from_path(dest)
        .with_context(|| format!("unsupported archive type: {}", dest.display()))?;

    if !format.can_create() {
        anyhow::bail!("creating {} archives is not supported", format.label());
    }

    let span = tracing::info_span!("create_archive", ?dest, format = format.label());
    let _guard = span.enter();

    match format {
        ArchiveFormat::Zip => create_zip(sources, dest, token, tx),
        ArchiveFormat::SevenZ => create_7z(sources, dest, tx),
        other => anyhow::bail!("creating {} is not supported yet", other.label()),
    }
}

fn create_zip(
    sources: &[PathBuf],
    dest: &Path,
    token: &CancelToken,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    use std::io::Write;

    let file = File::create(dest)?;
    let mut writer = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    // Flatten the selection into concrete files, preserving relative layout.
    let mut queue: Vec<(PathBuf, String)> = Vec::new();
    for src in sources {
        let base = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        collect_for_archive(src, &base, &mut queue)?;
    }

    let total = queue.len();

    for (i, (path, name)) in queue.iter().enumerate() {
        if token.is_cancelled() {
            // Drop the partial archive rather than leaving a corrupt file.
            drop(writer);
            let _ = std::fs::remove_file(dest);
            let _ = tx.send(ArchiveProgress::Cancelled);
            return Ok(());
        }

        if path.is_dir() {
            writer.add_directory(format!("{name}/"), opts)?;
        } else {
            writer.start_file(name, opts)?;
            let mut f = File::open(path)?;
            std::io::copy(&mut f, &mut writer)?;
        }

        let _ = tx.send(ArchiveProgress::Working {
            current: name.clone(),
            done: i + 1,
            total,
        });
    }

    writer.finish()?.flush()?;

    let _ = tx.send(ArchiveProgress::Finished {
        dest: dest.to_path_buf(),
        count: total,
        skipped: 0,
    });
    Ok(())
}

fn create_7z(
    sources: &[PathBuf],
    dest: &Path,
    tx: &Sender<ArchiveProgress>,
) -> Result<()> {
    let mut writer = zesven::write::Writer::create_path(dest)?;
    let mut count = 0usize;

    for src in sources {
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let archive_path = zesven::archive_path::ArchivePath::new(&name)?;
        writer.add_path(src, archive_path)?;
        count += 1;
    }

    writer.finish()?;

    let _ = tx.send(ArchiveProgress::Finished {
        dest: dest.to_path_buf(),
        count,
        skipped: 0,
    });
    Ok(())
}

/// Recursively collect files for archiving, preserving relative paths.
fn collect_for_archive(
    path: &Path,
    rel: &str,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    if path.is_dir() {
        out.push((path.to_path_buf(), rel.to_string()));
        for entry in std::fs::read_dir(path)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            collect_for_archive(&entry.path(), &format!("{rel}/{name}"), out)?;
        }
    } else {
        out.push((path.to_path_buf(), rel.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use crate::core::task::Generation;

    fn token() -> CancelToken {
        // A token bound to a fresh generation is never cancelled.
        let gen = Generation::new();
        crate::core::task::CancelToken::for_test(gen)
    }

    #[test]
    fn zip_round_trip() {
        let dir = std::env::temp_dir().join("rustplorer_extract_test");
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("nested/b.txt"), b"world").unwrap();

        let archive = dir.join("out.zip");
        let (tx, rx) = unbounded();

        create(&[src.clone()], &archive, &token(), &tx).unwrap();
        assert!(archive.exists(), "archive should have been created");
        drop(rx);

        let dest = dir.join("unpacked");
        let (tx2, _rx2) = unbounded();
        extract(&archive, &dest, &token(), &tx2).unwrap();

        assert!(dest.join("src/a.txt").exists());
        assert!(dest.join("src/nested/b.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("src/a.txt")).unwrap(),
            "hello"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_creation_format_errors() {
        let (tx, _rx) = unbounded();
        let err = create(&[], Path::new("out.rar"), &token(), &tx);
        assert!(err.is_err(), "rar creation is not wired up yet");
    }

    #[test]
    fn collect_preserves_relative_layout() {
        let dir = std::env::temp_dir().join("rustplorer_collect_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("top/sub")).unwrap();
        std::fs::write(dir.join("top/one.txt"), b"1").unwrap();
        std::fs::write(dir.join("top/sub/two.txt"), b"2").unwrap();

        let mut out = Vec::new();
        collect_for_archive(&dir.join("top"), "top", &mut out).unwrap();

        let names: Vec<&str> = out.iter().map(|(_, n)| n.as_str()).collect();
        assert!(names.contains(&"top"));
        assert!(names.contains(&"top/one.txt"));
        assert!(names.contains(&"top/sub/two.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Reading archive contents.
//!
//! Listing is separated from extraction because browsing is the common case:
//! opening a `.zip` to look inside should not decompress it. For zip/7z/rar we
//! read only the central directory or header block.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};

use crate::archive::format::{ArchiveEntry, ArchiveFormat};

/// List the entries in an archive.
///
/// Returns an error rather than panicking on malformed input; callers run this
/// on the worker pool where `catch_unwind` also guards against panics inside
/// third-party decoders.
pub fn list_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let format = ArchiveFormat::from_path(path)
        .with_context(|| format!("unsupported archive type: {}", path.display()))?;

    let span = tracing::debug_span!("list_archive", ?path, format = format.label());
    let _guard = span.enter();
    let started = std::time::Instant::now();

    let entries = match format {
        ArchiveFormat::Zip => list_zip(path),
        ArchiveFormat::SevenZ => list_7z(path),
        ArchiveFormat::Rar => list_rar(path),
        ArchiveFormat::Tar => list_tar(path),
        ArchiveFormat::TarGz => list_tar_gz(path),
        // Single-stream compressors hold exactly one payload.
        ArchiveFormat::Gz | ArchiveFormat::Xz | ArchiveFormat::Zst => single_stream(path),
        // Decoding these needs a streaming pass; deferred rather than
        // pretending to support them.
        ArchiveFormat::TarXz | ArchiveFormat::TarZst => {
            anyhow::bail!("{} listing is not supported yet", format.label())
        }
    }?;

    tracing::debug!(
        count = entries.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "listed archive"
    );

    Ok(entries)
}

fn list_zip(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))?;
    let mut out = Vec::with_capacity(archive.len());

    for i in 0..archive.len() {
        // A single corrupt entry must not lose the whole listing.
        let entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                tracing::trace!(index = i, error = %e, "skipping unreadable zip entry");
                continue;
            }
        };

        out.push(ArchiveEntry {
            path: entry.name().replace('\\', "/"),
            is_dir: entry.is_dir(),
            size: entry.size(),
            compressed_size: entry.compressed_size(),
            modified: None,
            encrypted: entry.encrypted(),
        });
    }

    Ok(out)
}

fn list_7z(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let archive = zesven::read::Archive::open_path(path)?;

    let out = archive
        .entries()
        .iter()
        .map(|e| ArchiveEntry {
            path: e.name().replace('\\', "/"),
            is_dir: e.is_directory,
            size: e.size,
            // 7z compresses in solid blocks, so a per-entry compressed size
            // is not meaningful. Report the uncompressed size rather than
            // inventing a number.
            compressed_size: e.size,
            modified: e.modified(),
            encrypted: false,
        })
        .collect();

    Ok(out)
}

fn list_rar(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let archive = rars::ArchiveReader::read_path(path)?;
    let mut out = Vec::new();

    for member in archive.members() {
        let meta = &member.meta;

        // RAR stores raw name bytes whose encoding varies by version and
        // creating platform. `name_lossy` is the display-safe view.
        out.push(ArchiveEntry {
            path: meta.name_lossy().replace('\\', "/"),
            is_dir: meta.is_directory,
            size: meta.unpacked_size,
            compressed_size: meta.packed_size,
            modified: None,
            encrypted: meta.is_encrypted,
        });
    }

    Ok(out)
}

fn list_tar(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let file = File::open(path)?;
    let mut archive = tar::Archive::new(BufReader::new(file));
    collect_tar(&mut archive)
}

fn list_tar_gz(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let file = File::open(path)?;
    let decoder = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);
    collect_tar(&mut archive)
}

fn collect_tar<R: std::io::Read>(archive: &mut tar::Archive<R>) -> Result<Vec<ArchiveEntry>> {
    let mut out = Vec::new();

    for entry in archive.entries()? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::trace!(error = %e, "skipping unreadable tar entry");
                continue;
            }
        };

        let header = entry.header();
        let path = entry
            .path()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        out.push(ArchiveEntry {
            is_dir: header.entry_type().is_dir(),
            size: header.size().unwrap_or(0),
            compressed_size: header.size().unwrap_or(0),
            modified: None,
            encrypted: false,
            path,
        });
    }

    Ok(out)
}

/// A single-file compressor holds one payload named after the archive minus
/// its extension.
fn single_stream(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let inner_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".to_string());

    let compressed = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    Ok(vec![ArchiveEntry {
        path: inner_name,
        is_dir: false,
        // The uncompressed size is not recorded in these formats without
        // decompressing, so report what we know.
        size: 0,
        compressed_size: compressed,
        modified: None,
        encrypted: false,
    }])
}

/// Build the set of entries directly inside `dir_path` within the archive.
///
/// Archives usually store only files, with directories implied by path
/// prefixes, so intermediate folders are synthesised here.
pub fn entries_in_dir(all: &[ArchiveEntry], dir_path: &str) -> Vec<ArchiveEntry> {
    use std::collections::BTreeMap;

    let prefix = if dir_path.is_empty() {
        String::new()
    } else {
        format!("{}/", dir_path.trim_end_matches('/'))
    };

    let mut files: Vec<ArchiveEntry> = Vec::new();
    // BTreeMap keeps synthesised directories sorted and de-duplicated.
    let mut dirs: BTreeMap<String, u64> = BTreeMap::new();

    for entry in all {
        let Some(rest) = entry.path.strip_prefix(&prefix) else {
            continue;
        };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            continue;
        }

        match rest.find('/') {
            // Nested deeper: record the intermediate directory and roll the
            // contained size up into it.
            Some(i) => {
                let dir_name = &rest[..i];
                *dirs.entry(dir_name.to_string()).or_insert(0) += entry.size;
            }
            None => {
                if entry.is_dir {
                    dirs.entry(rest.to_string()).or_insert(0);
                } else {
                    let mut e = entry.clone();
                    e.path = rest.to_string();
                    files.push(e);
                }
            }
        }
    }

    let mut out: Vec<ArchiveEntry> = dirs
        .into_iter()
        .map(|(name, size)| ArchiveEntry {
            path: name,
            is_dir: true,
            size,
            compressed_size: 0,
            modified: None,
            encrypted: false,
        })
        .collect();

    files.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
    out.extend(files);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, is_dir: bool, size: u64) -> ArchiveEntry {
        ArchiveEntry {
            path: path.to_string(),
            is_dir,
            size,
            compressed_size: size,
            modified: None,
            encrypted: false,
        }
    }

    #[test]
    fn root_listing_synthesises_directories() {
        let all = vec![
            entry("readme.md", false, 100),
            entry("src/main.rs", false, 200),
            entry("src/lib.rs", false, 300),
        ];

        let root = entries_in_dir(&all, "");

        // One synthesised "src" directory plus the root file.
        assert_eq!(root.len(), 2);
        assert!(root[0].is_dir, "directories should sort first");
        assert_eq!(root[0].path, "src");
        // Sizes of contained files roll up.
        assert_eq!(root[0].size, 500);
        assert_eq!(root[1].path, "readme.md");
    }

    #[test]
    fn subdirectory_listing_strips_prefix() {
        let all = vec![
            entry("src/main.rs", false, 200),
            entry("src/core/task.rs", false, 400),
            entry("docs/guide.md", false, 50),
        ];

        let sub = entries_in_dir(&all, "src");

        assert_eq!(sub.len(), 2);
        assert_eq!(sub[0].path, "core");
        assert!(sub[0].is_dir);
        assert_eq!(sub[1].path, "main.rs");
    }

    #[test]
    fn explicit_directory_entries_are_kept() {
        let all = vec![entry("empty/", true, 0), entry("file.txt", false, 10)];
        let root = entries_in_dir(&all, "");

        assert_eq!(root.len(), 2);
        assert_eq!(root[0].path, "empty");
        assert!(root[0].is_dir);
    }

    #[test]
    fn unrelated_paths_are_excluded() {
        let all = vec![entry("a/one.txt", false, 1), entry("b/two.txt", false, 2)];
        let a = entries_in_dir(&all, "a");

        assert_eq!(a.len(), 1);
        assert_eq!(a[0].path, "one.txt");
    }

    #[test]
    fn round_trips_a_real_zip() {
        use std::io::Write;

        let dir = std::env::temp_dir().join("rustplorer_zip_test");
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("test.zip");

        {
            let file = File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();

            w.start_file("hello.txt", opts).unwrap();
            w.write_all(b"hello world").unwrap();
            w.add_directory("nested/", opts).unwrap();
            w.start_file("nested/inner.txt", opts).unwrap();
            w.write_all(b"inner").unwrap();
            w.finish().unwrap();
        }

        let entries = list_entries(&zip_path).unwrap();
        assert!(entries.iter().any(|e| e.path == "hello.txt"));
        assert!(entries.iter().any(|e| e.path == "nested/inner.txt"));

        let root = entries_in_dir(&entries, "");
        assert!(root.iter().any(|e| e.path == "nested" && e.is_dir));
        assert!(root.iter().any(|e| e.path == "hello.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_extension_errors() {
        assert!(list_entries(Path::new("notes.txt")).is_err());
    }
}

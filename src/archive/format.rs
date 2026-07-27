//! Archive format detection and the shared entry model.

use std::path::Path;

/// Archive formats RustPlorer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    SevenZ,
    Rar,
    Tar,
    TarGz,
    TarXz,
    TarZst,
    Gz,
    Xz,
    Zst,
}

impl ArchiveFormat {
    /// Detect a format from a file name.
    ///
    /// Compound extensions are checked first: `.tar.gz` must not be mistaken
    /// for a plain `.gz`, or we would extract a tarball instead of unpacking
    /// its contents.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();

        for (suffix, format) in [
            (".tar.gz", Self::TarGz),
            (".tgz", Self::TarGz),
            (".tar.xz", Self::TarXz),
            (".txz", Self::TarXz),
            (".tar.zst", Self::TarZst),
            (".tzst", Self::TarZst),
        ] {
            if name.ends_with(suffix) {
                return Some(format);
            }
        }

        let ext = path.extension()?.to_string_lossy().to_lowercase();
        match ext.as_str() {
            "zip" => Some(Self::Zip),
            // Self-extracting and jar/apk variants are all zip containers.
            "jar" | "apk" | "epub" | "whl" => Some(Self::Zip),
            "7z" => Some(Self::SevenZ),
            "rar" => Some(Self::Rar),
            "tar" => Some(Self::Tar),
            "gz" => Some(Self::Gz),
            "xz" => Some(Self::Xz),
            "zst" => Some(Self::Zst),
            _ => None,
        }
    }

    /// True if the path looks like a browsable archive.
    pub fn is_archive(path: &Path) -> bool {
        Self::from_path(path).is_some()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::SevenZ => "7-Zip",
            Self::Rar => "RAR",
            Self::Tar => "TAR",
            Self::TarGz => "TAR.GZ",
            Self::TarXz => "TAR.XZ",
            Self::TarZst => "TAR.ZST",
            Self::Gz => "GZIP",
            Self::Xz => "XZ",
            Self::Zst => "ZSTD",
        }
    }

    /// Whether RustPlorer can create archives of this format.
    ///
    /// Single-stream compressors (`.gz`, `.xz`, `.zst` on their own) hold one
    /// file with no directory structure, so "create archive from selection"
    /// does not apply to them.
    pub fn can_create(self) -> bool {
        matches!(
            self,
            Self::Zip | Self::SevenZ | Self::Rar | Self::Tar | Self::TarGz
        )
    }

    /// Whether entries can be listed without decompressing everything.
    ///
    /// Solid and streaming formats must be decompressed sequentially, so
    /// listing them is proportionally expensive.
    pub fn supports_fast_listing(self) -> bool {
        matches!(self, Self::Zip | Self::SevenZ | Self::Rar)
    }
}

/// One entry inside an archive.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Path within the archive, using forward slashes.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub compressed_size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub encrypted: bool,
}

impl ArchiveEntry {
    /// The final path component.
    pub fn name(&self) -> &str {
        self.path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
    }

    /// Parent path within the archive, or `""` at the root.
    pub fn parent(&self) -> &str {
        let trimmed = self.path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(i) => &trimmed[..i],
            None => "",
        }
    }

    /// Depth from the archive root.
    pub fn depth(&self) -> usize {
        self.path.trim_end_matches('/').matches('/').count()
    }
}

/// Normalize a path found inside an archive.
///
/// **Security-critical.** Archives can contain `../../../Windows/System32/...`
/// or absolute paths; extracting those verbatim is the "Zip Slip"
/// vulnerability. Everything is forced to stay relative and inside the target.
pub fn sanitize_entry_path(raw: &str) -> Option<String> {
    let unified = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();

    for part in unified.split('/') {
        match part {
            // Skip no-ops and leading slashes.
            "" | "." => continue,
            // Reject traversal outright rather than trying to resolve it.
            ".." => return None,
            p => {
                // Windows drive prefixes ("C:") must never survive.
                if p.len() >= 2 && p.as_bytes()[1] == b':' {
                    return None;
                }
                parts.push(p);
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_simple_extensions() {
        assert_eq!(
            ArchiveFormat::from_path(&PathBuf::from("a.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_path(&PathBuf::from("a.7z")),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(
            ArchiveFormat::from_path(&PathBuf::from("a.rar")),
            Some(ArchiveFormat::Rar)
        );
    }

    #[test]
    fn compound_extensions_beat_simple_ones() {
        // The bug this prevents: treating archive.tar.gz as a plain .gz and
        // producing a .tar instead of the files inside it.
        assert_eq!(
            ArchiveFormat::from_path(&PathBuf::from("archive.tar.gz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(&PathBuf::from("archive.gz")),
            Some(ArchiveFormat::Gz)
        );
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(
            ArchiveFormat::from_path(&PathBuf::from("A.ZIP")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_path(&PathBuf::from("B.Tar.Gz")),
            Some(ArchiveFormat::TarGz)
        );
    }

    #[test]
    fn non_archives_are_rejected() {
        assert_eq!(ArchiveFormat::from_path(&PathBuf::from("notes.txt")), None);
        assert_eq!(ArchiveFormat::from_path(&PathBuf::from("noext")), None);
    }

    #[test]
    fn zip_container_formats_detected() {
        for n in ["app.jar", "app.apk", "book.epub"] {
            assert_eq!(
                ArchiveFormat::from_path(&PathBuf::from(n)),
                Some(ArchiveFormat::Zip),
                "{n} should be recognised as a zip container"
            );
        }
    }

    #[test]
    fn zip_slip_traversal_is_rejected() {
        assert_eq!(sanitize_entry_path("../../../etc/passwd"), None);
        assert_eq!(sanitize_entry_path("a/../../b"), None);
        assert_eq!(sanitize_entry_path("..\\..\\windows\\system32"), None);
    }

    #[test]
    fn absolute_and_drive_paths_are_rejected() {
        assert_eq!(sanitize_entry_path("C:/Windows/evil.dll"), None);
        // A leading slash is stripped rather than rejected: it is merely
        // sloppy, not an escape attempt.
        assert_eq!(
            sanitize_entry_path("/usr/local/bin"),
            Some("usr/local/bin".to_string())
        );
    }

    #[test]
    fn normal_paths_pass_through() {
        assert_eq!(
            sanitize_entry_path("src/main.rs"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            sanitize_entry_path("docs\\guide.md"),
            Some("docs/guide.md".to_string())
        );
    }

    #[test]
    fn entry_name_and_parent() {
        let e = ArchiveEntry {
            path: "src/core/task.rs".into(),
            is_dir: false,
            size: 0,
            compressed_size: 0,
            modified: None,
            encrypted: false,
        };
        assert_eq!(e.name(), "task.rs");
        assert_eq!(e.parent(), "src/core");
        assert_eq!(e.depth(), 2);
    }

    #[test]
    fn root_entry_has_empty_parent() {
        let e = ArchiveEntry {
            path: "readme.md".into(),
            is_dir: false,
            size: 0,
            compressed_size: 0,
            modified: None,
            encrypted: false,
        };
        assert_eq!(e.parent(), "");
        assert_eq!(e.depth(), 0);
    }

    #[test]
    fn directory_entry_name_ignores_trailing_slash() {
        let e = ArchiveEntry {
            path: "src/core/".into(),
            is_dir: true,
            size: 0,
            compressed_size: 0,
            modified: None,
            encrypted: false,
        };
        assert_eq!(e.name(), "core");
        assert_eq!(e.parent(), "src");
    }
}

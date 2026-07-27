//! The data model for a single filesystem entry.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What kind of thing an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    /// Ordering matters: `Ord` is derived, and Directory sorting before File
    /// gives the conventional "folders on top" listing for free.
    Directory,
    File,
    Symlink,
}

/// A single row in the file listing.
///
/// Deliberately a flat, owned struct. The UI renders from an immutable snapshot
/// of these, so they must not borrow from the scanner that produced them.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    /// Display name (file name only). Cached because `path.file_name()` is
    /// called on every frame for every visible row.
    pub name: String,
    pub kind: EntryKind,
    /// File size in bytes. `None` for directories whose size hasn't been
    /// computed yet — folder sizing is a separate, opt-in background job.
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub is_hidden: bool,
    pub is_readonly: bool,
    /// Lowercased extension, without the dot. Cached for cheap filtering.
    pub extension: Option<String>,
}

impl Entry {
    /// Build an `Entry` from a path and its metadata.
    pub fn from_metadata(path: PathBuf, meta: &std::fs::Metadata) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            // A path with no file name is a root such as `C:\`; show the whole
            // thing rather than an empty row.
            .unwrap_or_else(|| path.to_string_lossy().into_owned());

        let kind = if meta.is_dir() {
            EntryKind::Directory
        } else if meta.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };

        let size = if meta.is_dir() {
            None
        } else {
            Some(meta.len())
        };

        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase());

        Self {
            name,
            kind,
            size,
            modified: meta.modified().ok(),
            is_hidden: is_hidden(&path, meta),
            is_readonly: meta.permissions().readonly(),
            extension,
            path,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    /// Human-readable size, or an em dash for not-yet-known directory sizes.
    pub fn size_display(&self) -> String {
        match self.size {
            Some(bytes) => humansize::format_size(bytes, humansize::DECIMAL),
            None => "—".to_string(),
        }
    }

    /// Modified time formatted for display.
    pub fn modified_display(&self) -> String {
        match self.modified {
            Some(t) => {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%Y-%m-%d %H:%M").to_string()
            }
            None => "—".to_string(),
        }
    }
}

/// Detect hidden files.
///
/// On Windows this reads the real `FILE_ATTRIBUTE_HIDDEN` bit. Elsewhere it
/// falls back to the Unix dotfile convention, which keeps the type usable in
/// tests on a Linux build host.
#[cfg(windows)]
fn is_hidden(_path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn is_hidden(path: &Path, _meta: &std::fs::Metadata) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

/// How to order a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

/// Sort entries in place.
///
/// Directories always group before files regardless of the sort key — matching
/// the convention users expect from every file manager. Only *within* a group
/// does the key apply.
pub fn sort_entries(entries: &mut [Entry], key: SortKey, order: SortOrder) {
    entries.sort_by(|a, b| {
        let grouping = a.kind.cmp(&b.kind);
        if grouping != std::cmp::Ordering::Equal {
            return grouping;
        }

        let cmp = match key {
            // Case-insensitive so "Apple" and "apple" sort together rather
            // than splitting on ASCII case.
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Kind => a.extension.cmp(&b.extension),
        };

        match order {
            SortOrder::Ascending => cmp,
            SortOrder::Descending => cmp.reverse(),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.to_string(),
            kind,
            size: Some(size),
            modified: None,
            is_hidden: false,
            is_readonly: false,
            extension: None,
        }
    }

    #[test]
    fn directories_sort_before_files() {
        let mut v = vec![
            entry("zzz_file.txt", EntryKind::File, 10),
            entry("aaa_dir", EntryKind::Directory, 0),
        ];

        sort_entries(&mut v, SortKey::Name, SortOrder::Ascending);

        assert_eq!(v[0].name, "aaa_dir");
        assert_eq!(v[1].name, "zzz_file.txt");
    }

    #[test]
    fn directories_stay_first_when_descending() {
        let mut v = vec![
            entry("b_file", EntryKind::File, 10),
            entry("a_dir", EntryKind::Directory, 0),
        ];

        sort_entries(&mut v, SortKey::Name, SortOrder::Descending);

        // Grouping must survive reversal — this is the bug most naive
        // implementations ship.
        assert_eq!(v[0].kind, EntryKind::Directory);
    }

    #[test]
    fn name_sort_is_case_insensitive() {
        let mut v = vec![
            entry("banana", EntryKind::File, 0),
            entry("Apple", EntryKind::File, 0),
        ];

        sort_entries(&mut v, SortKey::Name, SortOrder::Ascending);

        assert_eq!(v[0].name, "Apple");
    }

    #[test]
    fn size_sort_orders_numerically() {
        let mut v = vec![
            entry("big", EntryKind::File, 1000),
            entry("small", EntryKind::File, 10),
        ];

        sort_entries(&mut v, SortKey::Size, SortOrder::Ascending);

        assert_eq!(v[0].name, "small");
    }
}

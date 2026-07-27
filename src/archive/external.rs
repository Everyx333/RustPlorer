//! Detecting an installed 7-Zip or WinRAR.
//!
//! The built-in Rust implementations work with no external dependency, but a
//! locally installed 7-Zip is faster and has two decades of hardening behind
//! it. When one is present we offer to use it — once, and remember the answer.

use std::path::PathBuf;

/// An archive tool found on the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalTool {
    pub name: &'static str,
    pub path: PathBuf,
}

impl ExternalTool {
    pub fn display(&self) -> String {
        format!("{} ({})", self.name, self.path.display())
    }
}

/// Standard install locations, checked in order of preference.
///
/// 7-Zip first: it handles more formats than WinRAR and is more commonly
/// installed. `PATH` is checked last, since an explicit install is a stronger
/// signal than a shim.
#[cfg(windows)]
const CANDIDATES: &[(&str, &str)] = &[
    ("7-Zip", r"C:\Program Files\7-Zip\7z.exe"),
    ("7-Zip", r"C:\Program Files (x86)\7-Zip\7z.exe"),
    ("NanaZip", r"C:\Program Files\NanaZip\NanaZipC.exe"),
    ("WinRAR", r"C:\Program Files\WinRAR\Rar.exe"),
    ("WinRAR", r"C:\Program Files (x86)\WinRAR\Rar.exe"),
];

#[cfg(not(windows))]
const CANDIDATES: &[(&str, &str)] = &[("7-Zip", "/usr/bin/7z"), ("7-Zip", "/usr/local/bin/7z")];

/// Look for an installed archive tool.
///
/// Cheap: a handful of `metadata` calls. Runs once at startup, and the answer
/// is cached in config.
pub fn detect() -> Option<ExternalTool> {
    for (name, path) in CANDIDATES {
        let p = PathBuf::from(path);
        if p.is_file() {
            tracing::info!(tool = name, ?p, "found external archive tool");
            return Some(ExternalTool { name, path: p });
        }
    }

    // Fall back to whatever is on PATH.
    if let Some(p) = which_on_path("7z") {
        tracing::info!(?p, "found 7z on PATH");
        return Some(ExternalTool {
            name: "7-Zip",
            path: p,
        });
    }

    tracing::debug!("no external archive tool found; using built-in support");
    None
}

/// Minimal `which`, avoiding an extra dependency for one lookup.
fn which_on_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;

    for dir in std::env::split_paths(&path_var) {
        // Try the bare name and, on Windows, the .exe form.
        for candidate in [dir.join(binary), dir.join(format!("{binary}.exe"))] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_does_not_panic() {
        // The result depends on the host; we only require that it is safe to
        // call and returns a sane shape.
        if let Some(tool) = detect() {
            assert!(!tool.name.is_empty());
            assert!(tool.path.is_file());
        }
    }

    #[test]
    fn which_finds_a_known_binary() {
        // `sh` exists on Unix CI; on Windows this simply returns None, which
        // is also a valid outcome for this test.
        let found = which_on_path("sh");
        if cfg!(unix) {
            assert!(found.is_some(), "sh should be on PATH on unix");
        }
    }

    #[test]
    fn which_rejects_nonsense() {
        assert!(which_on_path("definitely-not-a-real-binary-xyz").is_none());
    }
}

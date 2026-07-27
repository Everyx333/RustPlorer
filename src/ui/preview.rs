//! Spacebar quick preview.
//!
//! Select a file, press Space, see it — without launching another application.
//!
//! Loading happens on the worker pool, and every read is **size-capped**.
//! Previewing a 4 GB log by reading it into memory would be a self-inflicted
//! outage; only a leading slice is ever loaded.

use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::core::task::WorkerPool;

/// Largest image to decode. Beyond this, decoding costs more than the preview
/// is worth (a 100MP PNG expands to ~400MB in memory).
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Bytes of a text file to read. Enough to see what a file is; small enough to
/// be instant on a network share.
const MAX_TEXT_BYTES: usize = 256 * 1024;

/// What a preview can hold.
#[derive(Debug, Clone)]
pub enum PreviewContent {
    Text {
        body: String,
        truncated: bool,
    },
    /// Raw encoded bytes, handed to egui's image loader.
    Image {
        bytes: Vec<u8>,
        name: String,
    },
    /// No renderer for this type; show metadata instead.
    Unsupported {
        reason: String,
    },
    Loading,
    Failed(String),
}

/// A loaded preview.
#[derive(Debug, Clone)]
pub struct Preview {
    pub path: PathBuf,
    pub content: PreviewContent,
    pub size: u64,
}

/// Loads previews off the UI thread.
pub struct Previewer {
    current: Option<Preview>,
    tx: Sender<Preview>,
    rx: Receiver<Preview>,
    pub open: bool,
}

impl Default for Previewer {
    fn default() -> Self {
        Self::new()
    }
}

impl Previewer {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            current: None,
            tx,
            rx,
            open: false,
        }
    }

    pub fn current(&self) -> Option<&Preview> {
        self.current.as_ref()
    }

    pub fn close(&mut self) {
        self.open = false;
        // Drop the payload: an image preview can hold tens of MB, and keeping
        // it after the panel closes is a silent leak.
        self.current = None;
    }

    /// Toggle the preview for `path`.
    pub fn toggle(&mut self, pool: &WorkerPool, path: PathBuf) {
        if self.open && self.current.as_ref().is_some_and(|p| p.path == path) {
            self.close();
        } else {
            self.load(pool, path);
        }
    }

    /// Load a preview.
    pub fn load(&mut self, pool: &WorkerPool, path: PathBuf) {
        self.open = true;
        self.current = Some(Preview {
            path: path.clone(),
            content: PreviewContent::Loading,
            size: 0,
        });

        let tx = self.tx.clone();

        pool.submit("preview", move |_token| {
            let preview = build_preview(&path);
            let _ = tx.send(preview);
        });
    }

    /// Drain loaded previews. Non-blocking.
    pub fn poll(&mut self) {
        for preview in self.rx.try_iter() {
            // Discard results for a file the user has already moved past.
            let still_wanted = self
                .current
                .as_ref()
                .is_some_and(|c| c.path == preview.path);
            if still_wanted {
                self.current = Some(preview);
            }
        }
    }
}

/// Read and classify a file. Runs on a worker.
fn build_preview(path: &Path) -> Preview {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let content = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" => load_image(path, size),
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" | "xml" | "html" | "css"
        | "js" | "ts" | "py" | "c" | "h" | "cpp" | "hpp" | "java" | "go" | "sh" | "bat"
        | "ps1" | "ini" | "cfg" | "conf" | "log" | "csv" | "sql" => load_text(path),
        _ => PreviewContent::Unsupported {
            reason: format!("No preview available for .{ext} files"),
        },
    };

    Preview {
        path: path.to_path_buf(),
        content,
        size,
    }
}

fn load_image(path: &Path, size: u64) -> PreviewContent {
    if size > MAX_IMAGE_BYTES {
        return PreviewContent::Unsupported {
            reason: format!(
                "Image is too large to preview ({})",
                humansize::format_size(size, humansize::DECIMAL)
            ),
        };
    }

    match std::fs::read(path) {
        Ok(bytes) => PreviewContent::Image {
            bytes,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        },
        Err(e) => PreviewContent::Failed(format!("Could not read image: {e}")),
    }
}

fn load_text(path: &Path) -> PreviewContent {
    use std::io::Read;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return PreviewContent::Failed(format!("Could not open file: {e}")),
    };

    // Read a bounded prefix rather than the whole file.
    let mut buf = Vec::with_capacity(MAX_TEXT_BYTES.min(64 * 1024));
    let mut handle = file.take(MAX_TEXT_BYTES as u64);

    if let Err(e) = handle.read_to_end(&mut buf) {
        return PreviewContent::Failed(format!("Could not read file: {e}"));
    }

    let truncated = buf.len() >= MAX_TEXT_BYTES;

    // Binary files with a text-like extension exist; show a clear message
    // rather than a screen of replacement characters.
    if buf.contains(&0) {
        return PreviewContent::Unsupported {
            reason: "This looks like a binary file".to_string(),
        };
    }

    PreviewContent::Text {
        body: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
    }
}

/// Draw the preview panel.
pub fn draw(app: &mut crate::ui::app::RustPlorer, ctx: &egui::Context) {
    if !app.previewer.open {
        return;
    }

    let mut close = false;

    egui::SidePanel::right("preview")
        .resizable(true)
        .default_width(380.0)
        .width_range(240.0..=800.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Preview").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✖").on_hover_text("Close (Space)").clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();

            let Some(preview) = app.previewer.current() else {
                ui.weak("Nothing selected.");
                return;
            };

            ui.label(
                egui::RichText::new(
                    preview
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
                .strong(),
            );
            ui.label(
                egui::RichText::new(humansize::format_size(
                    preview.size,
                    humansize::DECIMAL,
                ))
                .weak()
                .small(),
            );
            ui.add_space(6.0);

            egui::ScrollArea::both().show(ui, |ui| match &preview.content {
                PreviewContent::Loading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading…");
                    });
                }
                PreviewContent::Text { body, truncated } => {
                    if *truncated {
                        ui.label(
                            egui::RichText::new("Showing the first 256 KB")
                                .weak()
                                .small()
                                .italics(),
                        );
                        ui.add_space(4.0);
                    }
                    ui.add(
                        egui::Label::new(egui::RichText::new(body).monospace().small())
                            .wrap(),
                    );
                }
                PreviewContent::Image { bytes, name } => {
                    // egui caches by URI, so the file name keys the texture.
                    let uri = format!("bytes://{name}");
                    ui.add(
                        egui::Image::from_bytes(uri, bytes.clone())
                            .max_width(ui.available_width())
                            .maintain_aspect_ratio(true),
                    );
                }
                PreviewContent::Unsupported { reason } => {
                    ui.add_space(12.0);
                    ui.weak(reason);
                }
                PreviewContent::Failed(err) => {
                    ui.add_space(12.0);
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
                }
            });
        });

    if close {
        app.previewer.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("rustplorer_preview_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn reads_a_text_file() {
        let p = temp_file("hello.txt", b"hello world");
        let preview = build_preview(&p);

        match preview.content {
            PreviewContent::Text { body, truncated } => {
                assert_eq!(body, "hello world");
                assert!(!truncated);
            }
            other => panic!("expected text, got {other:?}"),
        }

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn truncates_large_text() {
        let big = vec![b'x'; MAX_TEXT_BYTES + 5000];
        let p = temp_file("big.log", &big);
        let preview = build_preview(&p);

        match preview.content {
            PreviewContent::Text { body, truncated } => {
                assert!(truncated, "oversized file should report truncation");
                assert_eq!(body.len(), MAX_TEXT_BYTES);
            }
            other => panic!("expected text, got {other:?}"),
        }

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn detects_binary_masquerading_as_text() {
        // A .txt containing NUL bytes should not render as garbage.
        let p = temp_file("fake.txt", &[0x00, 0x01, 0x02, 0x00]);
        let preview = build_preview(&p);

        assert!(
            matches!(preview.content, PreviewContent::Unsupported { .. }),
            "binary content should be reported, not rendered"
        );

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unknown_extension_is_unsupported() {
        let p = temp_file("thing.xyz", b"data");
        let preview = build_preview(&p);
        assert!(matches!(preview.content, PreviewContent::Unsupported { .. }));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_file_fails_gracefully() {
        let preview = build_preview(Path::new("/definitely/not/here.txt"));
        assert!(matches!(preview.content, PreviewContent::Failed(_)));
    }

    #[test]
    fn closing_releases_content() {
        let mut p = Previewer::new();
        p.open = true;
        p.current = Some(Preview {
            path: PathBuf::from("x"),
            content: PreviewContent::Text {
                body: "y".into(),
                truncated: false,
            },
            size: 1,
        });

        p.close();

        assert!(!p.open);
        assert!(p.current.is_none(), "content must be dropped on close");
    }
}

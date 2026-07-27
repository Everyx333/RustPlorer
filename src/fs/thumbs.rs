//! Thumbnail generation with a hard memory ceiling.
//!
//! # The failure mode this is designed around
//!
//! The naive version — decode every image in the folder, keep the textures —
//! is how file managers end up using gigabytes. A folder of 500 photos at 24MP
//! is ~36 GB of decoded RGBA if you hold the full images.
//!
//! Four rules keep it bounded:
//!
//! 1. **Only visible rows** (plus a small lookahead) are ever queued.
//! 2. **Downscale, then drop the full decode.** A 6000×4000 photo becomes a
//!    128px thumbnail and the original buffer is freed immediately. Never hold
//!    a full-resolution decode to display a tiny cell.
//! 3. **LRU eviction on estimated bytes**, not entry count. 500 tiny icons and
//!    500 large previews cost very different amounts; counting entries would
//!    let the real figure drift by an order of magnitude.
//! 4. **Skip oversized files** unless asked. A 200 MB TIFF would occupy a
//!    worker for seconds to produce one small cell.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;

use crate::core::task::WorkerPool;

/// Edge length of a generated thumbnail, in pixels.
pub const THUMB_SIZE: u32 = 128;

/// Files larger than this are skipped. Decoding costs more than the thumbnail
/// is worth.
const MAX_SOURCE_BYTES: u64 = 24 * 1024 * 1024;

/// Concurrent decodes. Image decoding is CPU-bound, unlike directory scanning,
/// so this is a separate limit from the scan concurrency.
const MAX_CONCURRENT_DECODES: usize = 3;

/// Bytes one thumbnail occupies once uploaded: RGBA at THUMB_SIZE².
const BYTES_PER_THUMB: usize = (THUMB_SIZE * THUMB_SIZE * 4) as usize;

/// A decoded, downscaled thumbnail ready for upload.
#[derive(Debug, Clone)]
pub struct Thumbnail {
    pub path: PathBuf,
    /// RGBA8 pixels.
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Thumbnail {
    /// Approximate memory cost.
    pub fn byte_size(&self) -> usize {
        self.pixels.len()
    }
}

/// State of a thumbnail request.
#[derive(Debug, Clone)]
pub enum ThumbState {
    Pending,
    Ready(Arc<Thumbnail>),
    /// Not an image, too large, or failed to decode. Cached so we don't retry
    /// a broken file on every frame.
    Unavailable,
}

/// LRU cache bounded by total estimated bytes.
struct LruCache {
    entries: HashMap<PathBuf, (ThumbState, u64)>,
    /// Monotonic tick used as the recency stamp.
    clock: u64,
    bytes: usize,
    max_bytes: usize,
}

impl LruCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            bytes: 0,
            max_bytes,
        }
    }

    fn get(&mut self, path: &Path) -> Option<ThumbState> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(path).map(|(state, seen)| {
            *seen = clock;
            state.clone()
        })
    }

    fn insert(&mut self, path: PathBuf, state: ThumbState) {
        self.clock += 1;

        let cost = match &state {
            ThumbState::Ready(t) => t.byte_size(),
            _ => 0,
        };

        if let Some((old, _)) = self.entries.insert(path, (state, self.clock)) {
            if let ThumbState::Ready(t) = old {
                self.bytes = self.bytes.saturating_sub(t.byte_size());
            }
        }
        self.bytes += cost;

        self.evict_if_needed();
    }

    /// Drop least-recently-used entries until under budget.
    fn evict_if_needed(&mut self) {
        while self.bytes > self.max_bytes {
            // Find the oldest entry that actually holds pixels. Pending and
            // Unavailable markers cost nothing and are worth keeping — the
            // latter prevents re-decoding a known-bad file.
            let victim = self
                .entries
                .iter()
                .filter(|(_, (s, _))| matches!(s, ThumbState::Ready(_)))
                .min_by_key(|(_, (_, seen))| *seen)
                .map(|(p, _)| p.clone());

            let Some(path) = victim else {
                break;
            };

            if let Some((ThumbState::Ready(t), _)) = self.entries.remove(&path) {
                self.bytes = self.bytes.saturating_sub(t.byte_size());
                tracing::trace!(?path, "evicted thumbnail");
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn set_budget(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
        self.evict_if_needed();
    }
}

/// Generates and caches thumbnails.
pub struct ThumbnailCache {
    cache: Arc<Mutex<LruCache>>,
    active: Arc<AtomicUsize>,
    tx: Sender<(PathBuf, ThumbState)>,
    rx: Receiver<(PathBuf, ThumbState)>,
}

impl ThumbnailCache {
    /// Create a cache with a byte budget.
    pub fn new(max_bytes: usize) -> Self {
        let (tx, rx) = unbounded();
        Self {
            cache: Arc::new(Mutex::new(LruCache::new(max_bytes))),
            active: Arc::new(AtomicUsize::new(0)),
            tx,
            rx,
        }
    }

    /// Change the memory budget, evicting immediately if it shrank.
    pub fn set_budget(&self, max_bytes: usize) {
        self.cache.lock().set_budget(max_bytes);
    }

    /// Current estimated memory use.
    pub fn bytes_used(&self) -> usize {
        self.cache.lock().bytes
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.cache.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.cache.lock().clear();
    }

    /// Look up a thumbnail, queueing generation if it isn't cached.
    ///
    /// Safe to call every frame for every visible row.
    pub fn get_or_request(&self, pool: &WorkerPool, path: &Path) -> ThumbState {
        if let Some(state) = self.cache.lock().get(path) {
            return state;
        }

        if !is_image(path) {
            self.cache
                .lock()
                .insert(path.to_path_buf(), ThumbState::Unavailable);
            return ThumbState::Unavailable;
        }

        // Respect the decode concurrency cap. A row that misses out is simply
        // retried next frame.
        if self.active.load(Ordering::Acquire) >= MAX_CONCURRENT_DECODES {
            return ThumbState::Pending;
        }

        self.active.fetch_add(1, Ordering::AcqRel);
        self.cache
            .lock()
            .insert(path.to_path_buf(), ThumbState::Pending);

        let tx = self.tx.clone();
        let active = Arc::clone(&self.active);
        let job_path = path.to_path_buf();

        let submitted = pool.submit("thumbnail", move |token| {
            if token.is_cancelled() {
                active.fetch_sub(1, Ordering::AcqRel);
                return;
            }

            let state = match generate(&job_path) {
                Some(t) => ThumbState::Ready(Arc::new(t)),
                None => ThumbState::Unavailable,
            };

            let _ = tx.send((job_path, state));
            active.fetch_sub(1, Ordering::AcqRel);
        });

        if submitted.is_none() {
            // Queue full — release the slot and the placeholder so the row is
            // retried rather than stuck on "pending" forever.
            self.active.fetch_sub(1, Ordering::AcqRel);
            self.cache.lock().entries.remove(path);
        }

        ThumbState::Pending
    }

    /// Fold completed thumbnails into the cache. Non-blocking.
    pub fn poll(&self) -> usize {
        let mut count = 0;
        for (path, state) in self.rx.try_iter() {
            self.cache.lock().insert(path, state);
            count += 1;
        }
        count
    }
}

/// Extensions worth attempting.
fn is_image(path: &Path) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };
    matches!(
        ext.to_string_lossy().to_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico"
    )
}

/// Decode, downscale, and drop the full-resolution buffer.
///
/// The order matters: the full decode exists only inside this function, and is
/// freed when it returns. Holding it would defeat the entire budget.
fn generate(path: &Path) -> Option<Thumbnail> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_SOURCE_BYTES {
        tracing::trace!(?path, size = meta.len(), "skipping oversized image");
        return None;
    }

    let started = std::time::Instant::now();

    // Decode failures are common (truncated downloads, misnamed files) and
    // must not be treated as errors worth surfacing.
    let img = image::ImageReader::open(path).ok()?.decode().ok()?;

    // `thumbnail` uses a fast nearest/triangle path — appropriate here, since
    // the result is 128px and quality differences are invisible.
    let small = img.thumbnail(THUMB_SIZE, THUMB_SIZE);
    let rgba = small.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());

    tracing::trace!(
        ?path,
        elapsed_ms = started.elapsed().as_millis(),
        "thumbnail generated"
    );

    Some(Thumbnail {
        path: path.to_path_buf(),
        pixels: rgba.into_raw(),
        width,
        height,
    })
    // `img` and `small` drop here — the full-resolution decode is released.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_thumb(bytes: usize) -> Arc<Thumbnail> {
        Arc::new(Thumbnail {
            path: PathBuf::from("x"),
            pixels: vec![0u8; bytes],
            width: 1,
            height: 1,
        })
    }

    #[test]
    fn evicts_when_over_budget() {
        let mut cache = LruCache::new(1000);

        cache.insert(PathBuf::from("a"), ThumbState::Ready(fake_thumb(400)));
        cache.insert(PathBuf::from("b"), ThumbState::Ready(fake_thumb(400)));
        assert_eq!(cache.bytes, 800);

        // Pushes past the budget; the oldest must go.
        cache.insert(PathBuf::from("c"), ThumbState::Ready(fake_thumb(400)));

        assert!(cache.bytes <= 1000, "cache exceeded its byte budget");
        assert!(
            !cache.entries.contains_key(Path::new("a")),
            "least-recently-used entry should have been evicted"
        );
    }

    #[test]
    fn recent_access_protects_from_eviction() {
        let mut cache = LruCache::new(1000);

        cache.insert(PathBuf::from("a"), ThumbState::Ready(fake_thumb(400)));
        cache.insert(PathBuf::from("b"), ThumbState::Ready(fake_thumb(400)));

        // Touch "a" so "b" becomes the oldest.
        let _ = cache.get(Path::new("a"));

        cache.insert(PathBuf::from("c"), ThumbState::Ready(fake_thumb(400)));

        assert!(
            cache.entries.contains_key(Path::new("a")),
            "recently used entry should survive"
        );
        assert!(!cache.entries.contains_key(Path::new("b")));
    }

    #[test]
    fn budget_is_enforced_in_bytes_not_entries() {
        // Many small entries fit; a few large ones do not. Counting entries
        // instead of bytes would let real memory use drift by an order of
        // magnitude, which is the whole reason the budget is in bytes.
        let mut small = LruCache::new(1000);
        for i in 0..10 {
            small.insert(PathBuf::from(format!("s{i}")), ThumbState::Ready(fake_thumb(50)));
        }
        assert_eq!(small.entries.len(), 10, "ten small entries should all fit");
        assert_eq!(small.bytes, 500);

        let mut large = LruCache::new(1000);
        for i in 0..10 {
            large.insert(PathBuf::from(format!("l{i}")), ThumbState::Ready(fake_thumb(400)));
        }
        assert!(
            large.entries.len() < 10,
            "large entries must be evicted despite the same entry count"
        );
        assert!(large.bytes <= 1000, "byte budget must hold");
    }

    #[test]
    fn single_oversized_entry_is_evicted_not_retained() {
        // An item bigger than the whole budget is dropped rather than kept.
        // Retaining it would mean one pathological image permanently blows
        // the ceiling the user configured.
        let mut cache = LruCache::new(1000);
        cache.insert(PathBuf::from("huge"), ThumbState::Ready(fake_thumb(5000)));

        assert_eq!(cache.bytes, 0, "oversized entry must not stay resident");
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn shrinking_budget_evicts_immediately() {
        let mut cache = LruCache::new(10_000);
        for i in 0..5 {
            cache.insert(PathBuf::from(format!("f{i}")), ThumbState::Ready(fake_thumb(1000)));
        }
        assert_eq!(cache.bytes, 5000);

        cache.set_budget(2000);
        assert!(cache.bytes <= 2000, "shrinking the budget must evict");
    }

    #[test]
    fn unavailable_entries_cost_nothing_and_persist() {
        let mut cache = LruCache::new(100);

        cache.insert(PathBuf::from("not_an_image"), ThumbState::Unavailable);
        cache.insert(PathBuf::from("big"), ThumbState::Ready(fake_thumb(500)));

        // The marker must survive eviction: it is what stops us re-decoding a
        // known-bad file every frame.
        assert!(cache.entries.contains_key(Path::new("not_an_image")));
    }

    #[test]
    fn non_images_are_marked_unavailable() {
        assert!(!is_image(Path::new("notes.txt")));
        assert!(!is_image(Path::new("no_extension")));
        assert!(is_image(Path::new("photo.JPG")), "must be case-insensitive");
        assert!(is_image(Path::new("icon.png")));
    }

    #[test]
    fn thumbnail_byte_estimate_matches_pixels() {
        let t = fake_thumb(4096);
        assert_eq!(t.byte_size(), 4096);
    }

    #[test]
    fn generates_a_real_thumbnail() {
        let dir = std::env::temp_dir().join("rustplorer_thumb_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("test.png");

        // A 512x512 source must come back downscaled, not full size.
        let img = image::RgbaImage::from_pixel(512, 512, image::Rgba([200, 100, 50, 255]));
        img.save(&p).unwrap();

        let thumb = generate(&p).expect("thumbnail should be generated");

        assert!(thumb.width <= THUMB_SIZE && thumb.height <= THUMB_SIZE);
        assert_eq!(thumb.pixels.len(), (thumb.width * thumb.height * 4) as usize);
        assert!(
            thumb.byte_size() <= BYTES_PER_THUMB,
            "a thumbnail must not exceed the per-thumb budget"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_image_fails_without_panicking() {
        let dir = std::env::temp_dir().join("rustplorer_thumb_bad");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("broken.png");
        std::fs::write(&p, b"this is not a png").unwrap();

        assert!(generate(&p).is_none(), "corrupt input should return None");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

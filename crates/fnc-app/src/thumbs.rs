//! Thumbnails and file icons (SPEC ch.3 §4.1): rendered on worker threads (Windows
//! Shell, or the `image` crate for pictures elsewhere), uploaded as textures on the UI
//! thread, kept in an in-memory LRU and a disk cache (`thumbs/`, size-limited).
//! Requests are served newest first so the visible rows win while scrolling.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use fnc_platform::thumb::{Rgba, ShellImage};
use parking_lot::{Condvar, Mutex};

/// Identifies one image.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    /// File (for type icons: a representative file of the type).
    pub path: PathBuf,
    /// Edge length in pixels.
    pub size: u32,
    /// Icon or thumbnail.
    pub icon: bool,
    /// Changes when the file changes (date modified, size).
    pub stamp: u64,
}

impl ThumbKey {
    fn disk_name(&self) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        format!("{:016x}.png", h.finish())
    }
}

/// Picture extensions decoded without the Shell.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "jpe", "gif", "bmp", "webp", "ico", "tif", "tiff"];

/// `true` if `path` is a picture the built-in decoder handles.
pub fn is_picture(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Decodes `path` and scales it to fit `size` (premultiplied RGBA).
pub fn decode_picture(path: &Path, size: u32) -> Option<Rgba> {
    // Refuse absurd files (decoding bombs) before reading them.
    let md = std::fs::metadata(path).ok()?;
    if md.len() > 256 << 20 {
        return None;
    }
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 << 20);
    let mut reader = reader;
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = if img.width() > size || img.height() > size { img.thumbnail(size, size) } else { img };
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut pixels = rgba.into_raw();
    for p in pixels.as_chunks_mut::<4>().0 {
        let a = u16::from(p[3]);
        for c in &mut p[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
        }
    }
    Some(Rgba { width, height, pixels })
}

fn render(key: &ThumbKey, source: &Path) -> Option<Rgba> {
    if key.icon {
        return fnc_platform::thumb::shell_image(source, key.size, ShellImage::Icon).ok();
    }
    if is_picture(source)
        && let Some(r) = decode_picture(source, key.size)
    {
        return Some(r);
    }
    fnc_platform::thumb::shell_image(source, key.size, ShellImage::Thumbnail).ok()
}

fn load_disk(dir: &Path, key: &ThumbKey) -> Option<Rgba> {
    let img = image::open(dir.join(key.disk_name())).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(Rgba { width, height, pixels: img.into_raw() })
}

fn save_disk(dir: &Path, key: &ThumbKey, r: &Rgba) {
    let _ = std::fs::create_dir_all(dir);
    let _ = image::save_buffer(dir.join(key.disk_name()), &r.pixels, r.width, r.height, image::ColorType::Rgba8);
}

/// Deletes the least recently written files until the folder is under `limit` bytes.
pub fn trim_disk_cache(dir: &Path, limit: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            Some((md.modified().ok()?, md.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, p) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

struct Queue {
    jobs: Mutex<VecDeque<(ThumbKey, PathBuf)>>,
    ready: Condvar,
}

/// Texture cache with background rendering.
pub struct Thumbs {
    queue: Arc<Queue>,
    rx: Receiver<(ThumbKey, Option<Rgba>)>,
    textures: HashMap<ThumbKey, (egui::TextureHandle, u64)>,
    pending: HashSet<ThumbKey>,
    failed: HashSet<ThumbKey>,
    tick: u64,
    max_textures: usize,
}

const MAX_QUEUE: usize = 1024;

impl Thumbs {
    /// Starts `threads` workers; `disk` is the cache folder (`None` = memory only).
    pub fn new(threads: usize, disk: Option<PathBuf>) -> Self {
        let queue = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), ready: Condvar::new() });
        let (tx, rx): (Sender<(ThumbKey, Option<Rgba>)>, _) = crossbeam_channel::unbounded();
        for i in 0..threads.max(1) {
            let (q, tx, disk) = (queue.clone(), tx.clone(), disk.clone());
            let r = std::thread::Builder::new().name(format!("fennec-thumb-{i}")).spawn(move || {
                loop {
                    let (key, source) = {
                        let mut jobs = q.jobs.lock();
                        loop {
                            if let Some(k) = jobs.pop_back() {
                                break k;
                            }
                            q.ready.wait(&mut jobs);
                        }
                    };
                    let cached = disk.as_deref().filter(|_| !key.icon).and_then(|d| load_disk(d, &key));
                    let img = cached.or_else(|| {
                        // Decoders and shell extensions are third-party code: contain panics.
                        let r = std::panic::catch_unwind(|| render(&key, &source)).ok().flatten();
                        if let (Some(d), Some(img)) = (disk.as_deref(), r.as_ref())
                            && !key.icon
                        {
                            save_disk(d, &key, img);
                        }
                        r
                    });
                    if tx.send((key, img)).is_err() {
                        return;
                    }
                }
            });
            if let Err(e) = r {
                tracing::warn!(error = %e, "cannot start thumbnail worker");
            }
        }
        Self {
            queue,
            rx,
            textures: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            tick: 0,
            max_textures: 1500,
        }
    }

    /// Uploads finished images; call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut any = false;
        while let Ok((key, img)) = self.rx.try_recv() {
            self.pending.remove(&key);
            match img {
                Some(r) => {
                    let ci =
                        egui::ColorImage::from_rgba_premultiplied([r.width as usize, r.height as usize], &r.pixels);
                    let tex = ctx.load_texture(format!("thumb-{}", key.disk_name()), ci, egui::TextureOptions::LINEAR);
                    self.textures.insert(key, (tex, self.tick));
                    any = true;
                }
                None => {
                    self.failed.insert(key);
                }
            }
        }
        if self.textures.len() > self.max_textures {
            // Drop the least recently used quarter.
            let mut ticks: Vec<u64> = self.textures.values().map(|v| v.1).collect();
            ticks.sort_unstable();
            let cut = ticks[ticks.len() / 4];
            self.textures.retain(|_, v| v.1 > cut);
        }
        if any {
            ctx.request_repaint();
        }
        if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// The texture for `key` (rendered from the file `source`), or `None` while it is
    /// being rendered or if it is unavailable.
    pub fn get(&mut self, key: ThumbKey, source: &Path) -> Option<egui::TextureHandle> {
        self.tick += 1;
        if let Some(v) = self.textures.get_mut(&key) {
            v.1 = self.tick;
            return Some(v.0.clone());
        }
        if self.failed.contains(&key) || self.pending.contains(&key) {
            return None;
        }
        self.pending.insert(key.clone());
        let mut jobs = self.queue.jobs.lock();
        jobs.push_back((key, source.to_path_buf()));
        if jobs.len() > MAX_QUEUE {
            // Oldest requests (scrolled away) are dropped; they are asked for again if
            // they become visible.
            if let Some((old, _)) = jobs.pop_front() {
                self.pending.remove(&old);
            }
        }
        self.queue.ready.notify_one();
        None
    }
}

/// Icon key: shared per extension for ordinary files, per file for types with their
/// own icons, one for all folders.
pub fn icon_key(path: &Path, is_dir: bool, size: u32) -> ThumbKey {
    let own_icon =
        |e: &str| ["exe", "lnk", "ico", "url", "cur", "ani", "scr", "msc"].iter().any(|x| x.eq_ignore_ascii_case(e));
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let id = if is_dir {
        PathBuf::from("<folder>")
    } else if own_icon(ext) {
        path.to_path_buf()
    } else {
        PathBuf::from(format!("*.{}", ext.to_ascii_lowercase()))
    };
    ThumbKey { path: id, size, icon: true, stamp: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_scales_pictures() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.png");
        let img = image::RgbaImage::from_fn(400, 200, |x, _| image::Rgba([(x % 256) as u8, 0, 0, 128]));
        img.save(&p).unwrap();
        assert!(is_picture(&p));
        let r = decode_picture(&p, 100).unwrap();
        assert_eq!((r.width, r.height), (100, 50));
        assert_eq!(r.pixels.len(), 100 * 50 * 4);
        assert!(r.pixels[3] == 128 && r.pixels[0] <= 128, "premultiplied");
        assert!(decode_picture(&d.path().join("missing.png"), 100).is_none());
        std::fs::write(d.path().join("bad.jpg"), b"not a jpeg").unwrap();
        assert!(decode_picture(&d.path().join("bad.jpg"), 100).is_none());
    }

    #[test]
    fn disk_cache_roundtrip_and_trim() {
        let d = tempfile::tempdir().unwrap();
        let key = ThumbKey { path: "x.png".into(), size: 96, icon: false, stamp: 7 };
        let r = Rgba { width: 2, height: 1, pixels: vec![1, 2, 3, 255, 4, 5, 6, 255] };
        save_disk(d.path(), &key, &r);
        assert_eq!(load_disk(d.path(), &key), Some(r));
        trim_disk_cache(d.path(), 0);
        assert!(load_disk(d.path(), &key).is_none());
    }

    #[test]
    fn type_icons_are_shared() {
        let a = icon_key(Path::new(r"C:\a\x.TXT"), false, 16);
        let b = icon_key(Path::new(r"D:\y.txt"), false, 16);
        assert_eq!(a, b);
        let e1 = icon_key(Path::new(r"C:\a.exe"), false, 16);
        let e2 = icon_key(Path::new(r"C:\b.exe"), false, 16);
        assert_ne!(e1, e2, "executables have their own icons");
    }
}

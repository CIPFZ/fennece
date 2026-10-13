//! Property extraction (SPEC ch.1 §8.2): pure-Rust parsers for images/EXIF, audio,
//! video, PE executables, Office Open XML and PDF, plus an injectable
//! [`PropertyProvider`] fallback (the Windows Property System on Windows, supplied by the
//! app because this crate cannot use COM).
//!
//! Every parser reads through [`Bounded`] (byte and wall-clock budget) and runs inside
//! `catch_unwind`: malformed files yield fewer properties, never a crash.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{PropertyIndexConfig, sources};
use crate::schema::{Extractor, PropertyId};
use crate::value::Value;

pub mod audio;
pub mod image;
pub mod ooxml;
pub mod pdf;
pub mod pe;
pub mod video;
pub(crate) mod xml;

/// Extracted `(property, value)` pairs.
pub type Props = Vec<(PropertyId, Value)>;

/// Fallback source of properties (the app injects the Windows Property System:
/// `SHGetPropertyStoreFromParsingName` + `PSGetPropertyKeyFromName(def.system)`).
///
/// Called on extraction threads, never on the UI or search thread; must not panic and
/// should return quickly for unsupported files.
pub trait PropertyProvider: Send + Sync {
    /// Values of `ids` for the file at `path` (missing ones omitted).
    fn get(&self, path: &Path, ids: &[PropertyId]) -> Vec<(PropertyId, Value)>;
}

/// Per-file extraction budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractLimits {
    /// Maximum bytes a parser may read from one file (default 64 MiB).
    pub max_read_bytes: u64,
    /// Wall-clock budget per file (default 5 s).
    pub time_budget: Duration,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self { max_read_bytes: 64 << 20, time_budget: Duration::from_secs(5) }
    }
}

/// Reader with a byte and time budget; seeking is free (only bytes read count).
pub struct Bounded<R> {
    inner: R,
    read: u64,
    limit: u64,
    deadline: Instant,
}

impl<R> Bounded<R> {
    /// Wraps `inner` with `limits`.
    pub fn new(inner: R, limits: ExtractLimits) -> Self {
        Self { inner, read: 0, limit: limits.max_read_bytes, deadline: Instant::now() + limits.time_budget }
    }

    /// `true` once the time budget is spent.
    pub fn expired(&self) -> bool {
        Instant::now() > self.deadline
    }
}

impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read >= self.limit {
            return Err(io::Error::other("read budget exhausted"));
        }
        if self.expired() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "time budget exhausted"));
        }
        let max = (self.limit - self.read).min(buf.len() as u64) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        self.read += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for Bounded<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Opens `path` for bounded, buffered reading.
pub fn open(path: &Path, limits: ExtractLimits) -> io::Result<BufReader<Bounded<File>>> {
    Ok(BufReader::with_capacity(64 * 1024, Bounded::new(File::open(path)?, limits)))
}

/// Reads at most `limits.max_read_bytes` of `path` into memory (`None` if larger).
pub fn read_all(path: &Path, limits: ExtractLimits) -> io::Result<Option<Vec<u8>>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if len > limits.max_read_bytes {
        return Ok(None);
    }
    let mut buf = Vec::with_capacity(len as usize);
    f.read_to_end(&mut buf)?;
    Ok(Some(buf))
}

/// Runs `f`, turning a panic into `None` (logged).
pub fn contained<T>(what: &str, path: &Path, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::warn!(parser = what, path = %path.display(), "property parser panicked");
            None
        }
    }
}

/// File families recognised by extension.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Family {
    /// Raster images.
    Image,
    /// Audio.
    Audio,
    /// MP4/MOV/3GP.
    Mp4,
    /// Matroska/WebM.
    Matroska,
    /// Other videos (Windows fallback only).
    OtherVideo,
    /// PE executables and libraries.
    Executable,
    /// Office Open XML.
    Ooxml,
    /// PDF.
    Pdf,
    /// Anything else.
    Other,
}

/// Family of a (lowercase or not) extension without the dot.
pub fn family(ext: &str) -> Family {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "bmp" | "dib" | "tif" | "tiff" | "webp" | "ico" | "tga"
        | "heic" | "heif" | "avif" | "dng" | "cr2" | "nef" | "arw" | "orf" | "rw2" => Family::Image,
        "mp3" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "m4b" | "aac" | "wav" | "wave" | "aif" | "aiff" | "aifc"
        | "ape" | "wv" | "mpc" | "spx" => Family::Audio,
        "mp4" | "m4v" | "mov" | "qt" | "3gp" | "3g2" | "f4v" => Family::Mp4,
        "mkv" | "webm" | "mka" | "mk3d" => Family::Matroska,
        "avi" | "wmv" | "asf" | "flv" | "mpg" | "mpeg" | "ts" | "m2ts" | "mts" | "vob" | "ogv" | "rmvb" => {
            Family::OtherVideo
        }
        "exe" | "dll" | "sys" | "ocx" | "cpl" | "scr" | "efi" | "mui" | "drv" | "ax" | "node" => Family::Executable,
        "docx" | "docm" | "dotx" | "dotm" | "xlsx" | "xlsm" | "xltx" | "xltm" | "pptx" | "pptm" | "potx" | "potm"
        | "ppsx" | "ppsm" | "vsdx" => Family::Ooxml,
        "pdf" => Family::Pdf,
        _ => Family::Other,
    }
}

/// Built-in extractors plus the optional fallback provider.
#[derive(Clone, Default)]
pub struct PropertyExtractor {
    /// Budget per file.
    pub limits: ExtractLimits,
    /// Fallback for properties the built-in parsers did not produce.
    pub fallback: Option<Arc<dyn PropertyProvider>>,
}

impl std::fmt::Debug for PropertyExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PropertyExtractor")
            .field("limits", &self.limits)
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl PropertyExtractor {
    /// Extractor with default limits and no fallback.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a fallback provider.
    #[must_use]
    pub fn with_fallback(mut self, p: Arc<dyn PropertyProvider>) -> Self {
        self.fallback = Some(p);
        self
    }

    /// Properties of the file for the stored properties of `config`.
    pub fn extract_config(&self, path: &Path, ext: &str, config: &PropertyIndexConfig) -> Props {
        self.extract(path, ext, &config.stored())
    }

    /// Extracts `wanted` (derived properties are replaced by their sources; an empty list
    /// means everything the parsers produce) from the file at `path` with extension `ext`.
    pub fn extract(&self, path: &Path, ext: &str, wanted: &[PropertyId]) -> Props {
        let mut want: Vec<PropertyId> = Vec::new();
        for &p in wanted {
            for s in sources(p) {
                if !want.contains(s) {
                    want.push(*s);
                }
            }
        }
        let needs = |e: Extractor| want.is_empty() || want.iter().any(|p| p.extractor() == e);
        let mut out = Props::new();
        let l = self.limits;
        match family(ext) {
            Family::Image if needs(Extractor::Image) || needs(Extractor::Exif) || needs(Extractor::Document) => {
                contained("image", path, || image::extract(path, l, &mut out));
            }
            Family::Audio => {
                contained("audio", path, || audio::extract(path, ext, l, &mut out));
            }
            Family::Mp4 => {
                contained("mp4", path, || video::extract_mp4(path, l, &mut out));
                if needs(Extractor::Audio) || needs(Extractor::Document) {
                    contained("audio", path, || audio::extract(path, ext, l, &mut out));
                }
            }
            Family::Matroska => {
                contained("matroska", path, || video::extract_matroska(path, l, &mut out));
            }
            Family::Executable => {
                contained("pe", path, || pe::extract(path, l, &mut out));
            }
            Family::Ooxml => {
                contained("ooxml", path, || ooxml::extract_props(path, l, &mut out));
            }
            Family::Pdf => {
                contained("pdf", path, || pdf::extract_props(path, l, &mut out));
            }
            _ => {}
        }
        dedup(&mut out);
        if !want.is_empty() {
            out.retain(|(p, _)| want.contains(p));
        }
        if let Some(fb) = &self.fallback {
            let missing: Vec<PropertyId> = if want.is_empty() {
                Vec::new()
            } else {
                want.iter()
                    .copied()
                    .filter(|p| !out.iter().any(|(q, _)| q == p) && p.def().is_some_and(|d| !d.system.is_empty()))
                    .collect()
            };
            if !missing.is_empty()
                && let Some(extra) = contained("fallback", path, || fb.get(path, &missing))
            {
                for (p, v) in extra {
                    if missing.contains(&p)
                        && let Some(v) = v.coerce(p.ty())
                    {
                        out.push((p, v));
                    }
                }
            }
        }
        out
    }
}

/// Keeps the first value of every property and coerces to the property type.
fn dedup(out: &mut Props) {
    let mut seen = Vec::new();
    let mut kept = Props::with_capacity(out.len());
    for (p, v) in out.drain(..) {
        if seen.contains(&p) {
            continue;
        }
        if let Some(v) = v.coerce(p.ty()) {
            seen.push(p);
            kept.push((p, v));
        }
    }
    *out = kept;
}

/// Pushes `Value::text(s)` when non-empty.
pub(crate) fn push_text(out: &mut Props, p: PropertyId, s: &str) {
    if let Some(v) = Value::text(s) {
        out.push((p, v));
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    /// Writes `bytes` to a temp file named `name`; returns the dir guard and path.
    pub fn file(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        (dir, p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl PropertyProvider for Fake {
        fn get(&self, _: &Path, ids: &[PropertyId]) -> Vec<(PropertyId, Value)> {
            ids.iter().map(|&p| (p, Value::Text("from-shell".into()))).collect()
        }
    }

    #[test]
    fn families() {
        assert_eq!(family("JPG"), Family::Image);
        assert_eq!(family("flac"), Family::Audio);
        assert_eq!(family("mov"), Family::Mp4);
        assert_eq!(family("webm"), Family::Matroska);
        assert_eq!(family("dll"), Family::Executable);
        assert_eq!(family("xlsx"), Family::Ooxml);
        assert_eq!(family("pdf"), Family::Pdf);
        assert_eq!(family("txt"), Family::Other);
    }

    #[test]
    fn bounded_reader_limits() {
        let data = vec![7u8; 100];
        let mut r = Bounded::new(
            io::Cursor::new(data),
            ExtractLimits { max_read_bytes: 10, time_budget: Duration::from_secs(5) },
        );
        let mut buf = Vec::new();
        assert!(r.read_to_end(&mut buf).is_err());
        assert_eq!(buf.len(), 10);
        let mut r = Bounded::new(
            io::Cursor::new(vec![1u8; 4]),
            ExtractLimits { max_read_bytes: 10, time_budget: Duration::ZERO },
        );
        std::thread::sleep(Duration::from_millis(2));
        assert!(r.read(&mut [0u8; 4]).is_err());
    }

    #[test]
    fn garbage_never_panics_and_fallback_fills_gaps() {
        let ex = PropertyExtractor::new().with_fallback(Arc::new(Fake));
        for ext in ["jpg", "mp3", "mp4", "mkv", "exe", "docx", "pdf", "txt"] {
            let (_d, p) = testutil::file(&format!("x.{ext}"), b"\x00\x01garbage\xff\xfe not a real file");
            let v = ex.extract(&p, ext, &[PropertyId::TITLE, PropertyId::WIDTH]);
            assert!(v.iter().all(|(p, _)| *p == PropertyId::TITLE || *p == PropertyId::WIDTH), "{ext}: {v:?}");
            assert!(v.iter().any(|(p, v)| *p == PropertyId::TITLE && v.as_str() == Some("from-shell")), "{ext}");
        }
        let missing = Path::new("/definitely/not/here.jpg");
        assert!(PropertyExtractor::new().extract(missing, "jpg", &[]).is_empty());
        assert!(contained("t", missing, || panic!("boom")).is_none());
    }
}

//! Slow-path file access for searches on entries that are not indexed (SPEC ch.1 §8.4,
//! §9.4; ch.2 §8.6): the search engine calls an [`OnDemand`] source for each candidate.
//! The app installs [`DefaultOnDemand`] (optionally with the Windows Property System
//! fallback); tests can install fakes.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::content::{ContentExtractor, TextEncoding};
use crate::extract::{PropertyExtractor, PropertyProvider};
use crate::link::{LinkKind, MAX_LINK_FILE_BYTES, lnk_target, normalize_reparse_target, parent_dir, url_target};
use crate::schema::PropertyId;
use crate::value::Value;

/// File access used by the slow paths. Implementations must be thread-safe (called from
/// the search thread pool) and must not panic.
pub trait OnDemand: Send + Sync {
    /// Properties `ids` of the file.
    fn properties(&self, path: &Path, ext: &str, ids: &[PropertyId]) -> Vec<(PropertyId, Value)>;
    /// Extracted text (`content:` on files without indexed content).
    fn content(&self, path: &Path, ext: &str) -> Option<String>;
    /// File bytes interpreted as `enc` without document parsing (`textplain:`,
    /// `utf8content:`, `ansicontent:`, `utf16content:`, `utf16becontent:`).
    fn text_as(&self, path: &Path, enc: TextEncoding) -> Option<String>;
    /// Raw bytes (`binary:content:`), at most the configured file size limit.
    fn bytes(&self, path: &Path) -> Option<Vec<u8>>;
    /// At most the first `max` bytes (`fsi:` file signatures, `$I` recycle bin records).
    /// The default truncates [`OnDemand::bytes`]; implementations should read only `max`.
    fn head(&self, path: &Path, max: usize) -> Option<Vec<u8>> {
        self.bytes(path).map(|mut b| {
            b.truncate(max);
            b
        })
    }
    /// At most `len` bytes at `offset` (the ISO 9660 descriptor probe of `fsi:`). Default:
    /// not available.
    fn read_at(&self, _path: &Path, _offset: u64, _len: usize) -> Option<Vec<u8>> {
        None
    }
    /// Target of a link (`target:`): the reparse target of a symbolic link or junction,
    /// the target path of a `.lnk` shell link, or the URL of a `.url` internet shortcut.
    /// Default: not available.
    fn link_target(&self, _path: &Path, _kind: LinkKind) -> Option<String> {
        None
    }
}

/// First `max` bytes of a file.
fn read_head(path: &Path, max: usize) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(max.min(1 << 16));
    f.take(max as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// The built-in extractors.
#[derive(Clone, Debug, Default)]
pub struct DefaultOnDemand {
    /// Property extractor (with optional fallback provider).
    pub properties: PropertyExtractor,
    /// Content extractor (limits, ANSI code page).
    pub content: ContentExtractor,
}

impl DefaultOnDemand {
    /// Built-in extractors with a property fallback provider.
    pub fn with_fallback(p: Arc<dyn PropertyProvider>) -> Self {
        Self { properties: PropertyExtractor::new().with_fallback(p), content: ContentExtractor::default() }
    }
}

impl OnDemand for DefaultOnDemand {
    fn properties(&self, path: &Path, ext: &str, ids: &[PropertyId]) -> Vec<(PropertyId, Value)> {
        self.properties.extract(path, ext, ids)
    }

    fn content(&self, path: &Path, ext: &str) -> Option<String> {
        self.content.extract(path, ext)
    }

    fn text_as(&self, path: &Path, enc: TextEncoding) -> Option<String> {
        self.content.extract_plain(path, enc)
    }

    fn bytes(&self, path: &Path) -> Option<Vec<u8>> {
        self.content.read_head(path)
    }

    fn head(&self, path: &Path, max: usize) -> Option<Vec<u8>> {
        read_head(path, max)
    }

    fn read_at(&self, path: &Path, offset: u64, len: usize) -> Option<Vec<u8>> {
        let mut f = std::fs::File::open(path).ok()?;
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = Vec::with_capacity(len.min(1 << 16));
        f.take(len as u64).read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    fn link_target(&self, path: &Path, kind: LinkKind) -> Option<String> {
        let dir = parent_dir(path);
        match kind {
            LinkKind::Reparse => {
                let raw = std::fs::read_link(path).ok()?;
                let t = normalize_reparse_target(&raw.to_string_lossy(), dir.as_deref());
                (!t.is_empty()).then_some(t)
            }
            LinkKind::Shortcut => {
                let data = read_head(path, MAX_LINK_FILE_BYTES)?;
                lnk_target(&data, dir.as_deref(), self.content.ansi_code_page)
            }
            LinkKind::Internet => url_target(&read_head(path, MAX_LINK_FILE_BYTES)?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::testutil::file;

    #[test]
    fn default_source() {
        let o = DefaultOnDemand::default();
        let (_d, p) = file("a.txt", "quarterly plan".as_bytes());
        assert_eq!(o.content(&p, "txt").unwrap(), "quarterly plan");
        assert_eq!(o.text_as(&p, TextEncoding::Utf8).unwrap(), "quarterly plan");
        assert_eq!(o.bytes(&p).unwrap(), b"quarterly plan");
        let (_d, png) = file("a.png", &crate::extract::image::tests::png(5, 7));
        let v = o.properties(&png, "png", &[PropertyId::WIDTH]);
        assert_eq!(v, vec![(PropertyId::WIDTH, Value::Int(5))]);
        assert_eq!(o.head(&p, 9).unwrap(), b"quarterly");
        assert_eq!(o.read_at(&p, 10, 100).unwrap(), b"plan");
        assert!(o.head(Path::new("/nonexistent/x"), 4).is_none());
    }

    #[test]
    fn link_targets() {
        let o = DefaultOnDemand::default();
        let (d, lnk) = file("a.lnk", &crate::link::tests::lnk(None, "", false, Some("..\\t.txt")));
        let want = format!("{}", d.path().parent().unwrap().join("t.txt").display());
        assert_eq!(o.link_target(&lnk, LinkKind::Shortcut).unwrap(), want);
        let (_d2, url) = file("a.url", b"[InternetShortcut]\r\nURL=https://example.com/\r\n");
        assert_eq!(o.link_target(&url, LinkKind::Internet).unwrap(), "https://example.com/");
        assert!(o.link_target(&url, LinkKind::Reparse).is_none(), "not a reparse point");
        #[cfg(unix)]
        {
            let target = d.path().join("target.txt");
            std::fs::write(&target, b"x").unwrap();
            let link = d.path().join("sym");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert_eq!(o.link_target(&link, LinkKind::Reparse).unwrap(), target.display().to_string());
            let rel = d.path().join("rel");
            std::os::unix::fs::symlink("target.txt", &rel).unwrap();
            assert_eq!(o.link_target(&rel, LinkKind::Reparse).unwrap(), target.display().to_string());
        }
    }
}

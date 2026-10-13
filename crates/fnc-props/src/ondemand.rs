//! Slow-path file access for searches on entries that are not indexed (SPEC ch.1 §8.4,
//! §9.4; ch.2 §8.6): the search engine calls an [`OnDemand`] source for each candidate.
//! The app installs [`DefaultOnDemand`] (optionally with the Windows Property System
//! fallback); tests can install fakes.

use std::path::Path;
use std::sync::Arc;

use crate::content::{ContentExtractor, TextEncoding};
use crate::extract::{PropertyExtractor, PropertyProvider};
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
    }
}

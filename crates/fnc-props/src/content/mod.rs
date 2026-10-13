//! Content index (SPEC ch.1 §9): extraction and the compressed, token-indexed store.

#[cfg(feature = "extract")]
pub mod extract;
pub mod store;

#[cfg(feature = "extract")]
pub use extract::{
    ContentExtractor, ContentProvider, TextEncoding, decode, decode_forced, encoding_for_code_page,
    is_foreign_document, strip_html,
};
pub use store::{
    ContentHit, ContentIndex, ContentMatcher, ContentSnapshot, ContentStore, MatchScratch, TextStats, text_stats,
};

//! Fennec semantic search (SPEC ch.5): a second, optional retrieval channel next to the
//! literal index. Files in a user-chosen scope are split into chunks (file name, text
//! content, code), embedded with EmbeddingGemma-2 (llama.cpp) and stored in an HNSW vector
//! index; `sem:` queries embed the query text and look up the nearest chunks.

#![forbid(unsafe_code)]

pub mod chunk;
pub mod embed;
pub mod engine;
pub mod fusion;
#[cfg(feature = "llama")]
pub mod llama;
pub mod models;
pub mod prompt;
pub mod store;

use std::path::PathBuf;

pub use embed::{Embedder, HashEmbedder, Lane, ModelHost};
pub use engine::{Candidate, Catalog, EngineConfig, Scope, SemQuery, SemanticEngine, Status, TextSource};
pub use models::ModelSpec;
pub use prompt::{Prompts, Task};
pub use store::{DocHit, VectorStore};

/// What a chunk was made from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    /// File name and its two parent folders.
    Name = 0,
    /// Extracted text content.
    Content = 1,
    /// Source code.
    Code = 2,
    /// Picture (needs a multimodal projector; not indexed by this build).
    Image = 3,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Kind; 4] = [Kind::Name, Kind::Content, Kind::Code, Kind::Image];

    /// Bit in a [`KindMask`].
    pub const fn bit(self) -> KindMask {
        1 << self as u8
    }

    fn from_u8(v: u8) -> Option<Self> {
        Kind::ALL.get(usize::from(v)).copied()
    }
}

/// Set of [`Kind`]s.
pub type KindMask = u8;

/// All kinds.
pub const ALL_KINDS: KindMask = 0b1111;

/// Errors of the semantic channel.
#[derive(Debug, thiserror::Error)]
pub enum SemError {
    /// The model file is not there (SEM-07).
    #[error("model file not found: {0}")]
    ModelMissing(PathBuf),
    /// llama.cpp could not load the model.
    #[error("cannot load the model: {0}")]
    Load(String),
    /// Inference failed.
    #[error("embedding failed: {0}")]
    Embed(String),
    /// The vector index reported an error.
    #[error("vector index: {0}")]
    Index(String),
    /// The build has no embedder (feature `llama` off).
    #[error("this build has no semantic search")]
    Unsupported,
    /// File I/O.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The store file is damaged.
    #[error("semantic store is damaged: {0}")]
    Corrupt(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, SemError>;

//! Fennec search executor.
//!
//! * [`matcher`] — compiled text patterns (SIMD substring, prefix/suffix, wildcard and
//!   regex) over fold keys.
//! * [`eval`] — plan compilation (precomputed folder sets, counts) and per-entry
//!   evaluation.
//! * [`exec`] — parallel 64K-block scans with cancellation and result-set reuse.
//! * [`sort`] — result ordering (name fast-sort ranks, folders first).
//! * [`highlight`] — match ranges for visible rows.
//! * [`engine`] — the background search service used by every front end.

#![forbid(unsafe_code)]

pub mod cache;
pub mod engine;
pub mod eval;
pub mod exec;
pub mod ext;
pub mod fuzzy;
pub mod highlight;
pub mod matcher;
pub mod paths;
pub mod pinyin;
pub mod semantic;
pub mod sets;
pub mod sort;
pub mod tree;
pub mod types;

pub use cache::{Caches, CancelToken, Cancelled};
pub use engine::{
    EngineEvent, EngineHandle, SearchEngine, SearchOptions, SearchOutcome, SearchRequest, configure_threads,
};
pub use exec::{
    ContentHitInfo, ExecCtx, ExecStats, ResultExtra, ResultSet, apply_post, execute, execute_ext, execute_filter,
};
pub use ext::{ExecWarning, ExtCtx, Shared, SlowPolicy, content_highlights};
pub use highlight::Highlighter;
pub use semantic::{SemHit, SemanticSource};
pub use sort::{SortColumn, SortKey, SortSpec, sort_ids, sort_ids_ext};
pub use types::{TypeNameFn, TypeNames};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ext;
#[cfg(test)]
mod tests_sem;

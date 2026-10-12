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
pub mod highlight;
pub mod matcher;
pub mod paths;
pub mod sort;

pub use cache::{Caches, CancelToken, Cancelled};
pub use engine::{
    EngineEvent, EngineHandle, SearchEngine, SearchOptions, SearchOutcome, SearchRequest, configure_threads,
};
pub use exec::{ExecCtx, ExecStats, ResultSet, execute};
pub use highlight::Highlighter;
pub use sort::{SortColumn, SortSpec, sort_ids};

#[cfg(test)]
mod tests;

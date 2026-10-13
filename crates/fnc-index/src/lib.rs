//! Fennec index engine: the in-memory columnar entry table and everything that
//! maintains it.
//!
//! * [`IndexStore`] — SoA columns (name/fold arenas, parent, flags, frn, size, dates,
//!   attributes, extension id), children CSR, per-volume FRN maps, name fast-sort.
//! * [`Change`] / [`IndexStore::apply`] — incremental updates (USN, watchers).
//! * [`BulkLoader`] — initial enumeration (MFT, folder walk, file list).
//! * [`efu`] — Everything file list reader/writer.
//! * [`SharedIndex`] — `RwLock` handle with owned read snapshots.

#![forbid(unsafe_code)]

pub mod change;
pub mod children;
pub mod efu;
pub mod error;
pub mod ext;
pub mod frnmap;
pub mod order;
pub mod persist;
pub mod shared;
pub mod store;
pub mod volume;

pub use change::{ApplyStats, Change, ChangeBatch, EntryRef, Meta, PathChange, RelPath};
pub use error::{IndexError, Result};
pub use order::NameOrder;
pub use shared::{EpochIndex, IndexSnapshot, SharedIndex};
pub use store::{
    BulkLoader, BulkStats, IndexOptions, IndexStats, IndexStore, RunInfo, SIZE_UNKNOWN, path_frn, path_frn_probe,
};
pub use volume::{Volume, VolumeInfo, VolumeKind};

#[cfg(test)]
mod tests;

//! Fennec core: the data model shared by every other crate.
//!
//! * [`EntryId`], [`VolumeId`], [`FileTime`] — identifiers and timestamps.
//! * [`arena`] — append-only string pools (`NameArena` UTF-16, `FoldArena` UTF-8).
//! * [`fold`] — fold keys (case fold + NFKD + diacritic stripping) used for matching.
//! * [`natsort`] — natural ("logical") name comparison.
//! * [`settings`] — the `Settings` structure and `settings.toml` persistence.
//! * [`logging`] — `tracing` initialisation with rolling log files.
//! * [`crash`] — the panic hook and crash reports (`logs/crash-*.log`).
//! * [`paths`] — data directory resolution (installed / portable / instance).

#![forbid(unsafe_code)]

// Lets `#[derive(Settings)]` refer to `::fnc_core` from inside this crate.
extern crate self as fnc_core;

pub mod arena;
pub mod crash;
pub mod error;
pub mod flags;
pub mod fold;
pub mod ids;
pub mod logging;
pub mod natsort;
pub mod paths;
pub mod secret;
pub mod settings;
pub mod time;

pub use arena::{FoldArena, NameArena};
pub use error::{CoreError, Result};
pub use flags::EntryFlags;
pub use ids::{EntryId, VolumeId};
pub use time::FileTime;

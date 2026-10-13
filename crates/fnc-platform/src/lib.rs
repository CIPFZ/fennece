//! Platform abstraction.
//!
//! * [`source`] — the `VolumeSource` contract implemented by the NTFS backend
//!   (`fnc-ntfs`) and the portable [`walk::FolderSource`].
//! * [`shell`] — open / reveal in file manager.
//! * [`instance`] — single-instance detection and command forwarding.
//! * [`privilege`] — elevation checks, background thread priority.
//! * `props` (feature `props`) — the Windows Property System fallback for the property index.
//!
//! `unsafe` is permitted here (and in `fnc-ntfs`) for Win32 calls only; every block
//! carries a safety argument.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(unsafe_code)]

pub mod fileops;
pub mod instance;
pub mod integration;
pub mod locale;
pub mod privilege;
#[cfg(feature = "props")]
pub mod props;
pub mod shell;
pub mod source;
pub mod thumb;
pub mod walk;

pub use source::{ChangeSink, EntrySink, MetaSink, MetadataFetcher, RawEntry, SourceError, StopFlag, VolumeSource};

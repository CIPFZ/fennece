//! Platform abstraction.
//!
//! * [`source`] — the `VolumeSource` contract implemented by the NTFS backend
//!   (`fnc-ntfs`) and the portable [`walk::FolderSource`].
//! * [`shell`] — open / reveal in file manager.
//! * [`instance`] — single-instance detection and command forwarding.
//! * [`privilege`] — elevation checks, background thread priority.
//! * [`os`] — OS version text, console attach, unhandled-exception crash reports.
//! * `props` (feature `props`) — the Windows Property System fallback for the property index.
//! * [`ifilter`] — IFilter text extraction in a separate host process (client, protocol, host).
//! * [`session`] — session id / integrity level of IPC clients.
//! * [`compat`] — the hidden Everything-compatible IPC window.
//! * [`winsearch`] — queries of the Windows Search system index (`si:`).
//!
//! `unsafe` is permitted here (and in `fnc-ntfs`) for Win32 calls only; every block
//! carries a safety argument.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(unsafe_code)]

pub mod compat;
pub mod fileops;
pub mod ifilter;
pub mod instance;
pub mod integration;
pub mod locale;
pub mod os;
pub mod privilege;
pub mod process;
#[cfg(feature = "props")]
pub mod props;
pub mod session;
pub mod shell;
pub mod source;
#[cfg(feature = "sysindex")]
pub mod sysindex;
pub mod thumb;
pub mod walk;
pub mod winsearch;

pub use source::{ChangeSink, EntrySink, MetaSink, MetadataFetcher, RawEntry, SourceError, StopFlag, VolumeSource};

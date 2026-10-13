//! Platform abstraction.
//!
//! * [`source`] — the `VolumeSource` contract implemented by the NTFS backend
//!   (`fnc-ntfs`) and the portable [`walk::FolderSource`].
//! * [`shell`] — open / reveal in file manager.
//! * [`instance`] — single-instance detection and command forwarding.
//! * [`privilege`] — elevation checks.
//! * [`session`] — session id / integrity level of IPC clients.
//! * [`compat`] — the hidden Everything-compatible IPC window.
//!
//! `unsafe` is permitted here (and in `fnc-ntfs`) for Win32 calls only; every block
//! carries a safety argument.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(unsafe_code)]

pub mod compat;
pub mod fileops;
pub mod instance;
pub mod integration;
pub mod locale;
pub mod privilege;
pub mod session;
pub mod shell;
pub mod source;
pub mod thumb;
pub mod walk;

pub use source::{ChangeSink, EntrySink, MetaSink, MetadataFetcher, RawEntry, SourceError, StopFlag, VolumeSource};

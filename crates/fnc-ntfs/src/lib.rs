//! NTFS / ReFS backend (SPEC ch.1 §2).
//!
//! * [`record`] — `USN_RECORD_V2/V3` parsing (portable, unit-tested with byte samples).
//! * [`journal`] — folding USN records into index changes (portable).
//! * `win` / `source` (Windows only) — `FSCTL_ENUM_USN_DATA` MFT enumeration,
//!   `FSCTL_READ_USN_JOURNAL` monitoring, `OpenFileById` stat, `FindFirstFileExW`
//!   metadata back-fill, and the [`fnc_platform::VolumeSource`] implementation.
//!
//! No directory recursion is used to build NTFS indexes (AGENT.md iron rule 3).

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(windows, allow(unsafe_code))]

pub mod dirinfo;
pub mod journal;
pub mod record;

#[cfg(windows)]
pub mod source;
#[cfg(windows)]
pub mod win;

#[cfg(windows)]
pub use source::{NtfsOptions, NtfsSource, list_volumes};

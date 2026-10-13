//! Volume source contract (SPEC ch.1 §12).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fnc_core::{EntryId, VolumeId};
use fnc_index::{ChangeBatch, Meta, VolumeInfo};

/// Errors from volume sources.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// I/O or OS error.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Access to the volume was denied (not elevated and no service).
    #[error("access denied: {0}")]
    AccessDenied(String),
    /// The change journal was reset or wrapped: the volume must be rebuilt.
    #[error("change journal reset on {0}; full rebuild required")]
    JournalReset(String),
    /// The volume went offline.
    #[error("volume {0} is offline")]
    Offline(String),
    /// The operation was stopped.
    #[error("stopped")]
    Stopped,
    /// Not supported by this source.
    #[error("not supported: {0}")]
    Unsupported(&'static str),
}

impl SourceError {
    /// Wraps an I/O error with context.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            return Self::AccessDenied(context.into());
        }
        Self::Io { context: context.into(), source }
    }
}

/// Cooperative stop flag for long-running source operations.
#[derive(Clone, Default, Debug)]
pub struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    /// New, not stopped.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests stop.
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// `true` once stopped.
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// One enumerated record.
#[derive(Debug, Clone)]
pub struct RawEntry<'a> {
    /// File reference number (or synthetic id), unique within the volume.
    pub frn: u64,
    /// Parent's FRN.
    pub parent_frn: u64,
    /// Name (UTF-16).
    pub name: &'a [u16],
    /// Known metadata (MFT enumeration only provides attributes).
    pub meta: Meta,
}

/// Receives enumerated records.
pub trait EntrySink {
    /// One record. Returning an error aborts the enumeration.
    fn entry(&mut self, e: &RawEntry<'_>) -> Result<(), SourceError>;
    /// Progress report (`done` records so far, `total` estimate or 0).
    fn progress(&mut self, _done: u64, _total: u64) {}
}

/// Receives batches of changes while watching.
pub trait ChangeSink {
    /// Applies a batch; returns `false` to stop watching.
    fn changes(&mut self, batch: ChangeBatch) -> bool;
    /// Called when the source has caught up (no pending changes).
    fn idle(&mut self) {}
    /// Records the journal position after a batch (persisted with the index).
    fn checkpoint(&mut self, _journal_id: u64, _next_usn: i64) {}
}

/// Receives metadata for children of a folder (size/date back-fill).
pub trait MetaSink {
    /// `meta` for the child `name` of folder `dir`. `file_id` is the child's FRN when
    /// the source can report it reliably (NTFS); it identifies hard-link names that MFT
    /// enumeration does not return.
    fn meta(&mut self, dir: EntryId, name: &[u16], meta: Meta, file_id: Option<u64>);
}

/// A metadata reader independent of its source, so the back-fill can run while the
/// source is watching for changes.
pub trait MetadataFetcher: Send {
    /// Same contract as [`VolumeSource::fetch_metadata`].
    fn fetch(&self, dirs: &[(EntryId, String)], out: &mut dyn MetaSink, stop: &StopFlag) -> Result<(), SourceError>;
}

/// An index source: an NTFS/ReFS volume, a folder index root, ...
pub trait VolumeSource: Send {
    /// Static volume description.
    fn info(&self) -> VolumeInfo;
    /// FRN of the root folder (records with this FRN describe the root).
    fn root_frn(&self) -> u64;
    /// Enumerates every entry (any order; parents are resolved by the index).
    fn enumerate(&mut self, sink: &mut dyn EntrySink, stop: &StopFlag) -> Result<(), SourceError>;
    /// Watches for changes until `stop` or an error. Changes reference `volume`.
    fn watch(&mut self, volume: VolumeId, sink: &mut dyn ChangeSink, stop: &StopFlag) -> Result<(), SourceError>;
    /// Back-fills metadata for the children of the given folders (`(id, full path)`).
    fn fetch_metadata(
        &self,
        dirs: &[(EntryId, String)],
        out: &mut dyn MetaSink,
        stop: &StopFlag,
    ) -> Result<(), SourceError>;
    /// An independent fetcher for [`VolumeSource::fetch_metadata`], if the source can
    /// back-fill concurrently with [`VolumeSource::watch`].
    fn metadata_fetcher(&self) -> Option<Box<dyn MetadataFetcher>> {
        None
    }
    /// `true` if [`VolumeSource::enumerate`] already delivers sizes and dates.
    fn enumerates_metadata(&self) -> bool;
    /// Journal id and next USN captured before enumeration (NTFS/ReFS).
    fn journal_position(&self) -> Option<(u64, i64)> {
        None
    }
    /// Continues from a saved journal position instead of enumerating (database
    /// load). Fails if the journal was recreated or no longer holds `next_usn`; the
    /// caller then rebuilds the volume.
    fn resume(&mut self, _journal_id: u64, _next_usn: i64) -> Result<(), SourceError> {
        Err(SourceError::Unsupported("resume"))
    }
}

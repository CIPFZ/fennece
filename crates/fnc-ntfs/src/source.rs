//! NTFS/ReFS volume source: MFT enumeration + USN journal monitoring (Windows).

use std::time::{Duration, Instant};

use fnc_core::{EntryId, VolumeId};
use fnc_index::{Change, ChangeBatch, EntryRef, VolumeInfo, VolumeKind};
use fnc_platform::{ChangeSink, EntrySink, MetaSink, MetadataFetcher, RawEntry, SourceError, StopFlag, VolumeSource};

use crate::journal::fold_records;
use crate::record::records;
use crate::win::{self, AlignedBuf, DriveInfo, Handle, Journal, ReadError};

/// Journal creation and polling options (from settings).
#[derive(Debug, Clone, Copy)]
pub struct NtfsOptions {
    /// Create the journal if missing.
    pub create_journal: bool,
    /// Maximum journal size (bytes) when creating.
    pub max_size: u64,
    /// Allocation delta (bytes) when creating.
    pub allocation_delta: u64,
    /// Idle wait between empty journal reads.
    pub poll_interval: Duration,
}

impl Default for NtfsOptions {
    fn default() -> Self {
        Self {
            create_journal: true,
            max_size: 32 << 20,
            allocation_delta: 4 << 20,
            poll_interval: Duration::from_millis(1000),
        }
    }
}

/// MFT record number bits of an NTFS file reference.
const RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// MFT record of the `$Extend` metafile directory.
const EXTEND_RECORD: u64 = 11;

/// Lists local NTFS and ReFS volumes on fixed drives.
// SPEC-GAP: removable NTFS drives are skipped until device-arrival/removal handling
// (WM_DEVICECHANGE, DBT_DEVICEQUERYREMOVE) lands in M2; holding their volume handle
// would block safe removal.
pub fn list_volumes() -> Vec<DriveInfo> {
    win::drives().into_iter().filter(|d| matches!(d.fs_name.as_str(), "NTFS" | "ReFS") && d.drive_type == 3).collect()
}

/// One NTFS/ReFS volume opened for indexing.
pub struct NtfsSource {
    drive: DriveInfo,
    handle: Handle,
    root_frn: u64,
    journal: Journal,
    resume_usn: i64,
    opts: NtfsOptions,
}

fn io_err(ctx: String) -> impl FnOnce(std::io::Error) -> SourceError {
    move |e| SourceError::io(ctx, e)
}

impl NtfsSource {
    /// Opens the volume, reads (or creates) its USN journal and records the journal
    /// position *before* enumeration so no change is missed.
    pub fn open(drive: DriveInfo, opts: NtfsOptions) -> Result<Self, SourceError> {
        let handle = win::open_volume(drive.letter).map_err(io_err(format!("open volume {}", drive.root)))?;
        let root_frn = win::root_frn(drive.letter).map_err(io_err(format!("root of {}", drive.root)))?;
        let journal = match win::query_journal(&handle).map_err(io_err(format!("query journal {}", drive.root)))? {
            Some(j) => j,
            None if opts.create_journal => {
                tracing::info!(volume = %drive.root, "creating USN journal");
                win::create_journal(&handle, opts.max_size, opts.allocation_delta)
                    .map_err(io_err(format!("create journal {}", drive.root)))?;
                win::query_journal(&handle)
                    .map_err(io_err(format!("query journal {}", drive.root)))?
                    .ok_or(SourceError::Unsupported("USN journal unavailable"))?
            }
            None => return Err(SourceError::Unsupported("USN journal disabled and creation not allowed")),
        };
        Ok(Self { resume_usn: journal.next_usn, drive, handle, root_frn, journal, opts })
    }

    /// Continues from a persisted journal position instead of the current end. Fails
    /// (caller rebuilds) if the journal id changed or the position was overwritten.
    pub fn resume_from(&mut self, journal_id: u64, next_usn: i64) -> Result<(), SourceError> {
        if journal_id != self.journal.id || next_usn < self.journal.first_usn {
            return Err(SourceError::JournalReset(self.drive.root.clone()));
        }
        self.resume_usn = next_usn;
        Ok(())
    }

    /// The drive.
    pub fn drive(&self) -> &DriveInfo {
        &self.drive
    }
}

impl VolumeSource for NtfsSource {
    fn info(&self) -> VolumeInfo {
        let kind = if self.drive.fs_name == "ReFS" { VolumeKind::Refs } else { VolumeKind::Ntfs };
        let mut info = VolumeInfo::new(kind, self.drive.root.clone());
        info.display_name = if self.drive.label.is_empty() {
            format!("{}:", self.drive.letter)
        } else {
            format!("{} ({}:)", self.drive.label, self.drive.letter)
        };
        info.serial = self.drive.serial;
        info.guid = if self.drive.guid.is_empty() { self.drive.root.clone() } else { self.drive.guid.clone() };
        info.fs_name = self.drive.fs_name.clone();
        info.separator = '\\';
        info
    }

    fn root_frn(&self) -> u64 {
        self.root_frn
    }

    fn enumerate(&mut self, sink: &mut dyn EntrySink, stop: &StopFlag) -> Result<(), SourceError> {
        let mut name = Vec::with_capacity(256);
        let mut failed: Option<SourceError> = None;
        let mut done = 0u64;
        let t0 = Instant::now();
        let root_frn = self.root_frn;
        // ReFS has no MFT; its (hashed) ids must not be mistaken for record 11.
        let mut extend_emitted = self.drive.fs_name != "NTFS";
        // HighUsn = MAX: records modified while enumerating are included (not skipped);
        // replaying the journal from the position captured in `open` is idempotent.
        win::enum_mft(&self.handle, i64::MAX, |r| {
            if stop.is_stopped() {
                failed = Some(SourceError::Stopped);
                return false;
            }
            // `FSCTL_ENUM_USN_DATA` never returns the `$Extend` metafile directory (MFT
            // record 11), but does return its children (`$RmMetadata`, `$Txf`, ...).
            // Synthesize it under the root so they get their real path instead of
            // landing in `$Orphans`.
            if !extend_emitted && r.parent_frn & RECORD_MASK == EXTEND_RECORD && r.parent_frn != root_frn {
                extend_emitted = true;
                let extend: Vec<u16> = "$Extend".encode_utf16().collect();
                let e = RawEntry {
                    frn: r.parent_frn,
                    parent_frn: root_frn,
                    name: &extend,
                    meta: fnc_index::Meta::attrs(
                        fnc_core::flags::attr::DIRECTORY
                            | fnc_core::flags::attr::HIDDEN
                            | fnc_core::flags::attr::SYSTEM,
                    ),
                };
                if let Err(err) = sink.entry(&e) {
                    failed = Some(err);
                    return false;
                }
            }
            r.name_into(&mut name);
            let e = RawEntry {
                frn: r.frn,
                parent_frn: r.parent_frn,
                name: &name,
                meta: fnc_index::Meta::attrs(r.attributes),
            };
            if let Err(err) = sink.entry(&e) {
                failed = Some(err);
                return false;
            }
            done += 1;
            if done.is_multiple_of(65_536) {
                sink.progress(done, 0);
            }
            true
        })
        .map_err(io_err(format!("enumerate MFT {}", self.drive.root)))?;
        if let Some(e) = failed {
            return Err(e);
        }
        sink.progress(done, done);
        tracing::info!(volume = %self.drive.root, records = done, elapsed = ?t0.elapsed(), "MFT enumerated");
        Ok(())
    }

    fn watch(&mut self, volume: VolumeId, sink: &mut dyn ChangeSink, stop: &StopFlag) -> Result<(), SourceError> {
        let mut buf = AlignedBuf::new(64 * 1024);
        let mut next = self.resume_usn;
        let mut refresh_batch: ChangeBatch = Vec::new();
        while !stop.is_stopped() {
            let t = Instant::now();
            let (new_next, len) = match win::read_journal(&self.handle, self.journal.id, next, true, &mut buf) {
                Ok(v) => v,
                Err(ReadError::Reset) => return Err(SourceError::JournalReset(self.drive.root.clone())),
                Err(ReadError::Unavailable) => return Err(SourceError::Offline(self.drive.root.clone())),
                Err(ReadError::Io(e)) => return Err(SourceError::io(format!("read journal {}", self.drive.root), e)),
            };
            if len == 0 {
                sink.idle();
                // The read normally blocks until a record arrives (event-driven). If it
                // ever returns immediately without data, fall back to polling at the
                // configured interval instead of spinning.
                if t.elapsed() < Duration::from_millis(50) {
                    std::thread::sleep(self.opts.poll_interval);
                }
                next = new_next.max(next);
                continue;
            }
            let bytes = buf.bytes(len + 8);
            let parsed: Vec<_> = records(&bytes[8..]).filter_map(Result::ok).collect();
            let folded = fold_records(volume, parsed);
            next = new_next;
            if !folded.changes.is_empty() && !sink.changes(folded.changes) {
                return Ok(());
            }
            refresh_batch.clear();
            for frn in folded.refresh {
                match win::stat_frn(&self.handle, frn) {
                    Ok(meta) => refresh_batch.push(Change::Meta { target: EntryRef::Frn(volume, frn), meta }),
                    Err(e) => tracing::trace!(frn, error = %e, "stat by FRN failed (deleted meanwhile?)"),
                }
            }
            if !refresh_batch.is_empty() && !sink.changes(std::mem::take(&mut refresh_batch)) {
                return Ok(());
            }
            sink.checkpoint(self.journal.id, next);
        }
        Err(SourceError::Stopped)
    }

    fn fetch_metadata(
        &self,
        dirs: &[(EntryId, String)],
        out: &mut dyn MetaSink,
        stop: &StopFlag,
    ) -> Result<(), SourceError> {
        self.lister().fetch(dirs, out, stop)
    }

    fn metadata_fetcher(&self) -> Option<Box<dyn MetadataFetcher>> {
        Some(Box::new(self.lister()))
    }

    fn enumerates_metadata(&self) -> bool {
        false
    }

    fn journal_position(&self) -> Option<(u64, i64)> {
        Some((self.journal.id, self.resume_usn))
    }

    fn resume(&mut self, journal_id: u64, next_usn: i64) -> Result<(), SourceError> {
        self.resume_from(journal_id, next_usn)
    }
}

impl NtfsSource {
    fn lister(&self) -> DirLister {
        DirLister { file_ids: self.drive.fs_name == "NTFS" }
    }
}

/// Size/date back-fill by listing folders (`FileIdBothDirectoryInfo`); needs no volume
/// handle, so it runs concurrently with journal monitoring.
struct DirLister {
    /// The listing's 64-bit file ids equal the volume's FRNs (NTFS; not ReFS, whose
    /// ids are 128-bit).
    file_ids: bool,
}

impl MetadataFetcher for DirLister {
    fn fetch(&self, dirs: &[(EntryId, String)], out: &mut dyn MetaSink, stop: &StopFlag) -> Result<(), SourceError> {
        for (id, path) in dirs {
            if stop.is_stopped() {
                return Err(SourceError::Stopped);
            }
            let ids = self.file_ids;
            if let Err(e) = win::list_dir_ids(path, |name, meta, fid| out.meta(*id, name, meta, ids.then_some(fid))) {
                tracing::debug!(dir = %path, error = %e, "cannot list folder");
            }
        }
        Ok(())
    }
}

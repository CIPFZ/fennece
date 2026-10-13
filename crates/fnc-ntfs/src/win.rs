//! Win32 primitives for NTFS/ReFS indexing: volume enumeration, MFT enumeration
//! (`FSCTL_ENUM_USN_DATA`), journal queries/reads (`FSCTL_*_USN_JOURNAL`), stat by FRN
//! (`OpenFileById`) and directory listing with file ids (`FileIdBothDirectoryInfo`).
//!
//! Every `unsafe` block passes buffers that are owned by the caller, correctly sized
//! and alive for the duration of the call; handles are owned by [`Handle`] and closed
//! exactly once.

use std::io;

use fnc_core::FileTime;
use fnc_core::flags::attr;
use fnc_index::Meta;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileBasicInfo, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, FileIdInfo, FileIdType, FileStandardInfo, GetDriveTypeW,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetLogicalDrives, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW, OPEN_EXISTING, OpenFileById,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL,
    FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V1, READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V1,
};
use windows::core::PCWSTR;

use crate::record::{UsnRecord, reason, records};

/// Win32 error codes used here.
pub mod code {
    /// `ERROR_ACCESS_DENIED`
    pub const ACCESS_DENIED: u32 = 5;
    /// `ERROR_HANDLE_EOF`
    pub const HANDLE_EOF: u32 = 38;
    /// `ERROR_JOURNAL_DELETE_IN_PROGRESS`
    pub const JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
    /// `ERROR_JOURNAL_NOT_ACTIVE`
    pub const JOURNAL_NOT_ACTIVE: u32 = 1179;
    /// `ERROR_JOURNAL_ENTRY_DELETED`
    pub const JOURNAL_ENTRY_DELETED: u32 = 1181;
    /// `ERROR_NO_MORE_FILES`
    pub const NO_MORE_FILES: u32 = 18;
    /// `ERROR_NOT_READY` (media removed)
    pub const NOT_READY: u32 = 21;
    /// `ERROR_DEV_NOT_EXIST`
    pub const DEV_NOT_EXIST: u32 = 55;
    /// `ERROR_DEVICE_NOT_CONNECTED`
    pub const DEVICE_NOT_CONNECTED: u32 = 1167;
}

/// Win32 error code carried by a `windows` error (0 if not a Win32 HRESULT).
pub fn win32_code(e: &windows::core::Error) -> u32 {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 { h & 0xFFFF } else { 0 }
}

fn to_io(e: windows::core::Error) -> io::Error {
    match win32_code(&e) {
        0 => io::Error::other(e),
        c => io::Error::from_raw_os_error(c as i32),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Owned kernel handle.
#[derive(Debug)]
pub struct Handle(HANDLE);

// SAFETY: kernel handles are process-wide references; synchronous DeviceIoControl and
// file-information calls on one handle are safe from any thread.
unsafe impl Send for Handle {}
// SAFETY: see `Send`; no interior state besides the handle value.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by a successful open call and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// 8-byte aligned I/O buffer (USN records are 8-byte aligned).
pub struct AlignedBuf {
    words: Vec<u64>,
}

impl AlignedBuf {
    /// Buffer of at least `bytes` bytes.
    pub fn new(bytes: usize) -> Self {
        Self { words: vec![0u64; bytes.div_ceil(8)] }
    }

    fn len_bytes(&self) -> usize {
        self.words.len() * 8
    }

    fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
        self.words.as_mut_ptr().cast()
    }

    /// The first `len` bytes.
    pub fn bytes(&self, len: usize) -> &[u8] {
        let len = len.min(self.len_bytes());
        // SAFETY: `words` owns `len_bytes()` initialised bytes; `len` is clamped to that;
        // `u8` has no alignment requirement and the slice borrows `self`.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), len) }
    }
}

/// A local drive.
#[derive(Debug, Clone)]
pub struct DriveInfo {
    /// Drive letter.
    pub letter: char,
    /// `C:\`
    pub root: String,
    /// File system (`NTFS`, `ReFS`, `FAT32`, ...).
    pub fs_name: String,
    /// Volume serial number.
    pub serial: u32,
    /// Volume label.
    pub label: String,
    /// `\\?\Volume{GUID}\` (empty if unavailable).
    pub guid: String,
    /// `GetDriveTypeW` result (2 removable, 3 fixed, 4 remote, 5 cdrom, 6 ramdisk).
    pub drive_type: u32,
}

/// Lists mounted drive letters with their file systems.
pub fn drives() -> Vec<DriveInfo> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = char::from(b'A' + i as u8);
        let root = format!("{letter}:\\");
        let root_w = wide(&root);
        // SAFETY: `root_w` is NUL-terminated and outlives the call.
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) };
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        let mut serial = 0u32;
        // SAFETY: buffers are owned, sized as passed (slices carry their length) and
        // alive for the call; optional pointers are valid out-parameters.
        let ok = unsafe {
            GetVolumeInformationW(
                PCWSTR(root_w.as_ptr()),
                Some(&mut label),
                Some(&mut serial),
                None,
                None,
                Some(&mut fs),
            )
        }
        .is_ok();
        if !ok {
            continue;
        }
        let mut guid = [0u16; 64];
        // SAFETY: `root_w` is NUL-terminated; `guid` is an owned buffer of 64 units
        // (a volume GUID path needs 50).
        let guid = if unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(root_w.as_ptr()), &mut guid) }.is_ok() {
            from_wide_nul(&guid)
        } else {
            String::new()
        };
        out.push(DriveInfo {
            letter,
            root,
            fs_name: from_wide_nul(&fs),
            serial,
            label: from_wide_nul(&label),
            guid,
            drive_type,
        });
    }
    out
}

fn from_wide_nul(b: &[u16]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..end])
}

/// Opens `\\.\X:` for MFT/USN access (requires administrator rights).
///
/// Read+write access is requested first (needed by `FSCTL_CREATE_USN_JOURNAL`, as in
/// Microsoft's change-journal samples); read-only access is the fallback, which still
/// allows enumeration and journal reads.
pub fn open_volume(letter: char) -> io::Result<Handle> {
    let path = wide(&format!("\\\\.\\{letter}:"));
    let open = |access: u32| {
        // SAFETY: `path` is NUL-terminated and outlives the call; no security
        // attributes or template handle.
        unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
    };
    let h = open(GENERIC_READ.0 | GENERIC_WRITE.0).or_else(|_| open(GENERIC_READ.0)).map_err(to_io)?;
    Ok(Handle(h))
}

/// Opens the root folder of `letter` for attribute access (no privileges needed); it
/// serves as the volume hint for [`stat_frn`] when no volume handle is available.
pub fn open_root(letter: char) -> io::Result<Handle> {
    let path = wide(&format!("{letter}:\\"));
    // SAFETY: as in `open_volume`; BACKUP_SEMANTICS is required to open a directory.
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(to_io)?;
    Ok(Handle(h))
}

/// FRN of the root folder of `X:\` (record 5 with its sequence number on NTFS).
///
/// Read as the 128-bit `FileIdInfo` id and folded like `USN_RECORD_V3` ids, so the root
/// matches the parent ids of top-level records on ReFS too; falls back to the 64-bit
/// `nFileIndex` where `FileIdInfo` is unavailable.
pub fn root_frn(letter: char) -> io::Result<u64> {
    let h = open_root(letter)?;
    let mut id_info = FILE_ID_INFO::default();
    // SAFETY: `id_info` is an owned FILE_ID_INFO, the size passed is its exact size, and
    // `h` is an open handle.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            h.0,
            FileIdInfo,
            std::ptr::from_mut(&mut id_info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .is_ok();
    if ok {
        return Ok(crate::record::fold_file_id(&id_info.FileId.Identifier));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `info` is a valid out-structure; `h` is an open handle.
    unsafe { GetFileInformationByHandle(h.0, &mut info) }.map_err(to_io)?;
    Ok((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

/// USN journal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Journal {
    /// Journal instance id (changes when the journal is recreated).
    pub id: u64,
    /// First USN still in the journal.
    pub first_usn: i64,
    /// Next USN to be written.
    pub next_usn: i64,
    /// Lowest valid USN.
    pub lowest_valid_usn: i64,
    /// Maximum size.
    pub max_size: u64,
    /// Allocation delta.
    pub allocation_delta: u64,
}

fn ioctl<I>(
    h: &Handle,
    code: u32,
    input: Option<&I>,
    out: Option<(&mut AlignedBuf, usize)>,
) -> windows::core::Result<u32> {
    let mut returned = 0u32;
    let (in_ptr, in_len) = match input {
        Some(i) => (Some(std::ptr::from_ref(i).cast::<core::ffi::c_void>()), std::mem::size_of::<I>() as u32),
        None => (None, 0),
    };
    let (out_ptr, out_len) = match out {
        Some((b, n)) => {
            let n = n.min(b.len_bytes()) as u32;
            (Some(b.as_mut_ptr()), n)
        }
        None => (None, 0),
    };
    // SAFETY: the input points to a live `I` of the stated size; the output buffer is
    // owned, aligned and at least `out_len` bytes; the call is synchronous (no
    // OVERLAPPED), so no pointer outlives it.
    unsafe { DeviceIoControl(h.0, code, in_ptr, in_len, out_ptr, out_len, Some(&mut returned), None) }?;
    Ok(returned)
}

/// Queries the USN journal; `None` if no journal is active.
pub fn query_journal(h: &Handle) -> io::Result<Option<Journal>> {
    let mut buf = AlignedBuf::new(std::mem::size_of::<USN_JOURNAL_DATA_V1>());
    let n = std::mem::size_of::<USN_JOURNAL_DATA_V1>();
    match ioctl::<()>(h, FSCTL_QUERY_USN_JOURNAL, None, Some((&mut buf, n))) {
        Ok(_) => {
            let b = buf.bytes(n);
            let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8]));
            Ok(Some(Journal {
                id: u64_at(0),
                first_usn: u64_at(8) as i64,
                next_usn: u64_at(16) as i64,
                lowest_valid_usn: u64_at(24) as i64,
                max_size: u64_at(40),
                allocation_delta: u64_at(48),
            }))
        }
        Err(e) if matches!(win32_code(&e), code::JOURNAL_NOT_ACTIVE | code::JOURNAL_DELETE_IN_PROGRESS) => Ok(None),
        Err(e) => Err(to_io(e)),
    }
}

/// Creates (or resizes) the USN journal.
pub fn create_journal(h: &Handle, max_size: u64, allocation_delta: u64) -> io::Result<()> {
    let data = CREATE_USN_JOURNAL_DATA { MaximumSize: max_size, AllocationDelta: allocation_delta };
    ioctl(h, FSCTL_CREATE_USN_JOURNAL, Some(&data), None).map(|_| ()).map_err(to_io)
}

/// Deletes the USN journal `journal_id` of the volume (`-delete-usn-journal`); waits
/// until the deletion has finished.
pub fn delete_journal(h: &Handle, journal_id: u64) -> io::Result<()> {
    use windows::Win32::System::Ioctl::{
        DELETE_USN_JOURNAL_DATA, FSCTL_DELETE_USN_JOURNAL, USN_DELETE_FLAG_DELETE, USN_DELETE_FLAG_NOTIFY,
    };
    let data = DELETE_USN_JOURNAL_DATA {
        UsnJournalID: journal_id,
        DeleteFlags: USN_DELETE_FLAG_DELETE | USN_DELETE_FLAG_NOTIFY,
    };
    ioctl(h, FSCTL_DELETE_USN_JOURNAL, Some(&data), None).map(|_| ()).map_err(to_io)
}

/// Enumerates every MFT record with a USN ≤ `high_usn`, calling `f` for each.
/// Returns the number of records.
pub fn enum_mft(h: &Handle, high_usn: i64, mut f: impl FnMut(&UsnRecord<'_>) -> bool) -> io::Result<u64> {
    let mut count = 0u64;
    enum_mft_raw(h, high_usn, |chunk| {
        for r in records(chunk) {
            match r {
                Ok(r) => {
                    count += 1;
                    if !f(&r) {
                        return false;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "malformed MFT enumeration record");
                    break;
                }
            }
        }
        true
    })?;
    Ok(count)
}

/// Like [`enum_mft`], passing each raw output block (records after the 8-byte
/// next-FRN header) to `f` (the service forwards these unparsed).
pub fn enum_mft_raw(h: &Handle, high_usn: i64, mut f: impl FnMut(&[u8]) -> bool) -> io::Result<()> {
    let mut buf = AlignedBuf::new(1 << 20);
    let mut med = MFT_ENUM_DATA_V1 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: high_usn,
        MinMajorVersion: 2,
        MaxMajorVersion: 3,
    };
    loop {
        let cap = buf.len_bytes();
        let n = match ioctl(h, FSCTL_ENUM_USN_DATA, Some(&med), Some((&mut buf, cap))) {
            Ok(n) => n as usize,
            Err(e) if win32_code(&e) == code::HANDLE_EOF => return Ok(()),
            Err(e) => return Err(to_io(e)),
        };
        if n < 8 {
            return Ok(());
        }
        let b = buf.bytes(n);
        let next = u64::from_le_bytes(b[0..8].try_into().unwrap_or([0; 8]));
        if !f(&b[8..]) {
            return Ok(());
        }
        if next == med.StartFileReferenceNumber {
            return Ok(());
        }
        med.StartFileReferenceNumber = next;
    }
}

/// Errors reading the journal.
#[derive(Debug)]
pub enum ReadError {
    /// Records were overwritten (journal wrapped) or the journal was recreated.
    Reset,
    /// The journal is being deleted / volume going away.
    Unavailable,
    /// Other I/O failure.
    Io(io::Error),
}

/// Reads journal records from `start`. With `wait`, blocks up to ~1 s for new data.
/// Returns the next USN and the number of valid bytes in `buf` after the 8-byte header.
pub fn read_journal(
    h: &Handle,
    journal_id: u64,
    start: i64,
    wait: bool,
    buf: &mut AlignedBuf,
) -> Result<(i64, usize), ReadError> {
    let data = READ_USN_JOURNAL_DATA_V1 {
        StartUsn: start,
        ReasonMask: reason::ALL,
        ReturnOnlyOnClose: 0,
        // With BytesToWaitFor = 1 the call returns as soon as a record arrives; the
        // timeout is repeated while none are available, so the call blocks (event-driven,
        // no polling). SPEC-GAP: a stop request is therefore observed after the next
        // change; cancelling the wait (CancelSynchronousIo) comes with volume
        // removal/rebuild handling in M2.
        Timeout: u64::from(wait),
        BytesToWaitFor: u64::from(wait),
        UsnJournalID: journal_id,
        MinMajorVersion: 2,
        MaxMajorVersion: 3,
    };
    let cap = buf.len_bytes();
    match ioctl(h, FSCTL_READ_USN_JOURNAL, Some(&data), Some((buf, cap))) {
        Ok(n) if n >= 8 => {
            let b = buf.bytes(n as usize);
            let next = i64::from_le_bytes(b[0..8].try_into().unwrap_or([0; 8]));
            Ok((next, n as usize - 8))
        }
        Ok(_) => Ok((start, 0)),
        Err(e) => match win32_code(&e) {
            code::JOURNAL_ENTRY_DELETED | 0x0000_0057 /* ERROR_INVALID_PARAMETER: journal id mismatch */ => {
                Err(ReadError::Reset)
            }
            code::JOURNAL_DELETE_IN_PROGRESS
            | code::JOURNAL_NOT_ACTIVE
            | code::NOT_READY
            | code::DEVICE_NOT_CONNECTED
            | code::DEV_NOT_EXIST => Err(ReadError::Unavailable),
            _ => Err(ReadError::Io(to_io(e))),
        },
    }
}

/// Size, dates and attributes of the file with `frn` (no path needed).
// SPEC-GAP: genuine 128-bit ReFS ids are stored folded (see `record::fold_file_id`) and
// cannot be reopened with `FileIdType`; such refreshes fail quietly until the index
// keeps 128-bit ids (M2, with `ExtendedFileIdType`).
pub fn stat_frn(volume: &Handle, frn: u64) -> io::Result<Meta> {
    let desc = FILE_ID_DESCRIPTOR {
        dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
    };
    // SAFETY: `desc` is a fully initialised descriptor alive for the call; the volume
    // handle is open.
    let h = unsafe {
        OpenFileById(
            volume.0,
            &desc,
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    }
    .map_err(to_io)?;
    let h = Handle(h);
    let mut basic = FILE_BASIC_INFO::default();
    let mut std_info = FILE_STANDARD_INFO::default();
    // SAFETY: out-structures are owned, correctly typed for the info class and their
    // exact sizes are passed.
    unsafe {
        GetFileInformationByHandleEx(
            h.0,
            FileBasicInfo,
            std::ptr::from_mut(&mut basic).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .map_err(to_io)?;
    // SAFETY: as above.
    unsafe {
        GetFileInformationByHandleEx(
            h.0,
            FileStandardInfo,
            std::ptr::from_mut(&mut std_info).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    }
    .map_err(to_io)?;
    let is_dir = std_info.Directory;
    Ok(Meta {
        attributes: Some(basic.FileAttributes | if is_dir { attr::DIRECTORY } else { 0 }),
        size: (!is_dir).then_some(std_info.EndOfFile.max(0) as u64),
        date_modified: Some(FileTime(basic.LastWriteTime)),
        date_created: Some(FileTime(basic.CreationTime)),
        date_accessed: Some(FileTime(basic.LastAccessTime)),
    })
}

// The hand-written parser in `dirinfo` must match the Win32 layout.
const _: () = {
    use crate::dirinfo::offset as o;
    use std::mem::offset_of;
    use windows::Win32::Storage::FileSystem::FILE_ID_BOTH_DIR_INFO as I;
    assert!(offset_of!(I, NextEntryOffset) == o::NEXT);
    assert!(offset_of!(I, CreationTime) == o::CREATION);
    assert!(offset_of!(I, LastAccessTime) == o::ACCESS);
    assert!(offset_of!(I, LastWriteTime) == o::WRITE);
    assert!(offset_of!(I, EndOfFile) == o::SIZE);
    assert!(offset_of!(I, FileAttributes) == o::ATTRIBUTES);
    assert!(offset_of!(I, FileNameLength) == o::NAME_LEN);
    assert!(offset_of!(I, FileId) == o::FILE_ID);
    assert!(offset_of!(I, FileName) == o::NAME);
};

/// Lists the children of folder `dir` (full path) with their file ids, calling
/// `f(name, meta, file_id)` for each (`GetFileInformationByHandleEx`,
/// `FileIdBothDirectoryInfo`, 64 KiB per call).
pub fn list_dir_ids(dir: &str, mut f: impl FnMut(&[u16], Meta, u64)) -> io::Result<()> {
    // `\\?\` lifts the MAX_PATH limit for drive paths.
    let path = if dir.starts_with("\\\\") { dir.to_string() } else { format!("\\\\?\\{dir}") };
    let path = wide(&path);
    // SAFETY: `path` is NUL-terminated and outlives the call; BACKUP_SEMANTICS opens a
    // directory handle; no security attributes or template.
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(to_io)?;
    let h = Handle(h);
    let mut buf = AlignedBuf::new(64 * 1024);
    let mut name = Vec::with_capacity(256);
    let mut class = FileIdBothDirectoryRestartInfo;
    loop {
        let cap = buf.len_bytes() as u32;
        // SAFETY: `buf` is an owned, 8-byte aligned buffer of `cap` bytes; `h` is an
        // open directory handle; the call is synchronous.
        let r = unsafe { GetFileInformationByHandleEx(h.0, class, buf.as_mut_ptr(), cap) };
        match r {
            Ok(()) => crate::dirinfo::for_each_entry(buf.bytes(cap as usize), &mut name, &mut f),
            Err(e) if win32_code(&e) == code::NO_MORE_FILES => return Ok(()),
            Err(e) => return Err(to_io(e)),
        }
        class = FileIdBothDirectoryInfo;
    }
}

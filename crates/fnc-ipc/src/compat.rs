//! Everything 1.4 IPC compatibility (SPEC ch.4 §3.2): the `WM_COPYDATA` query structures and
//! `EVERYTHING_WM_IPC` commands of the Everything SDK (`everything_ipc.h`), as pure byte
//! encoders/decoders so the Win32 window plumbing in the app stays trivial.
//!
//! All structures are `#pragma pack(1)`, little endian, `DWORD` = `u32`. `W` variants carry
//! UTF-16LE strings, `A` variants 8-bit strings (decoded as UTF-8 with Latin-1 fallback,
//! encoded as UTF-8; SPEC-GAP: Everything uses the ANSI code page).
//!
//! ```text
//! EVERYTHING_IPC_QUERYW  (COPYDATA dwData = 2 / A = 1)          20-byte header
//!   +0  DWORD reply_hwnd             window receiving the reply
//!   +4  DWORD reply_copydata_message dwData of the reply WM_COPYDATA
//!   +8  DWORD search_flags           MATCHCASE 1 | MATCHWHOLEWORD 2 | MATCHPATH 4 | REGEX 8 | MATCHACCENTS 0x10
//!   +12 DWORD offset
//!   +16 DWORD max_results            0xFFFFFFFF = all
//!   +20 WCHAR search_string[]        NUL terminated
//!
//! EVERYTHING_IPC_LISTW   (reply)                                 28-byte header
//!   +0  DWORD totfolders  +4 DWORD totfiles  +8 DWORD totitems
//!   +12 DWORD numfolders  +16 DWORD numfiles +20 DWORD numitems  +24 DWORD offset
//!   +28 EVERYTHING_IPC_ITEMW items[numitems]: { DWORD flags; DWORD filename_offset; DWORD path_offset }
//!       then NUL-terminated UTF-16 strings; offsets are bytes from the start of the list.
//!       flags: FOLDER 1, DRIVE/ROOT 2. path = parent folder ("" for roots).
//!
//! EVERYTHING_IPC_QUERY2  (COPYDATA dwData = 18 / A = 17)         28-byte header
//!   +0 reply_hwnd +4 reply_copydata_message +8 search_flags +12 offset +16 max_results
//!   +20 DWORD request_flags  (EVERYTHING_IPC_QUERY2_REQUEST_*)
//!   +24 DWORD sort_type      (EVERYTHING_IPC_SORT_*)
//!   +28 search string, NUL terminated
//!
//! EVERYTHING_IPC_LIST2   (reply)                                 20-byte header
//!   +0 DWORD totitems +4 DWORD numitems +8 DWORD offset +12 DWORD request_flags +16 DWORD sort_type
//!   +20 EVERYTHING_IPC_ITEM2 items[numitems]: { DWORD flags; DWORD data_offset }
//!       then per item, at data_offset (bytes from the start of the list), the requested
//!       fields in this order:
//!         NAME, PATH, FULL_PATH_AND_NAME, EXTENSION   DWORD len (chars, no NUL) + chars + NUL
//!         SIZE                                         LARGE_INTEGER (folders/unknown: -1)
//!         DATE_CREATED, DATE_MODIFIED, DATE_ACCESSED   FILETIME (unknown: 0xFFFFFFFFFFFFFFFF)
//!         ATTRIBUTES                                   DWORD
//!         FILE_LIST_FILE_NAME                          string
//!         RUN_COUNT                                    DWORD
//!         DATE_RUN, DATE_RECENTLY_CHANGED              FILETIME
//!         HIGHLIGHTED_NAME, _PATH, _FULL_PATH_AND_NAME string, `*` toggles, `**` = `*`
//!
//! Run history (COPYDATA, reply = SendMessage return value):
//!   GET_RUN_COUNT  W 20 / A 19: filename            → run count
//!   SET_RUN_COUNT  W 22 / A 21: DWORD count + filename → TRUE
//!   INC_RUN_COUNT  W 24 / A 23: filename            → new run count
//! COMMAND_LINE_UTF8 (0): DWORD show_command + UTF-8 command line → TRUE if handled
//! ```
//!
//! The hidden window class is `EVERYTHING_TASKBAR_NOTIFICATION` (default instance) or
//! `EVERYTHING_TASKBAR_NOTIFICATION_(<instance>)` ([`window_class`]).

use std::time::Duration;

use fnc_exec::SortSpec;

use crate::backend::{AdminCommand, IndexBackend, Peer, PeerKind, SearchKey};
use crate::proto::{Column, Mods, UNKNOWN};
use crate::rows::{ColumnSet, Row, entry_row};

/// `EVERYTHING_WM_IPC` (`WM_USER`).
pub const EVERYTHING_WM_IPC: u32 = 0x0400;
/// Window class of the Everything IPC window.
pub const EVERYTHING_IPC_WNDCLASS: &str = "EVERYTHING_TASKBAR_NOTIFICATION";
/// Class of an Everything search window (used by some clients to detect the UI).
pub const EVERYTHING_IPC_SEARCH_CLIENT_WNDCLASS: &str = "EVERYTHING";
/// Registered window message broadcast when Everything starts.
pub const EVERYTHING_IPC_CREATED: &str = "EVERYTHING_IPC_CREATED";

/// Version reported by default: Everything 1.4.1 (so clients enable `QUERY2`), with
/// Fennec's own version encoded in the build number (`major*10000 + minor*100 + patch`).
/// See ADR-031.
pub const DEFAULT_EVERYTHING_VERSION: [u32; 4] = [1, 4, 1, fennec_build()];

const fn parse_u32(s: &str) -> u32 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut n = 0u32;
    while i < b.len() {
        n = n * 10 + (b[i] - b'0') as u32;
        i += 1;
    }
    n
}

const fn fennec_build() -> u32 {
    parse_u32(env!("CARGO_PKG_VERSION_MAJOR")) * 10000
        + parse_u32(env!("CARGO_PKG_VERSION_MINOR")) * 100
        + parse_u32(env!("CARGO_PKG_VERSION_PATCH"))
}

/// `EVERYTHING_WM_IPC` command ids (`wParam`).
pub mod cmd {
    /// Major version.
    pub const GET_MAJOR_VERSION: usize = 0;
    /// Minor version.
    pub const GET_MINOR_VERSION: usize = 1;
    /// Revision.
    pub const GET_REVISION: usize = 2;
    /// Build number.
    pub const GET_BUILD_NUMBER: usize = 3;
    /// Exit; returns 1 if the program closes.
    pub const EXIT: usize = 4;
    /// Target machine (`TARGET_MACHINE_*`).
    pub const GET_TARGET_MACHINE: usize = 5;
    /// Restart (Everything 1.5).
    pub const RESTART: usize = 6;
    /// Delete start menu shortcuts.
    pub const DELETE_START_MENU_SHORTCUTS: usize = 100;
    /// Delete quick launch shortcut.
    pub const DELETE_QUICK_LAUNCH_SHORTCUT: usize = 101;
    /// Delete desktop shortcut.
    pub const DELETE_DESKTOP_SHORTCUT: usize = 102;
    /// Delete folder context menu.
    pub const DELETE_FOLDER_CONTEXT_MENU: usize = 103;
    /// Delete run on system startup.
    pub const DELETE_RUN_ON_SYSTEM_STARTUP: usize = 104;
    /// Delete URL protocol.
    pub const DELETE_URL_PROTOCOL: usize = 105;
    /// Create start menu shortcuts.
    pub const CREATE_START_MENU_SHORTCUTS: usize = 200;
    /// Create quick launch shortcut.
    pub const CREATE_QUICK_LAUNCH_SHORTCUT: usize = 201;
    /// Create desktop shortcut.
    pub const CREATE_DESKTOP_SHORTCUT: usize = 202;
    /// Create folder context menu.
    pub const CREATE_FOLDER_CONTEXT_MENU: usize = 203;
    /// Create run on system startup.
    pub const CREATE_RUN_ON_SYSTEM_STARTUP: usize = 204;
    /// Create URL protocol.
    pub const CREATE_URL_PROTOCOL: usize = 205;
    /// Start menu shortcuts installed?
    pub const IS_START_MENU_SHORTCUTS: usize = 300;
    /// Quick launch shortcut installed?
    pub const IS_QUICK_LAUNCH_SHORTCUT: usize = 301;
    /// Desktop shortcut installed?
    pub const IS_DESKTOP_SHORTCUT: usize = 302;
    /// Folder context menu installed?
    pub const IS_FOLDER_CONTEXT_MENU: usize = 303;
    /// Run on system startup?
    pub const IS_RUN_ON_SYSTEM_STARTUP: usize = 304;
    /// URL protocol installed?
    pub const IS_URL_PROTOCOL: usize = 305;
    /// Running as a service?
    pub const IS_SERVICE: usize = 306;
    /// Is NTFS drive `lParam` (0 = A:) indexed?
    pub const IS_NTFS_DRIVE_INDEXED: usize = 400;
    /// Database loaded?
    pub const IS_DB_LOADED: usize = 401;
    /// Database busy?
    pub const IS_DB_BUSY: usize = 402;
    /// Running as administrator?
    pub const IS_ADMIN: usize = 403;
    /// Settings in %APPDATA%?
    pub const IS_APPDATA: usize = 404;
    /// Rebuild the database.
    pub const REBUILD_DB: usize = 405;
    /// Rescan all folder indexes.
    pub const UPDATE_ALL_FOLDER_INDEXES: usize = 406;
    /// Save the database.
    pub const SAVE_DB: usize = 407;
    /// Save run history.
    pub const SAVE_RUN_HISTORY: usize = 408;
    /// Delete run history.
    pub const DELETE_RUN_HISTORY: usize = 409;
    /// Is sort `lParam` (`SORT_*`) fast?
    pub const IS_FAST_SORT: usize = 410;
    /// Is file info `lParam` (`FILE_INFO_*`) indexed?
    pub const IS_FILE_INFO_INDEXED: usize = 411;
    /// Rebuild when the database is ready (Everything 1.5).
    pub const QUEUE_REBUILD_DB: usize = 412;
    /// Save the database and wait (Everything 1.5).
    pub const SAVE_DB_NOW: usize = 413;
    /// Search window: match case?
    pub const IS_MATCH_CASE: usize = 500;
    /// Search window: match whole word?
    pub const IS_MATCH_WHOLE_WORD: usize = 501;
    /// Search window: match path?
    pub const IS_MATCH_PATH: usize = 502;
    /// Search window: match diacritics?
    pub const IS_MATCH_DIACRITICS: usize = 503;
    /// Search window: regex?
    pub const IS_REGEX: usize = 504;
}

/// `WM_COPYDATA` ids (`COPYDATASTRUCT::dwData`).
pub mod copydata {
    /// `EVERYTHING_IPC_COMMAND_LINE` (UTF-8).
    pub const COMMAND_LINE_UTF8: usize = 0;
    /// `EVERYTHING_IPC_QUERYA`.
    pub const QUERYA: usize = 1;
    /// `EVERYTHING_IPC_QUERYW`.
    pub const QUERYW: usize = 2;
    /// `EVERYTHING_IPC_QUERY2` (ANSI search).
    pub const QUERY2A: usize = 17;
    /// `EVERYTHING_IPC_QUERY2` (UTF-16 search).
    pub const QUERY2W: usize = 18;
    /// Get run count (ANSI file name).
    pub const GET_RUN_COUNTA: usize = 19;
    /// Get run count (UTF-16 file name).
    pub const GET_RUN_COUNTW: usize = 20;
    /// Set run count (ANSI).
    pub const SET_RUN_COUNTA: usize = 21;
    /// Set run count (UTF-16).
    pub const SET_RUN_COUNTW: usize = 22;
    /// Increment run count (ANSI).
    pub const INC_RUN_COUNTA: usize = 23;
    /// Increment run count (UTF-16).
    pub const INC_RUN_COUNTW: usize = 24;
}

/// Search flags.
pub mod flags {
    /// Match case.
    pub const MATCHCASE: u32 = 0x1;
    /// Match whole word.
    pub const MATCHWHOLEWORD: u32 = 0x2;
    /// Match path.
    pub const MATCHPATH: u32 = 0x4;
    /// Regex.
    pub const REGEX: u32 = 0x8;
    /// Match diacritics (`MATCHACCENTS`).
    pub const MATCHACCENTS: u32 = 0x10;
    /// Match prefix (1.5).
    pub const MATCHPREFIX: u32 = 0x20;
    /// Match suffix (1.5).
    pub const MATCHSUFFIX: u32 = 0x40;
    /// Ignore punctuation (1.5).
    pub const IGNOREPUNCTUATION: u32 = 0x80;
    /// Ignore white space (1.5).
    pub const IGNOREWHITESPACE: u32 = 0x100;
    /// Item flag: folder.
    pub const ITEM_FOLDER: u32 = 0x1;
    /// Item flag: drive / root.
    pub const ITEM_DRIVE: u32 = 0x2;
    /// `max_results` meaning "all".
    pub const ALLRESULTS: u32 = 0xFFFF_FFFF;
}

/// `EVERYTHING_IPC_QUERY2_REQUEST_*` bits.
pub mod request {
    /// Name.
    pub const NAME: u32 = 0x1;
    /// Path.
    pub const PATH: u32 = 0x2;
    /// Full path and name.
    pub const FULL_PATH_AND_NAME: u32 = 0x4;
    /// Extension.
    pub const EXTENSION: u32 = 0x8;
    /// Size.
    pub const SIZE: u32 = 0x10;
    /// Date created.
    pub const DATE_CREATED: u32 = 0x20;
    /// Date modified.
    pub const DATE_MODIFIED: u32 = 0x40;
    /// Date accessed.
    pub const DATE_ACCESSED: u32 = 0x80;
    /// Attributes.
    pub const ATTRIBUTES: u32 = 0x100;
    /// File list file name.
    pub const FILE_LIST_FILE_NAME: u32 = 0x200;
    /// Run count.
    pub const RUN_COUNT: u32 = 0x400;
    /// Date run.
    pub const DATE_RUN: u32 = 0x800;
    /// Date recently changed.
    pub const DATE_RECENTLY_CHANGED: u32 = 0x1000;
    /// Highlighted name.
    pub const HIGHLIGHTED_NAME: u32 = 0x2000;
    /// Highlighted path.
    pub const HIGHLIGHTED_PATH: u32 = 0x4000;
    /// Highlighted full path and name.
    pub const HIGHLIGHTED_FULL_PATH_AND_NAME: u32 = 0x8000;
    /// Every bit this implementation answers.
    pub const ALL: u32 = 0xFFFF;
}

/// `EVERYTHING_IPC_TARGET_MACHINE_*`.
pub mod target {
    /// x86.
    pub const X86: isize = 1;
    /// x64.
    pub const X64: isize = 2;
    /// ARM.
    pub const ARM: isize = 3;
    /// ARM64.
    pub const ARM64: isize = 4;
}

/// `EVERYTHING_IPC_FILE_INFO_*`.
pub mod file_info {
    /// File size.
    pub const FILE_SIZE: isize = 1;
    /// Folder size.
    pub const FOLDER_SIZE: isize = 2;
    /// Date created.
    pub const DATE_CREATED: isize = 3;
    /// Date modified.
    pub const DATE_MODIFIED: isize = 4;
    /// Date accessed.
    pub const DATE_ACCESSED: isize = 5;
    /// Attributes.
    pub const ATTRIBUTES: isize = 6;
}

/// `EVERYTHING_IPC_SORT_*` (1-based; odd = ascending, even = descending):
/// name, path, size, extension, type name, date created, date modified, attributes,
/// file list file name, run count, date recently changed, date accessed, date run.
pub const SORT_COLUMNS: [Column; 13] = [
    Column::Name,
    Column::Path,
    Column::Size,
    Column::Extension,
    Column::Type,
    Column::DateCreated,
    Column::DateModified,
    Column::Attributes,
    Column::FileListName,
    Column::RunCount,
    Column::DateRecentlyChanged,
    Column::DateAccessed,
    Column::DateRun,
];

/// Decodes `EVERYTHING_IPC_SORT_*` (`1..=26`); `None` for other values.
pub fn decode_sort(sort_type: u32) -> Option<(Column, bool)> {
    if !(1..=26).contains(&sort_type) {
        return None;
    }
    let i = ((sort_type - 1) / 2) as usize;
    Some((SORT_COLUMNS[i], sort_type % 2 == 1))
}

/// Encodes a sort as `EVERYTHING_IPC_SORT_*`.
pub fn encode_sort(column: Column, ascending: bool) -> u32 {
    let i = SORT_COLUMNS.iter().position(|&c| c == column).unwrap_or(0) as u32;
    i * 2 + if ascending { 1 } else { 2 }
}

/// Window class name for `instance` (empty = default).
pub fn window_class(instance: &str) -> String {
    if instance.is_empty() {
        EVERYTHING_IPC_WNDCLASS.to_string()
    } else {
        format!("{EVERYTHING_IPC_WNDCLASS}_({instance})")
    }
}

/// Search options from `search_flags`.
pub fn mods_from_flags(f: u32) -> Mods {
    Mods {
        case: f & flags::MATCHCASE != 0,
        whole_word: f & flags::MATCHWHOLEWORD != 0,
        path: f & flags::MATCHPATH != 0,
        regex: f & flags::REGEX != 0,
        diacritics: f & flags::MATCHACCENTS != 0,
        prefix: f & flags::MATCHPREFIX != 0,
        suffix: f & flags::MATCHSUFFIX != 0,
        ignore_punctuation: f & flags::IGNOREPUNCTUATION != 0,
        ignore_whitespace: f & flags::IGNOREWHITESPACE != 0,
    }
}

/// `search_flags` from options.
pub fn flags_from_mods(m: &Mods) -> u32 {
    let mut f = 0;
    for (on, bit) in [
        (m.case, flags::MATCHCASE),
        (m.whole_word, flags::MATCHWHOLEWORD),
        (m.path, flags::MATCHPATH),
        (m.regex, flags::REGEX),
        (m.diacritics, flags::MATCHACCENTS),
        (m.prefix, flags::MATCHPREFIX),
        (m.suffix, flags::MATCHSUFFIX),
        (m.ignore_punctuation, flags::IGNOREPUNCTUATION),
        (m.ignore_whitespace, flags::IGNOREWHITESPACE),
    ] {
        if on {
            f |= bit;
        }
    }
    f
}

/// String encoding of a request/reply.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Charset {
    /// UTF-16LE (`W`).
    Wide,
    /// 8-bit (`A`).
    Ansi,
}

/// A decoded compatibility request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyDataRequest {
    /// `EVERYTHING_IPC_QUERY` (version 1).
    Query {
        /// Strings of the request and reply.
        charset: Charset,
        /// Reply window.
        reply_hwnd: u32,
        /// Reply `dwData`.
        reply_id: u32,
        /// Search flags.
        search_flags: u32,
        /// Offset.
        offset: u32,
        /// Max results.
        max_results: u32,
        /// Search.
        search: String,
    },
    /// `EVERYTHING_IPC_QUERY2`.
    Query2 {
        /// Strings of the request and reply.
        charset: Charset,
        /// Reply window.
        reply_hwnd: u32,
        /// Reply `dwData`.
        reply_id: u32,
        /// Search flags.
        search_flags: u32,
        /// Offset.
        offset: u32,
        /// Max results.
        max_results: u32,
        /// Request flags.
        request_flags: u32,
        /// Sort type.
        sort_type: u32,
        /// Search.
        search: String,
    },
    /// Get run count.
    GetRunCount {
        /// File name.
        path: String,
    },
    /// Set run count.
    SetRunCount {
        /// File name.
        path: String,
        /// Count.
        count: u32,
    },
    /// Increment run count.
    IncRunCount {
        /// File name.
        path: String,
    },
    /// Command line.
    CommandLine {
        /// `SW_*`.
        show_command: u32,
        /// Text.
        text: String,
    },
}

/// Decoding failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompatError {
    /// Unknown `dwData`.
    #[error("unsupported WM_COPYDATA id {0}")]
    Unsupported(usize),
    /// Data shorter than the fixed header.
    #[error("WM_COPYDATA data too short")]
    Truncated,
    /// Search or path too long.
    #[error("string too long")]
    TooLong,
}

/// Longest accepted search/path (UTF-16 units or bytes).
pub const MAX_STRING: usize = 32 * 1024;

fn dword(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn wide_string(b: &[u8]) -> Result<String, CompatError> {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
    if units.len() > MAX_STRING {
        return Err(CompatError::TooLong);
    }
    Ok(String::from_utf16_lossy(&units))
}

fn ansi_string(b: &[u8]) -> Result<String, CompatError> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    if end > MAX_STRING {
        return Err(CompatError::TooLong);
    }
    let bytes = &b[..end];
    Ok(match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&c| char::from(c)).collect(),
    })
}

fn string(b: &[u8], cs: Charset) -> Result<String, CompatError> {
    match cs {
        Charset::Wide => wide_string(b),
        Charset::Ansi => ansi_string(b),
    }
}

/// Decodes a `WM_COPYDATA` payload (`dw_data` = `COPYDATASTRUCT::dwData`, `bytes` = the
/// `cbData` bytes at `lpData`). Copy the bytes before returning from the window procedure.
pub fn decode_copydata(dw_data: usize, bytes: &[u8]) -> Result<CopyDataRequest, CompatError> {
    let d = |at| dword(bytes, at).ok_or(CompatError::Truncated);
    match dw_data {
        copydata::QUERYW | copydata::QUERYA => {
            let charset = if dw_data == copydata::QUERYW { Charset::Wide } else { Charset::Ansi };
            Ok(CopyDataRequest::Query {
                charset,
                reply_hwnd: d(0)?,
                reply_id: d(4)?,
                search_flags: d(8)?,
                offset: d(12)?,
                max_results: d(16)?,
                search: string(bytes.get(20..).unwrap_or(&[]), charset)?,
            })
        }
        copydata::QUERY2W | copydata::QUERY2A => {
            let charset = if dw_data == copydata::QUERY2W { Charset::Wide } else { Charset::Ansi };
            Ok(CopyDataRequest::Query2 {
                charset,
                reply_hwnd: d(0)?,
                reply_id: d(4)?,
                search_flags: d(8)?,
                offset: d(12)?,
                max_results: d(16)?,
                request_flags: d(20)?,
                sort_type: d(24)?,
                search: string(bytes.get(28..).unwrap_or(&[]), charset)?,
            })
        }
        copydata::GET_RUN_COUNTW | copydata::GET_RUN_COUNTA => {
            let cs = if dw_data == copydata::GET_RUN_COUNTW { Charset::Wide } else { Charset::Ansi };
            Ok(CopyDataRequest::GetRunCount { path: string(bytes, cs)? })
        }
        copydata::INC_RUN_COUNTW | copydata::INC_RUN_COUNTA => {
            let cs = if dw_data == copydata::INC_RUN_COUNTW { Charset::Wide } else { Charset::Ansi };
            Ok(CopyDataRequest::IncRunCount { path: string(bytes, cs)? })
        }
        copydata::SET_RUN_COUNTW | copydata::SET_RUN_COUNTA => {
            let cs = if dw_data == copydata::SET_RUN_COUNTW { Charset::Wide } else { Charset::Ansi };
            Ok(CopyDataRequest::SetRunCount { count: d(0)?, path: string(bytes.get(4..).unwrap_or(&[]), cs)? })
        }
        copydata::COMMAND_LINE_UTF8 => {
            Ok(CopyDataRequest::CommandLine { show_command: d(0)?, text: ansi_string(bytes.get(4..).unwrap_or(&[]))? })
        }
        other => Err(CompatError::Unsupported(other)),
    }
}

/// A `WM_COPYDATA` the app must send back: `SendMessage(hwnd, WM_COPYDATA, our_hwnd,
/// &COPYDATASTRUCT { dwData: copydata_id, cbData: bytes.len(), lpData: bytes })`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyDataReply {
    /// Reply window (`HWND`, from the request's 32-bit `reply_hwnd`).
    pub hwnd: isize,
    /// `dwData` of the reply.
    pub copydata_id: usize,
    /// `lpData` bytes.
    pub bytes: Vec<u8>,
}

/// Result of a `WM_COPYDATA`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyDataOutcome {
    /// Return value of the window procedure (`TRUE`/`FALSE`, or the run count).
    pub result: isize,
    /// Reply to send (queries only).
    pub reply: Option<CopyDataReply>,
}

struct StrWriter {
    cs: Charset,
}

impl StrWriter {
    fn len(&self, s: &str) -> u32 {
        match self.cs {
            Charset::Wide => s.encode_utf16().count() as u32,
            Charset::Ansi => s.len() as u32,
        }
    }

    fn put(&self, out: &mut Vec<u8>, s: &str) {
        match self.cs {
            Charset::Wide => {
                for u in s.encode_utf16() {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                out.extend_from_slice(&[0, 0]);
            }
            Charset::Ansi => {
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
        }
    }

    fn put_counted(&self, out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&self.len(s).to_le_bytes());
        self.put(out, s);
    }
}

fn item_flags(r: &Row) -> u32 {
    let mut f = 0;
    if r.is_folder() {
        f |= flags::ITEM_FOLDER;
    }
    if r.flags & crate::proto::FLAG_ROOT != 0 {
        f |= flags::ITEM_DRIVE;
    }
    f
}

fn set_dword(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Encodes `EVERYTHING_IPC_LIST` (v1). `rows` are the returned items (name and path
/// needed); the totals describe the whole result set.
pub fn encode_list(cs: Charset, tot_folders: u32, tot_files: u32, offset: u32, rows: &[Row]) -> Vec<u8> {
    let w = StrWriter { cs };
    let n = rows.len();
    let header = 28 + 12 * n;
    let mut out = vec![0u8; header];
    let num_folders = rows.iter().filter(|r| r.is_folder()).count() as u32;
    set_dword(&mut out, 0, tot_folders);
    set_dword(&mut out, 4, tot_files);
    set_dword(&mut out, 8, tot_folders.saturating_add(tot_files));
    set_dword(&mut out, 12, num_folders);
    set_dword(&mut out, 16, n as u32 - num_folders);
    set_dword(&mut out, 20, n as u32);
    set_dword(&mut out, 24, offset);
    for (i, r) in rows.iter().enumerate() {
        let item = 28 + 12 * i;
        set_dword(&mut out, item, item_flags(r));
        let name_off = out.len() as u32;
        w.put(&mut out, &r.name);
        let path_off = out.len() as u32;
        w.put(&mut out, &r.path);
        set_dword(&mut out, item + 4, name_off);
        set_dword(&mut out, item + 8, path_off);
    }
    out
}

/// Columns needed for `request_flags`.
pub fn request_columns(request_flags: u32) -> (ColumnSet, bool) {
    let mut c = ColumnSet::EMPTY;
    let pairs = [
        (request::NAME | request::HIGHLIGHTED_NAME, Column::Name),
        (request::PATH | request::HIGHLIGHTED_PATH, Column::Path),
        (request::FULL_PATH_AND_NAME | request::HIGHLIGHTED_FULL_PATH_AND_NAME, Column::FullPath),
        (request::EXTENSION, Column::Extension),
        (request::SIZE, Column::Size),
        (request::DATE_CREATED, Column::DateCreated),
        (request::DATE_MODIFIED, Column::DateModified),
        (request::DATE_ACCESSED, Column::DateAccessed),
        (request::ATTRIBUTES, Column::Attributes),
        (request::FILE_LIST_FILE_NAME, Column::FileListName),
        (request::RUN_COUNT, Column::RunCount),
        (request::DATE_RUN, Column::DateRun),
        (request::DATE_RECENTLY_CHANGED, Column::DateRecentlyChanged),
    ];
    for (bits, col) in pairs {
        if request_flags & bits != 0 {
            c.insert(col);
        }
    }
    let hl = request_flags
        & (request::HIGHLIGHTED_NAME | request::HIGHLIGHTED_PATH | request::HIGHLIGHTED_FULL_PATH_AND_NAME)
        != 0;
    (c, hl)
}

/// Encodes `EVERYTHING_IPC_LIST2`.
pub fn encode_list2(cs: Charset, total: u32, offset: u32, request_flags: u32, sort_type: u32, rows: &[Row]) -> Vec<u8> {
    let w = StrWriter { cs };
    let rf = request_flags & request::ALL;
    let n = rows.len();
    let mut out = vec![0u8; 20 + 8 * n];
    set_dword(&mut out, 0, total);
    set_dword(&mut out, 4, n as u32);
    set_dword(&mut out, 8, offset);
    set_dword(&mut out, 12, rf);
    set_dword(&mut out, 16, sort_type);
    let time = |out: &mut Vec<u8>, t: u64| out.extend_from_slice(&t.to_le_bytes());
    for (i, r) in rows.iter().enumerate() {
        let item = 20 + 8 * i;
        set_dword(&mut out, item, item_flags(r));
        let data_off = out.len() as u32;
        set_dword(&mut out, item + 4, data_off);
        if rf & request::NAME != 0 {
            w.put_counted(&mut out, &r.name);
        }
        if rf & request::PATH != 0 {
            w.put_counted(&mut out, &r.path);
        }
        if rf & request::FULL_PATH_AND_NAME != 0 {
            w.put_counted(&mut out, &r.full_path);
        }
        if rf & request::EXTENSION != 0 {
            w.put_counted(&mut out, &r.extension);
        }
        if rf & request::SIZE != 0 {
            // LARGE_INTEGER; -1 for folders and unknown sizes.
            out.extend_from_slice(&r.size.to_le_bytes());
        }
        if rf & request::DATE_CREATED != 0 {
            time(&mut out, r.date_created);
        }
        if rf & request::DATE_MODIFIED != 0 {
            time(&mut out, r.date_modified);
        }
        if rf & request::DATE_ACCESSED != 0 {
            time(&mut out, r.date_accessed);
        }
        if rf & request::ATTRIBUTES != 0 {
            let a = if r.attributes == u32::MAX { 0 } else { r.attributes };
            out.extend_from_slice(&a.to_le_bytes());
        }
        if rf & request::FILE_LIST_FILE_NAME != 0 {
            w.put_counted(&mut out, &r.file_list_name);
        }
        if rf & request::RUN_COUNT != 0 {
            out.extend_from_slice(&r.run_count.to_le_bytes());
        }
        if rf & request::DATE_RUN != 0 {
            time(&mut out, r.date_run);
        }
        if rf & request::DATE_RECENTLY_CHANGED != 0 {
            time(&mut out, r.date_recently_changed);
        }
        if rf & request::HIGHLIGHTED_NAME != 0 {
            w.put_counted(&mut out, r.highlighted_name.as_deref().unwrap_or(&r.name));
        }
        if rf & request::HIGHLIGHTED_PATH != 0 {
            w.put_counted(&mut out, r.highlighted_path.as_deref().unwrap_or(&r.path));
        }
        if rf & request::HIGHLIGHTED_FULL_PATH_AND_NAME != 0 {
            w.put_counted(&mut out, r.highlighted_full_path.as_deref().unwrap_or(&r.full_path));
        }
    }
    out
}

const COMPAT_PEER: Peer = Peer { kind: PeerKind::CopyData, pid: None };

/// Time limit of compatibility queries (clients wait synchronously).
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Executes a decoded request. Queries may take a while: call this on a worker thread
/// and send the reply from there (`SendMessageTimeoutW` works across threads).
pub fn execute_copydata(req: &CopyDataRequest, backend: &IndexBackend) -> CopyDataOutcome {
    let none = |result: isize| CopyDataOutcome { result, reply: None };
    match req {
        CopyDataRequest::Query { charset, reply_hwnd, reply_id, search_flags, offset, max_results, search } => {
            let key =
                SearchKey { search: search.clone(), mods: mods_from_flags(*search_flags), sort: SortSpec::default() };
            let Ok(rs) = backend.search(&key, QUERY_TIMEOUT) else { return none(0) };
            let snap = backend.index().snapshot();
            let valid = rs.instance == snap.instance();
            let tot_folders = if valid { rs.ids.iter().filter(|&&id| snap.is_dir(id)).count() as u32 } else { 0 };
            let tot_files = rs.ids.len() as u32 - tot_folders;
            let limit = u64::from(*max_results).min(backend.config().max_results) as usize;
            let start = (*offset as usize).min(rs.ids.len());
            let end = start.saturating_add(limit).min(rs.ids.len());
            let cols = ColumnSet::of(&[Column::Name, Column::Path]);
            let rows: Vec<Row> = rs.ids[start..end]
                .iter()
                .map(|&id| {
                    if valid && snap.is_live(id) {
                        entry_row(&snap, id, cols, backend.caches(), None)
                    } else {
                        Row::default()
                    }
                })
                .collect();
            drop(snap);
            let bytes = encode_list(*charset, tot_folders, tot_files, start as u32, &rows);
            CopyDataOutcome {
                result: 1,
                reply: Some(CopyDataReply { hwnd: *reply_hwnd as isize, copydata_id: *reply_id as usize, bytes }),
            }
        }
        CopyDataRequest::Query2 {
            charset,
            reply_hwnd,
            reply_id,
            search_flags,
            offset,
            max_results,
            request_flags,
            sort_type,
            search,
        } => {
            let (col, asc) = decode_sort(*sort_type).unwrap_or((Column::Name, true));
            let sort = crate::backend::sort_spec(&[(col, asc)], backend.host().folders_first());
            let key = SearchKey { search: search.clone(), mods: mods_from_flags(*search_flags), sort };
            let Ok(rs) = backend.search(&key, QUERY_TIMEOUT) else { return none(0) };
            let (cols, hl) = request_columns(*request_flags);
            let limit = u64::from(*max_results).min(backend.config().max_results);
            let page = backend.page(&rs, 0, u64::from(*offset), limit, &cols_vec(cols), hl);
            let rows: Vec<Row> = (0..page.len()).map(|i| page.row(i)).collect();
            let bytes = encode_list2(
                *charset,
                page.total.min(u64::from(u32::MAX)) as u32,
                page.offset as u32,
                *request_flags,
                encode_sort(col, asc),
                &rows,
            );
            CopyDataOutcome {
                result: 1,
                reply: Some(CopyDataReply { hwnd: *reply_hwnd as isize, copydata_id: *reply_id as usize, bytes }),
            }
        }
        CopyDataRequest::GetRunCount { path } => none(backend.run_count(path).map_or(0, |n| n as isize)),
        CopyDataRequest::IncRunCount { path } => none(backend.inc_run_count(path).map_or(0, |n| n as isize)),
        CopyDataRequest::SetRunCount { path, count } => none(isize::from(backend.set_run_count(path, *count).is_ok())),
        CopyDataRequest::CommandLine { show_command, text } => {
            none(isize::from(backend.host().command_line(*show_command, text)))
        }
    }
}

fn cols_vec(c: ColumnSet) -> Vec<Column> {
    Column::ALL.into_iter().filter(|&x| c.contains(x)).collect()
}

/// Decodes and executes a `WM_COPYDATA` in one step (convenience; blocks for the query).
/// `None` if the id is unknown or the data is malformed (return `FALSE`).
pub fn handle_copydata(dw_data: usize, bytes: &[u8], backend: &IndexBackend) -> Option<CopyDataOutcome> {
    match decode_copydata(dw_data, bytes) {
        Ok(req) => Some(execute_copydata(&req, backend)),
        Err(e) => {
            tracing::debug!(error = %e, "ignored WM_COPYDATA");
            None
        }
    }
}

fn target_machine() -> isize {
    if cfg!(target_arch = "x86_64") {
        target::X64
    } else if cfg!(target_arch = "aarch64") {
        target::ARM64
    } else if cfg!(target_arch = "x86") {
        target::X86
    } else if cfg!(target_arch = "arm") {
        target::ARM
    } else {
        0
    }
}

fn admin(backend: &IndexBackend, c: AdminCommand) -> isize {
    isize::from(backend.admin(c, &COMPAT_PEER).is_ok())
}

/// Answers `EVERYTHING_WM_IPC` (`wparam` = command, `lparam` = argument). Unknown
/// commands return 0. Admin commands are forwarded to [`crate::Host::command`].
pub fn handle_wm_ipc(wparam: usize, lparam: isize, backend: &IndexBackend) -> isize {
    let host = backend.host();
    let v = host.everything_version();
    match wparam {
        cmd::GET_MAJOR_VERSION => v[0] as isize,
        cmd::GET_MINOR_VERSION => v[1] as isize,
        cmd::GET_REVISION => v[2] as isize,
        cmd::GET_BUILD_NUMBER => v[3] as isize,
        cmd::EXIT => admin(backend, AdminCommand::Exit),
        cmd::GET_TARGET_MACHINE => target_machine(),
        cmd::IS_DB_LOADED => isize::from(host.is_db_loaded()),
        cmd::IS_DB_BUSY => isize::from(host.is_busy()),
        cmd::IS_ADMIN => isize::from(host.is_admin()),
        cmd::IS_APPDATA => isize::from(host.is_appdata()),
        cmd::REBUILD_DB | cmd::QUEUE_REBUILD_DB => admin(backend, AdminCommand::Rebuild),
        cmd::UPDATE_ALL_FOLDER_INDEXES => admin(backend, AdminCommand::Update),
        cmd::SAVE_DB | cmd::SAVE_DB_NOW => admin(backend, AdminCommand::SaveDb),
        cmd::SAVE_RUN_HISTORY => admin(backend, AdminCommand::SaveRunHistory),
        cmd::DELETE_RUN_HISTORY => admin(backend, AdminCommand::DeleteRunHistory),
        // Every sort runs on in-memory columns (≤ 80 ms for 1 M results, ADR-012); report
        // all valid sorts as fast so clients do not sort themselves.
        cmd::IS_FAST_SORT => isize::from(u32::try_from(lparam).ok().and_then(decode_sort).is_some()),
        cmd::IS_FILE_INFO_INDEXED => match lparam {
            file_info::FILE_SIZE | file_info::DATE_CREATED | file_info::DATE_MODIFIED | file_info::ATTRIBUTES => 1,
            file_info::DATE_ACCESSED => isize::from(date_accessed_indexed(backend)),
            _ => 0,
        },
        cmd::IS_NTFS_DRIVE_INDEXED => {
            let Ok(i) = u8::try_from(lparam) else { return 0 };
            if i >= 26 {
                return 0;
            }
            let letter = char::from(b'A' + i);
            let snap = backend.index().snapshot();
            isize::from(snap.volumes().iter().any(|v| {
                !v.is_retired()
                    && v.info.kind == fnc_index::VolumeKind::Ntfs
                    && v.info.root_path.chars().next().is_some_and(|c| c.eq_ignore_ascii_case(&letter))
            }))
        }
        _ => 0,
    }
}

fn date_accessed_indexed(backend: &IndexBackend) -> bool {
    let snap = backend.index().snapshot();
    (0..snap.len().min(4096) as u32)
        .map(fnc_core::EntryId)
        .any(|id| snap.is_live(id) && snap.date_accessed(id).is_known())
}

/// A decoded `EVERYTHING_IPC_LIST2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List2 {
    /// `totitems`.
    pub total: u32,
    /// `offset`.
    pub offset: u32,
    /// `request_flags`.
    pub request_flags: u32,
    /// `sort_type`.
    pub sort_type: u32,
    /// `(item flags, values)` per item.
    pub items: Vec<(u32, Row)>,
}

/// Reads `EVERYTHING_IPC_LIST2` back (tests and diagnostic tools).
pub fn decode_list2(cs: Charset, b: &[u8]) -> Option<List2> {
    let total = dword(b, 0)?;
    let n = dword(b, 4)? as usize;
    let offset = dword(b, 8)?;
    let rf = dword(b, 12)?;
    let sort = dword(b, 16)?;
    let mut items = Vec::with_capacity(n.min(1 << 16));
    for i in 0..n {
        let fl = dword(b, 20 + 8 * i)?;
        let mut p = dword(b, 24 + 8 * i)? as usize;
        let mut r = Row::default();
        let text = |p: &mut usize| -> Option<String> {
            let len = dword(b, *p)? as usize;
            *p += 4;
            let unit = if cs == Charset::Wide { 2 } else { 1 };
            let s = string(b.get(*p..*p + len * unit)?, cs).ok()?;
            *p += (len + 1) * unit;
            Some(s)
        };
        let q = |p: &mut usize| -> Option<u64> {
            let v = u64::from_le_bytes(b.get(*p..*p + 8)?.try_into().ok()?);
            *p += 8;
            Some(v)
        };
        if rf & request::NAME != 0 {
            r.name = text(&mut p)?;
        }
        if rf & request::PATH != 0 {
            r.path = text(&mut p)?;
        }
        if rf & request::FULL_PATH_AND_NAME != 0 {
            r.full_path = text(&mut p)?;
        }
        if rf & request::EXTENSION != 0 {
            r.extension = text(&mut p)?;
        }
        if rf & request::SIZE != 0 {
            r.size = q(&mut p)?;
        }
        if rf & request::DATE_CREATED != 0 {
            r.date_created = q(&mut p)?;
        }
        if rf & request::DATE_MODIFIED != 0 {
            r.date_modified = q(&mut p)?;
        }
        if rf & request::DATE_ACCESSED != 0 {
            r.date_accessed = q(&mut p)?;
        }
        if rf & request::ATTRIBUTES != 0 {
            r.attributes = dword(b, p)?;
            p += 4;
        }
        if rf & request::FILE_LIST_FILE_NAME != 0 {
            r.file_list_name = text(&mut p)?;
        }
        if rf & request::RUN_COUNT != 0 {
            r.run_count = dword(b, p)?;
            p += 4;
        }
        if rf & request::DATE_RUN != 0 {
            r.date_run = q(&mut p)?;
        }
        if rf & request::DATE_RECENTLY_CHANGED != 0 {
            r.date_recently_changed = q(&mut p)?;
        }
        if rf & request::HIGHLIGHTED_NAME != 0 {
            r.highlighted_name = Some(text(&mut p)?);
        }
        if rf & request::HIGHLIGHTED_PATH != 0 {
            r.highlighted_path = Some(text(&mut p)?);
        }
        if rf & request::HIGHLIGHTED_FULL_PATH_AND_NAME != 0 {
            r.highlighted_full_path = Some(text(&mut p)?);
        }
        items.push((fl, r));
    }
    Some(List2 { total, offset, request_flags: rf, sort_type: sort, items })
}

/// Encodes an `EVERYTHING_IPC_QUERY2` request with a UTF-16 search (tests, SDK-style
/// clients); the arguments mirror the C structure fields.
#[allow(clippy::too_many_arguments)]
pub fn encode_query2(
    reply_hwnd: u32,
    reply_id: u32,
    mods: &Mods,
    offset: u32,
    max: u32,
    request_flags: u32,
    sort_type: u32,
    search: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(30 + search.len() * 2);
    for v in [reply_hwnd, reply_id, flags_from_mods(mods), offset, max, request_flags, sort_type] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for u in search.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Encodes an `EVERYTHING_IPC_QUERYW` (v1) request.
pub fn encode_query1(reply_hwnd: u32, reply_id: u32, mods: &Mods, offset: u32, max: u32, search: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + search.len() * 2);
    for v in [reply_hwnd, reply_id, flags_from_mods(mods), offset, max] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for u in search.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// `UNKNOWN` re-exported for byte-level callers (sizes/dates of `-1`).
pub const UNKNOWN_VALUE: u64 = UNKNOWN;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::backend::DefaultHost;

    fn backend() -> IndexBackend {
        IndexBackend::new(crate::backend::tests::fixture(), Arc::new(DefaultHost))
    }

    fn w(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
        v.extend_from_slice(&[0, 0]);
        v
    }

    #[test]
    fn sdk_constants() {
        assert_eq!(EVERYTHING_WM_IPC, 0x400);
        assert_eq!(cmd::IS_DB_LOADED, 401);
        assert_eq!(cmd::IS_FILE_INFO_INDEXED, 411);
        assert_eq!(copydata::QUERY2W, 18);
        assert_eq!(copydata::INC_RUN_COUNTW, 24);
        assert_eq!(request::HIGHLIGHTED_FULL_PATH_AND_NAME, 0x8000);
        assert_eq!(decode_sort(1), Some((Column::Name, true)));
        assert_eq!(decode_sort(6), Some((Column::Size, false)));
        assert_eq!(decode_sort(14), Some((Column::DateModified, false)));
        assert_eq!(decode_sort(26), Some((Column::DateRun, false)));
        assert_eq!(decode_sort(0), None);
        assert_eq!(decode_sort(27), None);
        for t in 1..=26 {
            let (c, a) = decode_sort(t).unwrap();
            assert_eq!(encode_sort(c, a), t);
        }
        assert_eq!(window_class(""), "EVERYTHING_TASKBAR_NOTIFICATION");
        assert_eq!(window_class("x"), "EVERYTHING_TASKBAR_NOTIFICATION_(x)");
        assert_eq!(DEFAULT_EVERYTHING_VERSION[..3], [1, 4, 1]);
    }

    #[test]
    fn decodes_query_structures() {
        let q1 = encode_query1(0x1234, 7, &Mods { case: true, ..Mods::default() }, 3, 10, "abc");
        assert_eq!(q1.len(), 20 + 8);
        assert_eq!(
            decode_copydata(copydata::QUERYW, &q1).unwrap(),
            CopyDataRequest::Query {
                charset: Charset::Wide,
                reply_hwnd: 0x1234,
                reply_id: 7,
                search_flags: 1,
                offset: 3,
                max_results: 10,
                search: "abc".into()
            }
        );
        let q2 = encode_query2(
            1,
            2,
            &Mods { path: true, regex: true, ..Mods::default() },
            0,
            flags::ALLRESULTS,
            0x55,
            6,
            "中文",
        );
        assert_eq!(q2.len(), 28 + 6);
        match decode_copydata(copydata::QUERY2W, &q2).unwrap() {
            CopyDataRequest::Query2 { search_flags, request_flags, sort_type, search, max_results, .. } => {
                assert_eq!(search_flags, flags::MATCHPATH | flags::REGEX);
                assert_eq!(request_flags, 0x55);
                assert_eq!(sort_type, 6);
                assert_eq!(max_results, u32::MAX);
                assert_eq!(search, "中文");
            }
            other => panic!("{other:?}"),
        }
        // ANSI variant and missing terminator.
        let mut a = encode_query1(1, 2, &Mods::default(), 0, 5, "");
        a.truncate(20);
        a.extend_from_slice(b"xyz");
        assert!(
            matches!(decode_copydata(copydata::QUERYA, &a).unwrap(), CopyDataRequest::Query { search, .. } if search == "xyz")
        );
        assert_eq!(decode_copydata(copydata::QUERYW, &[0; 10]), Err(CompatError::Truncated));
        assert_eq!(decode_copydata(99, &[]), Err(CompatError::Unsupported(99)));
        let mut set = 5u32.to_le_bytes().to_vec();
        set.extend(w("C:\\a"));
        assert_eq!(
            decode_copydata(copydata::SET_RUN_COUNTW, &set).unwrap(),
            CopyDataRequest::SetRunCount { path: "C:\\a".into(), count: 5 }
        );
    }

    #[test]
    fn list1_layout() {
        let rows = vec![
            Row { flags: 1, name: "dir".into(), path: "C:".into(), ..Row::default() },
            Row { name: "a.txt".into(), path: "C:\\dir".into(), ..Row::default() },
        ];
        let b = encode_list(Charset::Wide, 4, 6, 2, &rows);
        let d = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        assert_eq!([d(0), d(4), d(8), d(12), d(16), d(20), d(24)], [4, 6, 10, 1, 1, 2, 2]);
        // items at 28, strings after 28 + 24 = 52.
        assert_eq!(d(28), 1);
        assert_eq!(d(32), 52);
        let name_at = d(32) as usize;
        assert_eq!(&b[name_at..name_at + 8], &w("dir")[..]);
        let path_at = d(36) as usize;
        assert_eq!(path_at, 52 + 8);
        assert_eq!(&b[path_at..path_at + 6], &w("C:")[..]);
        assert_eq!(d(40), 0);
        assert_eq!(wide_string(&b[d(44) as usize..]).unwrap(), "a.txt");
        assert_eq!(wide_string(&b[d(48) as usize..]).unwrap(), "C:\\dir");
        assert_eq!(*b.last().unwrap(), 0);
    }

    #[test]
    fn list2_layout() {
        let rows = vec![Row {
            flags: 1 | 2,
            name: "C:".into(),
            full_path: "C:".into(),
            size: UNKNOWN,
            date_modified: 0x0102_0304_0506_0708,
            attributes: 16,
            run_count: 3,
            highlighted_name: Some("*C*:".into()),
            ..Row::default()
        }];
        let rf = request::NAME
            | request::SIZE
            | request::DATE_MODIFIED
            | request::ATTRIBUTES
            | request::RUN_COUNT
            | request::HIGHLIGHTED_NAME
            | 0x10000;
        let b = encode_list2(Charset::Wide, 9, 4, rf, 1, &rows);
        let d = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        assert_eq!([d(0), d(4), d(8), d(12), d(16)], [9, 1, 4, rf & 0xFFFF, 1]);
        assert_eq!(d(20), 3, "folder | drive");
        let p = d(24) as usize;
        assert_eq!(p, 28);
        // NAME: len 2, "C:", NUL
        assert_eq!(d(p), 2);
        assert_eq!(&b[p + 4..p + 10], &w("C:")[..]);
        let p = p + 10;
        assert_eq!(&b[p..p + 8], &[0xff; 8], "folder size -1");
        assert_eq!(u64::from_le_bytes(b[p + 8..p + 16].try_into().unwrap()), 0x0102_0304_0506_0708);
        assert_eq!(d(p + 16), 16);
        assert_eq!(d(p + 20), 3);
        assert_eq!(d(p + 24), 4, "highlighted name length");
        assert_eq!(wide_string(&b[p + 28..]).unwrap(), "*C*:");
        assert_eq!(b.len(), p + 28 + 10);
        let l = decode_list2(Charset::Wide, &b).unwrap();
        assert_eq!((l.total, l.offset, l.request_flags, l.sort_type), (9, 4, rf & 0xFFFF, 1));
        assert_eq!(l.items[0].1.name, "C:");
        assert_eq!(l.items[0].1.highlighted_name.as_deref(), Some("*C*:"));
    }

    #[test]
    fn copydata_queries_against_the_index() {
        let b = backend();
        let req = encode_query2(
            0x42,
            99,
            &Mods::default(),
            0,
            flags::ALLRESULTS,
            request::FULL_PATH_AND_NAME | request::SIZE,
            6,
            "*.iso",
        );
        let out = handle_copydata(copydata::QUERY2W, &req, &b).unwrap();
        assert_eq!(out.result, 1);
        let reply = out.reply.unwrap();
        assert_eq!((reply.hwnd, reply.copydata_id), (0x42, 99));
        let l = decode_list2(Charset::Wide, &reply.bytes).unwrap();
        assert_eq!((l.total, l.sort_type), (3, 6));
        let got: Vec<(String, u64)> = l.items.iter().map(|(_, r)| (r.full_path.clone(), r.size)).collect();
        assert_eq!(
            got,
            [
                ("C:\\Data\\big.iso".into(), 5000),
                ("C:\\Data\\mid.iso".into(), 2500),
                ("C:\\Data\\small.iso".into(), 100)
            ]
        );
        // es -n 1 -o 1
        let req = encode_query2(1, 2, &Mods::default(), 1, 1, request::NAME, 0, "iso");
        let reply = handle_copydata(copydata::QUERY2W, &req, &b).unwrap().reply.unwrap();
        let l = decode_list2(Charset::Wide, &reply.bytes).unwrap();
        assert_eq!((l.total, l.offset, l.sort_type, l.items.len()), (4, 1, 1, 1));
        // v1
        let req = encode_query1(5, 6, &Mods::default(), 0, flags::ALLRESULTS, "iso");
        let reply = handle_copydata(copydata::QUERYW, &req, &b).unwrap().reply.unwrap();
        let d = |at: usize| u32::from_le_bytes(reply.bytes[at..at + 4].try_into().unwrap());
        assert_eq!([d(0), d(4), d(8), d(20)], [1, 3, 4, 4], "the isos folder + 3 files");
        assert_eq!(d(28), 1, "folders first");
        // run counts
        let path = w("C:\\Data\\notes.txt");
        assert_eq!(handle_copydata(copydata::INC_RUN_COUNTW, &path, &b).unwrap().result, 1);
        assert_eq!(handle_copydata(copydata::INC_RUN_COUNTW, &path, &b).unwrap().result, 2);
        assert_eq!(handle_copydata(copydata::GET_RUN_COUNTW, &path, &b).unwrap().result, 2);
        assert!(handle_copydata(1234, &[], &b).is_none());
    }

    #[test]
    fn wm_ipc_commands() {
        let b = backend();
        assert_eq!(handle_wm_ipc(cmd::GET_MAJOR_VERSION, 0, &b), 1);
        assert_eq!(handle_wm_ipc(cmd::GET_MINOR_VERSION, 0, &b), 4);
        assert_eq!(handle_wm_ipc(cmd::GET_REVISION, 0, &b), 1);
        assert_eq!(handle_wm_ipc(cmd::IS_DB_LOADED, 0, &b), 1);
        assert_eq!(handle_wm_ipc(cmd::IS_FAST_SORT, 6, &b), 1);
        assert_eq!(handle_wm_ipc(cmd::IS_FAST_SORT, 99, &b), 0);
        assert_eq!(handle_wm_ipc(cmd::IS_FILE_INFO_INDEXED, file_info::FILE_SIZE, &b), 1);
        assert_eq!(handle_wm_ipc(cmd::IS_FILE_INFO_INDEXED, file_info::FOLDER_SIZE, &b), 0);
        assert_eq!(handle_wm_ipc(cmd::REBUILD_DB, 0, &b), 0, "default host does not rebuild");
        assert_eq!(handle_wm_ipc(cmd::IS_NTFS_DRIVE_INDEXED, 2, &b), 0, "file list, not NTFS");
        assert_eq!(handle_wm_ipc(cmd::IS_NTFS_DRIVE_INDEXED, -1, &b), 0);
        assert_eq!(handle_wm_ipc(9999, 0, &b), 0);
    }
}

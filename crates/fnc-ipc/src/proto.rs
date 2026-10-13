//! Native wire protocol (SPEC ch.4 §3.1).
//!
//! Transport: the local socket `Fennec.<instance>.ipc` (`\\.\pipe\Fennec.<instance>.ipc` on
//! Windows, an abstract/namespaced Unix socket elsewhere). Every message is one frame:
//!
//! ```text
//! frame    = len:u32 (little endian) | payload[len]
//! payload  = MessagePack (named fields, rmp-serde `to_vec_named`)
//! request  = Request  { id: u32, op: Op }
//! response = Response { id: u32, status: Status, message: str, payload: Payload }
//! ```
//!
//! Requests are limited to [`MAX_REQUEST_FRAME`] bytes, responses to [`MAX_RESPONSE_FRAME`].
//! Decoding is strict: unknown fields, unknown operations, trailing bytes, over-long strings,
//! NUL characters and too many columns/sort keys are rejected ([`Request::decode`]).
//!
//! Query results are **columnar** ([`ResultColumns`]): one array per requested column, all
//! of the same length. Sizes and dates use [`UNKNOWN`] (`u64::MAX`) for "not known", like the
//! Everything SDK; dates are `FILETIME` values (100 ns ticks since 1601-01-01 UTC).

use std::io::{self, Read, Write};

use fnc_exec::SortColumn;
use serde::{Deserialize, Serialize};

/// Protocol version (bumped on incompatible changes).
pub const PROTOCOL_VERSION: u32 = 1;
/// Largest accepted request frame.
pub const MAX_REQUEST_FRAME: usize = 1 << 20;
/// Largest response frame a server sends / a client accepts.
pub const MAX_RESPONSE_FRAME: usize = 256 << 20;
/// Longest accepted search text (bytes, UTF-8).
pub const MAX_SEARCH_LEN: usize = 64 * 1024;
/// Longest accepted path argument (bytes, UTF-8).
pub const MAX_PATH_LEN: usize = 32 * 1024;
/// Most columns per query.
pub const MAX_COLUMNS: usize = 32;
/// Most sort keys per query (primary + two tie breakers).
pub const MAX_SORT_KEYS: usize = 3;
/// Default (and maximum) number of results per request; larger result sets are paged
/// with [`Op::QueryContinue`].
pub const DEFAULT_MAX_RESULTS: u64 = 1_000_000;
/// "Unknown" marker for sizes and dates.
pub const UNKNOWN: u64 = u64::MAX;
/// Item flag: the result is a folder.
pub const FLAG_FOLDER: u8 = 1;
/// Item flag: the result is a volume/index root.
pub const FLAG_ROOT: u8 = 2;

/// Local socket name of the IPC server of `instance` (empty = default instance).
///
/// Path separators in the instance name are replaced so the name stays a single pipe
/// component.
pub fn pipe_name(instance: &str) -> String {
    let inst: String = instance.chars().map(|c| if matches!(c, '\\' | '/' | '\0') { '_' } else { c }).collect();
    format!("Fennec.{inst}.ipc")
}

/// A result column.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    /// File name.
    Name,
    /// Parent folder path.
    Path,
    /// Full path and name (es "Filename").
    FullPath,
    /// Extension (without dot; empty for folders).
    Extension,
    /// Type description (`Text Document`, `File folder`, ...).
    Type,
    /// Size in bytes ([`UNKNOWN`] for folders and unknown sizes).
    Size,
    /// Date modified (FILETIME).
    DateModified,
    /// Date created (FILETIME).
    DateCreated,
    /// Date accessed (FILETIME).
    DateAccessed,
    /// Win32 attributes (folders always carry `FILE_ATTRIBUTE_DIRECTORY`).
    Attributes,
    /// Run count.
    RunCount,
    /// Date last run (FILETIME).
    DateRun,
    /// Date recently changed (index journal, FILETIME).
    DateRecentlyChanged,
    /// File list (EFU) the result comes from; empty for other indexes.
    FileListName,
}

impl Column {
    /// Every column, in Everything's canonical order.
    pub const ALL: [Column; 14] = [
        Column::Name,
        Column::Path,
        Column::FullPath,
        Column::Extension,
        Column::Type,
        Column::Size,
        Column::DateModified,
        Column::DateCreated,
        Column::DateAccessed,
        Column::Attributes,
        Column::RunCount,
        Column::DateRun,
        Column::DateRecentlyChanged,
        Column::FileListName,
    ];

    /// Parses es/HTTP/SDK style names; dashes, underscores and case are ignored
    /// (`date-modified`, `date_modified`, `dm`, `full-path-and-name`, `filename`, ...).
    pub fn parse(s: &str) -> Option<Self> {
        let n: String = s.chars().filter(|c| !matches!(c, '-' | '_' | ' ')).collect::<String>().to_ascii_lowercase();
        Some(match n.as_str() {
            "name" => Column::Name,
            "path" | "pathcolumn" => Column::Path,
            "fullpath" | "fullpathandname" | "filename" | "filenamecolumn" | "pathandname" => Column::FullPath,
            "ext" | "extension" => Column::Extension,
            "type" | "typename" => Column::Type,
            "size" => Column::Size,
            "dm" | "datemodified" | "modified" => Column::DateModified,
            "dc" | "datecreated" | "created" => Column::DateCreated,
            "da" | "dateaccessed" | "accessed" => Column::DateAccessed,
            "attrib" | "attribs" | "attributes" => Column::Attributes,
            "runcount" => Column::RunCount,
            "dr" | "daterun" => Column::DateRun,
            "rc" | "daterecentlychanged" | "recentchange" => Column::DateRecentlyChanged,
            "filelistfilename" | "filelistname" | "filelist" => Column::FileListName,
            _ => return None,
        })
    }

    /// Header text (es / Everything export names).
    pub fn header(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Path => "Path",
            Column::FullPath => "Filename",
            Column::Extension => "Extension",
            Column::Type => "Type",
            Column::Size => "Size",
            Column::DateModified => "Date Modified",
            Column::DateCreated => "Date Created",
            Column::DateAccessed => "Date Accessed",
            Column::Attributes => "Attributes",
            Column::RunCount => "Run Count",
            Column::DateRun => "Date Run",
            Column::DateRecentlyChanged => "Date Recently Changed",
            Column::FileListName => "File List Name",
        }
    }

    /// JSON property name (the header lowercased, spaces → `_`, as es and Everything's
    /// HTTP server do).
    pub fn json_name(self) -> &'static str {
        match self {
            Column::Name => "name",
            Column::Path => "path",
            Column::FullPath => "filename",
            Column::Extension => "extension",
            Column::Type => "type",
            Column::Size => "size",
            Column::DateModified => "date_modified",
            Column::DateCreated => "date_created",
            Column::DateAccessed => "date_accessed",
            Column::Attributes => "attributes",
            Column::RunCount => "run_count",
            Column::DateRun => "date_run",
            Column::DateRecentlyChanged => "date_recently_changed",
            Column::FileListName => "file_list_name",
        }
    }

    /// Sort column used when sorting by this column.
    pub fn sort_column(self) -> SortColumn {
        match self {
            Column::Name => SortColumn::Name,
            Column::Path => SortColumn::Path,
            Column::FullPath => SortColumn::FullPath,
            Column::Extension | Column::Type => SortColumn::Extension,
            Column::Size => SortColumn::Size,
            Column::DateModified => SortColumn::DateModified,
            Column::DateCreated => SortColumn::DateCreated,
            Column::DateAccessed => SortColumn::DateAccessed,
            Column::Attributes => SortColumn::Attributes,
            Column::RunCount => SortColumn::RunCount,
            Column::DateRun => SortColumn::DateRun,
            Column::DateRecentlyChanged => SortColumn::DateRecentlyChanged,
            Column::FileListName => SortColumn::Volume,
        }
    }

    /// Default direction when a sort on this column gives none (es: sizes, dates and
    /// counts sort descending, text ascending).
    pub fn default_ascending(self) -> bool {
        !matches!(
            self,
            Column::Size
                | Column::DateModified
                | Column::DateCreated
                | Column::DateAccessed
                | Column::RunCount
                | Column::DateRun
                | Column::DateRecentlyChanged
        )
    }
}

/// Search options of a query (Everything's `search_flags`).
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(default, deny_unknown_fields)]
pub struct Mods {
    /// Match case.
    pub case: bool,
    /// Match whole words.
    #[serde(rename = "ww")]
    pub whole_word: bool,
    /// Match the full path.
    pub path: bool,
    /// The whole search is one regular expression.
    pub regex: bool,
    /// Match diacritics.
    pub diacritics: bool,
    /// Match prefix (Everything 1.5).
    pub prefix: bool,
    /// Match suffix (Everything 1.5).
    pub suffix: bool,
    /// Ignore punctuation (Everything 1.5).
    pub ignore_punctuation: bool,
    /// Ignore white space (Everything 1.5).
    pub ignore_whitespace: bool,
}

/// A search without paging (for counts and sizes).
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SearchSpec {
    /// Search text (Fennec/Everything syntax).
    pub search: String,
    /// Options.
    pub mods: Mods,
}

/// A query.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    /// Search text.
    pub search: String,
    /// Options.
    pub mods: Mods,
    /// Sort keys `(column, ascending)`, primary first; empty = name ascending.
    pub sort: Vec<(Column, bool)>,
    /// First result to return.
    pub offset: u64,
    /// Most results to return (capped at the server's limit, default
    /// [`DEFAULT_MAX_RESULTS`]); `u64::MAX` = as many as allowed.
    pub limit: u64,
    /// Columns to return (empty = full path only).
    pub columns: Vec<Column>,
    /// Also return `*`-highlighted name/path/full path for the requested text columns.
    pub highlight: bool,
    /// Server-side time limit in milliseconds (0 = server default).
    pub timeout_ms: u32,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            search: String::new(),
            mods: Mods::default(),
            sort: Vec::new(),
            offset: 0,
            limit: u64::MAX,
            columns: vec![Column::FullPath],
            highlight: false,
            timeout_ms: 0,
        }
    }
}

/// An operation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum Op {
    /// Runs a query and returns one page of results.
    Query(Query),
    /// Returns another page of a previous query's result set.
    QueryContinue {
        /// Id from [`Results::query_id`].
        query_id: u64,
        /// First result.
        offset: u64,
        /// Most results.
        limit: u64,
    },
    /// Frees a cached result set.
    ReleaseQuery {
        /// Id from [`Results::query_id`].
        query_id: u64,
    },
    /// Number of results of a search.
    GetResultCount(SearchSpec),
    /// Total size of the files (not folders) matching a search.
    GetTotalSize(SearchSpec),
    /// Versions.
    GetVersion,
    /// Whether the database is loaded (searches return complete results).
    IsDbLoaded,
    /// Index status.
    GetIndexStatus,
    /// Rescans every index.
    Rebuild,
    /// Rescans folder indexes.
    Update,
    /// Saves the database.
    SaveDb,
    /// Saves the run history.
    SaveRunHistory,
    /// Deletes the run history.
    DeleteRunHistory,
    /// Exits the application.
    Exit,
    /// Run count of a file.
    GetRunCount {
        /// Full path.
        path: String,
    },
    /// Increments the run count of a file; returns the new count.
    IncRunCount {
        /// Full path.
        path: String,
    },
    /// Sets the run count of a file.
    SetRunCount {
        /// Full path.
        path: String,
        /// New count.
        count: u32,
    },
    /// Indexed volumes.
    ListVolumes,
    /// Filters.
    ListFilters,
    /// Bookmarks.
    ListBookmarks,
}

impl Op {
    /// `true` for operations that change state (admin commands).
    pub fn is_admin(&self) -> bool {
        matches!(self, Op::Rebuild | Op::Update | Op::SaveDb | Op::SaveRunHistory | Op::DeleteRunHistory | Op::Exit)
    }
}

/// A request frame.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Caller-chosen id echoed in the response.
    pub id: u32,
    /// Operation.
    pub op: Op,
}

/// Response status.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Success.
    Ok,
    /// Malformed or invalid request.
    BadRequest,
    /// Unknown query id / path not in the index.
    NotFound,
    /// The query exceeded its time limit.
    Timeout,
    /// Too many concurrent clients / requests.
    Busy,
    /// Not allowed for this client.
    Denied,
    /// Not supported by this host.
    Unsupported,
    /// The response would exceed [`MAX_RESPONSE_FRAME`] (request fewer results).
    TooLarge,
    /// Internal error.
    Internal,
}

/// A response frame.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Response {
    /// Request id.
    pub id: u32,
    /// Status.
    pub status: Status,
    /// Error message (empty on success).
    pub message: String,
    /// Result.
    pub payload: Payload,
}

/// Response payload.
// Built once per response and moved into the frame encoder; boxing would only add an
// allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Payload {
    /// No data.
    None,
    /// Query results.
    Results(Results),
    /// A count.
    Count(u64),
    /// A size in bytes.
    Size(u64),
    /// Versions.
    Version(VersionInfo),
    /// A flag.
    Bool(bool),
    /// Index status.
    IndexStatus(IndexStatus),
    /// A run count.
    RunCount(u32),
    /// Volumes.
    Volumes(Vec<VolumeEntry>),
    /// Filters.
    Filters(Vec<FilterEntry>),
    /// Bookmarks.
    Bookmarks(Vec<BookmarkEntry>),
}

/// One page of a result set.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Results {
    /// Id for [`Op::QueryContinue`] / [`Op::ReleaseQuery`].
    pub query_id: u64,
    /// Number of results of the whole query.
    pub total: u64,
    /// Offset of the first returned row.
    pub offset: u64,
    /// Effective sort keys.
    pub sort: Vec<(Column, bool)>,
    /// Columns of the returned rows.
    pub columns: ResultColumns,
}

impl Results {
    /// Rows in this page.
    pub fn len(&self) -> usize {
        self.columns.flags.len()
    }

    /// `true` if the page is empty.
    pub fn is_empty(&self) -> bool {
        self.columns.flags.is_empty()
    }
}

/// Columnar result rows. `flags` is always present; other vectors are present when the
/// column was requested and then have the same length as `flags`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ResultColumns {
    /// [`FLAG_FOLDER`] / [`FLAG_ROOT`] bits per row.
    pub flags: Vec<u8>,
    /// Names.
    pub name: Option<Vec<String>>,
    /// Parent paths.
    pub path: Option<Vec<String>>,
    /// Full paths.
    pub full_path: Option<Vec<String>>,
    /// Extensions.
    pub extension: Option<Vec<String>>,
    /// Type descriptions.
    pub type_name: Option<Vec<String>>,
    /// Sizes ([`UNKNOWN`] if unknown or a folder).
    pub size: Option<Vec<u64>>,
    /// Dates modified.
    pub date_modified: Option<Vec<u64>>,
    /// Dates created.
    pub date_created: Option<Vec<u64>>,
    /// Dates accessed.
    pub date_accessed: Option<Vec<u64>>,
    /// Attributes.
    pub attributes: Option<Vec<u32>>,
    /// Run counts.
    pub run_count: Option<Vec<u32>>,
    /// Dates run.
    pub date_run: Option<Vec<u64>>,
    /// Dates recently changed.
    pub date_recently_changed: Option<Vec<u64>>,
    /// File list names.
    pub file_list_name: Option<Vec<String>>,
    /// Highlighted names (`*` toggles highlighting, `**` is a literal `*`).
    pub highlighted_name: Option<Vec<String>>,
    /// Highlighted parent paths.
    pub highlighted_path: Option<Vec<String>>,
    /// Highlighted full paths.
    pub highlighted_full_path: Option<Vec<String>>,
}

/// Versions.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct VersionInfo {
    /// Fennec version (`major.minor.patch`).
    pub fennec: String,
    /// Protocol version ([`PROTOCOL_VERSION`]).
    pub protocol: u32,
    /// Version reported to Everything 1.4 clients (`major, minor, revision, build`).
    pub everything: [u32; 4],
}

/// Index status.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct IndexStatus {
    /// The database is loaded.
    pub db_loaded: bool,
    /// The index is busy (indexing, rebuilding).
    pub busy: bool,
    /// Live entries.
    pub entries: u64,
    /// Live folders.
    pub folders: u64,
    /// Volumes in the index.
    pub volumes: u32,
    /// Index generation (changes on every update).
    pub generation: u64,
    /// Free-form status text from the host (e.g. "Indexing C:").
    pub message: String,
}

/// An indexed volume.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct VolumeEntry {
    /// Root path (`C:\`, `D:\backup.efu`, `/home/u`).
    pub root_path: String,
    /// Display name.
    pub display_name: String,
    /// Kind (`ntfs`, `refs`, `folder`, `filelist`).
    pub kind: String,
    /// File system name.
    pub fs_name: String,
    /// Reachable.
    pub online: bool,
    /// Part of searches.
    pub included: bool,
}

/// A filter.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct FilterEntry {
    /// Name.
    pub name: String,
    /// Search.
    pub search: String,
    /// Macro name (empty = none).
    pub macro_name: String,
    /// Options of the filter's search.
    pub mods: Mods,
}

/// A bookmark.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct BookmarkEntry {
    /// Name.
    pub name: String,
    /// Search.
    pub search: String,
    /// Filter name (empty = Everything).
    pub filter: String,
    /// Sort (`size-descending`), empty = keep.
    pub sort: String,
}

/// Protocol errors.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    /// MessagePack decoding failed.
    #[error("malformed message: {0}")]
    Decode(String),
    /// Encoding failed.
    #[error("cannot encode message: {0}")]
    Encode(String),
    /// Bytes after the message.
    #[error("trailing bytes after message")]
    Trailing,
    /// A field violates a limit.
    #[error("invalid request: {0}")]
    Invalid(String),
}

fn decode_exact<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ProtoError> {
    let mut cursor = io::Cursor::new(bytes);
    let value = {
        let mut de = rmp_serde::Deserializer::new(&mut cursor);
        T::deserialize(&mut de).map_err(|e| ProtoError::Decode(e.to_string()))?
    };
    if cursor.position() as usize != bytes.len() {
        return Err(ProtoError::Trailing);
    }
    Ok(value)
}

fn check_text(what: &str, s: &str, max: usize) -> Result<(), ProtoError> {
    if s.len() > max {
        return Err(ProtoError::Invalid(format!("{what} longer than {max} bytes")));
    }
    if s.contains('\0') {
        return Err(ProtoError::Invalid(format!("{what} contains NUL")));
    }
    Ok(())
}

impl Request {
    /// Encodes as a MessagePack payload.
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        rmp_serde::to_vec_named(self).map_err(|e| ProtoError::Encode(e.to_string()))
    }

    /// Strictly decodes and validates an untrusted payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        if bytes.len() > MAX_REQUEST_FRAME {
            return Err(ProtoError::Invalid("request too large".into()));
        }
        let r: Request = decode_exact(bytes)?;
        r.validate()?;
        Ok(r)
    }

    /// Checks limits.
    pub fn validate(&self) -> Result<(), ProtoError> {
        match &self.op {
            Op::Query(q) => {
                check_text("search", &q.search, MAX_SEARCH_LEN)?;
                if q.columns.len() > MAX_COLUMNS {
                    return Err(ProtoError::Invalid(format!("more than {MAX_COLUMNS} columns")));
                }
                if q.sort.len() > MAX_SORT_KEYS {
                    return Err(ProtoError::Invalid(format!("more than {MAX_SORT_KEYS} sort keys")));
                }
            }
            Op::GetResultCount(s) | Op::GetTotalSize(s) => check_text("search", &s.search, MAX_SEARCH_LEN)?,
            Op::GetRunCount { path } | Op::IncRunCount { path } | Op::SetRunCount { path, .. } => {
                check_text("path", path, MAX_PATH_LEN)?;
                if path.is_empty() {
                    return Err(ProtoError::Invalid("empty path".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Response {
    /// Success response.
    pub fn ok(id: u32, payload: Payload) -> Self {
        Self { id, status: Status::Ok, message: String::new(), payload }
    }

    /// Error response.
    pub fn error(id: u32, status: Status, message: impl Into<String>) -> Self {
        Self { id, status, message: message.into(), payload: Payload::None }
    }

    /// Encodes as a MessagePack payload.
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        rmp_serde::to_vec_named(self).map_err(|e| ProtoError::Encode(e.to_string()))
    }

    /// Decodes a payload (client side).
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        decode_exact(bytes)
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Writes one frame (`len u32 LE | payload`).
pub fn write_frame(w: &mut impl Write, payload: &[u8], max: usize) -> io::Result<()> {
    if payload.len() > max || payload.len() > u32::MAX as usize {
        return Err(bad("frame too large"));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Reads one frame; `None` on a clean end of stream before the length prefix.
pub fn read_frame(r: &mut impl Read, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated frame header")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > max {
        return Err(bad("frame too large"));
    }
    // Grow while reading so a lying length prefix cannot make us allocate `max` up front.
    let mut buf = Vec::with_capacity(n.min(64 * 1024));
    r.take(n as u64).read_to_end(&mut buf)?;
    if buf.len() != n {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated frame"));
    }
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(op: Op) {
        let r = Request { id: 7, op };
        let bytes = r.encode().unwrap();
        assert_eq!(Request::decode(&bytes).unwrap(), r);
    }

    #[test]
    fn requests_roundtrip() {
        roundtrip(Op::Query(Query {
            search: "abc ext:pdf".into(),
            mods: Mods { case: true, regex: true, ..Mods::default() },
            sort: vec![(Column::Size, false), (Column::Name, true)],
            offset: 5,
            limit: 10,
            columns: vec![Column::FullPath, Column::Size],
            highlight: true,
            timeout_ms: 1000,
        }));
        roundtrip(Op::QueryContinue { query_id: 3, offset: 10, limit: 20 });
        roundtrip(Op::GetResultCount(SearchSpec { search: "x".into(), mods: Mods::default() }));
        for op in [
            Op::GetVersion,
            Op::IsDbLoaded,
            Op::GetIndexStatus,
            Op::Rebuild,
            Op::Update,
            Op::SaveDb,
            Op::Exit,
            Op::ListVolumes,
            Op::ListFilters,
            Op::ListBookmarks,
        ] {
            roundtrip(op);
        }
        roundtrip(Op::SetRunCount { path: "C:\\a.txt".into(), count: 4 });
    }

    #[test]
    fn responses_roundtrip() {
        let mut cols = ResultColumns { flags: vec![0, FLAG_FOLDER], ..Default::default() };
        cols.full_path = Some(vec!["C:\\a".into(), "C:\\b".into()]);
        cols.size = Some(vec![1, UNKNOWN]);
        let r = Response::ok(
            9,
            Payload::Results(Results {
                query_id: 1,
                total: 2,
                offset: 0,
                sort: vec![(Column::Name, true)],
                columns: cols,
            }),
        );
        let b = r.encode().unwrap();
        assert_eq!(Response::decode(&b).unwrap(), r);
        let e = Response::error(1, Status::Timeout, "slow");
        assert_eq!(Response::decode(&e.encode().unwrap()).unwrap(), e);
    }

    #[test]
    fn strict_decoding() {
        let good = Request { id: 1, op: Op::GetVersion }.encode().unwrap();
        let mut trailing = good.clone();
        trailing.push(0xc0);
        assert!(matches!(Request::decode(&trailing), Err(ProtoError::Trailing)));
        assert!(Request::decode(&good[..good.len() - 1]).is_err());
        assert!(Request::decode(&[0xff, 0x00, 0x13]).is_err());
        // Unknown field in a query.
        #[derive(Serialize)]
        struct Evil {
            id: u32,
            op: EvilOp,
        }
        #[derive(Serialize)]
        enum EvilOp {
            Query { search: String, bogus: u32 },
        }
        let evil =
            rmp_serde::to_vec_named(&Evil { id: 1, op: EvilOp::Query { search: "a".into(), bogus: 1 } }).unwrap();
        assert!(Request::decode(&evil).is_err());
        // Unknown operation.
        #[derive(Serialize)]
        enum Unknown {
            FormatDisk,
        }
        #[derive(Serialize)]
        struct Wrap {
            id: u32,
            op: Unknown,
        }
        let unk = rmp_serde::to_vec_named(&Wrap { id: 1, op: Unknown::FormatDisk }).unwrap();
        assert!(Request::decode(&unk).is_err());
        // Limits.
        let long =
            Request { id: 1, op: Op::Query(Query { search: "a".repeat(MAX_SEARCH_LEN + 1), ..Query::default() }) };
        assert!(Request::decode(&long.encode().unwrap()).is_err());
        let nul = Request { id: 1, op: Op::IncRunCount { path: "C:\\a\0b".into() } };
        assert!(Request::decode(&nul.encode().unwrap()).is_err());
        let cols = Request { id: 1, op: Op::Query(Query { columns: vec![Column::Name; 33], ..Query::default() }) };
        assert!(Request::decode(&cols.encode().unwrap()).is_err());
    }

    #[test]
    fn frames() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello", 16).unwrap();
        write_frame(&mut buf, b"", 16).unwrap();
        assert!(write_frame(&mut Vec::new(), &[0; 17], 16).is_err());
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r, 16).unwrap().unwrap(), b"hello");
        assert_eq!(read_frame(&mut r, 16).unwrap().unwrap(), b"");
        assert!(read_frame(&mut r, 16).unwrap().is_none());
        // Oversized length prefix is rejected before allocating.
        let mut r = &[0xff, 0xff, 0xff, 0x7f][..];
        assert!(read_frame(&mut r, 1 << 20).is_err());
        // Truncated payload.
        let mut r = &[5, 0, 0, 0, b'a'][..];
        assert!(read_frame(&mut r, 16).is_err());
    }

    #[test]
    fn columns() {
        for c in Column::ALL {
            assert_eq!(Column::parse(c.header()), Some(c), "{c:?}");
            assert_eq!(Column::parse(c.json_name()), Some(c), "{c:?}");
        }
        assert_eq!(Column::parse("date-modified"), Some(Column::DateModified));
        assert_eq!(Column::parse("full-path-and-name"), Some(Column::FullPath));
        assert_eq!(Column::parse("bogus"), None);
        assert_eq!(pipe_name(""), "Fennec..ipc");
        assert_eq!(pipe_name("a\\b"), "Fennec.a_b.ipc");
    }
}

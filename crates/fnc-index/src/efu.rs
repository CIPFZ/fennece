//! Everything file lists (`.efu`): CSV with header
//! `Filename,Size,Date Modified,Date Created,Attributes` (SPEC ch.1 §4).
//!
//! * `Filename` is a full path; folders carry the directory attribute (16) and may
//!   end with a separator. Missing intermediate folders are created implicitly.
//! * Dates are FILETIME decimals or ISO-8601 (`2026-07-02T10:52:00[Z]`); sizes are
//!   decimal bytes. Empty fields mean unknown. UTF-8 with optional BOM.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use fnc_core::flags::attr;
use fnc_core::{EntryId, FileTime, VolumeId};

use crate::change::Meta;
use crate::error::{IndexError, Result};
use crate::store::{BulkStats, IndexStore};
use crate::volume::{VolumeInfo, VolumeKind};

/// Column indexes of a file list.
#[derive(Copy, Clone, Debug)]
struct Columns {
    filename: usize,
    size: Option<usize>,
    dm: Option<usize>,
    dc: Option<usize>,
    da: Option<usize>,
    attributes: Option<usize>,
}

impl Columns {
    const DEFAULT: Columns =
        Columns { filename: 0, size: Some(1), dm: Some(2), dc: Some(3), da: None, attributes: Some(4) };

    fn from_header(fields: &[String]) -> Option<Columns> {
        let find = |name: &str| fields.iter().position(|f| f.trim().eq_ignore_ascii_case(name));
        Some(Columns {
            filename: find("Filename")?,
            size: find("Size"),
            dm: find("Date Modified"),
            dc: find("Date Created"),
            da: find("Date Accessed"),
            attributes: find("Attributes"),
        })
    }
}

/// Splits one CSV line into `out` (RFC 4180 quoting, `""` = literal quote).
pub fn split_csv_line(line: &str, out: &mut Vec<String>) {
    out.clear();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => in_quotes = true,
                ',' => out.push(std::mem::take(&mut field)),
                _ => field.push(c),
            }
        }
    }
    if any {
        out.push(field);
    }
}

fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() { None } else { s.parse().ok() }
}

/// Parses a FILETIME decimal or an ISO-8601 date-time (UTC if suffixed `Z` or with an
/// offset, local otherwise).
pub fn parse_efu_date(s: &str) -> Option<FileTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<i64>().ok().map(FileTime);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(FileTime::from_utc(dt.with_timezone(&chrono::Utc)));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(FileTime::from_local(dt));
        }
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)).map(FileTime::from_local)
}

/// Root of a path and the remainder: `C:\a\b` → (`C:`, `a\b`), `\\srv\share\x` →
/// (`\\srv\share`, `x`), `/a/b` → (`/`, `a/b`).
fn split_root(path: &str) -> (&str, &str) {
    if let Some(rest) = path.strip_prefix("\\\\") {
        // UNC: \\server\share
        let mut parts = rest.splitn(3, '\\');
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        let root_len = 2 + server.len() + if share.is_empty() { 0 } else { 1 + share.len() };
        let tail = path.get(root_len..).unwrap_or("").trim_start_matches('\\');
        return (&path[..root_len.min(path.len())], tail);
    }
    if path.starts_with('/') {
        return ("/", path.trim_start_matches('/'));
    }
    match path.find(['\\', '/']) {
        Some(p) => (&path[..p], &path[p + 1..]),
        None => (path, ""),
    }
}

struct Loader<'s> {
    dirs: HashMap<String, (u64, EntryId)>,
    next_frn: u64,
    bulk: crate::store::BulkLoader<'s>,
    units: Vec<u16>,
}

impl Loader<'_> {
    /// Returns `(frn, id)` of folder `path` (no trailing separator), creating it
    /// and its ancestors when missing.
    fn ensure_dir(&mut self, path: &str, sep: char) -> Result<(u64, EntryId)> {
        if let Some(&d) = self.dirs.get(path) {
            return Ok(d);
        }
        let (root, rest) = split_root(path);
        let entry = if rest.is_empty() {
            let frn = self.next_frn;
            self.next_frn += 1;
            let id = self.bulk.add_root(frn, root)?;
            (frn, id)
        } else {
            let (parent, name) = match path.rfind(sep) {
                Some(p) if p >= root.len() => (&path[..p], &path[p + 1..]),
                _ => (root, rest),
            };
            let (pfrn, _) = self.ensure_dir(parent, sep)?;
            let frn = self.next_frn;
            self.next_frn += 1;
            self.units.clear();
            self.units.extend(name.encode_utf16());
            let id = self.bulk.push(frn, pfrn, &self.units, &Meta::attrs(attr::DIRECTORY))?;
            (frn, id)
        };
        self.dirs.insert(path.to_string(), entry);
        Ok(entry)
    }
}

/// Mounts an EFU stream as a new `FileList` volume. `source` names the volume.
pub fn load_efu<R: BufRead>(store: &mut IndexStore, mut reader: R, source: &str) -> Result<(VolumeId, BulkStats)> {
    let io_err = |e| IndexError::Io { path: source.into(), source: e };
    let mut line = String::new();
    let mut fields = Vec::new();
    let mut line_no = 0usize;
    // Reads the next non-empty line (without line terminator) into `line`.
    let mut next_line = |line: &mut String, line_no: &mut usize| -> Result<bool> {
        loop {
            line.clear();
            if reader.read_line(line).map_err(io_err)? == 0 {
                return Ok(false);
            }
            *line_no += 1;
            let trimmed = line.trim_end_matches(['\r', '\n']).len();
            line.truncate(trimmed);
            if *line_no == 1 && line.starts_with('\u{feff}') {
                line.drain(..'\u{feff}'.len_utf8());
            }
            if !line.trim().is_empty() {
                return Ok(true);
            }
        }
    };

    let mut have_line = next_line(&mut line, &mut line_no)?;
    let mut cols = Columns::DEFAULT;
    if have_line {
        split_csv_line(&line, &mut fields);
        if let Some(c) = Columns::from_header(&fields) {
            cols = c;
            have_line = next_line(&mut line, &mut line_no)?;
        }
    }
    let unix_paths = have_line && {
        split_csv_line(&line, &mut fields);
        fields.get(cols.filename).is_some_and(|p| p.starts_with('/'))
    };
    let sep = if unix_paths { '/' } else { '\\' };

    let mut info = VolumeInfo::new(VolumeKind::FileList, source);
    info.display_name =
        Path::new(source).file_name().map_or_else(|| source.to_string(), |n| n.to_string_lossy().into_owned());
    info.separator = sep;
    let vol = store.add_volume(info, None)?;

    let mut loader = Loader { dirs: HashMap::new(), next_frn: 1, bulk: store.bulk_load(vol), units: Vec::new() };
    while have_line {
        split_csv_line(&line, &mut fields);
        let Some(raw_path) = fields.get(cols.filename) else {
            return Err(IndexError::FileList {
                path: source.into(),
                line: line_no,
                message: "missing Filename".into(),
            });
        };
        let get = |i: Option<usize>| i.and_then(|i| fields.get(i)).map(String::as_str).unwrap_or("");
        let attributes = parse_u64(get(cols.attributes)).map_or(0, |a| a as u32);
        let mut path = raw_path.as_str();
        let trailing_sep = path.len() > 1 && path.ends_with(sep);
        if trailing_sep {
            path = path.trim_end_matches(sep);
        }
        let is_dir = attributes & attr::DIRECTORY != 0 || trailing_sep;
        let meta = Meta {
            attributes: Some(attributes | if is_dir { attr::DIRECTORY } else { 0 }),
            size: parse_u64(get(cols.size)),
            date_modified: parse_efu_date(get(cols.dm)),
            date_created: parse_efu_date(get(cols.dc)),
            date_accessed: parse_efu_date(get(cols.da)),
        };
        let (root, rest) = split_root(path);
        if rest.is_empty() {
            let (_, id) = loader.ensure_dir(root, sep)?;
            loader.bulk.update(id, &meta);
        } else if let Some(&(_, id)) = loader.dirs.get(path).filter(|_| is_dir) {
            loader.bulk.update(id, &meta);
        } else {
            let (parent, name) = match path.rfind(sep) {
                Some(p) if p >= root.len() => (&path[..p], &path[p + 1..]),
                _ => (root, rest),
            };
            let (pfrn, _) = loader.ensure_dir(parent, sep)?;
            let frn = loader.next_frn;
            loader.next_frn += 1;
            loader.units.clear();
            loader.units.extend(name.encode_utf16());
            let id = loader.bulk.push(frn, pfrn, &loader.units, &meta)?;
            if is_dir {
                loader.dirs.insert(path.to_string(), (frn, id));
            }
        }
        have_line = next_line(&mut line, &mut line_no)?;
    }
    let stats = loader.bulk.finish()?;
    store.set_volume_online(vol, false);
    store.shrink_to_fit();
    Ok((vol, stats))
}

/// Mounts an EFU file.
pub fn load_efu_file(store: &mut IndexStore, path: &Path) -> Result<(VolumeId, BulkStats)> {
    let f = std::fs::File::open(path).map_err(|e| IndexError::Io { path: path.into(), source: e })?;
    load_efu(store, std::io::BufReader::with_capacity(1 << 20, f), &path.to_string_lossy())
}

/// Streaming EFU writer.
pub struct EfuWriter<W: Write> {
    out: W,
    line: String,
}

impl<W: Write> EfuWriter<W> {
    /// Writes the header.
    pub fn new(mut out: W) -> std::io::Result<Self> {
        out.write_all(b"Filename,Size,Date Modified,Date Created,Attributes\r\n")?;
        Ok(Self { out, line: String::with_capacity(512) })
    }

    /// Writes one record. `None` fields are left empty.
    pub fn write(
        &mut self,
        path: &str,
        size: Option<u64>,
        dm: FileTime,
        dc: FileTime,
        attributes: u32,
    ) -> std::io::Result<()> {
        use std::fmt::Write as _;
        self.line.clear();
        self.line.push('"');
        for c in path.chars() {
            if c == '"' {
                self.line.push('"');
            }
            self.line.push(c);
        }
        self.line.push_str("\",");
        if let Some(s) = size {
            let _ = write!(self.line, "{s}");
        }
        self.line.push(',');
        if dm.is_known() {
            let _ = write!(self.line, "{}", dm.0);
        }
        self.line.push(',');
        if dc.is_known() {
            let _ = write!(self.line, "{}", dc.0);
        }
        let _ = write!(self.line, ",{attributes}\r\n");
        self.out.write_all(self.line.as_bytes())
    }

    /// Writes entry `id` of `store`.
    pub fn write_entry(&mut self, store: &IndexStore, id: EntryId, path_buf: &mut String) -> std::io::Result<()> {
        store.full_path(id, path_buf);
        let size = if store.is_dir(id) { None } else { store.size(id) };
        let mut attributes = store.attributes(id);
        if store.is_dir(id) {
            attributes |= attr::DIRECTORY;
        }
        self.write(path_buf, size, store.date_modified(id), store.date_created(id), attributes)
    }

    /// Flushes and returns the inner writer.
    pub fn finish(mut self) -> std::io::Result<W> {
        self.out.flush()?;
        Ok(self.out)
    }
}

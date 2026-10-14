//! `.fnx` index database (ADR-003, SPEC ch.1 §5).
//!
//! ```text
//! Header  "FNX1" | version u32 | flags u32 | reserved u32 | created FILETIME i64
//! Segment*        8-byte aligned, written in the order of `Tag`
//! Footer  directory entries { tag [u8;4], 0u32, offset u64, len u64, xxh3 u64 } *
//!         count u32 | 0u32 | xxh3(directory) u64 | "FNX1END\0"
//! ```
//!
//! Everything is little-endian. Rows are saved as they are (tombstones included) so a
//! save never renumbers the live index; [`load`] compacts when worthwhile. Derived
//! structures (children CSR, FRN maps, counters) are rebuilt on load; the name ranks
//! are stored when they are complete.
//!
//! Deviation from ADR-003: the file is read into memory instead of `mmap`ed — every
//! column is copied into a mutable `Vec` anyway and `fnc-index` forbids `unsafe`.
//!
//! Versions (ADR-038): files of layout `OLDEST_READABLE..=VERSION` are decoded (one
//! decoder per layout); [`load_or_recover`] moves an older, newer or damaged file to
//! `index.fnx.v<N>.bak` / `index.fnx.corrupt.bak` so the caller rebuilds, and [`save`]
//! never replaces a newer file without that backup. Header flags are additive: a reader
//! ignores flags it does not know (`FLAG_PINYIN_RANKS` appeared after 0.1.0 in layout 1;
//! a 0.1.0 file has it clear, i.e. code-point ranks, which are rebuilt under pinyin).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use fnc_core::upgrade::{Recovery, Unreadable, check_version};
use fnc_core::{EntryFlags, EntryId, FileTime, FoldArena, NameArena, VolumeId};
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::ext::ExtDict;
use crate::order::NameOrder;
use crate::store::{IndexOptions, IndexStore, RunInfo};
use crate::volume::{Volume, VolumeInfo, VolumeKind};

/// File magic.
pub const MAGIC: [u8; 4] = *b"FNX1";
const END_MAGIC: [u8; 8] = *b"FNX1END\0";
/// Layout version; bump on any change (and add a decoder for the old one, or raise
/// [`OLDEST_READABLE`]).
pub const VERSION: u32 = 1;
/// Oldest layout [`decode`] still reads. Layout 1 is the only one so far (0.1.0, 0.2.0).
pub const OLDEST_READABLE: u32 = 1;
const HEADER_LEN: usize = 24;
const DIR_ENTRY_LEN: usize = 32;
const FOOTER_TAIL_LEN: usize = 24;
const FLAG_DATE_ACCESSED: u32 = 1;
/// The `RANK` column was built with pinyin collation (`fnc_core::natsort`).
const FLAG_PINYIN_RANKS: u32 = 2;

/// Errors reading or writing a database.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// I/O error.
    #[error("{path}: {source}")]
    Io {
        /// File.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Not a Fennec database.
    #[error("not a Fennec index database")]
    BadMagic,
    /// Written by another format version.
    #[error("unsupported index database version {0}")]
    Version(u32),
    /// A segment failed its checksum.
    #[error("index database segment {0} is corrupt (checksum)")]
    Checksum(String),
    /// Structurally invalid content.
    #[error("index database is corrupt: {0}")]
    Corrupt(&'static str),
}

impl PersistError {
    /// How the file is set aside: an unsupported version (older or newer than this
    /// build reads) or damage. `None` for a missing file.
    pub fn unreadable(&self) -> Option<Unreadable> {
        match self {
            PersistError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => None,
            PersistError::Version(v) => {
                Some(check_version(*v, OLDEST_READABLE, VERSION).err().unwrap_or(Unreadable::Corrupt))
            }
            _ => Some(Unreadable::Corrupt),
        }
    }
}

type PResult<T> = std::result::Result<T, PersistError>;

/// What a save wrote.
#[derive(Debug, Clone, Copy, Default)]
pub struct SaveStats {
    /// Rows written (including tombstones).
    pub rows: usize,
    /// File size in bytes.
    pub bytes: u64,
}

// ------------------------------------------------------------------ writing

struct SegWriter<W: Write> {
    out: W,
    pos: u64,
    dir: Vec<([u8; 4], u64, u64, u64)>,
    cur: Option<([u8; 4], u64, Xxh3)>,
    staging: Vec<u8>,
}

impl<W: Write> SegWriter<W> {
    fn raw(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.out.write_all(b)?;
        if let Some((_, _, h)) = &mut self.cur {
            h.update(b);
        }
        self.pos += b.len() as u64;
        Ok(())
    }

    fn begin(&mut self, tag: [u8; 4]) -> std::io::Result<()> {
        let pad = (8 - (self.pos % 8) as usize) % 8;
        self.out.write_all(&[0u8; 8][..pad])?;
        self.pos += pad as u64;
        self.cur = Some((tag, self.pos, Xxh3::new()));
        Ok(())
    }

    fn end(&mut self) {
        if let Some((tag, start, h)) = self.cur.take() {
            self.dir.push((tag, start, self.pos - start, h.digest()));
        }
    }

    /// Writes a column of fixed-size values through a staging buffer.
    fn column<T: Copy, const N: usize>(
        &mut self,
        tag: [u8; 4],
        v: &[T],
        enc: impl Fn(T) -> [u8; N],
    ) -> std::io::Result<()> {
        self.begin(tag)?;
        for chunk in v.chunks(16 * 1024) {
            self.staging.clear();
            for &x in chunk {
                self.staging.extend_from_slice(&enc(x));
            }
            let staging = std::mem::take(&mut self.staging);
            self.raw(&staging)?;
            self.staging = staging;
        }
        self.end();
        Ok(())
    }

    fn blob(&mut self, tag: [u8; 4], b: &[u8]) -> std::io::Result<()> {
        self.begin(tag)?;
        self.raw(b)?;
        self.end();
        Ok(())
    }
}

/// Small helper for variable-length segments.
#[derive(Default)]
struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
}

fn kind_code(k: VolumeKind) -> u8 {
    match k {
        VolumeKind::Ntfs => 0,
        VolumeKind::Refs => 1,
        VolumeKind::Folder => 2,
        VolumeKind::FileList => 3,
    }
}

fn kind_from(c: u8) -> PResult<VolumeKind> {
    Ok(match c {
        0 => VolumeKind::Ntfs,
        1 => VolumeKind::Refs,
        2 => VolumeKind::Folder,
        3 => VolumeKind::FileList,
        _ => return Err(PersistError::Corrupt("volume kind")),
    })
}

fn encode_volumes(store: &IndexStore) -> Vec<u8> {
    let mut e = Enc::default();
    e.u32(store.volumes.len() as u32);
    for (v, orphan) in store.volumes.iter().zip(&store.orphans) {
        e.u8(kind_code(v.info.kind));
        e.u8(u8::from(v.online));
        e.u8(u8::from(v.include_in_index));
        e.u8(u8::from(v.metadata_complete));
        e.u32(v.info.separator as u32);
        e.u32(v.info.serial);
        e.u64(v.usn_journal_id);
        e.i64(v.next_usn);
        e.str(&v.info.root_path);
        e.str(&v.info.display_name);
        e.str(&v.info.guid);
        e.str(&v.info.fs_name);
        e.u32(v.roots.len() as u32);
        for r in &v.roots {
            e.u32(r.0);
        }
        e.u32(orphan.map_or(u32::MAX, |o| o.0));
    }
    e.0
}

fn encode_extras(store: &IndexStore) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut ext = Enc::default();
    ext.u32(store.ext_dict.len() as u32);
    for id in 1..=store.ext_dict.len() as u16 {
        ext.bytes(store.ext_dict.name(id));
    }
    let mut run = Enc::default();
    let mut runs: Vec<_> = store.run.iter().collect();
    runs.sort_by_key(|(k, _)| k.0);
    run.u32(runs.len() as u32);
    for (k, r) in runs {
        run.u32(k.0);
        run.u32(r.count);
        run.i64(r.last.0);
    }
    let mut rc = Enc::default();
    let mut recent: Vec<_> = store.recent.iter().collect();
    recent.sort_by_key(|(k, _)| k.0);
    rc.u32(recent.len() as u32);
    for (k, t) in recent {
        rc.u32(k.0);
        rc.i64(t.0);
    }
    let mut links = Enc::default();
    let mut ls: Vec<_> = store.links.iter().collect();
    ls.sort_by_key(|((v, f), _)| (v.0, *f));
    links.u32(ls.len() as u32);
    for ((v, frn), ids) in ls {
        links.u16(v.0);
        links.u64(*frn);
        links.u32(ids.len() as u32);
        for id in ids {
            links.u32(id.0);
        }
    }
    (ext.0, run.0, rc.0, links.0)
}

/// Saves `store` to `path` atomically: writes `path.tmp`, flushes it to disk, keeps
/// the previous file as `path.bak` and renames the new file into place. A previous file
/// of a newer layout is moved to `path.v<N>.bak` instead (never replaced, ADR-038).
pub fn save(store: &IndexStore, path: &Path) -> PResult<SaveStats> {
    let tmp = path.with_extension("fnx.tmp");
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |source| PersistError::Io { path: p, source }
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
    }
    let file = File::create(&tmp).map_err(io(&tmp))?;
    let mut w = SegWriter {
        out: BufWriter::with_capacity(1 << 20, file),
        pos: 0,
        dir: Vec::new(),
        cur: None,
        staging: Vec::new(),
    };
    let rows = write_all(store, &mut w).map_err(io(&tmp))?;
    let bytes = w.pos;
    let file = w.out.into_inner().map_err(|e| PersistError::Io { path: tmp.clone(), source: e.into_error() })?;
    file.sync_all().map_err(io(&tmp))?;
    drop(file);
    fnc_core::upgrade::protect_future(path, MAGIC, VERSION).map_err(io(path))?;
    if path.exists() {
        let bak = path.with_extension("fnx.bak");
        std::fs::rename(path, &bak).map_err(io(&bak))?;
    }
    std::fs::rename(&tmp, path).map_err(io(path))?;
    Ok(SaveStats { rows, bytes })
}

fn write_all<W: Write>(s: &IndexStore, w: &mut SegWriter<W>) -> std::io::Result<usize> {
    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(&MAGIC);
    header.extend_from_slice(&VERSION.to_le_bytes());
    let mut flags = if s.da.is_some() { FLAG_DATE_ACCESSED } else { 0 };
    if fnc_core::natsort::collation() == fnc_core::natsort::Collation::Pinyin {
        flags |= FLAG_PINYIN_RANKS;
    }
    header.extend_from_slice(&flags.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&FileTime::now().0.to_le_bytes());
    w.raw(&header)?;

    w.blob(*b"VOLS", &encode_volumes(s))?;
    let (nd, no, nl) = s.names.raw_parts();
    w.column(*b"NAMD", nd, u16::to_le_bytes)?;
    w.column(*b"NAMO", no, u32::to_le_bytes)?;
    w.column(*b"NAML", nl, u16::to_le_bytes)?;
    let (fd, fo, fl) = s.folds.raw_parts();
    w.blob(*b"FLDD", fd)?;
    w.column(*b"FLDO", fo, u32::to_le_bytes)?;
    w.column(*b"FLDL", fl, u16::to_le_bytes)?;
    w.column(*b"PARN", &s.parent, |x: EntryId| x.0.to_le_bytes())?;
    w.column(*b"VOLM", &s.volume, |x: VolumeId| x.0.to_le_bytes())?;
    w.column(*b"FLAG", &s.flags, |x: EntryFlags| x.0.to_le_bytes())?;
    w.column(*b"FRN_", &s.frn, u64::to_le_bytes)?;
    w.column(*b"EXTI", &s.ext, u16::to_le_bytes)?;
    w.column(*b"SIZE", &s.size, u64::to_le_bytes)?;
    w.column(*b"DMOD", &s.dm, |x: FileTime| x.0.to_le_bytes())?;
    w.column(*b"DCRE", &s.dc, |x: FileTime| x.0.to_le_bytes())?;
    if let Some(da) = &s.da {
        w.column(*b"DACC", da, |x: FileTime| x.0.to_le_bytes())?;
    }
    w.column(*b"ATTR", &s.attr, u32::to_le_bytes)?;
    let (ext, run, rc, links) = encode_extras(s);
    w.blob(*b"EXTD", &ext)?;
    w.blob(*b"RUNH", &run)?;
    w.blob(*b"RCHG", &rc)?;
    w.blob(*b"LINK", &links)?;
    if let Some(ranks) = s.name_order.complete_ranks(s.len()) {
        w.column(*b"RANK", ranks, u32::to_le_bytes)?;
    }

    // Footer.
    let mut dir = Vec::with_capacity(w.dir.len() * DIR_ENTRY_LEN);
    for (tag, off, len, hash) in &w.dir {
        dir.extend_from_slice(tag);
        dir.extend_from_slice(&0u32.to_le_bytes());
        dir.extend_from_slice(&off.to_le_bytes());
        dir.extend_from_slice(&len.to_le_bytes());
        dir.extend_from_slice(&hash.to_le_bytes());
    }
    let pad = (8 - (w.pos % 8) as usize) % 8;
    w.raw(&[0u8; 8][..pad])?;
    w.raw(&dir)?;
    let mut tail = Vec::with_capacity(FOOTER_TAIL_LEN);
    tail.extend_from_slice(&(w.dir.len() as u32).to_le_bytes());
    tail.extend_from_slice(&0u32.to_le_bytes());
    tail.extend_from_slice(&xxh3_64(&dir).to_le_bytes());
    tail.extend_from_slice(&END_MAGIC);
    w.raw(&tail)?;
    w.out.flush()?;
    Ok(s.len())
}

// ------------------------------------------------------------------ reading

struct Dec<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> PResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(PersistError::Corrupt("length"))?;
        let s = self.b.get(self.pos..end).ok_or(PersistError::Corrupt("truncated segment"))?;
        self.pos = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> PResult<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| PersistError::Corrupt("truncated segment"))
    }
    fn u8(&mut self) -> PResult<u8> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> PResult<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> PResult<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> PResult<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn i64(&mut self) -> PResult<i64> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    fn bytes(&mut self) -> PResult<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn str(&mut self) -> PResult<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| PersistError::Corrupt("string"))
    }
    /// Count of items that each need at least `min` bytes (guards huge allocations).
    fn count(&mut self, min: usize) -> PResult<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min) > self.b.len() - self.pos {
            return Err(PersistError::Corrupt("count"));
        }
        Ok(n)
    }
}

fn col<T, const N: usize>(seg: &[u8], dec: impl Fn([u8; N]) -> T) -> PResult<Vec<T>> {
    let (chunks, rest) = seg.as_chunks::<N>();
    if !rest.is_empty() {
        return Err(PersistError::Corrupt("column length"));
    }
    Ok(chunks.iter().map(|c| dec(*c)).collect())
}

/// Segments by tag.
type Segments<'a> = HashMap<[u8; 4], &'a [u8]>;

/// Checks the header and returns the layout version; reads the directory and verifies
/// every checksum.
fn segments(buf: &[u8]) -> PResult<(u32, Segments<'_>)> {
    if buf.len() < HEADER_LEN + FOOTER_TAIL_LEN || buf[..4] != MAGIC {
        return Err(PersistError::BadMagic);
    }
    let version = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    check_version(version, OLDEST_READABLE, VERSION).map_err(|_| PersistError::Version(version))?;
    let tail = &buf[buf.len() - FOOTER_TAIL_LEN..];
    if tail[16..] != END_MAGIC {
        return Err(PersistError::Corrupt("footer"));
    }
    let count = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as usize;
    let dir_len = count.checked_mul(DIR_ENTRY_LEN).ok_or(PersistError::Corrupt("footer"))?;
    let dir_start = (buf.len() - FOOTER_TAIL_LEN)
        .checked_sub(dir_len)
        .filter(|&s| s >= HEADER_LEN)
        .ok_or(PersistError::Corrupt("footer"))?;
    let dir = &buf[dir_start..buf.len() - FOOTER_TAIL_LEN];
    let mut d = Dec { b: tail, pos: 8 };
    if xxh3_64(dir) != d.u64()? {
        return Err(PersistError::Checksum("directory".into()));
    }
    let mut out = HashMap::new();
    for e in dir.as_chunks::<DIR_ENTRY_LEN>().0 {
        let mut d = Dec { b: e, pos: 0 };
        let tag: [u8; 4] = d.arr()?;
        d.u32()?;
        let (off, len, hash) = (d.u64()? as usize, d.u64()? as usize, d.u64()?);
        let seg = off
            .checked_add(len)
            .filter(|&end| end <= dir_start)
            .and_then(|end| buf.get(off..end))
            .ok_or(PersistError::Corrupt("segment bounds"))?;
        if xxh3_64(seg) != hash {
            return Err(PersistError::Checksum(String::from_utf8_lossy(&tag).into_owned()));
        }
        out.insert(tag, seg);
    }
    Ok((version, out))
}

/// Loads a database written by [`save`]. Any inconsistency is an error (the caller
/// rebuilds); the loaded index is compacted if a fifth or more of it is tombstones.
pub fn load(path: &Path) -> PResult<IndexStore> {
    let buf = std::fs::read(path).map_err(|source| PersistError::Io { path: path.to_path_buf(), source })?;
    decode(&buf)
}

/// What [`load_or_recover`] found.
pub enum Loaded {
    /// No database yet.
    Missing,
    /// The database.
    Index(Box<IndexStore>),
    /// The file was not usable and was set aside (or could not be): rebuild.
    Rebuild(Recovery, PersistError),
}

/// [`load`] for start-up: a file that cannot be used — older layout than
/// [`OLDEST_READABLE`], newer than [`VERSION`], damaged — is moved to
/// `index.fnx.v<N>.bak` / `index.fnx.corrupt.bak` (never deleted) and the caller rebuilds
/// (and tells the user what [`Recovery`] says).
pub fn load_or_recover(path: &Path) -> Loaded {
    match load(path) {
        Ok(s) => Loaded::Index(Box::new(s)),
        Err(e) => match e.unreadable() {
            None => Loaded::Missing,
            Some(why) => Loaded::Rebuild(Recovery::quarantined(path, why), e),
        },
    }
}

/// [`load`] from a buffer.
pub fn decode(buf: &[u8]) -> PResult<IndexStore> {
    let (version, segs) = segments(buf)?;
    // One decoder per readable layout; layout 1 is the only one so far.
    debug_assert_eq!(version, VERSION);
    decode_v1(buf, &segs)
}

/// Layout 1.
fn decode_v1(buf: &[u8], segs: &Segments<'_>) -> PResult<IndexStore> {
    let flags = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let seg = |t: &[u8; 4]| segs.get(t).copied().ok_or(PersistError::Corrupt("missing segment"));

    let mut store = IndexStore::new(IndexOptions { date_accessed: flags & FLAG_DATE_ACCESSED != 0 });
    let names = NameArena::from_raw_parts(
        col(seg(b"NAMD")?, u16::from_le_bytes)?,
        col(seg(b"NAMO")?, u32::from_le_bytes)?,
        col(seg(b"NAML")?, u16::from_le_bytes)?,
    )
    .ok_or(PersistError::Corrupt("name arena"))?;
    let folds = FoldArena::from_raw_parts(
        seg(b"FLDD")?.to_vec(),
        col(seg(b"FLDO")?, u32::from_le_bytes)?,
        col(seg(b"FLDL")?, u16::from_le_bytes)?,
    )
    .ok_or(PersistError::Corrupt("fold arena"))?;
    let n = names.len();
    store.names = names;
    store.folds = folds;
    store.parent = col(seg(b"PARN")?, |b| EntryId(u32::from_le_bytes(b)))?;
    store.volume = col(seg(b"VOLM")?, |b| VolumeId(u16::from_le_bytes(b)))?;
    store.flags = col(seg(b"FLAG")?, |b| EntryFlags(u16::from_le_bytes(b)))?;
    store.frn = col(seg(b"FRN_")?, u64::from_le_bytes)?;
    store.ext = col(seg(b"EXTI")?, u16::from_le_bytes)?;
    store.size = col(seg(b"SIZE")?, u64::from_le_bytes)?;
    store.dm = col(seg(b"DMOD")?, |b| FileTime(i64::from_le_bytes(b)))?;
    store.dc = col(seg(b"DCRE")?, |b| FileTime(i64::from_le_bytes(b)))?;
    if store.da.is_some() {
        store.da = Some(col(seg(b"DACC")?, |b| FileTime(i64::from_le_bytes(b)))?);
    }
    store.attr = col(seg(b"ATTR")?, u32::from_le_bytes)?;
    let lens = [
        store.folds.len(),
        store.parent.len(),
        store.volume.len(),
        store.flags.len(),
        store.frn.len(),
        store.ext.len(),
        store.size.len(),
        store.dm.len(),
        store.dc.len(),
        store.attr.len(),
        store.da.as_ref().map_or(n, Vec::len),
    ];
    if lens.iter().any(|&l| l != n) {
        return Err(PersistError::Corrupt("column lengths differ"));
    }

    decode_volumes(&mut store, seg(b"VOLS")?)?;
    decode_extras(&mut store, seg(b"EXTD")?, seg(b"RUNH")?, seg(b"RCHG")?, seg(b"LINK")?)?;
    validate(&store)?;
    // Ranks of another collation are rebuilt (they would disagree with the comparator).
    let same_collation =
        (flags & FLAG_PINYIN_RANKS != 0) == (fnc_core::natsort::collation() == fnc_core::natsort::Collation::Pinyin);
    let ranks = match segs.get(b"RANK").filter(|_| same_collation) {
        Some(r) => Some(col(r, u32::from_le_bytes)?).filter(|r| r.len() == n),
        None => None,
    };
    store.finish_load(ranks.map(NameOrder::from_ranks));
    if store.len() > 1024 && store.live_count() * 5 < store.len() * 4 {
        store.compact();
    }
    Ok(store)
}

fn decode_volumes(store: &mut IndexStore, b: &[u8]) -> PResult<()> {
    let mut d = Dec { b, pos: 0 };
    let n = d.count(32)?;
    for i in 0..n {
        let kind = kind_from(d.u8()?)?;
        let online = d.u8()? != 0;
        let include_in_index = d.u8()? != 0;
        let metadata_complete = d.u8()? != 0;
        let separator = char::from_u32(d.u32()?).ok_or(PersistError::Corrupt("separator"))?;
        let serial = d.u32()?;
        let usn_journal_id = d.u64()?;
        let next_usn = d.i64()?;
        let info = VolumeInfo {
            kind,
            root_path: d.str()?,
            display_name: d.str()?,
            guid: d.str()?,
            fs_name: d.str()?,
            serial,
            separator,
        };
        let nr = d.count(4)?;
        let roots = (0..nr).map(|_| d.u32().map(EntryId)).collect::<PResult<Vec<_>>>()?;
        let orphan = Some(EntryId(d.u32()?)).filter(|o| o.0 != u32::MAX);
        let id = VolumeId(u16::try_from(i).map_err(|_| PersistError::Corrupt("volumes"))?);
        store.volumes.push(Volume {
            id,
            info,
            roots,
            usn_journal_id,
            next_usn,
            online,
            include_in_index,
            metadata_complete,
        });
        store.orphans.push(orphan);
    }
    Ok(())
}

fn decode_extras(store: &mut IndexStore, ext: &[u8], run: &[u8], rc: &[u8], links: &[u8]) -> PResult<()> {
    let mut d = Dec { b: ext, pos: 0 };
    let n = d.count(4)?;
    let mut dict = ExtDict::new();
    for i in 0..n {
        let name = d.bytes()?;
        if name.is_empty() || dict.intern(name) as usize != i + 1 {
            return Err(PersistError::Corrupt("extension dictionary"));
        }
    }
    store.ext_dict = dict;
    let mut d = Dec { b: run, pos: 0 };
    for _ in 0..d.count(16)? {
        let id = EntryId(d.u32()?);
        let count = d.u32()?;
        store.run.insert(id, RunInfo { count, last: FileTime(d.i64()?) });
    }
    let mut d = Dec { b: rc, pos: 0 };
    for _ in 0..d.count(12)? {
        let id = EntryId(d.u32()?);
        store.recent.insert(id, FileTime(d.i64()?));
    }
    let mut d = Dec { b: links, pos: 0 };
    for _ in 0..d.count(14)? {
        let v = VolumeId(d.u16()?);
        let frn = d.u64()?;
        let k = d.count(4)?;
        let ids = (0..k).map(|_| d.u32().map(EntryId)).collect::<PResult<Vec<_>>>()?;
        store.links.insert((v, frn), ids);
    }
    Ok(())
}

/// Every id stored in the file refers to an existing row / volume / extension.
fn validate(s: &IndexStore) -> PResult<()> {
    let n = s.len();
    let in_range = |id: EntryId| id.idx() < n;
    if s.parent.iter().any(|&p| p.is_some() && !in_range(p)) {
        return Err(PersistError::Corrupt("parent id"));
    }
    if s.volume.iter().any(|v| v.idx() >= s.volumes.len()) {
        return Err(PersistError::Corrupt("volume id"));
    }
    let ext_max = s.ext_dict.len() as u16;
    if s.ext.iter().any(|&e| e > ext_max && e != crate::ext::OVERFLOW_EXT) {
        return Err(PersistError::Corrupt("extension id"));
    }
    let ids_ok = s.volumes.iter().all(|v| v.roots.iter().all(|&r| in_range(r)))
        && s.orphans.iter().flatten().all(|&o| in_range(o))
        && s.run.keys().all(|&k| in_range(k))
        && s.recent.keys().all(|&k| in_range(k))
        && s.links.iter().all(|((v, _), ids)| v.idx() < s.volumes.len() && ids.iter().all(|&i| in_range(i)));
    if !ids_ok {
        return Err(PersistError::Corrupt("entry id"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Change, EntryRef, Meta};

    fn u16s(s: &str) -> Box<[u16]> {
        s.encode_utf16().collect()
    }

    fn sample() -> IndexStore {
        let mut s = IndexStore::new(IndexOptions { date_accessed: true });
        let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
        s.set_journal_position(vol, 77, 123_456);
        let mut b = s.bulk_load(vol);
        let dir = Meta::attrs(fnc_core::flags::attr::DIRECTORY);
        let file = |n: u64| Meta {
            attributes: Some(fnc_core::flags::attr::ARCHIVE),
            size: Some(n),
            date_modified: Some(FileTime(n as i64 * 1000)),
            date_created: Some(FileTime(1)),
            date_accessed: Some(FileTime(2)),
        };
        b.push(10, 5, &u16s("Users"), &dir).unwrap();
        b.push(11, 10, &u16s("季度报告.docx"), &file(4096)).unwrap();
        b.push(12, 10, &u16s("photo.JPG"), &file(1 << 20)).unwrap();
        b.push(13, 999, &u16s("lost.bin"), &file(1)).unwrap();
        b.finish().unwrap();
        s.rebuild_name_order();
        // A hard-link alias, a tombstone, run history and a recent change.
        s.apply(vec![Change::Touch {
            volume: vol,
            frn: 12,
            parent: EntryRef::Frn(vol, 5),
            name: u16s("photo-link.jpg"),
            meta: Meta::default(),
        }]);
        s.apply(vec![Change::Delete(EntryRef::Frn(vol, 11))]);
        let photo = s.resolve(EntryRef::Frn(vol, 12)).unwrap();
        s.record_run(photo);
        s
    }

    fn dump(s: &IndexStore) -> Vec<String> {
        let mut v: Vec<String> = (0..s.len() as u32)
            .map(EntryId)
            .filter(|&id| s.is_live(id))
            .map(|id| {
                let mut p = String::new();
                s.full_path(id, &mut p);
                format!("{p}|{:?}|{:?}|{}", s.size(id), s.date_modified(id), s.hardlink_count(id))
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn roundtrip_preserves_everything_observable() {
        let s = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.fnx");
        let st = save(&s, &path).unwrap();
        assert_eq!(st.rows, s.len());
        let l = load(&path).unwrap();
        assert_eq!(dump(&l), dump(&s));
        assert_eq!(l.live_count(), s.live_count());
        let vol = &l.volumes()[0];
        assert_eq!((vol.usn_journal_id, vol.next_usn), (77, 123_456));
        let photo = l.resolve(EntryRef::Frn(VolumeId(0), 12)).unwrap();
        assert_eq!(l.run_info(photo).count, 1);
        assert_eq!(l.hardlink_count(photo), 2);
        let recent = |st: &IndexStore| {
            (0..st.len() as u32).map(EntryId).filter(|&i| st.date_recently_changed(i).is_known()).count()
        };
        assert_eq!(recent(&l), recent(&s));
        assert!(recent(&l) > 0);
        assert!(l.date_accessed(photo).is_known());
        assert!(path.exists() && !path.with_extension("fnx.tmp").exists());
        // Saving again keeps the previous file as .bak.
        save(&l, &path).unwrap();
        assert!(path.with_extension("fnx.bak").exists());
        // The loaded index keeps working incrementally.
        let mut l = l;
        let st = l.apply(vec![Change::Create {
            volume: VolumeId(0),
            frn: 50,
            parent: EntryRef::Frn(VolumeId(0), 10),
            name: u16s("new.txt"),
            meta: Meta::default(),
        }]);
        assert_eq!(st.created, 1);
    }

    #[test]
    fn every_corruption_is_detected() {
        let s = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.fnx");
        save(&s, &path).unwrap();
        let good = std::fs::read(&path).unwrap();
        assert!(decode(&good).is_ok());
        // Flip one byte at a spread of positions: every one must be rejected (or, for
        // padding bytes, still decode to the same index).
        let step = (good.len() / 97).max(1);
        for i in (0..good.len()).step_by(step) {
            let mut bad = good.clone();
            bad[i] ^= 0x5A;
            match decode(&bad) {
                Err(_) => {}
                Ok(l) => assert_eq!(dump(&l), dump(&s), "byte {i} changed the index silently"),
            }
        }
        for cut in [0, 10, good.len() / 2, good.len() - 1] {
            assert!(decode(&good[..cut]).is_err());
        }
        let mut v2 = good.clone();
        v2[4] = 9;
        assert!(matches!(decode(&v2), Err(PersistError::Version(9))));
    }

    /// Sets the header version of a saved file (the checksums do not cover the header).
    fn with_version(good: &[u8], v: u32) -> Vec<u8> {
        let mut b = good.to_vec();
        b[4..8].copy_from_slice(&v.to_le_bytes());
        b
    }

    #[test]
    fn header_cases_current_old_corrupt_future() {
        let s = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.fnx");
        assert!(matches!(load_or_recover(&path), Loaded::Missing));
        save(&s, &path).unwrap();
        let good = std::fs::read(&path).unwrap();
        // Current layout: loaded, nothing set aside.
        match load_or_recover(&path) {
            Loaded::Index(l) => assert_eq!(dump(&l), dump(&s)),
            _ => panic!("current layout not loaded"),
        }
        assert!(path.exists());
        let cases: [(Vec<u8>, Unreadable, &str); 3] = [
            (with_version(&good, OLDEST_READABLE - 1), Unreadable::Old(OLDEST_READABLE - 1), "index.fnx.v0.bak"),
            (with_version(&good, VERSION + 1), Unreadable::Future(VERSION + 1), "index.fnx.v2.bak"),
            (
                {
                    let mut b = good.clone();
                    let mid = b.len() / 2;
                    b[mid] ^= 0xFF;
                    b
                },
                Unreadable::Corrupt,
                "index.fnx.corrupt.bak",
            ),
        ];
        for (bytes, why, bak) in cases {
            std::fs::write(&path, &bytes).unwrap();
            match load_or_recover(&path) {
                Loaded::Rebuild(r, _) => {
                    assert_eq!(r.why, why);
                    assert_eq!(r.backup.unwrap(), dir.path().join(bak));
                }
                _ => panic!("{why:?}: not set aside"),
            }
            assert!(!path.exists(), "{why:?}: set aside, not left for the next save to overwrite");
            assert_eq!(std::fs::read(dir.path().join(bak)).unwrap(), bytes, "{why:?}: original kept byte for byte");
        }
        // A rebuild saved later never replaces a newer file it did not load (`-reindex`).
        let future = with_version(&good, 7);
        std::fs::write(&path, &future).unwrap();
        save(&s, &path).unwrap();
        assert_eq!(std::fs::read(dir.path().join("index.fnx.v7.bak")).unwrap(), future);
        assert!(load(&path).is_ok());
        assert!(!dir.path().join("index.fnx.bak").exists(), "the newer file went to .v7.bak, not the rolling .bak");
    }

    #[test]
    fn loads_a_database_written_by_0_1_0() {
        // `tests/fixtures/0.1.0/index.fnx`: written by the 0.1.0 sources (layout 1, no
        // pinyin-ranks flag): loads as it is, ids, journal position and run counts kept.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/0.1.0/index.fnx");
        let l = load(&path).unwrap();
        let vol = &l.volumes()[0];
        assert_eq!((vol.info.root_path.as_str(), vol.usn_journal_id, vol.next_usn), ("C:\\", 77, 123_456));
        let photo = l.resolve(EntryRef::Frn(VolumeId(0), 12)).unwrap();
        assert_eq!(l.run_info(photo).count, 2);
        let mut p = String::new();
        l.full_path(l.resolve(EntryRef::Frn(VolumeId(0), 11)).unwrap(), &mut p);
        assert_eq!(p, "C:\\Users\\季度报告.docx");
        assert_eq!(l.live_count(), 7);
    }

    #[test]
    fn large_index_roundtrip_and_compaction() {
        let mut s = IndexStore::default();
        let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "D:\\"), Some(5)).unwrap();
        let mut b = s.bulk_load(vol);
        for i in 0..20_000u64 {
            b.push(100 + i, if i < 100 { 5 } else { 100 + i % 100 }, &u16s(&format!("f{i}.txt")), &Meta::default())
                .unwrap();
        }
        b.finish().unwrap();
        s.rebuild_name_order();
        let doomed: Vec<_> = (200..10_000u64).map(|f| Change::Delete(EntryRef::Frn(vol, f))).collect();
        s.apply(doomed);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.fnx");
        save(&s, &path).unwrap();
        let l = load(&path).unwrap();
        assert_eq!(l.live_count(), s.live_count());
        assert_eq!(l.len(), l.live_count(), "tombstones were compacted away");
        assert_eq!(dump(&l), dump(&s));
    }
}

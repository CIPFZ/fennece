//! Container format shared by `props.fnx`, `content.fnx` and `hashes.fnx` (same layout
//! rules as the index database, ADR-003/012):
//!
//! ```text
//! Header  magic [u8;4] | version u32 | flags u32 | reserved u32 | created FILETIME i64
//! Segment*        8-byte aligned
//! Footer  directory entries { tag [u8;4], 0u32, offset u64, len u64, xxh3 u64 } *
//!         count u32 | 0u32 | xxh3(directory) u64 | magic [u8;4] "END\0"
//! ```
//!
//! Every segment is checksummed; tags may repeat (one segment per property column).
//! Writes are atomic (`.tmp` + fsync + rename, previous file kept as `.bak`).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use fnc_core::FileTime;
use xxhash_rust::xxh3::xxh3_64;

const HEADER_LEN: usize = 24;
const DIR_ENTRY_LEN: usize = 32;
const FOOTER_TAIL_LEN: usize = 24;

/// Errors reading or writing a store file.
#[derive(Debug, thiserror::Error)]
pub enum FnxError {
    /// I/O error.
    #[error("{path}: {source}")]
    Io {
        /// File.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Wrong magic (not this kind of file).
    #[error("not a Fennec {0} file")]
    BadMagic(&'static str),
    /// Another format version.
    #[error("unsupported file version {0}")]
    Version(u32),
    /// A segment failed its checksum.
    #[error("segment {0} is corrupt (checksum)")]
    Checksum(String),
    /// Structurally invalid.
    #[error("file is corrupt: {0}")]
    Corrupt(&'static str),
    /// The store belongs to another index instance.
    #[error("store belongs to another index instance")]
    Instance,
}

/// Result alias.
pub type FnxResult<T> = Result<T, FnxError>;

/// In-memory builder of a container file.
pub struct FnxWriter {
    buf: Vec<u8>,
    dir: Vec<([u8; 4], u64, u64, u64)>,
    magic: [u8; 4],
}

impl FnxWriter {
    /// Starts a file with `magic` and `version`.
    pub fn new(magic: [u8; 4], version: u32) -> Self {
        let mut buf = Vec::with_capacity(1 << 16);
        buf.extend_from_slice(&magic);
        buf.extend_from_slice(&version.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&FileTime::now().0.to_le_bytes());
        Self { buf, dir: Vec::new(), magic }
    }

    fn pad(&mut self) {
        while !self.buf.len().is_multiple_of(8) {
            self.buf.push(0);
        }
    }

    /// Appends a segment.
    pub fn segment(&mut self, tag: [u8; 4], data: &[u8]) {
        self.pad();
        let off = self.buf.len() as u64;
        self.buf.extend_from_slice(data);
        self.dir.push((tag, off, data.len() as u64, xxh3_64(data)));
    }

    /// Finishes the footer and returns the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        self.pad();
        let mut dir = Vec::with_capacity(self.dir.len() * DIR_ENTRY_LEN);
        for (tag, off, len, hash) in &self.dir {
            dir.extend_from_slice(tag);
            dir.extend_from_slice(&0u32.to_le_bytes());
            dir.extend_from_slice(&off.to_le_bytes());
            dir.extend_from_slice(&len.to_le_bytes());
            dir.extend_from_slice(&hash.to_le_bytes());
        }
        self.buf.extend_from_slice(&dir);
        self.buf.extend_from_slice(&(self.dir.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(&0u32.to_le_bytes());
        self.buf.extend_from_slice(&xxh3_64(&dir).to_le_bytes());
        self.buf.extend_from_slice(&self.magic);
        self.buf.extend_from_slice(b"END\0");
        self.buf
    }
}

/// Writes `bytes` to `path` atomically, keeping the previous file as `*.bak`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> FnxResult<()> {
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |source| FnxError::Io { path: p, source }
    };
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let file = File::create(&tmp).map_err(io(&tmp))?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        w.write_all(bytes).map_err(io(&tmp))?;
        let file = w.into_inner().map_err(|e| FnxError::Io { path: tmp.clone(), source: e.into_error() })?;
        file.sync_all().map_err(io(&tmp))?;
    }
    if path.exists() {
        let mut bak = path.as_os_str().to_owned();
        bak.push(".bak");
        let bak = PathBuf::from(bak);
        std::fs::rename(path, &bak).map_err(io(&bak))?;
    }
    std::fs::rename(&tmp, path).map_err(io(path))?;
    Ok(())
}

/// Reads a whole file.
pub fn read_file(path: &Path) -> FnxResult<Vec<u8>> {
    std::fs::read(path).map_err(|source| FnxError::Io { path: path.to_path_buf(), source })
}

/// Verifies header, footer and every checksum; returns the segments in file order.
pub fn segments<'a>(
    buf: &'a [u8],
    magic: [u8; 4],
    version: u32,
    what: &'static str,
) -> FnxResult<Vec<([u8; 4], &'a [u8])>> {
    if buf.len() < HEADER_LEN + FOOTER_TAIL_LEN || buf[..4] != magic {
        return Err(FnxError::BadMagic(what));
    }
    let v = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if v != version {
        return Err(FnxError::Version(v));
    }
    let tail = &buf[buf.len() - FOOTER_TAIL_LEN..];
    if tail[16..20] != magic || &tail[20..] != b"END\0" {
        return Err(FnxError::Corrupt("footer"));
    }
    let count = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as usize;
    let dir_len = count.checked_mul(DIR_ENTRY_LEN).ok_or(FnxError::Corrupt("footer"))?;
    let dir_start = (buf.len() - FOOTER_TAIL_LEN).checked_sub(dir_len).ok_or(FnxError::Corrupt("footer"))?;
    if dir_start < HEADER_LEN {
        return Err(FnxError::Corrupt("footer"));
    }
    let dir = &buf[dir_start..buf.len() - FOOTER_TAIL_LEN];
    let mut h = [0u8; 8];
    h.copy_from_slice(&tail[8..16]);
    if xxh3_64(dir) != u64::from_le_bytes(h) {
        return Err(FnxError::Checksum("directory".into()));
    }
    let mut out = Vec::with_capacity(count);
    for e in dir.as_chunks::<DIR_ENTRY_LEN>().0 {
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&e[..4]);
        let rd = |r: std::ops::Range<usize>| {
            let mut a = [0u8; 8];
            a.copy_from_slice(&e[r]);
            u64::from_le_bytes(a)
        };
        let (off, len, hash) = (rd(8..16), rd(16..24), rd(24..32));
        let end = off.checked_add(len).ok_or(FnxError::Corrupt("segment bounds"))?;
        if off < HEADER_LEN as u64 || end > dir_start as u64 {
            return Err(FnxError::Corrupt("segment bounds"));
        }
        let seg = &buf[off as usize..end as usize];
        if xxh3_64(seg) != hash {
            return Err(FnxError::Checksum(String::from_utf8_lossy(&tag).into_owned()));
        }
        out.push((tag, seg));
    }
    Ok(out)
}

/// Little-endian encoder.
#[derive(Default)]
pub struct Enc(pub Vec<u8>);

impl Enc {
    /// `u8`.
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    /// `u16`.
    pub fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// `u32`.
    pub fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// `u64`.
    pub fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// `i64`.
    pub fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// `f64`.
    pub fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    /// Length-prefixed bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    /// Length-prefixed UTF-8.
    pub fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
}

/// Bounds-checked little-endian decoder.
pub struct Dec<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    /// Decodes `b` from the start.
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    /// `true` when everything was consumed.
    pub fn is_empty(&self) -> bool {
        self.pos >= self.b.len()
    }
    /// Next `n` bytes.
    pub fn take(&mut self, n: usize) -> FnxResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(FnxError::Corrupt("length"))?;
        let s = self.b.get(self.pos..end).ok_or(FnxError::Corrupt("truncated segment"))?;
        self.pos = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> FnxResult<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| FnxError::Corrupt("truncated segment"))
    }
    /// `u8`.
    pub fn u8(&mut self) -> FnxResult<u8> {
        Ok(self.arr::<1>()?[0])
    }
    /// `u16`.
    pub fn u16(&mut self) -> FnxResult<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    /// `u32`.
    pub fn u32(&mut self) -> FnxResult<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    /// `u64`.
    pub fn u64(&mut self) -> FnxResult<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    /// `i64`.
    pub fn i64(&mut self) -> FnxResult<i64> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    /// `f64`.
    pub fn f64(&mut self) -> FnxResult<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    /// Length-prefixed bytes.
    pub fn bytes(&mut self) -> FnxResult<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    /// Length-prefixed UTF-8.
    pub fn str(&mut self) -> FnxResult<&'a str> {
        std::str::from_utf8(self.bytes()?).map_err(|_| FnxError::Corrupt("string"))
    }
    /// Item count where each item needs at least `min` bytes (guards huge allocations).
    pub fn count(&mut self, min: usize) -> FnxResult<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min.max(1)) > self.b.len() - self.pos.min(self.b.len()) {
            return Err(FnxError::Corrupt("count"));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corruption() {
        let mut w = FnxWriter::new(*b"TEST", 3);
        w.segment(*b"AAAA", b"hello");
        w.segment(*b"AAAA", b"again");
        w.segment(*b"BBBB", &[]);
        let bytes = w.finish();
        let segs = segments(&bytes, *b"TEST", 3, "test").unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[1], (*b"AAAA", &b"again"[..]));
        assert!(matches!(segments(&bytes, *b"TEST", 4, "test"), Err(FnxError::Version(3))));
        assert!(matches!(segments(&bytes, *b"NOPE", 3, "test"), Err(FnxError::BadMagic(_))));
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x41;
            // Header padding/reserved/created bytes are not covered; everything else is.
            if (8..24).contains(&i) {
                continue;
            }
            let r = segments(&b, *b"TEST", 3, "test");
            assert!(r.is_err() || r.as_ref().is_ok_and(|s| s == &segs), "flip at {i} undetected");
        }
        assert!(segments(&bytes[..bytes.len() - 1], *b"TEST", 3, "test").is_err());
    }

    #[test]
    fn enc_dec() {
        let mut e = Enc::default();
        e.u8(1);
        e.u16(2);
        e.u32(3);
        e.u64(4);
        e.i64(-5);
        e.f64(2.5);
        e.str("é");
        let mut d = Dec::new(&e.0);
        assert_eq!(d.u8().unwrap(), 1);
        assert_eq!(d.u16().unwrap(), 2);
        assert_eq!(d.u32().unwrap(), 3);
        assert_eq!(d.u64().unwrap(), 4);
        assert_eq!(d.i64().unwrap(), -5);
        assert_eq!(d.f64().unwrap(), 2.5);
        assert_eq!(d.str().unwrap(), "é");
        assert!(d.is_empty());
        assert!(d.u8().is_err());
        let mut d = Dec::new(&[255, 255, 255, 255]);
        assert!(d.count(1).is_err());
    }

    #[test]
    fn atomic_write_keeps_backup() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.fnx");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(read_file(&p).unwrap(), b"two");
        assert_eq!(std::fs::read(dir.path().join("x.fnx.bak")).unwrap(), b"one");
    }
}

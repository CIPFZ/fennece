//! File hashes (`crc32:` `crc64:` `md5:` `sha1:` `sha256:` `sha512:`, SPEC ch.2 §5.4) and
//! their cache (`hashes.fnx`).
//!
//! Hashes are computed on demand for search candidates and cached by file identity:
//! volume (root path), FRN (or the folded path when the volume has no stable FRNs),
//! size and date modified — so a modified file is re-hashed and the cache needs no
//! remapping when the index is compacted or rebuilt.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use fnc_core::EntryId;
use fnc_index::IndexStore;
use md5::Digest;
use parking_lot::Mutex;
use xxhash_rust::xxh3::xxh3_64;

use crate::fnx::{Dec, Enc, FnxError, FnxResult, FnxWriter, segments_versioned};

/// `hashes.fnx` magic.
pub const HASHES_MAGIC: [u8; 4] = *b"FNXH";
/// `hashes.fnx` layout version.
pub const HASHES_VERSION: u32 = 1;
/// Oldest layout still read (ADR-038).
pub const HASHES_OLDEST_READABLE: u32 = 1;

/// Hash algorithm.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HashAlgo {
    /// CRC-32 (IEEE, as zip/7-Zip).
    Crc32,
    /// CRC-64/XZ (ECMA-182 polynomial, reflected; as 7-Zip).
    Crc64,
    /// MD5.
    Md5,
    /// SHA-1.
    Sha1,
    /// SHA-256.
    Sha256,
    /// SHA-512.
    Sha512,
}

impl HashAlgo {
    /// All algorithms.
    pub const ALL: [HashAlgo; 6] =
        [HashAlgo::Crc32, HashAlgo::Crc64, HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256, HashAlgo::Sha512];

    /// Search-function name.
    pub fn name(self) -> &'static str {
        match self {
            HashAlgo::Crc32 => "crc32",
            HashAlgo::Crc64 => "crc64",
            HashAlgo::Md5 => "md5",
            HashAlgo::Sha1 => "sha1",
            HashAlgo::Sha256 => "sha256",
            HashAlgo::Sha512 => "sha512",
        }
    }

    /// Looks up by name (case-insensitive, `-` ignored).
    pub fn lookup(name: &str) -> Option<HashAlgo> {
        let n = crate::schema::normalize(name);
        Self::ALL.into_iter().find(|a| a.name() == n)
    }

    /// Digest length in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            HashAlgo::Crc32 => 4,
            HashAlgo::Crc64 => 8,
            HashAlgo::Md5 => 16,
            HashAlgo::Sha1 => 20,
            HashAlgo::Sha256 => 32,
            HashAlgo::Sha512 => 64,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

const CRC64_POLY: u64 = 0xC96C_5795_D787_0F42;

static CRC64_TABLE: std::sync::LazyLock<[u64; 256]> = std::sync::LazyLock::new(|| {
    let mut t = [0u64; 256];
    for (i, slot) in t.iter_mut().enumerate() {
        let mut c = i as u64;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ CRC64_POLY } else { c >> 1 };
        }
        *slot = c;
    }
    t
});

/// Streaming CRC-64/XZ.
#[derive(Clone)]
pub struct Crc64(u64);

impl Default for Crc64 {
    fn default() -> Self {
        Crc64(u64::MAX)
    }
}

impl Crc64 {
    /// Feeds bytes.
    pub fn update(&mut self, b: &[u8]) {
        let t = &*CRC64_TABLE;
        let mut c = self.0;
        for &x in b {
            c = t[((c ^ u64::from(x)) & 0xff) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    /// Final value.
    pub fn finish(&self) -> u64 {
        self.0 ^ u64::MAX
    }
}

enum Hasher {
    Crc32(crc32fast::Hasher),
    Crc64(Crc64),
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl Hasher {
    fn new(a: HashAlgo) -> Self {
        match a {
            HashAlgo::Crc32 => Hasher::Crc32(crc32fast::Hasher::new()),
            HashAlgo::Crc64 => Hasher::Crc64(Crc64::default()),
            HashAlgo::Md5 => Hasher::Md5(md5::Md5::new()),
            HashAlgo::Sha1 => Hasher::Sha1(sha1::Sha1::new()),
            HashAlgo::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
            HashAlgo::Sha512 => Hasher::Sha512(sha2::Sha512::new()),
        }
    }

    fn update(&mut self, b: &[u8]) {
        match self {
            Hasher::Crc32(h) => h.update(b),
            Hasher::Crc64(h) => h.update(b),
            Hasher::Md5(h) => h.update(b),
            Hasher::Sha1(h) => h.update(b),
            Hasher::Sha256(h) => h.update(b),
            Hasher::Sha512(h) => h.update(b),
        }
    }

    fn finish(self) -> Box<[u8]> {
        match self {
            Hasher::Crc32(h) => h.finalize().to_be_bytes().into(),
            Hasher::Crc64(h) => h.finish().to_be_bytes().into(),
            Hasher::Md5(h) => h.finalize().to_vec().into(),
            Hasher::Sha1(h) => h.finalize().to_vec().into(),
            Hasher::Sha256(h) => h.finalize().to_vec().into(),
            Hasher::Sha512(h) => h.finalize().to_vec().into(),
        }
    }
}

/// Digests per algorithm.
pub type DigestList = Vec<(HashAlgo, Box<[u8]>)>;

/// Digests of `r` for every algorithm in `algos` in one pass; `Ok(None)` if `cancelled()`
/// became true (checked every MiB).
pub fn hash_reader(
    r: &mut dyn Read,
    algos: &[HashAlgo],
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Option<DigestList>> {
    let mut hs: Vec<(HashAlgo, Hasher)> = algos.iter().map(|&a| (a, Hasher::new(a))).collect();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancelled() {
            return Ok(None);
        }
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        for (_, h) in &mut hs {
            h.update(&buf[..n]);
        }
    }
    Ok(Some(hs.into_iter().map(|(a, h)| (a, h.finish())).collect()))
}

/// Digests of the file at `path`.
pub fn hash_file(path: &Path, algos: &[HashAlgo], cancelled: &dyn Fn() -> bool) -> io::Result<Option<DigestList>> {
    hash_reader(&mut File::open(path)?, algos, cancelled)
}

/// Lowercase hex.
pub fn to_hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

/// `true` if `digest`'s hex form starts with `prefix` (hex digits, case-insensitive;
/// spaces, `-` and a `0x` prefix ignored).
pub fn hex_prefix_matches(digest: &[u8], prefix: &str) -> bool {
    let p = prefix.trim();
    let p = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")).unwrap_or(p);
    let mut nibbles = p.chars().filter(|c| !matches!(c, ' ' | '-' | ':'));
    for &b in digest {
        for n in [b >> 4, b & 15] {
            match nibbles.next() {
                None => return true,
                Some(c) => match c.to_digit(16) {
                    Some(d) if d as u8 == n => {}
                    _ => return false,
                },
            }
        }
    }
    nibbles.next().is_none()
}

/// `true` if `s` is a (possibly partial) hex digest.
pub fn is_hex(s: &str) -> bool {
    let p = s.trim();
    let p = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")).unwrap_or(p);
    !p.is_empty() && p.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, ' ' | '-' | ':'))
}

/// Identity of a file's content for the cache.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct HashKey {
    /// xxh3 of the volume root path.
    pub volume: u64,
    /// FRN, or xxh3 of the folded full path for volumes without stable FRNs.
    pub file: u64,
    /// Size in bytes.
    pub size: u64,
    /// Date modified (FILETIME).
    pub dm: i64,
}

impl HashKey {
    /// Key of a file entry (`None` for folders and entries without a known size).
    pub fn of(s: &IndexStore, id: EntryId) -> Option<HashKey> {
        Self::of_with(s, id, &mut Vec::new())
    }

    /// [`HashKey::of`] with a reusable path buffer (no allocation once it has grown).
    pub fn of_with(s: &IndexStore, id: EntryId, buf: &mut Vec<u8>) -> Option<HashKey> {
        if s.is_dir(id) {
            return None;
        }
        let size = s.size(id)?;
        let vol = s.volume_info(s.volume_of(id))?;
        let stable = matches!(vol.info.kind, fnc_index::VolumeKind::Ntfs | fnc_index::VolumeKind::Refs);
        let file = if stable {
            s.frn(id)
        } else {
            buf.clear();
            s.full_path_fold(id, buf);
            xxh3_64(buf)
        };
        Some(HashKey { volume: xxh3_64(vol.info.root_path.as_bytes()), file, size, dm: s.date_modified(id).0 })
    }
}

type Digests = [Option<Box<[u8]>>; 6];

/// Thread-safe cache of computed digests.
#[derive(Debug)]
pub struct HashCache {
    map: Mutex<HashMap<HashKey, Digests>>,
    generation: AtomicU64,
    /// Entry limit; beyond it the cache is cleared (default 4 M).
    pub max_entries: usize,
}

impl Default for HashCache {
    fn default() -> Self {
        Self { map: Mutex::new(HashMap::new()), generation: AtomicU64::new(0), max_entries: 4 << 20 }
    }
}

impl HashCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of cached files.
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Incremented by every insertion (save when it changed).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Cached digest.
    pub fn get(&self, key: &HashKey, algo: HashAlgo) -> Option<Box<[u8]>> {
        self.map.lock().get(key).and_then(|d| d[algo.index()].clone())
    }

    /// Stores a digest.
    pub fn insert(&self, key: HashKey, algo: HashAlgo, digest: Box<[u8]>) {
        if digest.len() != algo.digest_len() {
            return;
        }
        let mut m = self.map.lock();
        if m.len() >= self.max_entries.max(1) && !m.contains_key(&key) {
            m.clear();
        }
        m.entry(key).or_default()[algo.index()] = Some(digest);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Cached digest, or computes the missing ones of `algos` for the file at `path`
    /// (one pass) and caches them. `Ok(None)` when cancelled.
    pub fn get_or_compute(
        &self,
        key: HashKey,
        path: &Path,
        algo: HashAlgo,
        also: &[HashAlgo],
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Option<Box<[u8]>>> {
        if let Some(d) = self.get(&key, algo) {
            return Ok(Some(d));
        }
        let mut algos = vec![algo];
        for &a in also {
            if !algos.contains(&a) && self.get(&key, a).is_none() {
                algos.push(a);
            }
        }
        let Some(out) = hash_file(path, &algos, cancelled)? else { return Ok(None) };
        let mut found = None;
        for (a, d) in out {
            if a == algo {
                found = Some(d.clone());
            }
            self.insert(key, a, d);
        }
        Ok(found)
    }

    /// Serialises the cache.
    pub fn to_bytes(&self) -> Vec<u8> {
        let m = self.map.lock();
        let mut e = Enc::default();
        e.u32(m.len() as u32);
        for (k, d) in m.iter() {
            e.u64(k.volume);
            e.u64(k.file);
            e.u64(k.size);
            e.i64(k.dm);
            let mask: u8 = d.iter().enumerate().filter(|(_, v)| v.is_some()).fold(0, |a, (i, _)| a | (1 << i));
            e.u8(mask);
            for v in d.iter().flatten() {
                e.0.extend_from_slice(v);
            }
        }
        let mut w = FnxWriter::new(HASHES_MAGIC, HASHES_VERSION);
        w.segment(*b"HASH", &e.0);
        w.finish()
    }

    /// Saves atomically.
    pub fn save(&self, path: &Path) -> FnxResult<()> {
        crate::fnx::write_atomic(path, &self.to_bytes())
    }

    /// Loads a cache file.
    pub fn load(path: &Path) -> FnxResult<Self> {
        Self::from_bytes(&crate::fnx::read_file(path)?)
    }

    /// Decodes cache bytes.
    pub fn from_bytes(buf: &[u8]) -> FnxResult<Self> {
        // One decoder per readable layout; layout 1 is the only one so far.
        let (_layout, segs) =
            segments_versioned(buf, HASHES_MAGIC, HASHES_OLDEST_READABLE, HASHES_VERSION, "hash cache")?;
        let seg =
            segs.iter().find(|(t, _)| t == b"HASH").map(|(_, s)| *s).ok_or(FnxError::Corrupt("missing segment"))?;
        let mut d = Dec::new(seg);
        let n = d.count(33)?;
        let mut map = HashMap::with_capacity(n);
        for _ in 0..n {
            let key = HashKey { volume: d.u64()?, file: d.u64()?, size: d.u64()?, dm: d.i64()? };
            let mask = d.u8()?;
            let mut v: Digests = Default::default();
            for a in HashAlgo::ALL {
                if mask & (1 << a.index()) != 0 {
                    v[a.index()] = Some(d.take(a.digest_len())?.into());
                }
            }
            map.insert(key, v);
        }
        Ok(Self { map: Mutex::new(map), ..Self::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_of(data: &[u8], a: HashAlgo) -> String {
        let r = hash_reader(&mut io::Cursor::new(data), &[a], &|| false).unwrap().unwrap();
        to_hex(&r[0].1)
    }

    #[test]
    fn known_vectors() {
        let d = b"123456789";
        assert_eq!(hex_of(d, HashAlgo::Crc32), "cbf43926");
        assert_eq!(hex_of(d, HashAlgo::Crc64), "995dc9bbdf1939fa");
        assert_eq!(hex_of(b"abc", HashAlgo::Md5), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex_of(b"abc", HashAlgo::Sha1), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex_of(b"abc", HashAlgo::Sha256),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(hex_of(b"abc", HashAlgo::Sha512).starts_with("ddaf35a193617aba"));
        assert_eq!(hex_of(b"", HashAlgo::Crc32), "00000000");
        let all = hash_reader(&mut io::Cursor::new(b"abc"), &HashAlgo::ALL, &|| false).unwrap().unwrap();
        assert_eq!(all.len(), 6);
        assert!(all.iter().all(|(a, d)| d.len() == a.digest_len()));
        assert!(hash_reader(&mut io::Cursor::new(b"abc"), &[HashAlgo::Md5], &|| true).unwrap().is_none());
        assert_eq!(HashAlgo::lookup("SHA-256"), Some(HashAlgo::Sha256));
    }

    #[test]
    fn hex_prefixes() {
        let d = [0xba, 0x78, 0x16];
        assert!(hex_prefix_matches(&d, "ba78"));
        assert!(hex_prefix_matches(&d, "BA7816"));
        assert!(hex_prefix_matches(&d, "0xba7"));
        assert!(hex_prefix_matches(&d, ""));
        assert!(!hex_prefix_matches(&d, "ba79"));
        assert!(!hex_prefix_matches(&d, "ba781600"));
        assert!(!hex_prefix_matches(&d, "zz"));
        assert!(is_hex("DEAD-beef") && !is_hex("unknown") && !is_hex(""));
    }

    #[test]
    fn cache_compute_persist() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"abc").unwrap();
        let c = HashCache::new();
        let key = HashKey { volume: 1, file: 2, size: 3, dm: 4 };
        assert!(c.get(&key, HashAlgo::Sha256).is_none());
        let d = c.get_or_compute(key, &p, HashAlgo::Sha256, &[HashAlgo::Md5], &|| false).unwrap().unwrap();
        assert_eq!(to_hex(&d), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(c.get(&key, HashAlgo::Md5).is_some(), "computed in the same pass");
        // Cached: the file is not read again.
        std::fs::remove_file(&p).unwrap();
        assert!(c.get_or_compute(key, &p, HashAlgo::Sha256, &[], &|| false).unwrap().is_some());
        assert!(c.get_or_compute(key, &p, HashAlgo::Crc32, &[], &|| false).is_err());
        let bytes = c.to_bytes();
        let back = HashCache::from_bytes(&bytes).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.get(&key, HashAlgo::Md5), c.get(&key, HashAlgo::Md5));
        assert!(back.get(&key, HashAlgo::Sha1).is_none());
        let mut bad = bytes.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 1;
        assert!(HashCache::from_bytes(&bad).is_err());
        c.insert(key, HashAlgo::Sha1, vec![0u8; 3].into());
        assert!(c.get(&key, HashAlgo::Sha1).is_none(), "wrong length rejected");
        let small = HashCache { max_entries: 1, ..HashCache::new() };
        small.insert(key, HashAlgo::Crc32, vec![0u8; 4].into());
        small.insert(HashKey { file: 9, ..key }, HashAlgo::Crc32, vec![0u8; 4].into());
        assert_eq!(small.len(), 1);
        assert!(small.generation() >= 2);
    }
}

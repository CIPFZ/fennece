//! Vector store (SPEC ch.5 §3.1 "向量存储", §3.4): an HNSW index (usearch, cosine, int8)
//! over chunk vectors plus the chunk table `chunk → (document, kind, byte range, text
//! hash)`. Documents are keyed by full path so the store survives index rebuilds and
//! entry renumbering; they are mapped to entries when a query runs.
//!
//! On disk: `store.bin` (chunk table, xxh3-checked) and `vectors.usearch`, both written
//! to `.tmp` and renamed. A different model or dimension starts an empty store.

use std::collections::HashMap;
use std::path::Path;

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::embed::{cosine, fit};
use crate::{ALL_KINDS, Kind, KindMask, Result, SemError};

/// One chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkRec {
    /// Owning document.
    pub doc: u32,
    /// What it was made from.
    pub kind: Kind,
    /// Byte range in the document text (0..0 for names).
    pub start: u32,
    /// Byte length.
    pub len: u32,
    /// xxh3 of the embedded text: unchanged chunks are not embedded again.
    pub hash: u64,
}

/// One document (file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRec {
    /// Full path.
    pub path: String,
    /// Size when indexed.
    pub size: u64,
    /// Date modified when indexed (FILETIME ticks).
    pub mtime: i64,
    /// Its chunk keys.
    pub chunks: Vec<u64>,
}

/// A chunk to store: with a vector, or `None` to keep the document's existing chunk of the
/// same kind and hash.
pub struct NewChunk {
    /// Kind.
    pub kind: Kind,
    /// Byte offset.
    pub start: u32,
    /// Byte length.
    pub len: u32,
    /// Text hash.
    pub hash: u64,
    /// Full-dimension embedding (truncated to the store's dimension here).
    pub vector: Option<Vec<f32>>,
}

/// Best chunk of a document for a query.
#[derive(Clone, Debug, PartialEq)]
pub struct DocHit {
    /// Document path.
    pub path: String,
    /// Cosine similarity of its best chunk.
    pub score: f32,
    /// Kind of that chunk.
    pub kind: Kind,
    /// Its byte range (preview highlighting).
    pub start: u32,
    /// Its length.
    pub len: u32,
}

const MAGIC: &[u8; 4] = b"FSEM";
const VERSION: u32 = 1;

/// Paths compare case-insensitively (Windows file systems).
fn fold(path: &str) -> String {
    path.to_lowercase()
}

/// Documents, chunks and their vectors.
pub struct VectorStore {
    model_id: String,
    dim: usize,
    index: Index,
    docs: Vec<Option<DocRec>>,
    free_docs: Vec<u32>,
    by_path: HashMap<String, u32>,
    chunks: HashMap<u64, ChunkRec>,
    next_key: u64,
    removed: usize,
    dirty: bool,
}

fn new_index(dim: usize) -> Result<Index> {
    let opts = IndexOptions {
        dimensions: dim,
        metric: MetricKind::Cos,
        quantization: ScalarKind::I8,
        connectivity: 16,
        expansion_add: 128,
        expansion_search: 64,
        multi: false,
    };
    Index::new(&opts).map_err(|e| SemError::Index(e.to_string()))
}

fn idx_err(e: impl std::fmt::Display) -> SemError {
    SemError::Index(e.to_string())
}

impl VectorStore {
    /// An empty store for `model_id` vectors truncated to `dim`.
    pub fn new(model_id: &str, dim: usize) -> Result<Self> {
        Ok(Self {
            model_id: model_id.into(),
            dim,
            index: new_index(dim)?,
            docs: Vec::new(),
            free_docs: Vec::new(),
            by_path: HashMap::new(),
            chunks: HashMap::new(),
            next_key: 1,
            removed: 0,
            dirty: false,
        })
    }

    /// Loads the store in `dir`; a missing, damaged or incompatible (other model or
    /// dimension) store yields an empty one and `true` ("rebuild").
    pub fn open(dir: &Path, model_id: &str, dim: usize) -> Result<(Self, bool)> {
        match Self::load(dir, model_id, dim) {
            Ok(Some(s)) => Ok((s, false)),
            Ok(None) => Ok((Self::new(model_id, dim)?, true)),
            Err(e) => {
                tracing::warn!(error = %e, "semantic store unusable, starting over");
                Ok((Self::new(model_id, dim)?, true))
            }
        }
    }

    fn load(dir: &Path, model_id: &str, dim: usize) -> Result<Option<Self>> {
        let bytes = match std::fs::read(dir.join("store.bin")) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let corrupt = |m: &str| SemError::Corrupt(m.to_string());
        if bytes.len() < 16 || &bytes[..4] != MAGIC {
            return Err(corrupt("bad header"));
        }
        let (body, sum) = bytes.split_at(bytes.len() - 8);
        if xxhash_rust::xxh3::xxh3_64(body) != u64::from_le_bytes(sum.try_into().unwrap_or_default()) {
            return Err(corrupt("checksum mismatch"));
        }
        let mut r = Reader { b: body, at: 4 };
        if r.u32()? != VERSION {
            return Err(corrupt("unknown version"));
        }
        let stored_model = r.str()?;
        let stored_dim = r.u32()? as usize;
        if stored_model != model_id || stored_dim != dim {
            tracing::info!(%stored_model, stored_dim, model_id, dim, "semantic model changed, rebuilding vectors");
            return Ok(None);
        }
        let mut s = Self::new(model_id, dim)?;
        s.next_key = r.u64()?;
        let ndocs = r.u32()?;
        for _ in 0..ndocs {
            let path = r.str()?;
            let size = r.u64()?;
            let mtime = r.u64()? as i64;
            let n = r.u32()?;
            let doc = s.docs.len() as u32;
            let mut keys = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let key = r.u64()?;
                let kind = Kind::from_u8(r.u8()?).ok_or_else(|| corrupt("bad kind"))?;
                let rec = ChunkRec { doc, kind, start: r.u32()?, len: r.u32()?, hash: r.u64()? };
                s.chunks.insert(key, rec);
                keys.push(key);
            }
            s.by_path.insert(fold(&path), doc);
            s.docs.push(Some(DocRec { path, size, mtime, chunks: keys }));
        }
        let vec_path = dir.join("vectors.usearch");
        s.index.load(&vec_path.to_string_lossy()).map_err(idx_err)?;
        if s.index.size() != s.chunks.len() || s.chunks.keys().any(|&k| !s.index.contains(k)) {
            return Err(corrupt("vectors do not match the chunk table"));
        }
        Ok(Some(s))
    }

    /// Writes the store to `dir` if it changed.
    pub fn save(&mut self, dir: &Path) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        std::fs::create_dir_all(dir)?;
        let mut b = Vec::with_capacity(64 + self.chunks.len() * 40);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        put_str(&mut b, &self.model_id);
        b.extend_from_slice(&(self.dim as u32).to_le_bytes());
        b.extend_from_slice(&self.next_key.to_le_bytes());
        let live: Vec<&DocRec> = self.docs.iter().flatten().collect();
        b.extend_from_slice(&(live.len() as u32).to_le_bytes());
        for d in live {
            put_str(&mut b, &d.path);
            b.extend_from_slice(&d.size.to_le_bytes());
            b.extend_from_slice(&(d.mtime as u64).to_le_bytes());
            b.extend_from_slice(&(d.chunks.len() as u32).to_le_bytes());
            for k in &d.chunks {
                let c = &self.chunks[k];
                b.extend_from_slice(&k.to_le_bytes());
                b.push(c.kind as u8);
                b.extend_from_slice(&c.start.to_le_bytes());
                b.extend_from_slice(&c.len.to_le_bytes());
                b.extend_from_slice(&c.hash.to_le_bytes());
            }
        }
        let sum = xxhash_rust::xxh3::xxh3_64(&b);
        b.extend_from_slice(&sum.to_le_bytes());
        let vec_tmp = dir.join("vectors.usearch.tmp");
        self.index.save(&vec_tmp.to_string_lossy()).map_err(idx_err)?;
        let store_tmp = dir.join("store.bin.tmp");
        std::fs::write(&store_tmp, &b)?;
        std::fs::rename(&vec_tmp, dir.join("vectors.usearch"))?;
        std::fs::rename(&store_tmp, dir.join("store.bin"))?;
        self.dirty = false;
        Ok(())
    }

    /// Model the vectors came from.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Stored dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Number of documents.
    pub fn doc_count(&self) -> usize {
        self.by_path.len()
    }

    /// Number of chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Approximate memory in bytes (vectors and graph).
    pub fn memory_bytes(&self) -> usize {
        self.index.memory_usage() + self.chunks.len() * 48 + self.by_path.len() * 96
    }

    /// The document at `path`.
    pub fn doc(&self, path: &str) -> Option<&DocRec> {
        self.by_path.get(&fold(path)).and_then(|&d| self.docs[d as usize].as_ref())
    }

    /// Every document.
    pub fn docs(&self) -> impl Iterator<Item = &DocRec> {
        self.docs.iter().flatten()
    }

    /// `true` if the document at `path` has a chunk of `kind` with text hash `hash`.
    pub fn has_chunk(&self, path: &str, kind: Kind, hash: u64) -> bool {
        self.doc(path).is_some_and(|d| {
            d.chunks.iter().any(|k| self.chunks.get(k).is_some_and(|c| c.kind == kind && c.hash == hash))
        })
    }

    fn reserve_one(&mut self) -> Result<()> {
        if self.index.size() + 1 > self.index.capacity() {
            let cap = (self.index.capacity() * 2).max(1024);
            self.index.reserve(cap).map_err(idx_err)?;
        }
        Ok(())
    }

    /// Replaces the chunks of the document at `path` (SPEC ch.5 §3.4: unchanged chunks keep
    /// their vectors).
    pub fn put_doc(&mut self, path: &str, size: u64, mtime: i64, chunks: Vec<NewChunk>) -> Result<()> {
        let key = fold(path);
        let doc = match self.by_path.get(&key) {
            Some(&d) => d,
            None => {
                let d = match self.free_docs.pop() {
                    Some(d) => d,
                    None => {
                        self.docs.push(None);
                        (self.docs.len() - 1) as u32
                    }
                };
                self.by_path.insert(key, d);
                self.docs[d as usize] = Some(DocRec { path: path.into(), size, mtime, chunks: Vec::new() });
                d
            }
        };
        let mut old: Vec<u64> =
            self.docs[doc as usize].as_mut().map(|d| std::mem::take(&mut d.chunks)).unwrap_or_default();
        let mut keys = Vec::with_capacity(chunks.len());
        for c in chunks {
            match c.vector {
                None => {
                    // Reuse an old chunk with the same text.
                    if let Some(pos) = old
                        .iter()
                        .position(|k| self.chunks.get(k).is_some_and(|r| r.kind == c.kind && r.hash == c.hash))
                    {
                        let k = old.swap_remove(pos);
                        if let Some(r) = self.chunks.get_mut(&k) {
                            r.start = c.start;
                            r.len = c.len;
                        }
                        keys.push(k);
                    }
                }
                Some(v) => {
                    self.reserve_one()?;
                    let k = self.next_key;
                    self.next_key += 1;
                    self.index.add(k, &fit(&v, self.dim)).map_err(idx_err)?;
                    self.chunks.insert(k, ChunkRec { doc, kind: c.kind, start: c.start, len: c.len, hash: c.hash });
                    keys.push(k);
                }
            }
        }
        for k in old {
            self.drop_chunk(k)?;
        }
        if let Some(d) = self.docs[doc as usize].as_mut() {
            d.path = path.into();
            d.size = size;
            d.mtime = mtime;
            d.chunks = keys;
        }
        self.dirty = true;
        Ok(())
    }

    fn drop_chunk(&mut self, k: u64) -> Result<()> {
        if self.chunks.remove(&k).is_some() {
            self.index.remove(k).map_err(idx_err)?;
            self.removed += 1;
        }
        Ok(())
    }

    /// Removes the document at `path` (logical delete of its vectors).
    pub fn remove_doc(&mut self, path: &str) -> Result<bool> {
        let Some(d) = self.by_path.remove(&fold(path)) else { return Ok(false) };
        if let Some(rec) = self.docs[d as usize].take() {
            for k in rec.chunks {
                self.drop_chunk(k)?;
            }
        }
        self.free_docs.push(d);
        self.dirty = true;
        Ok(true)
    }

    fn hit(&self, key: u64, score: f32) -> Option<(u32, DocHit)> {
        let c = self.chunks.get(&key)?;
        let d = self.docs[c.doc as usize].as_ref()?;
        Some((c.doc, DocHit { path: d.path.clone(), score, kind: c.kind, start: c.start, len: c.len }))
    }

    /// Up to `k` documents nearest to `query` (full-dimension, unit length) among chunks of
    /// `kinds`, best chunk per document, scores ≥ `min_score`, best first.
    pub fn search(&self, query: &[f32], k: usize, kinds: KindMask, min_score: f32) -> Result<Vec<DocHit>> {
        if self.chunks.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let q = fit(query, self.dim);
        // Several chunks may belong to one document: ask for more chunks than documents.
        let want = (k * 3).min(self.chunks.len());
        let m = if kinds & ALL_KINDS == ALL_KINDS {
            self.index.search(&q, want)
        } else {
            self.index.filtered_search(&q, want, |key| self.chunks.get(&key).is_some_and(|c| kinds & c.kind.bit() != 0))
        }
        .map_err(idx_err)?;
        let mut best: HashMap<u32, DocHit> = HashMap::new();
        for (key, dist) in m.keys.iter().zip(&m.distances) {
            let score = 1.0 - dist;
            if score < min_score {
                continue;
            }
            if let Some((doc, h)) = self.hit(*key, score) {
                let e = best.entry(doc).or_insert_with(|| h.clone());
                if h.score > e.score {
                    *e = h;
                }
            }
        }
        let mut out: Vec<DocHit> = best.into_values().collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out.truncate(k);
        Ok(out)
    }

    /// Exact scores of the documents at `paths` (brute force over their chunks; used when
    /// the literal filters leave few candidates, SPEC ch.5 §3.2).
    pub fn score_paths<'a>(
        &self,
        query: &[f32],
        paths: impl Iterator<Item = &'a str>,
        kinds: KindMask,
        min_score: f32,
    ) -> Vec<DocHit> {
        let q = fit(query, self.dim);
        let mut buf = vec![0.0f32; self.dim];
        let mut out = Vec::new();
        for p in paths {
            let Some(d) = self.doc(p) else { continue };
            let mut best: Option<DocHit> = None;
            for &k in &d.chunks {
                let Some(c) = self.chunks.get(&k) else { continue };
                if kinds & c.kind.bit() == 0 || self.index.get(k, &mut buf).is_err() {
                    continue;
                }
                let mut v = buf.clone();
                crate::embed::normalize(&mut v);
                let score = cosine(&q, &v);
                if score >= min_score && best.as_ref().is_none_or(|b| score > b.score) {
                    best = Some(DocHit { path: d.path.clone(), score, kind: c.kind, start: c.start, len: c.len });
                }
            }
            out.extend(best);
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out
    }

    /// Rebuilds the graph when at least a fifth of the vectors were removed (SPEC ch.5 §3.4).
    pub fn compact_if_needed(&mut self) -> Result<bool> {
        if self.removed * 5 < self.chunks.len().max(1) || self.removed < 1000 {
            return Ok(false);
        }
        let fresh = new_index(self.dim)?;
        fresh.reserve(self.chunks.len().max(1024)).map_err(idx_err)?;
        let mut buf = vec![0.0f32; self.dim];
        for &k in self.chunks.keys() {
            self.index.get(k, &mut buf).map_err(idx_err)?;
            fresh.add(k, &buf).map_err(idx_err)?;
        }
        self.index = fresh;
        self.removed = 0;
        self.dirty = true;
        Ok(true)
    }
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let s = self.b.get(self.at..self.at + n).ok_or_else(|| SemError::Corrupt("truncated".into()))?;
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap_or_default()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap_or_default()))
    }
    fn str(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| SemError::Corrupt("bad text".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::{Embedder, HashEmbedder, Lane};

    fn chunk(e: &HashEmbedder, kind: Kind, text: &str) -> NewChunk {
        let v = e.embed(Lane::Index, &[text.into()]).unwrap().pop().unwrap();
        NewChunk {
            kind,
            start: 0,
            len: text.len() as u32,
            hash: xxhash_rust::xxh3::xxh3_64(text.as_bytes()),
            vector: Some(v),
        }
    }

    #[test]
    fn search_update_remove_and_persist() {
        let e = HashEmbedder::new(64);
        let mut s = VectorStore::new("hash", 64).unwrap();
        s.put_doc(r"C:\a\budget.xlsx", 10, 1, vec![chunk(&e, Kind::Name, "quarterly budget")]).unwrap();
        s.put_doc(r"C:\a\photos.zip", 10, 1, vec![chunk(&e, Kind::Name, "holiday photos")]).unwrap();
        s.put_doc(
            r"C:\a\notes.md",
            10,
            1,
            vec![chunk(&e, Kind::Name, "notes"), chunk(&e, Kind::Content, "budget review for the quarter")],
        )
        .unwrap();
        let q = e.embed(Lane::Query, &["budget".into()]).unwrap().pop().unwrap();
        let hits = s.search(&q, 10, ALL_KINDS, 0.45).unwrap();
        assert_eq!(hits[0].path, r"C:\a\budget.xlsx");
        assert!(hits.iter().all(|h| h.path != r"C:\a\photos.zip"), "{hits:?}");
        // Kind filter.
        let content = s.search(&q, 10, Kind::Content.bit(), 0.1).unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!((content[0].path.as_str(), content[0].kind), (r"C:\a\notes.md", Kind::Content));
        // Brute force agrees.
        let exact =
            s.score_paths(&q, [r"c:\A\BUDGET.xlsx", r"C:\a\photos.zip", r"C:\none"].into_iter(), ALL_KINDS, 0.45);
        assert_eq!(exact.len(), 1);
        // Unchanged chunks are reused, changed ones replaced.
        assert!(s.has_chunk(r"C:\a\notes.md", Kind::Name, xxhash_rust::xxh3::xxh3_64(b"notes")));
        let keep = NewChunk { vector: None, ..chunk(&e, Kind::Name, "notes") };
        s.put_doc(r"C:\a\notes.md", 11, 2, vec![keep, chunk(&e, Kind::Content, "holiday plans")]).unwrap();
        assert_eq!(s.chunk_count(), 4);
        assert_eq!(s.search(&q, 10, Kind::Content.bit(), 0.3).unwrap().len(), 0);
        assert!(s.remove_doc(r"C:\A\PHOTOS.ZIP").unwrap());
        assert_eq!((s.doc_count(), s.chunk_count()), (2, 3));

        let d = tempfile::tempdir().unwrap();
        s.save(d.path()).unwrap();
        let (mut t, rebuilt) = VectorStore::open(d.path(), "hash", 64).unwrap();
        assert!(!rebuilt);
        assert_eq!((t.doc_count(), t.chunk_count()), (2, 3));
        assert_eq!(t.search(&q, 10, ALL_KINDS, 0.45).unwrap()[0].path, r"C:\a\budget.xlsx");
        assert_eq!(t.doc(r"c:\a\notes.md").unwrap().mtime, 2);
        // New keys continue after the loaded ones.
        t.put_doc(r"C:\b.txt", 1, 1, vec![chunk(&e, Kind::Name, "b")]).unwrap();
        assert_eq!(t.chunk_count(), 4);
        // Another model or dimension: empty store.
        assert!(VectorStore::open(d.path(), "other", 64).unwrap().1);
        assert!(VectorStore::open(d.path(), "hash", 32).unwrap().1);
        // Damage: empty store, no panic.
        let p = d.path().join("store.bin");
        let mut b = std::fs::read(&p).unwrap();
        b[10] ^= 0xFF;
        std::fs::write(&p, b).unwrap();
        let (u, rebuilt) = VectorStore::open(d.path(), "hash", 64).unwrap();
        assert!(rebuilt && u.doc_count() == 0);
    }

    #[test]
    fn compaction_keeps_vectors() {
        let e = HashEmbedder::new(32);
        let mut s = VectorStore::new("hash", 32).unwrap();
        for i in 0..1500 {
            s.put_doc(&format!("C:\\f{i}.txt"), 1, 1, vec![chunk(&e, Kind::Name, &format!("file number {i}"))])
                .unwrap();
        }
        for i in 0..1200 {
            s.remove_doc(&format!("C:\\f{i}.txt")).unwrap();
        }
        assert!(s.compact_if_needed().unwrap());
        let q = e.embed(Lane::Query, &["file number 1400".into()]).unwrap().pop().unwrap();
        assert_eq!(s.search(&q, 1, ALL_KINDS, 0.0).unwrap()[0].path, "C:\\f1400.txt");
    }
}

//! The semantic engine: owns the model host and the vector store, runs the background
//! semantic indexer and answers `sem:` queries (SPEC ch.5 §3).
//!
//! The indexer polls the file index (through [`Catalog`]) when its generation changes,
//! embeds new files right away and changed files after a debounce (10 s), and drops
//! deleted files at once (SEM-05). It embeds on the index lane, whose thread count is the
//! CPU limit (SEM-01); queries use their own lane. The model is unloaded when idle (SEM-08).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};

use crate::chunk;
use crate::embed::{Embedder, ModelHost};
use crate::prompt::Task;
use crate::store::{DocHit, NewChunk, VectorStore};
use crate::{ALL_KINDS, Kind, KindMask, Result, SemError};

/// What gets embedded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// Folders (full paths); files outside are never embedded.
    pub folders: Vec<String>,
    /// Extensions whose names get a name chunk (empty: every file).
    pub name_exts: Vec<String>,
    /// Extensions whose text content is chunked.
    pub text_exts: Vec<String>,
    /// Extensions chunked as code.
    pub code_exts: Vec<String>,
    /// Largest file whose content is read (bytes).
    pub max_size: u64,
    /// Chunks per file at most.
    pub max_chunks: usize,
    /// Kinds enabled.
    pub kinds: KindMask,
}

impl Default for Scope {
    fn default() -> Self {
        let v = |s: &str| s.split(';').map(str::to_string).collect::<Vec<_>>();
        Self {
            folders: Vec::new(),
            name_exts: v(
                "doc;docx;docm;dot;dotx;odt;rtf;txt;md;pdf;xls;xlsx;xlsm;ods;csv;ppt;pptx;odp;key;pages;numbers;epub;\
                jpg;jpeg;png;gif;bmp;tif;tiff;webp;heic;raw;cr2;nef;svg;psd;mp4;mkv;avi;mov;wmv;webm;mp3;flac;wav;m4a;ogg;\
                zip;7z;rar",
            ),
            text_exts: v(
                "txt;md;markdown;rst;org;tex;log;csv;tsv;html;htm;xml;json;docx;pptx;xlsx;odt;ods;odp;rtf;pdf;eml",
            ),
            code_exts: v(
                "rs;py;js;mjs;ts;tsx;jsx;go;java;kt;kts;c;h;cpp;hpp;cc;cxx;cs;rb;php;swift;lua;sh;bash;ps1;psm1;sql;\
                scala;m;mm;vue;svelte;dart;zig;r;jl;hs;ex;exs;erl;fs;vb;pl;toml;yaml;yml;gradle;cmake",
            ),
            max_size: 16 << 20,
            max_chunks: 32,
            kinds: Kind::Name.bit() | Kind::Content.bit() | Kind::Code.bit(),
        }
    }
}

fn ext_of(path: &str) -> &str {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    name.rsplit_once('.').map_or("", |(_, e)| e)
}

fn has_ext(list: &[String], ext: &str) -> bool {
    !ext.is_empty() && list.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

fn under(path: &str, folder: &str) -> bool {
    let f = folder.trim_end_matches(['\\', '/']);
    path.len() > f.len()
        && path.is_char_boundary(f.len())
        && path[..f.len()].eq_ignore_ascii_case(f)
        && matches!(path.as_bytes()[f.len()], b'\\' | b'/')
}

impl Scope {
    /// `true` if `path` is inside one of the folders.
    pub fn contains(&self, path: &str) -> bool {
        self.folders.iter().any(|f| under(path, f))
    }

    /// Kinds to embed for a file (0 = none).
    pub fn kinds_for(&self, path: &str, size: u64) -> KindMask {
        if !self.contains(path) {
            return 0;
        }
        let ext = ext_of(path);
        let mut k = 0;
        if self.kinds & Kind::Name.bit() != 0 && (self.name_exts.is_empty() || has_ext(&self.name_exts, ext)) {
            k |= Kind::Name.bit();
        }
        if size <= self.max_size {
            if self.kinds & Kind::Code.bit() != 0 && has_ext(&self.code_exts, ext) {
                k |= Kind::Code.bit();
            } else if self.kinds & Kind::Content.bit() != 0 && has_ext(&self.text_exts, ext) {
                k |= Kind::Content.bit();
            }
        }
        k
    }
}

/// A file the catalog reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Full path.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Date modified (FILETIME ticks).
    pub mtime: i64,
}

/// The file index as the semantic indexer sees it.
pub trait Catalog: Send + Sync {
    /// Changes whenever the file index changes.
    fn generation(&self) -> u64;
    /// Calls `out` for every file below `folders`.
    fn files(&self, folders: &[String], out: &mut dyn FnMut(Candidate));
}

/// Text of a document (content extraction).
pub trait TextSource: Send + Sync {
    /// The text of `path` (at most `max_bytes` read), `None` if it has none.
    fn text(&self, path: &str, max_bytes: u64) -> Option<String>;
}

/// Reads plain-text files (UTF-8, UTF-16 with BOM); binary files have no text.
pub struct PlainText;

impl TextSource for PlainText {
    fn text(&self, path: &str, max_bytes: u64) -> Option<String> {
        use std::io::Read;
        let mut b = Vec::new();
        std::fs::File::open(path).ok()?.take(max_bytes).read_to_end(&mut b).ok()?;
        let utf16 = |be: bool, b: &[u8]| {
            let u: Vec<u16> = b
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| if be { u16::from_be_bytes(*c) } else { u16::from_le_bytes(*c) })
                .collect();
            String::from_utf16_lossy(&u)
        };
        Some(match b.as_slice() {
            [0xFF, 0xFE, rest @ ..] => utf16(false, rest),
            [0xFE, 0xFF, rest @ ..] => utf16(true, rest),
            [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
            b if b.iter().take(4096).any(|&c| c == 0) => return None,
            b => String::from_utf8_lossy(b).into_owned(),
        })
    }
}

/// Engine settings.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineConfig {
    /// Folder of `store.bin` / `vectors.usearch`.
    pub dir: PathBuf,
    /// Model id (stored with the vectors).
    pub model_id: String,
    /// Stored dimension.
    pub dim: usize,
    /// What to embed.
    pub scope: Scope,
    /// Unload the model after this long unused.
    pub idle_unload: Duration,
    /// Changed files wait this long before they are embedded again.
    pub debounce: Duration,
    /// Default similarity threshold.
    pub min_score: f32,
    /// Default number of semantic candidates.
    pub top: usize,
    /// Stop background indexing.
    pub paused: bool,
}

impl EngineConfig {
    /// SPEC defaults for a store in `dir`.
    pub fn new(dir: PathBuf, model_id: &str) -> Self {
        Self {
            dir,
            model_id: model_id.into(),
            dim: 256,
            scope: Scope::default(),
            idle_unload: Duration::from_secs(600),
            debounce: Duration::from_secs(10),
            min_score: 0.45,
            top: 200,
            paused: false,
        }
    }
}

/// What the status bar shows (SPEC ch.5 §3.3).
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Nothing in scope.
    Idle,
    /// The model file is missing (SEM-07).
    NoModel(PathBuf),
    /// Loading the model.
    Loading,
    /// Up to date.
    Ready {
        /// Documents indexed.
        docs: usize,
        /// Chunks indexed.
        chunks: usize,
    },
    /// Embedding.
    Indexing {
        /// Documents done (in scope and up to date).
        done: usize,
        /// Documents in scope.
        total: usize,
        /// Chunks stored.
        chunks: usize,
    },
    /// Paused by the user.
    Paused,
    /// Something failed.
    Error(String),
}

/// A semantic query (`sem:` and friends).
#[derive(Clone, Debug, PartialEq)]
pub struct SemQuery {
    /// Natural-language text.
    pub text: String,
    /// Kinds searched.
    pub kinds: KindMask,
    /// Threshold (`sem:>0.6`), default from the settings.
    pub min_score: Option<f32>,
    /// Candidates (`sem-top:`), default from the settings.
    pub top: Option<usize>,
}

impl SemQuery {
    /// A query over every kind with default limits.
    pub fn new(text: &str) -> Self {
        Self { text: text.into(), kinds: ALL_KINDS, min_score: None, top: None }
    }

    fn task(&self) -> Task {
        if self.kinds == Kind::Code.bit() { Task::Code } else { Task::Search }
    }
}

/// Largest candidate count for an expanding ANN search (SPEC ch.5 §3.2).
pub const MAX_TOP: usize = 5000;

/// A recent query and its embedding.
type CachedQuery = (Task, String, Arc<Vec<f32>>);

struct Pending {
    cand: Candidate,
    kinds: KindMask,
    ready_at: Instant,
}

/// See the module docs.
pub struct SemanticEngine {
    host: Arc<ModelHost>,
    store: RwLock<VectorStore>,
    config: RwLock<EngineConfig>,
    status: Mutex<Status>,
    catalog: Arc<dyn Catalog>,
    text: Arc<dyn TextSource>,
    wake: (Sender<()>, Receiver<()>),
    stop: AtomicBool,
    generation: AtomicU64,
    query_cache: Mutex<VecDeque<CachedQuery>>,
    model_path: RwLock<PathBuf>,
}

impl SemanticEngine {
    /// Opens the store in `config.dir`. `model_path` is shown when the model is missing.
    pub fn new(
        config: EngineConfig,
        host: Arc<ModelHost>,
        model_path: PathBuf,
        catalog: Arc<dyn Catalog>,
        text: Arc<dyn TextSource>,
    ) -> Result<Arc<Self>> {
        let (store, _) = VectorStore::open(&config.dir, &config.model_id, config.dim)?;
        host.set_idle(config.idle_unload);
        Ok(Arc::new(Self {
            host,
            store: RwLock::new(store),
            config: RwLock::new(config),
            status: Mutex::new(Status::Idle),
            catalog,
            text,
            wake: crossbeam_channel::unbounded(),
            stop: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            query_cache: Mutex::new(VecDeque::new()),
            model_path: RwLock::new(model_path),
        }))
    }

    /// Starts the background indexer.
    pub fn start(self: &Arc<Self>) -> std::io::Result<std::thread::JoinHandle<()>> {
        let me = self.clone();
        std::thread::Builder::new().name("fennec-semantic".into()).spawn(move || me.run())
    }

    /// Stops the indexer (it saves the store) — returns at once.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.0.send(());
    }

    /// Applies new settings; a different model or dimension starts a new store.
    pub fn set_config(&self, config: EngineConfig, model_path: PathBuf) -> Result<()> {
        let old = self.config.read().clone();
        if old.model_id != config.model_id || old.dim != config.dim || old.dir != config.dir {
            let _ = self.store.write().save(&old.dir);
            let (s, _) = VectorStore::open(&config.dir, &config.model_id, config.dim)?;
            *self.store.write() = s;
            self.query_cache.lock().clear();
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        if *self.model_path.read() != model_path || old.model_id != config.model_id {
            self.host.unload();
            *self.model_path.write() = model_path;
        }
        self.host.set_idle(config.idle_unload);
        *self.config.write() = config;
        let _ = self.wake.0.send(());
        Ok(())
    }

    /// Current status.
    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// `true` while the model is in memory.
    pub fn model_loaded(&self) -> bool {
        self.host.is_loaded()
    }

    /// Bumps when the stored vectors change (searches showing semantic results re-run).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Default threshold and candidate count.
    pub fn defaults(&self) -> (f32, usize) {
        let c = self.config.read();
        (c.min_score, c.top)
    }

    /// `(documents, chunks, bytes)` stored.
    pub fn stats(&self) -> (usize, usize, usize) {
        let s = self.store.read();
        (s.doc_count(), s.chunk_count(), s.memory_bytes())
    }

    fn model_error(&self, e: SemError) -> SemError {
        if let SemError::ModelMissing(p) = &e {
            *self.status.lock() = Status::NoModel(p.clone());
        }
        e
    }

    /// The query's embedding (cached for the last few queries: typing re-runs them).
    pub fn query_vector(&self, task: Task, text: &str) -> Result<Arc<Vec<f32>>> {
        if let Some(v) = self.query_cache.lock().iter().find(|(t, s, _)| *t == task && s == text).map(|e| e.2.clone()) {
            return Ok(v);
        }
        let m = self.host.get().map_err(|e| self.model_error(e))?;
        let v = Arc::new(m.embed_query(task, text)?);
        let mut c = self.query_cache.lock();
        c.push_front((task, text.to_string(), v.clone()));
        c.truncate(16);
        Ok(v)
    }

    /// Nearest documents for `q` (ANN).
    pub fn search(&self, q: &SemQuery) -> Result<Vec<DocHit>> {
        let (min, top) = self.defaults();
        let v = self.query_vector(q.task(), &q.text)?;
        self.store.read().search(&v, q.top.unwrap_or(top).min(MAX_TOP), q.kinds, q.min_score.unwrap_or(min))
    }

    /// Exact scores for given documents (few literal candidates).
    pub fn score_paths<'a>(&self, q: &SemQuery, paths: impl Iterator<Item = &'a str>) -> Result<Vec<DocHit>> {
        let (min, _) = self.defaults();
        let v = self.query_vector(q.task(), &q.text)?;
        Ok(self.store.read().score_paths(&v, paths, q.kinds, q.min_score.unwrap_or(min)))
    }

    /// `true` if `path` has vectors (brute-force candidates).
    pub fn has_doc(&self, path: &str) -> bool {
        self.store.read().doc(path).is_some()
    }

    /// Saves the store now.
    pub fn save(&self) -> Result<()> {
        let dir = self.config.read().dir.clone();
        self.store.write().save(&dir)
    }

    /// Forgets every vector (Options → "rebuild").
    pub fn rebuild(&self) -> Result<()> {
        let c = self.config.read().clone();
        *self.store.write() = VectorStore::new(&c.model_id, c.dim)?;
        self.query_cache.lock().clear();
        self.generation.fetch_add(1, Ordering::Relaxed);
        let _ = self.wake.0.send(());
        Ok(())
    }

    fn run(&self) {
        let mut pending: HashMap<String, Pending> = HashMap::new();
        let mut synced: Option<(u64, Scope)> = None;
        let mut last_save = Instant::now();
        let mut in_scope = 0usize;
        loop {
            let _ = self.wake.1.recv_timeout(Duration::from_secs(2));
            while self.wake.1.try_recv().is_ok() {}
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            let cfg = self.config.read().clone();
            if cfg.paused {
                *self.status.lock() = Status::Paused;
                self.host.unload_if_idle();
                continue;
            }
            if cfg.scope.folders.is_empty() {
                *self.status.lock() = Status::Idle;
                self.host.unload_if_idle();
                continue;
            }
            let generation = self.catalog.generation();
            if synced.as_ref().is_none_or(|(g, s)| *g != generation || *s != cfg.scope) {
                in_scope = self.sync(&cfg, &mut pending);
                synced = Some((generation, cfg.scope.clone()));
            }
            let started = Instant::now();
            // Embed what is due, re-checking settings and stop about every second.
            while started.elapsed() < Duration::from_secs(1) && !self.stop.load(Ordering::Relaxed) {
                let now = Instant::now();
                let Some(key) = pending.iter().filter(|(_, p)| p.ready_at <= now).map(|(k, _)| k.clone()).next() else {
                    break;
                };
                let model = match self.host.get() {
                    Ok(m) => m,
                    Err(e) => {
                        let path = self.model_path.read().clone();
                        *self.status.lock() = match e {
                            SemError::ModelMissing(_) => Status::NoModel(path),
                            e => Status::Error(e.to_string()),
                        };
                        break;
                    }
                };
                let Some(p) = pending.remove(&key) else { break };
                if let Err(e) = self.index_doc(model.as_ref(), &cfg, &p) {
                    tracing::warn!(path = %p.cand.path, error = %e, "semantic indexing failed");
                }
                self.generation.fetch_add(1, Ordering::Relaxed);
            }
            let (docs, chunks) = {
                let s = self.store.read();
                (s.doc_count(), s.chunk_count())
            };
            let mut status = self.status.lock();
            if !matches!(*status, Status::NoModel(_) | Status::Error(_)) || pending.is_empty() {
                *status = if pending.is_empty() {
                    Status::Ready { docs, chunks }
                } else {
                    Status::Indexing { done: in_scope.saturating_sub(pending.len()), total: in_scope, chunks }
                };
            }
            drop(status);
            if !pending.is_empty() && pending.values().any(|p| p.ready_at <= Instant::now()) {
                let _ = self.wake.0.send(());
            }
            if last_save.elapsed() > Duration::from_secs(60) {
                self.save_and_compact(&cfg.dir);
                last_save = Instant::now();
            }
            if pending.is_empty() {
                self.host.unload_if_idle();
            }
        }
        self.save_and_compact(&self.config.read().dir.clone());
    }

    fn save_and_compact(&self, dir: &Path) {
        let mut s = self.store.write();
        if let Err(e) = s.compact_if_needed() {
            tracing::warn!(error = %e, "semantic store compaction failed");
        }
        if let Err(e) = s.save(dir) {
            tracing::warn!(error = %e, "cannot save the semantic store");
        }
    }

    /// Compares the catalog with the store: queues new and changed files, drops deleted
    /// ones. Returns the number of files in scope.
    fn sync(&self, cfg: &EngineConfig, pending: &mut HashMap<String, Pending>) -> usize {
        let now = Instant::now();
        let mut seen = HashSet::new();
        {
            let store = self.store.read();
            self.catalog.files(&cfg.scope.folders, &mut |c| {
                let kinds = cfg.scope.kinds_for(&c.path, c.size);
                if kinds == 0 {
                    return;
                }
                let key = c.path.to_lowercase();
                seen.insert(key.clone());
                match store.doc(&c.path) {
                    Some(d) if d.size == c.size && d.mtime == c.mtime => {
                        pending.remove(&key);
                    }
                    existing => {
                        let changed = existing.is_some();
                        match pending.get_mut(&key) {
                            // Still changing: wait for it to settle.
                            Some(p) if p.cand != c => {
                                p.cand = c;
                                p.kinds = kinds;
                                if changed {
                                    p.ready_at = now + cfg.debounce;
                                }
                            }
                            Some(_) => {}
                            None => {
                                let ready_at = if changed { now + cfg.debounce } else { now };
                                pending.insert(key, Pending { cand: c, kinds, ready_at });
                            }
                        }
                    }
                }
            });
        }
        pending.retain(|k, _| seen.contains(k));
        let gone: Vec<String> = self
            .store
            .read()
            .docs()
            .filter(|d| !seen.contains(&d.path.to_lowercase()))
            .map(|d| d.path.clone())
            .collect();
        if !gone.is_empty() {
            let mut s = self.store.write();
            for p in &gone {
                let _ = s.remove_doc(p);
            }
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        seen.len()
    }

    /// Chunks and embeds one file.
    fn index_doc(&self, model: &dyn Embedder, cfg: &EngineConfig, p: &Pending) -> Result<()> {
        let path = &p.cand.path;
        let mut pieces: Vec<(Kind, u32, u32, Option<String>, String)> = Vec::new();
        if p.kinds & Kind::Name.bit() != 0
            && let Some((title, text)) = chunk::name_text(path)
        {
            pieces.push((Kind::Name, 0, 0, Some(title), text));
        }
        let code = p.kinds & Kind::Code.bit() != 0;
        if (code || p.kinds & Kind::Content.bit() != 0)
            && let Some(text) = self.text.text(path, cfg.scope.max_size)
        {
            let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_string();
            let max = cfg.scope.max_chunks;
            let parts = if code {
                chunk::code_chunks(&text, 80, 10, max)
            } else {
                let budget = 512.min(model.max_tokens().saturating_sub(32)).max(32);
                chunk::text_chunks(&text, budget, 64, max, &|s| model.count_tokens(s))
            };
            let kind = if code { Kind::Code } else { Kind::Content };
            for part in parts {
                let t = part.of(&text).trim();
                if !t.is_empty() {
                    pieces.push((kind, part.start as u32, part.len as u32, Some(name.clone()), t.to_string()));
                }
            }
        }
        let hash = |k: Kind, title: &Option<String>, text: &str| {
            let mut h = xxhash_rust::xxh3::Xxh3::new();
            h.update(&[k as u8]);
            h.update(title.as_deref().unwrap_or("").as_bytes());
            h.update(&[0]);
            h.update(text.as_bytes());
            h.digest()
        };
        let mut chunks = Vec::with_capacity(pieces.len());
        let mut todo = Vec::new();
        {
            let store = self.store.read();
            for (i, (k, start, len, title, text)) in pieces.iter().enumerate() {
                let h = hash(*k, title, text);
                if !store.has_chunk(path, *k, h) {
                    todo.push(i);
                }
                chunks.push(NewChunk { kind: *k, start: *start, len: *len, hash: h, vector: None });
            }
        }
        for group in todo.chunks(8) {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let docs: Vec<(Option<&str>, &str)> =
                group.iter().map(|&i| (pieces[i].3.as_deref(), pieces[i].4.as_str())).collect();
            let vecs = model.embed_docs(&docs)?;
            for (&i, v) in group.iter().zip(vecs) {
                chunks[i].vector = Some(v);
            }
        }
        self.store.write().put_doc(path, p.cand.size, p.cand.mtime, chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;

    #[derive(Default)]
    struct FakeCatalog {
        files: Mutex<Vec<Candidate>>,
        generation: AtomicU64,
    }

    impl FakeCatalog {
        fn set(&self, files: Vec<Candidate>) {
            *self.files.lock() = files;
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Catalog for FakeCatalog {
        fn generation(&self) -> u64 {
            self.generation.load(Ordering::Relaxed)
        }
        fn files(&self, folders: &[String], out: &mut dyn FnMut(Candidate)) {
            for f in self.files.lock().iter().filter(|f| folders.iter().any(|d| under(&f.path, d))) {
                out(f.clone());
            }
        }
    }

    fn wait_for(e: &SemanticEngine, what: impl Fn(&SemanticEngine) -> bool) {
        let t = Instant::now();
        while !what(e) {
            assert!(t.elapsed() < Duration::from_secs(20), "timeout; status {:?}", e.status());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn scope_rules() {
        let s = Scope { folders: vec![r"D:\Docs\".into()], ..Scope::default() };
        assert_eq!(s.kinds_for(r"D:\Docs\a\budget.xlsx", 10), Kind::Name.bit() | Kind::Content.bit());
        assert_eq!(s.kinds_for(r"d:\docs\src\usn.rs", 10), Kind::Code.bit());
        assert_eq!(s.kinds_for(r"D:\Docs\notes.md", 1 << 30), Kind::Name.bit(), "too large for content");
        assert_eq!(s.kinds_for(r"D:\Docsx\a.txt", 10), 0);
        assert_eq!(s.kinds_for(r"C:\a.txt", 10), 0);
        assert_eq!(s.kinds_for(r"D:\Docs\app.exe", 10), 0);
    }

    #[test]
    fn indexes_updates_and_removes_in_the_background() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("docs");
        std::fs::create_dir_all(&root).unwrap();
        let file = |n: &str| root.join(n).display().to_string();
        std::fs::write(file("notes.txt"), "How to rotate an API key.\n\nOpen the console and press rotate.").unwrap();
        std::fs::write(file("code.rs"), "fn read_usn_journal() {}\n").unwrap();
        let catalog = Arc::new(FakeCatalog::default());
        let cands = |list: &[(&str, i64)]| -> Vec<Candidate> {
            list.iter()
                .map(|(n, m)| Candidate {
                    path: file(n),
                    size: std::fs::metadata(file(n)).map_or(0, |m| m.len()),
                    mtime: *m,
                })
                .collect()
        };
        catalog.set(cands(&[("notes.txt", 1), ("code.rs", 1), ("2026Q2_budget.xlsx", 1)]));
        let host = Arc::new(ModelHost::new(
            Box::new(|| Ok(Arc::new(HashEmbedder::new(64)) as Arc<dyn Embedder>)),
            Duration::from_millis(200),
        ));
        let mut cfg = EngineConfig::new(d.path().join("sem"), "hash");
        cfg.dim = 64;
        cfg.min_score = 0.05;
        cfg.debounce = Duration::from_millis(300);
        cfg.idle_unload = Duration::from_millis(200);
        cfg.scope.folders = vec![root.display().to_string()];
        let e = SemanticEngine::new(cfg.clone(), host, "m.gguf".into(), catalog.clone(), Arc::new(PlainText)).unwrap();
        let _t = e.start().unwrap();
        wait_for(&e, |e| matches!(e.status(), Status::Ready { docs: 3, .. }));
        let hits = e.search(&SemQuery::new("rotate api key")).unwrap();
        assert_eq!(hits[0].path, file("notes.txt"));
        assert_eq!(hits[0].kind, Kind::Content);
        let budget = e.search(&SemQuery { kinds: Kind::Name.bit(), ..SemQuery::new("budget") }).unwrap();
        assert_eq!(budget[0].path, file("2026Q2_budget.xlsx"));
        let code = e.search(&SemQuery { kinds: Kind::Code.bit(), ..SemQuery::new("read usn journal") }).unwrap();
        assert_eq!(code[0].path, file("code.rs"));

        // Change: waits for the debounce, then the new text is found and the old is gone.
        std::fs::write(file("notes.txt"), "Holiday photos from the beach.").unwrap();
        catalog.set(cands(&[("notes.txt", 2), ("code.rs", 1), ("2026Q2_budget.xlsx", 1)]));
        let changed_at = Instant::now();
        wait_for(&e, |e| {
            e.search(&SemQuery { kinds: Kind::Content.bit(), ..SemQuery::new("holiday beach") })
                .is_ok_and(|h| h.first().is_some_and(|h| h.score > 0.3))
        });
        assert!(changed_at.elapsed() >= Duration::from_millis(250), "debounced");
        let old = e
            .search(&SemQuery { kinds: Kind::Content.bit(), min_score: Some(0.3), ..SemQuery::new("rotate api key") })
            .unwrap();
        assert!(old.is_empty(), "{old:?}");

        // Delete: removed at once.
        catalog.set(cands(&[("code.rs", 1)]));
        wait_for(&e, |e| matches!(e.status(), Status::Ready { docs: 1, .. }));
        // Idle unload, transparent reload on the next query.
        wait_for(&e, |e| !e.model_loaded());
        assert!(!e.search(&SemQuery::new("read usn journal")).unwrap().is_empty());
        e.stop();
        _t.join().unwrap();
        let (s, rebuilt) = VectorStore::open(&cfg.dir, "hash", 64).unwrap();
        assert!(!rebuilt && s.doc_count() == 1, "saved on stop");
    }

    #[test]
    fn missing_model_is_a_status_not_a_crash() {
        let d = tempfile::tempdir().unwrap();
        let catalog = Arc::new(FakeCatalog::default());
        let root = d.path().display().to_string();
        catalog.set(vec![Candidate { path: format!("{root}/report.txt"), size: 1, mtime: 1 }]);
        let host = Arc::new(ModelHost::new(
            Box::new(|| Err(SemError::ModelMissing("m.gguf".into()))),
            Duration::from_secs(60),
        ));
        let mut cfg = EngineConfig::new(d.path().join("sem"), "x");
        cfg.scope.folders = vec![root];
        let e = SemanticEngine::new(cfg, host, "m.gguf".into(), catalog, Arc::new(PlainText)).unwrap();
        let _t = e.start().unwrap();
        wait_for(&e, |e| matches!(e.status(), Status::NoModel(_)));
        assert!(matches!(e.search(&SemQuery::new("x")), Err(SemError::ModelMissing(_) | SemError::Load(_))));
        e.stop();
    }
}

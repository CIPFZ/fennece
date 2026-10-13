//! Semantic search in the app (SPEC ch.5): builds the [`SemanticEngine`] from the
//! settings, feeds it the file index ([`Catalog`]), answers `sem:` terms for the search
//! engine ([`SemanticSource`]) and downloads the model on request.
//!
//! Nothing here runs while `semantic_enabled` is off: no thread, no model, and the
//! search options carry no semantic source (SEM-06).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use fnc_core::EntryId;
use fnc_core::settings::Settings;
use fnc_exec::{SemHit, SemanticSource, Shared};
use fnc_index::{IndexStore, SharedIndex};
use fnc_query::SemPred;
use fnc_semantic::engine::PlainText;
use fnc_semantic::{
    Candidate, Catalog, EngineConfig, Kind, ModelHost, ModelSpec, SemError, SemQuery, SemanticEngine, Status,
    TextSource,
};
use parking_lot::Mutex;

use crate::i18n::{t, t_args};

/// The file index as the semantic indexer sees it.
struct IndexCatalog {
    shared: SharedIndex,
}

impl Catalog for IndexCatalog {
    fn generation(&self) -> u64 {
        self.shared.try_snapshot_for(Duration::from_millis(50)).map_or(0, |s| s.generation())
    }

    fn files(&self, folders: &[String], out: &mut dyn FnMut(Candidate)) {
        let snap = self.shared.snapshot();
        let mut path = String::new();
        for f in folders {
            let Some(root) = snap.find_path(f) else { continue };
            snap.for_each_descendant(root, |id| {
                if snap.is_live(id) && !snap.is_dir(id) {
                    snap.full_path(id, &mut path);
                    out(Candidate {
                        path: path.clone(),
                        size: snap.size(id).unwrap_or(0),
                        mtime: snap.date_modified(id).0,
                    });
                }
            });
        }
    }
}

/// `sem:` terms for the search engine.
struct Source {
    engine: Arc<SemanticEngine>,
}

/// User-facing reason for a failed semantic query.
fn reason(e: &SemError) -> String {
    match e {
        SemError::ModelMissing(p) => t_args("sem-err-no-model", &[("path", p.display().to_string().into())]),
        e => e.to_string(),
    }
}

impl SemanticSource for Source {
    fn generation(&self) -> u64 {
        self.engine.generation()
    }

    fn default_top(&self) -> usize {
        self.engine.defaults().1
    }

    fn search(
        &self,
        q: &SemPred,
        s: &IndexStore,
        candidates: Option<&[EntryId]>,
        top: usize,
    ) -> Result<Vec<SemHit>, String> {
        if q.kinds == fnc_query::semantic::kinds::IMAGE {
            return Err(t("sem-err-no-images"));
        }
        // Bits of `fnc_query::semantic::kinds` equal `Kind::bit()`.
        let query = SemQuery { text: q.text.clone(), kinds: q.kinds, min_score: q.min_score_f32(), top: Some(top) };
        let hit = |id: EntryId, h: &fnc_semantic::DocHit| SemHit {
            id,
            score: h.score,
            range: (h.kind != Kind::Name && h.len > 0).then_some((h.start, h.len)),
        };
        match candidates {
            Some(ids) => {
                let mut paths: Vec<String> = Vec::with_capacity(ids.len());
                let mut by_path: HashMap<String, EntryId> = HashMap::with_capacity(ids.len());
                let mut p = String::new();
                for &id in ids {
                    s.full_path(id, &mut p);
                    if self.engine.has_doc(&p) {
                        by_path.insert(p.to_lowercase(), id);
                        paths.push(p.clone());
                    }
                }
                let hits = self.engine.score_paths(&query, paths.iter().map(String::as_str)).map_err(|e| reason(&e))?;
                Ok(hits
                    .iter()
                    .filter_map(|h| by_path.get(&h.path.to_lowercase()).map(|&id| hit(id, h)))
                    .take(top)
                    .collect())
            }
            None => {
                let hits = self.engine.search(&query).map_err(|e| reason(&e))?;
                Ok(hits.iter().filter_map(|h| s.find_path(&h.path).map(|id| hit(id, h))).collect())
            }
        }
    }
}

/// A model download in progress.
pub struct Download {
    /// Bytes on disk so far.
    pub done: Arc<AtomicU64>,
    /// Expected size (0 = unknown).
    pub total: u64,
    /// `Some` when finished.
    pub result: Arc<Mutex<Option<Result<(), String>>>>,
    cancel: Arc<AtomicBool>,
}

impl Download {
    /// Asks the download to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Settings that select the engine configuration (a change re-applies it).
fn config_key(s: &Settings) -> String {
    format!(
        "{}|{}|{}|{:?}|{}{}{}|{}|{}|{}|{}|{}|{}|{}|{}",
        s.semantic_model,
        s.semantic_model_path,
        s.semantic_dim,
        s.semantic_folders,
        s.semantic_names,
        s.semantic_content,
        s.semantic_code,
        s.semantic_max_file_mb,
        s.semantic_max_chunks,
        s.semantic_cpu_limit,
        s.semantic_idle_unload_min,
        s.semantic_min_score,
        s.semantic_top,
        s.semantic_debounce,
        s.semantic_paused
    )
}

/// Documents and Desktop of the current user (the default scope, SPEC ch.5 §6.7).
pub fn default_folders() -> Vec<String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let Some(home) = home else { return Vec::new() };
    ["Documents", "Desktop"]
        .iter()
        .map(|d| home.join(d))
        .filter(|p| p.is_dir())
        .map(|p| p.display().to_string())
        .collect()
}

/// Semantic search of the running app.
pub struct SemanticSearch {
    engine: Arc<SemanticEngine>,
    source: Shared<dyn SemanticSource>,
    worker: Option<std::thread::JoinHandle<()>>,
    key: String,
    spec: ModelSpec,
    model_path: PathBuf,
    models_dir: PathBuf,
    download: Option<Download>,
}

/// The model's file: the setting, or `models/<file>` in the data folder.
fn model_path(s: &Settings, spec: &ModelSpec, models_dir: &Path) -> PathBuf {
    if !s.semantic_model_path.trim().is_empty() {
        return PathBuf::from(s.semantic_model_path.trim());
    }
    let f = Path::new(&spec.file);
    if f.is_absolute() { f.to_path_buf() } else { models_dir.join(f) }
}

fn spec_for(s: &Settings, data_dir: &Path) -> ModelSpec {
    let registry = fnc_semantic::models::registry(Some(&data_dir.join("models.toml")));
    registry
        .iter()
        .find(|m| m.id == s.semantic_model)
        .or_else(|| registry.iter().find(|m| m.id == fnc_semantic::models::DEFAULT_MODEL))
        .cloned()
        .unwrap_or_default()
}

fn engine_config(s: &Settings, spec: &ModelSpec, data_dir: &Path) -> EngineConfig {
    let mut c = EngineConfig::new(data_dir.join("vectors"), &spec.id);
    c.dim = spec.store_dim(s.semantic_dim as usize);
    c.scope.folders = if s.semantic_folders.is_empty() { default_folders() } else { s.semantic_folders.clone() };
    c.scope.kinds =
        [(s.semantic_names, Kind::Name), (s.semantic_content, Kind::Content), (s.semantic_code, Kind::Code)]
            .iter()
            .filter(|(on, _)| *on)
            .fold(0, |m, (_, k)| m | k.bit());
    c.scope.max_size = u64::from(s.semantic_max_file_mb.max(1)) << 20;
    c.scope.max_chunks = s.semantic_max_chunks.max(1) as usize;
    c.idle_unload = Duration::from_secs(u64::from(s.semantic_idle_unload_min.max(1)) * 60);
    c.debounce = Duration::from_secs(u64::from(s.semantic_debounce));
    c.min_score = s.semantic_min_score.min(100) as f32 / 100.0;
    c.top = (s.semantic_top as usize).clamp(1, fnc_semantic::engine::MAX_TOP);
    c.paused = s.semantic_paused;
    c
}

impl SemanticSearch {
    /// Starts semantic search for `settings` with data in `data_dir`.
    pub fn start(
        s: &Settings,
        data_dir: &Path,
        shared: SharedIndex,
        text: Arc<dyn TextSource>,
    ) -> anyhow::Result<Self> {
        let spec = spec_for(s, data_dir);
        let models_dir = data_dir.join("models");
        let path = model_path(s, &spec, &models_dir);
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let index_threads = (cores * s.semantic_cpu_limit.clamp(1, 100) as usize / 100).max(1) as i32;
        let loader: fnc_semantic::embed::Loader = {
            let (path, spec) = (path.clone(), spec.clone());
            Box::new(move || {
                let mut o = fnc_semantic::llama::LlamaOptions::new(path.clone(), &spec.id);
                o.prompts = spec.prompts.clone();
                o.index_threads = index_threads;
                Ok(Arc::new(fnc_semantic::llama::LlamaEmbedder::load(o)?) as Arc<dyn fnc_semantic::Embedder>)
            })
        };
        let cfg = engine_config(s, &spec, data_dir);
        let host = Arc::new(ModelHost::new(loader, cfg.idle_unload));
        let engine = SemanticEngine::new(cfg, host, path.clone(), Arc::new(IndexCatalog { shared }), text)?;
        let worker = engine.start()?;
        let source: Arc<dyn SemanticSource> = Arc::new(Source { engine: engine.clone() });
        Ok(Self {
            engine,
            source: Shared::new(source),
            worker: Some(worker),
            key: config_key(s),
            spec,
            model_path: path,
            models_dir,
            download: None,
        })
    }

    /// Plain-text extraction for content chunks when no content extractor is plugged in.
    pub fn plain_text() -> Arc<dyn TextSource> {
        Arc::new(PlainText)
    }

    /// The source for `SearchOptions::semantic`.
    pub fn source(&self) -> Shared<dyn SemanticSource> {
        self.source.clone()
    }

    /// Applies changed settings. Returns `false` if the engine must be rebuilt (the model
    /// or its CPU limit changed: the loader is fixed at start).
    pub fn reconfigure(&mut self, s: &Settings, data_dir: &Path) -> bool {
        let key = config_key(s);
        if key == self.key {
            return true;
        }
        let spec = spec_for(s, data_dir);
        let path = model_path(s, &spec, &self.models_dir);
        let old_cpu = self.key.split('|').nth(9).map(str::to_string);
        if spec.id != self.spec.id
            || path != self.model_path
            || old_cpu.as_deref() != Some(&s.semantic_cpu_limit.to_string())
        {
            return false;
        }
        if let Err(e) = self.engine.set_config(engine_config(s, &spec, data_dir), path) {
            tracing::warn!(error = %e, "cannot apply semantic settings");
        }
        self.key = key;
        true
    }

    /// Engine status.
    pub fn status(&self) -> Status {
        self.engine.status()
    }

    /// Changes whenever the vectors change.
    pub fn generation(&self) -> u64 {
        self.engine.generation()
    }

    /// Status-bar text (SPEC ch.5 §3.3).
    pub fn status_text(&self) -> String {
        if let Some(d) = &self.download {
            let done = d.done.load(Ordering::Relaxed);
            let pct = (done * 100).checked_div(d.total).unwrap_or(0);
            return t_args("sem-status-download", &[("percent", pct.into())]);
        }
        match self.engine.status() {
            Status::Idle => t("sem-status-idle"),
            Status::NoModel(_) => t("sem-status-no-model"),
            Status::Loading => t("sem-status-loading"),
            Status::Ready { chunks, .. } => t_args("sem-status-ready", &[("chunks", chunks.into())]),
            Status::Indexing { done, total, chunks } => t_args(
                "sem-status-indexing",
                &[("chunks", chunks.into()), ("percent", (done * 100 / total.max(1)).into())],
            ),
            Status::Paused => t("sem-status-paused"),
            Status::Error(e) => t_args("sem-status-error", &[("error", e.into())]),
        }
    }

    /// `true` if the model file exists.
    pub fn model_present(&self) -> bool {
        self.model_path.is_file()
    }

    /// The model's file.
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// The model entry.
    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// `(documents, chunks, bytes)` stored.
    pub fn stats(&self) -> (usize, usize, usize) {
        self.engine.stats()
    }

    /// `true` while the model is in memory.
    pub fn model_loaded(&self) -> bool {
        self.engine.model_loaded()
    }

    /// Forgets every vector.
    pub fn rebuild(&self) {
        if let Err(e) = self.engine.rebuild() {
            tracing::warn!(error = %e, "cannot rebuild the semantic index");
        }
    }

    /// Starts downloading the model (`curl`); no-op if one is running or there is no URL.
    pub fn start_download(&mut self) -> Result<(), String> {
        if self.download.as_ref().is_some_and(|d| d.result.lock().is_none()) {
            return Ok(());
        }
        let url = self.spec.url.clone().ok_or_else(|| t("sem-err-no-url"))?;
        let d = Download {
            done: Arc::new(AtomicU64::new(0)),
            total: self.spec.size.unwrap_or(0),
            result: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (done, result, cancel) = (d.done.clone(), d.result.clone(), d.cancel.clone());
        let (dest, sha, size) = (self.model_path.clone(), self.spec.sha256.clone(), self.spec.size);
        std::thread::Builder::new()
            .name("fennec-model-download".into())
            .spawn(move || {
                let r = download(&url, &dest, size, sha.as_deref(), &done, &cancel);
                if let Err(e) = &r {
                    tracing::warn!(%url, error = %e, "model download failed");
                }
                *result.lock() = Some(r);
            })
            .map_err(|e| e.to_string())?;
        self.download = Some(d);
        Ok(())
    }

    /// The running or finished download.
    pub fn download(&self) -> Option<&Download> {
        self.download.as_ref()
    }

    /// Clears a finished download; returns its result.
    pub fn take_finished_download(&mut self) -> Option<Result<(), String>> {
        let r = self.download.as_ref()?.result.lock().clone()?;
        self.download = None;
        Some(r)
    }

    /// Stops the indexer (saves the vectors) and waits for it.
    pub fn stop(mut self) {
        if let Some(d) = &self.download {
            d.cancel();
        }
        self.engine.stop();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Downloads `url` to `dest` through `.part` with `curl` (resumable), checks size and
/// SHA-256, then renames it into place.
fn download(
    url: &str,
    dest: &Path,
    size: Option<u64>,
    sha256: Option<&str>,
    done: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let part = dest.with_extension("part");
    let mut cmd = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    cmd.args(["-L", "--fail", "-s", "-S", "-C", "-", "--retry", "3", "-o"]).arg(&part).arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("curl: {e}"))?;
    let status = loop {
        done.store(std::fs::metadata(&part).map_or(0, |m| m.len()), Ordering::Relaxed);
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(t("sem-download-cancelled"));
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    };
    if !status.success() {
        let mut msg = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut msg);
        }
        return Err(format!("curl {status}: {}", msg.trim()));
    }
    let len = std::fs::metadata(&part).map_err(|e| e.to_string())?.len();
    done.store(len, Ordering::Relaxed);
    if let Some(want) = size
        && want != len
    {
        let _ = std::fs::remove_file(&part);
        return Err(t_args("sem-download-bad-size", &[("got", len.into()), ("want", want.into())]));
    }
    if let Some(want) = sha256 {
        let mut f = std::fs::File::open(&part).map_err(|e| e.to_string())?;
        let got = fnc_props::hash::hash_reader(&mut f, &[fnc_props::hash::HashAlgo::Sha256], &|| {
            cancel.load(Ordering::Relaxed)
        })
        .map_err(|e| e.to_string())?
        .and_then(|v| v.into_iter().next())
        .map(|(_, d)| d.iter().map(|b| format!("{b:02x}")).collect::<String>())
        .unwrap_or_default();
        if !got.eq_ignore_ascii_case(want) {
            let _ = std::fs::remove_file(&part);
            return Err(t("sem-download-bad-hash"));
        }
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnc_index::efu;

    #[test]
    fn catalog_lists_files_under_folders() {
        let mut s = IndexStore::default();
        let csv = "Filename,Size,Date Modified,Date Created,Attributes\n\"C:\\Docs\",,,,16\n\"C:\\Docs\\a.txt\",5,7,,32\n\"C:\\Docs\\sub\",,,,16\n\"C:\\Docs\\sub\\b.md\",6,8,,32\n\"C:\\Other\\c.txt\",1,,,32\n";
        efu::load_efu(&mut s, csv.as_bytes(), "t.efu").unwrap();
        let cat = IndexCatalog { shared: SharedIndex::new(s) };
        let mut got = Vec::new();
        cat.files(&["c:\\docs".into(), "C:\\Missing".into()], &mut |c| got.push((c.path, c.size, c.mtime)));
        got.sort();
        assert_eq!(got, vec![("C:\\Docs\\a.txt".to_string(), 5, 7), ("C:\\Docs\\sub\\b.md".to_string(), 6, 8)]);
        assert!(cat.generation() > 0 || cat.generation() == 0);
    }

    #[test]
    fn settings_map_to_the_engine() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let mut s = Settings {
            semantic_folders: vec!["D:\\Docs".into()],
            semantic_code: false,
            semantic_min_score: 60,
            ..Settings::default()
        };
        let spec = spec_for(&s, d.path());
        assert_eq!(spec.id, "embeddinggemma-2");
        let c = engine_config(&s, &spec, d.path());
        assert_eq!(c.dim, 256);
        assert_eq!(c.scope.folders, vec!["D:\\Docs".to_string()]);
        assert_eq!(c.scope.kinds, Kind::Name.bit() | Kind::Content.bit());
        assert!((c.min_score - 0.6).abs() < 1e-6);
        assert_eq!(
            model_path(&s, &spec, &d.path().join("models")),
            d.path().join("models").join("embeddinggemma-2-Q8_0.gguf")
        );
        s.semantic_model_path = "E:\\m.gguf".into();
        assert_eq!(model_path(&s, &spec, d.path()), PathBuf::from("E:\\m.gguf"));
    }

    #[test]
    fn missing_model_answers_with_a_reason() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let s = Settings { semantic_folders: vec![d.path().display().to_string()], ..Settings::default() };
        let mut sem =
            SemanticSearch::start(&s, d.path(), SharedIndex::new(IndexStore::default()), SemanticSearch::plain_text())
                .unwrap();
        assert!(!sem.model_present());
        let q = SemPred { text: "budget".into(), kinds: fnc_query::semantic::kinds::ALL, min_score: None, top: None };
        let err = sem.source().search(&q, &IndexStore::default(), None, 10).unwrap_err();
        assert!(err.contains("embeddinggemma-2-Q8_0.gguf"), "{err}");
        assert!(sem.reconfigure(&s, d.path()));
        sem.stop();
    }
}

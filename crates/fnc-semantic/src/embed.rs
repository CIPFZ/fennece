//! The [`Embedder`] trait, Matryoshka truncation, a deterministic test embedder, and
//! [`ModelHost`], which loads the model on first use and unloads it after an idle period
//! (SPEC ch.5 §2.3.3, SEM-08).

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::prompt::{Prompts, Task};
use crate::{Result, SemError};

/// Which context serves a request: queries get their own so they never wait behind an
/// indexing batch (SPEC ch.5 §6.4).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lane {
    /// Interactive query.
    Query,
    /// Background indexing.
    Index,
}

/// A text embedding model.
pub trait Embedder: Send + Sync {
    /// Identifies the model (stored with the vectors; a change triggers a rebuild).
    fn model_id(&self) -> &str;
    /// Native output dimension.
    fn dim(&self) -> usize;
    /// Longest input in tokens.
    fn max_tokens(&self) -> usize;
    /// Tokens of `text` as the model counts them (without special tokens).
    fn count_tokens(&self, text: &str) -> usize;
    /// L2-normalised embeddings of already prefixed texts.
    fn embed(&self, lane: Lane, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Prompt templates.
    fn prompts(&self) -> &Prompts;

    /// Embeds a query with its task prefix.
    fn embed_query(&self, task: Task, text: &str) -> Result<Vec<f32>> {
        let q = self.prompts().query(task, text);
        self.embed(Lane::Query, &[q])?.pop().ok_or_else(|| SemError::Embed("no output".into()))
    }

    /// Embeds documents `(title, text)` with the document prefix.
    fn embed_docs(&self, docs: &[(Option<&str>, &str)]) -> Result<Vec<Vec<f32>>> {
        let texts: Vec<String> = docs.iter().map(|(t, x)| self.prompts().document(*t, x)).collect();
        self.embed(Lane::Index, &texts)
    }
}

/// Scales `v` to unit length (zero vectors stay zero).
pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// Matryoshka truncation: the first `dim` components, re-normalised (SPEC ch.5 §6.3).
pub fn fit(v: &[f32], dim: usize) -> Vec<f32> {
    let mut out = v[..dim.min(v.len())].to_vec();
    normalize(&mut out);
    out
}

/// Dot product of two unit vectors (cosine similarity).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Deterministic bag-of-words/trigram embedder for tests and for exercising the pipeline
/// without a model. Texts sharing words land close together; it understands no meaning.
pub struct HashEmbedder {
    dim: usize,
    prompts: Prompts,
}

impl HashEmbedder {
    /// An embedder producing `dim`-dimensional vectors; prefixes are not added.
    pub fn new(dim: usize) -> Self {
        Self { dim, prompts: Prompts::plain() }
    }

    fn one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        let lower = text.to_lowercase();
        let mut add = |s: &str, w: f32| {
            let h = xxhash_rust::xxh3::xxh3_64(s.as_bytes());
            let i = (h % self.dim as u64) as usize;
            v[i] += if h & (1 << 63) == 0 { w } else { -w };
        };
        for word in lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
            add(word, 1.0);
            let chars: Vec<char> = word.chars().collect();
            for t in chars.windows(3) {
                add(&t.iter().collect::<String>(), 0.3);
            }
            // CJK text has no spaces: characters count as words.
            for c in chars.iter().filter(|c| !c.is_ascii()) {
                add(&c.to_string(), 0.5);
            }
        }
        normalize(&mut v);
        v
    }
}

impl Embedder for HashEmbedder {
    fn model_id(&self) -> &str {
        "hash"
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn max_tokens(&self) -> usize {
        8192
    }
    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count() + text.chars().filter(|c| !c.is_ascii()).count()
    }
    fn embed(&self, _lane: Lane, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.one(t)).collect())
    }
    fn prompts(&self) -> &Prompts {
        &self.prompts
    }
}

/// Creates the embedder (loads the model).
pub type Loader = Box<dyn Fn() -> Result<Arc<dyn Embedder>> + Send + Sync>;

struct HostState {
    model: Option<Arc<dyn Embedder>>,
    last_used: Instant,
    /// Last load error, kept so a missing model is not retried on every keystroke.
    error: Option<(Instant, CachedError)>,
}

#[derive(Clone)]
enum CachedError {
    Missing(std::path::PathBuf),
    Other(String),
}

impl CachedError {
    fn of(e: &SemError) -> Self {
        match e {
            SemError::ModelMissing(p) => CachedError::Missing(p.clone()),
            e => CachedError::Other(e.to_string()),
        }
    }

    fn error(&self) -> SemError {
        match self {
            CachedError::Missing(p) => SemError::ModelMissing(p.clone()),
            CachedError::Other(m) => SemError::Load(m.clone()),
        }
    }
}

/// Owns the loaded model: loads on demand, unloads after `idle`.
pub struct ModelHost {
    loader: Loader,
    idle: Mutex<Duration>,
    state: Mutex<HostState>,
    /// Serialises loads (a query and the indexer may ask at once).
    loading: Mutex<()>,
}

/// How long a failed load is remembered before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(5);

impl ModelHost {
    /// A host that unloads the model after `idle` without use.
    pub fn new(loader: Loader, idle: Duration) -> Self {
        Self {
            loader,
            idle: Mutex::new(idle),
            state: Mutex::new(HostState { model: None, last_used: Instant::now(), error: None }),
            loading: Mutex::new(()),
        }
    }

    /// The model, loading it if needed.
    pub fn get(&self) -> Result<Arc<dyn Embedder>> {
        {
            let mut s = self.state.lock();
            s.last_used = Instant::now();
            if let Some(m) = &s.model {
                return Ok(m.clone());
            }
            if let Some((at, e)) = &s.error
                && at.elapsed() < RETRY_AFTER
            {
                return Err(e.error());
            }
        }
        let _g = self.loading.lock();
        if let Some(m) = &self.state.lock().model {
            return Ok(m.clone());
        }
        let t = Instant::now();
        match (self.loader)() {
            Ok(m) => {
                tracing::info!(model = m.model_id(), ms = t.elapsed().as_millis() as u64, "semantic model loaded");
                let mut s = self.state.lock();
                s.model = Some(m.clone());
                s.error = None;
                s.last_used = Instant::now();
                Ok(m)
            }
            Err(e) => {
                self.state.lock().error = Some((Instant::now(), CachedError::of(&e)));
                Err(e)
            }
        }
    }

    /// `true` while the model is in memory.
    pub fn is_loaded(&self) -> bool {
        self.state.lock().model.is_some()
    }

    /// Changes the idle timeout.
    pub fn set_idle(&self, idle: Duration) {
        *self.idle.lock() = idle;
    }

    /// Unloads the model if unused for the idle period; returns `true` if it did.
    pub fn unload_if_idle(&self) -> bool {
        let idle = *self.idle.lock();
        let mut s = self.state.lock();
        if s.model.is_some() && s.last_used.elapsed() >= idle {
            s.model = None;
            tracing::info!("semantic model unloaded after {} s idle", idle.as_secs());
            return true;
        }
        false
    }

    /// Drops the model now (settings changed).
    pub fn unload(&self) {
        let mut s = self.state.lock();
        s.model = None;
        s.error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn fit_truncates_and_renormalises() {
        let v = vec![3.0, 4.0, 12.0];
        let f = fit(&v, 2);
        assert!((f[0] - 0.6).abs() < 1e-6 && (f[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn hash_embedder_groups_shared_words() {
        let e = HashEmbedder::new(128);
        let v = e
            .embed(Lane::Index, &["quarterly budget".into(), "budget quarterly report".into(), "holiday photos".into()])
            .unwrap();
        assert!(cosine(&v[0], &v[1]) > cosine(&v[0], &v[2]));
        assert!((cosine(&v[0], &v[0]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn host_loads_lazily_and_unloads_when_idle() {
        let loads = Arc::new(AtomicUsize::new(0));
        let l = loads.clone();
        let host = ModelHost::new(
            Box::new(move || {
                l.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(HashEmbedder::new(8)) as Arc<dyn Embedder>)
            }),
            Duration::from_millis(30),
        );
        assert!(!host.is_loaded());
        host.get().unwrap();
        host.get().unwrap();
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert!(!host.unload_if_idle());
        std::thread::sleep(Duration::from_millis(40));
        assert!(host.unload_if_idle());
        host.get().unwrap();
        assert_eq!(loads.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn failed_loads_are_not_retried_immediately() {
        let loads = Arc::new(AtomicUsize::new(0));
        let l = loads.clone();
        let host = ModelHost::new(
            Box::new(move || {
                l.fetch_add(1, Ordering::Relaxed);
                Err(SemError::ModelMissing("m.gguf".into()))
            }),
            Duration::from_secs(60),
        );
        assert!(host.get().is_err());
        assert!(matches!(host.get(), Err(SemError::ModelMissing(_))));
        assert_eq!(loads.load(Ordering::Relaxed), 1);
    }
}

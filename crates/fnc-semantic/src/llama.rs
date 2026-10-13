//! llama.cpp embedder (SPEC ch.5 §3.1): loads a GGUF embedding model, runs one context per
//! [`Lane`] on its own thread (a context is `Send` but not `Sync`), mean-pooled per the
//! model's GGUF metadata.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crossbeam_channel::{Receiver, Sender};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;

use crate::embed::{Embedder, Lane, normalize};
use crate::prompt::Prompts;
use crate::{Result, SemError};

/// How to load the model.
#[derive(Clone, Debug)]
pub struct LlamaOptions {
    /// The `.gguf` file.
    pub path: PathBuf,
    /// Model id stored with the vectors.
    pub model_id: String,
    /// Prompt templates.
    pub prompts: Prompts,
    /// Threads of the query context.
    pub query_threads: i32,
    /// Threads of the indexing context (the CPU limit, SPEC ch.5 §3.1 "调度").
    pub index_threads: i32,
    /// Context length in tokens.
    pub n_ctx: u32,
    /// Layers offloaded to a GPU backend (0 = CPU).
    pub gpu_layers: u32,
}

impl LlamaOptions {
    /// Defaults for `path`: query context on half the cores, indexing on a quarter.
    pub fn new(path: PathBuf, model_id: &str) -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get()) as i32;
        Self {
            path,
            model_id: model_id.into(),
            prompts: Prompts::default(),
            query_threads: (cores / 2).clamp(1, 8),
            index_threads: (cores / 4).max(1),
            n_ctx: 2048,
            gpu_layers: 0,
        }
    }
}

fn backend() -> Result<&'static LlamaBackend> {
    static BACKEND: OnceLock<std::result::Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            // llama.cpp logs every tensor at load; route only warnings and errors.
            llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(false));
            LlamaBackend::init().map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| SemError::Load(e.clone()))
}

type Job = (Vec<String>, Sender<Result<Vec<Vec<f32>>>>);

/// A loaded GGUF embedding model.
pub struct LlamaEmbedder {
    model: Arc<LlamaModel>,
    opts: LlamaOptions,
    dim: usize,
    query: Sender<Job>,
    index: Sender<Job>,
}

impl LlamaEmbedder {
    /// Loads the model and starts the two lanes.
    pub fn load(opts: LlamaOptions) -> Result<Self> {
        if !opts.path.is_file() {
            return Err(SemError::ModelMissing(opts.path.clone()));
        }
        let backend = backend()?;
        let params = LlamaModelParams::default().with_n_gpu_layers(opts.gpu_layers);
        let model =
            LlamaModel::load_from_file(backend, &opts.path, &params).map_err(|e| SemError::Load(e.to_string()))?;
        let dim = usize::try_from(model.n_embd_out()).unwrap_or(0);
        if dim == 0 {
            return Err(SemError::Load("the model has no embedding output".into()));
        }
        let model = Arc::new(model);
        let n_ctx = opts.n_ctx.min(model.n_ctx_train()).max(64);
        let start = |name: &str, threads: i32| -> Result<Sender<Job>> {
            let (tx, rx) = crossbeam_channel::unbounded::<Job>();
            let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
            let model = model.clone();
            std::thread::Builder::new()
                .name(format!("fennec-embed-{name}"))
                .spawn(move || lane(&model, backend, n_ctx, threads, &rx, &ready_tx))?;
            ready_rx.recv().map_err(|_| SemError::Load("embedding thread failed".into()))??;
            Ok(tx)
        };
        let query = start("query", opts.query_threads)?;
        let index = start("index", opts.index_threads)?;
        Ok(Self { model, opts: LlamaOptions { n_ctx, ..opts }, dim, query, index })
    }
}

/// One context serving jobs until the embedder is dropped (the channel closes).
fn lane(
    model: &LlamaModel,
    backend: &LlamaBackend,
    n_ctx: u32,
    threads: i32,
    rx: &Receiver<Job>,
    ready: &Sender<Result<()>>,
) {
    let params = LlamaContextParams::default()
        .with_embeddings(true)
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(n_ctx)
        .with_n_ubatch(n_ctx)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    let mut ctx = match model.new_context(backend, params) {
        Ok(c) => {
            let _ = ready.send(Ok(()));
            c
        }
        Err(e) => {
            let _ = ready.send(Err(SemError::Load(e.to_string())));
            return;
        }
    };
    let mut batch = LlamaBatch::new(n_ctx as usize, 1);
    while let Ok((texts, reply)) = rx.recv() {
        let mut out = Vec::with_capacity(texts.len());
        let mut failed = None;
        for t in &texts {
            let mut toks = model.vocab().tokenize(t.as_bytes(), true, false);
            toks.truncate(n_ctx as usize);
            batch.clear();
            let r = batch
                .add_sequence(&toks, 0, false)
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    ctx.clear_kv_cache();
                    ctx.decode(&mut batch).map_err(|e| e.to_string())
                })
                .and_then(|()| ctx.embeddings_seq_ith(0).map(<[f32]>::to_vec).map_err(|e| e.to_string()));
            match r {
                Ok(mut v) => {
                    if v.iter().any(|x| !x.is_finite()) {
                        failed = Some("the model produced NaN (fp16 weights?)".to_string());
                        break;
                    }
                    normalize(&mut v);
                    out.push(v);
                }
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        let _ = reply.send(match failed {
            None => Ok(out),
            Some(e) => Err(SemError::Embed(e)),
        });
    }
}

impl Embedder for LlamaEmbedder {
    fn model_id(&self) -> &str {
        &self.opts.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn max_tokens(&self) -> usize {
        self.opts.n_ctx as usize
    }

    fn count_tokens(&self, text: &str) -> usize {
        self.model.vocab().tokenize(text.as_bytes(), false, false).len()
    }

    fn embed(&self, lane: Lane, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let q = match lane {
            Lane::Query => &self.query,
            Lane::Index => &self.index,
        };
        q.send((texts.to_vec(), tx)).map_err(|_| SemError::Embed("embedding thread stopped".into()))?;
        rx.recv().map_err(|_| SemError::Embed("embedding thread stopped".into()))?
    }

    fn prompts(&self) -> &Prompts {
        &self.opts.prompts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Task;
    use crate::embed::cosine;

    #[test]
    fn missing_model_is_reported() {
        let e = LlamaEmbedder::load(LlamaOptions::new("/nonexistent/m.gguf".into(), "x"));
        assert!(matches!(e, Err(SemError::ModelMissing(_))));
    }

    /// Needs the model: `FENNEC_TEST_MODEL=/path/embeddinggemma-2-Q8_0.gguf`.
    #[test]
    #[ignore = "needs an EmbeddingGemma GGUF file"]
    fn embeddinggemma_ranks_by_meaning() {
        let path = std::env::var("FENNEC_TEST_MODEL").expect("FENNEC_TEST_MODEL");
        let e = LlamaEmbedder::load(LlamaOptions::new(path.into(), "embeddinggemma-2")).unwrap();
        assert_eq!(e.dim(), 768);
        let q = e.embed_query(Task::Search, "季度预算表").unwrap();
        let d = e
            .embed_docs(&[
                (Some("2026Q2_budget.xlsx"), "Finance\\2026Q2_budget.xlsx"),
                (Some("holiday_photos.zip"), "Pictures\\holiday_photos.zip"),
            ])
            .unwrap();
        assert!(cosine(&q, &d[0]) > cosine(&q, &d[1]) + 0.05, "{} vs {}", cosine(&q, &d[0]), cosine(&q, &d[1]));
        assert!(e.count_tokens("hello world") >= 2);
    }
}

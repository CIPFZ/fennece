//! `sem-eval` — semantic search evaluation (SPEC ch.5 §5): Recall@1 / Recall@10 / MRR of
//! query→file samples over a folder, for choosing a model, a dimension and chunking.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fnc_semantic::engine::PlainText;
use fnc_semantic::{Candidate, Catalog, EngineConfig, Kind, ModelHost, SemQuery, SemanticEngine, Status};

struct Args {
    model: PathBuf,
    folder: PathBuf,
    samples: Option<PathBuf>,
    generate: usize,
    dim: usize,
    kinds: u8,
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: sem-eval --model FILE.gguf --folder DIR (--samples FILE.tsv | --generate N) [--dim 256] [--kinds name,content,code]"
    );
    ExitCode::from(2)
}

fn parse() -> Option<Args> {
    let mut a = Args {
        model: PathBuf::new(),
        folder: PathBuf::new(),
        samples: None,
        generate: 0,
        dim: 256,
        kinds: fnc_semantic::ALL_KINDS,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next()?;
        match k.as_str() {
            "--model" => a.model = v.into(),
            "--folder" => a.folder = v.into(),
            "--samples" => a.samples = Some(v.into()),
            "--generate" => a.generate = v.parse().ok()?,
            "--dim" => a.dim = v.parse().ok()?,
            "--kinds" => {
                a.kinds = v
                    .split(',')
                    .map(|k| match k {
                        "name" => Kind::Name.bit(),
                        "content" => Kind::Content.bit(),
                        "code" => Kind::Code.bit(),
                        _ => 0,
                    })
                    .fold(0, |x, y| x | y)
            }
            _ => return None,
        }
    }
    (!a.model.as_os_str().is_empty() && !a.folder.as_os_str().is_empty() && (a.samples.is_some() || a.generate > 0))
        .then_some(a)
}

/// The folder as a catalog (walked once).
struct Walked(Vec<Candidate>);

impl Catalog for Walked {
    fn generation(&self) -> u64 {
        1
    }
    fn files(&self, _folders: &[String], out: &mut dyn FnMut(Candidate)) {
        self.0.iter().cloned().for_each(out);
    }
}

fn walk(dir: &Path) -> Vec<Candidate> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
            Some(Candidate { path: e.path().display().to_string(), size: md.len(), mtime })
        })
        .collect()
}

/// Known-item samples: the first sentence of `n` text files (a stand-in for "I remember
/// roughly what it said").
fn generate(files: &[Candidate], n: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let step = (files.len() / n.max(1)).max(1);
    for f in files.iter().step_by(step) {
        let Some(text) = fnc_semantic::TextSource::text(&PlainText, &f.path, 64 << 10) else { continue };
        let sentence: String = text
            .split(['.', '。', '\n'])
            .map(str::trim)
            .find(|s| s.split_whitespace().count() >= 4 || s.chars().count() >= 8)
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        if !sentence.is_empty() {
            out.push((sentence, f.path.clone()));
        }
        if out.len() >= n {
            break;
        }
    }
    out
}

fn main() -> ExitCode {
    let Some(a) = parse() else { return usage() };
    let folder = a.folder.canonicalize().unwrap_or(a.folder.clone());
    let files = walk(&folder);
    let samples: Vec<(String, String)> = match &a.samples {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(t) => t
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(q, f)| (q.trim().to_string(), f.trim().to_string()))
                .collect(),
            Err(e) => {
                eprintln!("sem-eval: {}: {e}", p.display());
                return ExitCode::FAILURE;
            }
        },
        None => generate(&files, a.generate),
    };
    let data = std::env::temp_dir().join(format!("sem-eval-{}", std::process::id()));
    let mut cfg = EngineConfig::new(data.clone(), "eval");
    cfg.dim = a.dim;
    cfg.min_score = -1.0;
    cfg.scope.folders = vec![folder.display().to_string()];
    cfg.scope.kinds = a.kinds;
    cfg.scope.name_exts.clear();
    #[cfg(feature = "llama")]
    let loader: fnc_semantic::embed::Loader = {
        let model = a.model.clone();
        Box::new(move || {
            let mut o = fnc_semantic::llama::LlamaOptions::new(model.clone(), "eval");
            o.index_threads = o.query_threads;
            Ok(Arc::new(fnc_semantic::llama::LlamaEmbedder::load(o)?) as Arc<dyn fnc_semantic::Embedder>)
        })
    };
    #[cfg(not(feature = "llama"))]
    let loader: fnc_semantic::embed::Loader = Box::new(|| Err(fnc_semantic::SemError::Unsupported));
    let host = Arc::new(ModelHost::new(loader, Duration::from_secs(3600)));
    let engine =
        match SemanticEngine::new(cfg, host, a.model.clone(), Arc::new(Walked(files.clone())), Arc::new(PlainText)) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("sem-eval: {e}");
                return ExitCode::FAILURE;
            }
        };
    let worker = engine.start();
    let t = Instant::now();
    let last = AtomicU64::new(0);
    loop {
        match engine.status() {
            Status::Ready { docs, chunks } if docs > 0 || files.is_empty() => {
                eprintln!("indexed {docs} files, {chunks} chunks in {:.1} s", t.elapsed().as_secs_f32());
                break;
            }
            Status::NoModel(p) => {
                eprintln!("sem-eval: model not found: {}", p.display());
                return ExitCode::FAILURE;
            }
            Status::Error(e) => {
                eprintln!("sem-eval: {e}");
                return ExitCode::FAILURE;
            }
            Status::Indexing { done, total, .. } if t.elapsed().as_secs() / 10 != last.load(Ordering::Relaxed) => {
                last.store(t.elapsed().as_secs() / 10, Ordering::Relaxed);
                eprintln!("  {done}/{total}");
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let (mut r1, mut r10, mut mrr, mut n) = (0usize, 0usize, 0f64, 0usize);
    let mut latency = Vec::new();
    for (q, target) in &samples {
        let matches = |p: &str| {
            let p = p.replace('/', "\\").to_lowercase();
            let t = target.replace('/', "\\").to_lowercase();
            p == t || p.ends_with(&format!("\\{}", t.trim_start_matches('\\')))
        };
        let s = Instant::now();
        let hits = match engine.search(&SemQuery { kinds: a.kinds, top: Some(10), ..SemQuery::new(q) }) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("sem-eval: {e}");
                return ExitCode::FAILURE;
            }
        };
        latency.push(s.elapsed());
        n += 1;
        let rank = hits.iter().position(|h| matches(&h.path));
        match rank {
            Some(0) => {
                r1 += 1;
                r10 += 1;
                mrr += 1.0;
            }
            Some(r) => {
                r10 += 1;
                mrr += 1.0 / (r + 1) as f64;
            }
            None => eprintln!("miss\t{q}\t{target}\ttop: {}", hits.first().map_or("-", |h| h.path.as_str())),
        }
    }
    latency.sort();
    let p95 = latency.get(latency.len() * 95 / 100).or(latency.last()).copied().unwrap_or_default();
    let pct = |x: usize| 100.0 * x as f64 / n.max(1) as f64;
    eprintln!(
        "samples {n}  dim {}  Recall@1 {:.1}%  Recall@10 {:.1}%  MRR {:.3}  query p95 {} ms",
        a.dim,
        pct(r1),
        pct(r10),
        mrr / n.max(1) as f64,
        p95.as_millis()
    );
    engine.stop();
    if let Ok(w) = worker {
        let _ = w.join();
    }
    let _ = std::fs::remove_dir_all(&data);
    ExitCode::SUCCESS
}

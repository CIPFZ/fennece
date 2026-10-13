//! `si:` / `system-index:` — items of the Windows Search system index (SPEC ch.2 §5,
//! Everything 1.5 "search functions that access the disk or system index").
//!
//! The search hands the term's Advanced Query Syntax text to a [`SystemIndexSource`]
//! (the app installs one backed by `fnc_platform::winsearch`) once, when the plan is
//! compiled; the returned paths are resolved to index entries and the leaf becomes a bit
//! set, so the per-entry cost is a bit test and searches without `si:` never touch any
//! of this. Results are cached across searches for a short time ([`SystemIndexCache`]):
//! index refreshes and re-sorts do not re-query Windows Search, which keeps its own
//! index up to date asynchronously anyway.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fnc_core::EntryId;
use fnc_core::fold::{FoldOptions, fold_str_into};
use fnc_index::IndexStore;
use parking_lot::Mutex;

use crate::cache::{Caches, CancelToken, Cancelled};
use crate::eval::{Bits, Leaf};
use crate::ext::{ExecWarning, SlowState};
use crate::tree::{resolve_path, root_paths};

/// Items returned by a system index query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemIndexHits {
    /// Full paths of the matching files and folders.
    pub paths: Vec<String>,
    /// The source stopped at this many items (more matched).
    pub limit: Option<usize>,
}

/// Why a system index query returned nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemIndexError {
    /// No system index (Windows Search not installed or not running, other platforms).
    Unavailable,
    /// The query failed (message for the status bar).
    Failed(String),
    /// The search was cancelled while waiting.
    Cancelled,
}

/// Source of `si:` results.
pub trait SystemIndexSource: Send + Sync {
    /// Runs the Advanced Query Syntax query `aqs` (`""` = every indexed item); `cancel`
    /// returns `true` once the search is superseded (poll it while waiting).
    fn search(&self, aqs: &str, cancel: &dyn Fn() -> bool) -> Result<SystemIndexHits, SystemIndexError>;
}

type Outcome = Result<Arc<SystemIndexHits>, SystemIndexError>;

/// Cross-search cache of system index results by query text (and source).
#[derive(Default)]
pub struct SystemIndexCache {
    map: Mutex<HashMap<(usize, String), (Instant, Outcome)>>,
}

impl SystemIndexCache {
    /// Successful results are reused this long.
    pub const TTL: Duration = Duration::from_secs(30);
    /// "Unavailable" is remembered this long (no new attempt on every keystroke).
    pub const UNAVAILABLE_TTL: Duration = Duration::from_secs(10);
    const MAX: usize = 64;

    /// Result of `aqs` from `src`, queried unless a fresh one is cached.
    pub fn get(&self, src: &Arc<dyn SystemIndexSource>, aqs: &str, cancel: &CancelToken) -> Outcome {
        let key = (Arc::as_ptr(src).cast::<()>() as usize, aqs.to_string());
        if let Some((at, r)) = self.map.lock().get(&key) {
            let ttl = if r.is_ok() { Self::TTL } else { Self::UNAVAILABLE_TTL };
            if at.elapsed() < ttl {
                return r.clone();
            }
        }
        let r = src.search(aqs, &|| cancel.is_cancelled()).map(Arc::new);
        if cancel.is_cancelled() {
            return Err(SystemIndexError::Cancelled);
        }
        if matches!(r, Ok(_) | Err(SystemIndexError::Unavailable)) {
            let mut m = self.map.lock();
            if m.len() >= Self::MAX {
                m.retain(|_, (at, _)| at.elapsed() < Self::TTL);
                if m.len() >= Self::MAX {
                    m.clear();
                }
            }
            m.insert(key, (Instant::now(), r.clone()));
        }
        r
    }

    /// Forgets every cached result.
    pub fn clear(&self) {
        self.map.lock().clear();
    }

    /// Number of cached queries.
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    /// `true` if nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Above this many paths, children are found through the (cached) name lookup instead of
/// scanning each parent's children.
const LOOKUP_FROM: usize = 64;

/// Entries of `s` at `paths` (full paths as Windows reports them; unknown ones skipped).
pub fn resolve_paths(
    s: &IndexStore,
    paths: &[String],
    caches: &Caches,
    cancel: &CancelToken,
) -> Result<Bits, Cancelled> {
    let mut bits = Bits::new(s.len());
    if paths.is_empty() {
        return Ok(bits);
    }
    let roots = root_paths(s);
    let lookup = (paths.len() > LOOKUP_FROM).then(|| caches.name_lookup(s));
    let mut fold = String::new();
    for (i, p) in paths.iter().enumerate() {
        if i % 4096 == 0 && cancel.is_cancelled() {
            return Err(Cancelled);
        }
        fold.clear();
        fold_str_into(p, FoldOptions::DEFAULT, &mut fold);
        let trimmed = fold.trim_end_matches(['\\', '/']);
        let path = if trimmed.is_empty() { fold.as_str() } else { trimmed };
        let child = |d: EntryId, name: &[u8]| match &lookup {
            Some(l) => l.child(s, d, name),
            None => s.find_child(d, name),
        };
        if let Some(id) = resolve_path(s, path.as_bytes(), None, &roots, child)
            && s.is_live(id)
        {
            bits.set(id.idx());
        }
    }
    Ok(bits)
}

/// Compiles `si:<aqs>`: queries the source (or the cache) and marks the entries found.
pub(crate) fn compile(
    aqs: &str,
    s: &IndexStore,
    caches: &Caches,
    cancel: &CancelToken,
    src: Option<&Arc<dyn SystemIndexSource>>,
    slow: Option<&Arc<SlowState>>,
) -> Result<Leaf, Cancelled> {
    let warn = |w: ExecWarning| {
        if let Some(st) = slow {
            st.warn(w);
        }
    };
    let Some(src) = src else {
        warn(ExecWarning::SystemIndexUnavailable);
        return Ok(Leaf::Nothing);
    };
    let hits = match caches.system_index().get(src, aqs, cancel) {
        Ok(h) => h,
        Err(SystemIndexError::Cancelled) => return Err(Cancelled),
        Err(SystemIndexError::Unavailable) => {
            warn(ExecWarning::SystemIndexUnavailable);
            return Ok(Leaf::Nothing);
        }
        Err(SystemIndexError::Failed(m)) => {
            warn(ExecWarning::SystemIndexFailed(m));
            return Ok(Leaf::Nothing);
        }
    };
    if let Some(n) = hits.limit {
        warn(ExecWarning::SystemIndexLimit(n));
    }
    Ok(Leaf::In(resolve_paths(s, &hits.paths, caches, cancel)?))
}

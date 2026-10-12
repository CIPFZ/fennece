//! Query execution: parallel filtered scan producing a [`ResultSet`].

use std::time::{Duration, Instant};

use fnc_core::{EntryFlags, EntryId};
use fnc_index::IndexStore;
use fnc_query::plan::{InputSet, Node, PlannedQuery};
use rayon::prelude::*;

use crate::cache::{Caches, CancelToken, Cancelled};
use crate::eval::{CHUNK, Compiled, Scratch};
use crate::sort::SortSpec;

/// Execution statistics.
#[derive(Clone, Debug, Default)]
pub struct ExecStats {
    /// Wall time of the scan (excluding sort).
    pub scan: Duration,
    /// Wall time of the sort.
    pub sort: Duration,
    /// Entries examined.
    pub examined: usize,
    /// The previous result set was reused as input.
    pub reused: bool,
    /// Invalid patterns.
    pub errors: Vec<String>,
}

/// Ordered result of a query.
#[derive(Clone, Debug, Default)]
pub struct ResultSet {
    /// Matching entries in display order.
    pub ids: Vec<EntryId>,
    /// Index generation the ids are valid for.
    pub generation: u64,
    /// Index instance (id space).
    pub instance: u64,
    /// Current order of `ids` (`None` = id order).
    pub sort: Option<SortSpec>,
    /// Statistics.
    pub stats: ExecStats,
}

impl ResultSet {
    /// Number of results.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// `true` if the ids are valid for `s` without re-running the query.
    pub fn is_current(&self, s: &IndexStore) -> bool {
        self.generation == s.generation() && self.instance == s.instance()
    }
}

/// Inputs to [`execute`].
pub struct ExecCtx<'a> {
    /// Cancellation.
    pub cancel: &'a CancelToken,
    /// Previous result set (used when the plan's input is `Previous`).
    pub prev: Option<&'a ResultSet>,
    /// Shared caches.
    pub caches: &'a Caches,
}

#[inline]
fn visible(s: &IndexStore, id: EntryId) -> bool {
    !s.flags(id).contains(EntryFlags::DELETED)
}

/// Runs `plan` against snapshot `s`. Output preserves the input order: id order for a
/// full scan, the previous display order when filtering the previous result set.
pub fn execute(plan: &PlannedQuery, s: &IndexStore, ctx: &ExecCtx<'_>) -> Result<ResultSet, Cancelled> {
    let t0 = Instant::now();
    let prev = match (plan.input, ctx.prev) {
        (InputSet::Previous, Some(p)) if p.is_current(s) => Some(p),
        _ => None,
    };
    let compiled = Compiled::new(&plan.root, s, ctx.caches, ctx.cancel)?;
    let cancel = ctx.cancel;
    let (ids, examined) = match prev {
        Some(p) => {
            let ids = filter_ids(&p.ids, s, &compiled, cancel)?;
            (ids, p.ids.len())
        }
        None => (scan_all(&plan.root, s, &compiled, cancel)?, s.len()),
    };
    Ok(ResultSet {
        ids,
        generation: s.generation(),
        instance: s.instance(),
        sort: prev.and_then(|p| p.sort),
        stats: ExecStats {
            scan: t0.elapsed(),
            sort: Duration::ZERO,
            examined,
            reused: prev.is_some(),
            errors: compiled.errors.clone(),
        },
    })
}

fn scan_all(root: &Node, s: &IndexStore, c: &Compiled, cancel: &CancelToken) -> Result<Vec<EntryId>, Cancelled> {
    let n = s.len();
    let chunks = n.div_ceil(CHUNK);
    let match_all = *root == Node::All;
    let parts: Vec<Option<Vec<EntryId>>> = (0..chunks)
        .into_par_iter()
        .map_init(Scratch::default, |sc, k| {
            if cancel.is_cancelled() {
                return None;
            }
            let lo = k * CHUNK;
            let hi = (lo + CHUNK).min(n);
            let mut out = Vec::new();
            for i in lo..hi {
                let id = EntryId(i as u32);
                if visible(s, id) && (match_all || c.matches(s, id, sc)) {
                    out.push(id);
                }
            }
            Some(out)
        })
        .collect();
    concat(parts, cancel)
}

fn filter_ids(
    input: &[EntryId],
    s: &IndexStore,
    c: &Compiled,
    cancel: &CancelToken,
) -> Result<Vec<EntryId>, Cancelled> {
    let parts: Vec<Option<Vec<EntryId>>> = input
        .par_chunks(CHUNK)
        .map_init(Scratch::default, |sc, chunk| {
            if cancel.is_cancelled() {
                return None;
            }
            Some(chunk.iter().copied().filter(|&id| s.is_live(id) && c.matches(s, id, sc)).collect())
        })
        .collect();
    concat(parts, cancel)
}

fn concat(parts: Vec<Option<Vec<EntryId>>>, cancel: &CancelToken) -> Result<Vec<EntryId>, Cancelled> {
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    let total = parts.iter().map(|p| p.as_ref().map_or(0, Vec::len)).sum();
    let mut out = Vec::with_capacity(total);
    for p in parts {
        out.extend(p.ok_or(Cancelled)?);
    }
    Ok(out)
}

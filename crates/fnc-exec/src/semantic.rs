//! Semantic terms in execution (SPEC ch.5 §3.2). The vectors live outside this crate
//! behind [`SemanticSource`]; this module decides how each `sem:` term is evaluated:
//!
//! * a top-level conjunct (`sem:x ext:pdf`): the other terms run first; with fewer than
//!   [`EXACT_LIMIT`] candidates their vectors are scored exactly, otherwise an ANN query
//!   is intersected with them, widening `k` (up to [`MAX_TOP`]) while too few survive;
//! * a nested term (`<sem:a | sem:b>`): one ANN query, a fixed set of hits.
//!
//! The best similarity per result is kept for the relevance column and sorting; the
//! query-wide `?sem:` switch fuses literal and semantic rankings (RRF, k = 60).

use std::collections::HashMap;
use std::sync::Arc;

use fnc_core::EntryId;
use fnc_index::IndexStore;
use fnc_query::plan::{Node, Pred};
use fnc_query::semantic::SemPred;

use crate::cache::{CancelToken, Cancelled};
use crate::eval::Bits;

/// Candidate count below which vectors are scored exactly.
pub const EXACT_LIMIT: usize = 5000;
/// Largest ANN `k`.
pub const MAX_TOP: usize = 5000;
/// RRF constant.
pub const RRF_K: f32 = 60.0;

/// The embedding index as the executor sees it (implemented by the app).
pub trait SemanticSource: Send + Sync {
    /// Changes whenever stored vectors change (cached results are recomputed).
    fn generation(&self) -> u64;
    /// Candidates per term when the query does not say (`sem-top:`).
    fn default_top(&self) -> usize;
    /// Entries most similar to `q` with their cosine similarity, best first, at most
    /// `top`, honouring `q.min_score` (or the default threshold). `candidates`: score
    /// exactly these entries instead of an ANN query. `Err` carries a user-facing
    /// reason (model missing, semantic search off).
    fn search(
        &self,
        q: &SemPred,
        s: &IndexStore,
        candidates: Option<&[EntryId]>,
        top: usize,
    ) -> Result<Vec<(EntryId, f32)>, String>;
}

/// Hits of one nested term.
pub struct SemSet {
    /// Matching entries.
    pub bits: Bits,
}

/// Semantic leaves that are direct children of the root conjunction (or the root).
pub fn split_top(root: &Node) -> Option<(Vec<SemPred>, Node)> {
    let (sem, rest): (Vec<&Node>, Vec<&Node>) = match root {
        Node::Leaf(Pred::Semantic(_)) => (vec![root], Vec::new()),
        Node::And(v) => v.iter().partition(|n| matches!(n, Node::Leaf(Pred::Semantic(_)))),
        _ => return None,
    };
    if sem.is_empty() {
        return None;
    }
    let preds = sem
        .into_iter()
        .filter_map(|n| match n {
            Node::Leaf(Pred::Semantic(p)) => Some(p.clone()),
            _ => None,
        })
        .collect();
    let rest = match rest.len() {
        0 => Node::All,
        1 => rest[0].clone(),
        _ => Node::And(rest.into_iter().cloned().collect()),
    };
    Some((preds, rest))
}

/// Every semantic leaf in `n`.
pub fn collect(n: &Node, out: &mut Vec<SemPred>) {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().for_each(|c| collect(c, out)),
        Node::Not(c) => collect(c, out),
        Node::Leaf(Pred::Semantic(p)) if !out.contains(p) => out.push(p.clone()),
        _ => {}
    }
}

/// Evaluates nested terms by ANN; scores go to `scores` (best per entry).
pub fn nested_sets(
    preds: &[SemPred],
    src: Option<&Arc<dyn SemanticSource>>,
    s: &IndexStore,
    scores: &mut HashMap<u32, f32>,
    errors: &mut Vec<String>,
) -> HashMap<SemPred, Arc<SemSet>> {
    let mut out = HashMap::new();
    for p in preds {
        let mut bits = Bits::new(s.len());
        match run(p, src, s, None, None) {
            Ok(hits) => {
                for (id, score) in hits {
                    bits.set(id.idx());
                    keep_best(scores, id, score);
                }
            }
            Err(e) => push_unique(errors, e),
        }
        out.insert(p.clone(), Arc::new(SemSet { bits }));
    }
    out
}

fn push_unique(v: &mut Vec<String>, e: String) {
    if !v.contains(&e) {
        v.push(e);
    }
}

fn keep_best(scores: &mut HashMap<u32, f32>, id: EntryId, score: f32) {
    let e = scores.entry(id.0).or_insert(score);
    if score > *e {
        *e = score;
    }
}

fn run(
    p: &SemPred,
    src: Option<&Arc<dyn SemanticSource>>,
    s: &IndexStore,
    candidates: Option<&[EntryId]>,
    top: Option<usize>,
) -> Result<Vec<(EntryId, f32)>, String> {
    let src = src.ok_or_else(|| "semantic search is off".to_string())?;
    let top = top.unwrap_or_else(|| p.top.map_or(src.default_top(), |t| t as usize)).clamp(1, MAX_TOP);
    src.search(p, s, candidates, top)
}

/// Filters `candidates` (in their order) by the top-level terms `preds`; returns the
/// survivors and records their best similarity in `scores`.
pub fn filter_top(
    preds: &[SemPred],
    candidates: Option<Vec<EntryId>>,
    src: Option<&Arc<dyn SemanticSource>>,
    s: &IndexStore,
    scores: &mut HashMap<u32, f32>,
    errors: &mut Vec<String>,
    cancel: &CancelToken,
) -> Result<Vec<EntryId>, Cancelled> {
    let mut current = candidates;
    for p in preds {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let want = p.top.map_or_else(|| src.map_or(1, |s| s.default_top()), |t| t as usize).clamp(1, MAX_TOP);
        let hits = match &current {
            // Only the semantic terms: plain ANN.
            None => run(p, src, s, None, Some(want)),
            Some(c) if c.is_empty() => Ok(Vec::new()),
            // Few candidates: score exactly (results agree with the filters).
            Some(c) if c.len() < EXACT_LIMIT => run(p, src, s, Some(c), Some(want)),
            // Many: ANN, widened while the intersection is too small.
            Some(c) => {
                let mut member = Bits::new(s.len());
                for id in c {
                    member.set(id.idx());
                }
                let target = want.min(c.len());
                let mut k = want;
                loop {
                    match run(p, src, s, None, Some(k)) {
                        Ok(h) => {
                            let got = h.len();
                            let inside: Vec<(EntryId, f32)> =
                                h.into_iter().filter(|(id, _)| member.get(id.idx())).collect();
                            if inside.len() >= target || got < k || k >= MAX_TOP {
                                break Ok(inside.into_iter().take(want).collect());
                            }
                            k = (k * 4).min(MAX_TOP);
                        }
                        Err(e) => break Err(e),
                    }
                }
            }
        };
        let hits = match hits {
            Ok(h) => h,
            Err(e) => {
                push_unique(errors, e);
                Vec::new()
            }
        };
        let mut found: HashMap<u32, f32> = HashMap::with_capacity(hits.len());
        for (id, score) in hits {
            if s.is_live(id) {
                found.insert(id.0, score);
            }
        }
        current = Some(match current {
            None => {
                let mut ids: Vec<EntryId> = found.keys().map(|&i| EntryId(i)).collect();
                ids.sort_unstable();
                ids
            }
            Some(c) => c.into_iter().filter(|id| found.contains_key(&id.0)).collect(),
        });
        for (&i, &score) in &found {
            keep_best(scores, EntryId(i), score);
        }
    }
    let out = current.unwrap_or_default();
    let keep: std::collections::HashSet<u32> = out.iter().map(|id| id.0).collect();
    scores.retain(|i, _| keep.contains(i));
    Ok(out)
}

/// Reciprocal Rank Fusion of a literal ranking and a semantic one (both best first):
/// the fused order. Literal results beyond the semantic list keep their order at the end
/// (their RRF score is below every semantic-only hit's).
pub fn fuse(literal: &[EntryId], semantic: &[(EntryId, f32)]) -> Vec<EntryId> {
    let head_len = literal.len().min(semantic.len().max(1));
    let mut score: HashMap<u32, f32> = HashMap::with_capacity(head_len + semantic.len());
    let mut first_seen: HashMap<u32, usize> = HashMap::new();
    for (r, id) in literal[..head_len].iter().enumerate() {
        *score.entry(id.0).or_default() += 1.0 / (RRF_K + r as f32 + 1.0);
        first_seen.entry(id.0).or_insert(r);
    }
    let sem_set: std::collections::HashSet<u32> = semantic.iter().map(|(id, _)| id.0).collect();
    let lit_pos: HashMap<u32, usize> =
        literal.iter().enumerate().filter(|(_, id)| sem_set.contains(&id.0)).map(|(r, id)| (id.0, r)).collect();
    for (r, (id, _)) in semantic.iter().enumerate() {
        let e = score.entry(id.0).or_default();
        *e += 1.0 / (RRF_K + r as f32 + 1.0);
        // A semantic hit ranked low literally still gets its literal share.
        if let Some(&lr) = lit_pos.get(&id.0)
            && lr >= head_len
        {
            *e += 1.0 / (RRF_K + lr as f32 + 1.0);
        }
        first_seen.entry(id.0).or_insert(head_len + r);
    }
    let mut head: Vec<(u32, f32)> = score.into_iter().collect();
    head.sort_by(|a, b| b.1.total_cmp(&a.1).then(first_seen[&a.0].cmp(&first_seen[&b.0])));
    let mut out: Vec<EntryId> = head.iter().map(|(i, _)| EntryId(*i)).collect();
    out.extend(literal[head_len..].iter().filter(|id| !sem_set.contains(&id.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[u32]) -> Vec<EntryId> {
        v.iter().map(|&i| EntryId(i)).collect()
    }

    #[test]
    fn fusion_promotes_shared_hits_and_keeps_the_tail() {
        let lit = ids(&[1, 2, 3, 4, 5, 6]);
        let sem = vec![(EntryId(9), 0.8), (EntryId(3), 0.7)];
        let f = fuse(&lit, &sem);
        // In both lists: first. The literal head and the semantic-only hit follow.
        assert_eq!(f[0], EntryId(3), "{f:?}");
        assert_eq!(f.len(), 7);
        assert_eq!(&f[1..4], &ids(&[1, 9, 2])[..], "{f:?}");
        // Beyond the semantic list's length the literal order is kept.
        assert_eq!(&f[4..], &ids(&[4, 5, 6])[..]);
        assert_eq!(fuse(&lit, &[]), lit);
        assert_eq!(fuse(&[], &sem), ids(&[9, 3]));
    }
}

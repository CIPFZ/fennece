//! Reciprocal Rank Fusion (SPEC ch.5 §3.1 "融合"): `score(d) = Σ 1 / (k + rank)`.

use std::collections::HashMap;
use std::hash::Hash;

/// The RRF constant from the SPEC.
pub const RRF_K: f32 = 60.0;

/// Fuses ranked lists (best first) into one, best first; ties keep first-list order.
pub fn rrf<T: Copy + Eq + Hash>(lists: &[&[T]], k: f32) -> Vec<(T, f32)> {
    let mut score: HashMap<T, (f32, usize)> = HashMap::new();
    let mut order = 0usize;
    for list in lists {
        for (rank, &item) in list.iter().enumerate() {
            let e = score.entry(item).or_insert_with(|| {
                order += 1;
                (0.0, order)
            });
            e.0 += 1.0 / (k + rank as f32 + 1.0);
        }
    }
    let mut out: Vec<(T, f32, usize)> = score.into_iter().map(|(t, (s, o))| (t, s, o)).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    out.into_iter().map(|(t, s, _)| (t, s)).collect()
}

/// Cosine similarity as the 0–100 "relevance" column.
pub fn relevance(score: f32) -> u8 {
    (score.clamp(0.0, 1.0) * 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_in_both_lists_win() {
        let lexical = [1, 2, 3, 4];
        let semantic = [9, 3, 8];
        let f = rrf(&[&lexical, &semantic], RRF_K);
        assert_eq!(f[0].0, 3, "{f:?}");
        assert_eq!(f.len(), 6);
        assert_eq!(f[1].0, 1);
        assert_eq!(relevance(0.724), 72);
        assert_eq!(relevance(-0.2), 0);
    }
}

//! Match highlighting for displayed rows (SPEC ch.2 §8.7, ch.3 §4.1).
//!
//! Computed lazily for visible rows only: the displayed string is folded with a byte
//! map back to the original, matched with the term's patterns, and the ranges mapped
//! back. Wildcard terms highlight their literal fragments.

use std::ops::Range;

use fnc_core::fold::{FoldOptions, fold_str_mapped};
use fnc_query::plan::{Node, PatternKind, PlannedQuery, Pred, Target, TextPred};
use memchr::memmem;
use regex::bytes::RegexBuilder;

// Few per query; size does not matter.
#[allow(clippy::large_enum_variant)]
enum HPat {
    Lit(memmem::Finder<'static>),
    Re(regex::bytes::Regex),
}

struct HTerm {
    pats: Vec<HPat>,
    opts: FoldOptions,
    path: bool,
    original: bool,
}

/// Highlighter for one query.
pub struct Highlighter {
    terms: Vec<HTerm>,
}

fn collect<'a>(n: &'a Node, out: &mut Vec<&'a TextPred>) {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().for_each(|c| collect(c, out)),
        Node::Leaf(Pred::Text(t)) if t.highlight => out.push(t),
        // Negated terms and non-text predicates are not highlighted.
        _ => {}
    }
}

impl Highlighter {
    /// Builds from a plan.
    pub fn new(plan: &PlannedQuery) -> Self {
        let mut preds = Vec::new();
        collect(&plan.root, &mut preds);
        let terms = preds
            .into_iter()
            .filter_map(|t| {
                let path = matches!(t.target, Target::FullPath | Target::PathPart);
                if matches!(t.target, Target::Ext) {
                    return None;
                }
                let opts = t.fold_options();
                let pats = match t.kind {
                    PatternKind::Regex => {
                        let re = RegexBuilder::new(&t.pattern).case_insensitive(!t.case).build().ok()?;
                        vec![HPat::Re(re)]
                    }
                    PatternKind::Wildcard => t
                        .folded_pattern()
                        .split(['*', '?'])
                        .filter(|f| !f.is_empty())
                        .map(|f| HPat::Lit(memmem::Finder::new(f.as_bytes()).into_owned()))
                        .collect(),
                    _ => {
                        let f = t.folded_pattern();
                        if f.is_empty() {
                            return None;
                        }
                        vec![HPat::Lit(memmem::Finder::new(f.as_bytes()).into_owned())]
                    }
                };
                Some(HTerm { pats, opts, path, original: t.kind == PatternKind::Regex })
            })
            .collect();
        Self { terms }
    }

    /// `true` if nothing would ever be highlighted.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    fn ranges(&self, text: &str, want_path_terms: bool, out: &mut Vec<Range<usize>>) {
        let mut folded = String::new();
        let mut map = Vec::new();
        for term in self.terms.iter().filter(|t| t.path == want_path_terms || !want_path_terms) {
            let (subject, mapped): (&[u8], bool) = if term.original {
                (text.as_bytes(), false)
            } else {
                fold_str_mapped(text, term.opts, &mut folded, &mut map);
                (folded.as_bytes(), true)
            };
            for pat in &term.pats {
                let mut push = |a: usize, b: usize| {
                    if a >= b {
                        return;
                    }
                    let (sa, sb) = if mapped {
                        let sa = map[a] as usize;
                        // End maps to the start of the char after the last matched byte.
                        let last = map[b - 1] as usize;
                        let sb = text[last..].chars().next().map_or(text.len(), |c| last + c.len_utf8());
                        (sa, sb)
                    } else {
                        (a, b)
                    };
                    out.push(sa..sb);
                };
                match pat {
                    HPat::Lit(f) => {
                        for i in f.find_iter(subject) {
                            push(i, i + f.needle().len());
                        }
                    }
                    HPat::Re(r) => {
                        for m in r.find_iter(subject) {
                            push(m.start(), m.end());
                        }
                    }
                }
            }
        }
        merge(out);
    }

    /// Highlight ranges (byte offsets) within a displayed name.
    pub fn name_ranges(&self, name: &str, out: &mut Vec<Range<usize>>) {
        out.clear();
        self.ranges(name, false, out);
    }

    /// Highlight ranges within a displayed path (path terms only).
    pub fn path_ranges(&self, path: &str, out: &mut Vec<Range<usize>>) {
        out.clear();
        if self.terms.iter().any(|t| t.path) {
            self.ranges(path, true, out);
        }
    }
}

fn merge(v: &mut Vec<Range<usize>>) {
    v.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v.drain(..) {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    *v = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnc_query::{ParseEnv, PlanEnv, parse, plan};

    fn hl(q: &str, name: &str) -> Vec<Range<usize>> {
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let h = Highlighter::new(&p);
        let mut out = Vec::new();
        h.name_ranges(name, &mut out);
        out
    }

    #[test]
    fn literal_and_folded() {
        assert_eq!(hl("abc", "xxABCxxabc"), vec![2..5, 7..10]);
        assert_eq!(hl("cafe", "Le Café"), vec![3..8]);
        assert_eq!(hl("ss", "Straße"), vec![4..6]);
        assert_eq!(hl("a b", "ab ba"), vec![0..2, 3..5]);
    }

    #[test]
    fn wildcard_fragments_and_not() {
        assert_eq!(hl("*.jpg", "IMG.JPG"), vec![3..7]);
        assert_eq!(hl("!abc", "abc"), Vec::<Range<usize>>::new());
        assert_eq!(hl("nohighlight:abc", "abc"), Vec::<Range<usize>>::new());
        assert_eq!(hl("regex:\\d+", "a12b3"), vec![1..3, 4..5]);
    }
}

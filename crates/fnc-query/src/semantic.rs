//! Semantic search terms (SPEC ch.5 §3.2, Fennec extension): `sem:` `semantic:`
//! `semname:` `semcontent:` `semcode:` `semimage:`, thresholds (`sem:>0.6 text`,
//! `sem-min-score:0.6`), candidate counts (`sem-top:500`) and the query-wide `?sem:`
//! switch (send the whole search to the semantic channel too and fuse).

use crate::ast::{CmpOp, Expr, Term, Value};
use crate::funcs::FuncId;
use crate::plan::{Node, Pred};

/// Chunk kinds a term searches (bits match `fnc_semantic::Kind`).
pub mod kinds {
    /// File names.
    pub const NAME: u8 = 1;
    /// Document text.
    pub const CONTENT: u8 = 2;
    /// Source code.
    pub const CODE: u8 = 4;
    /// Pictures.
    pub const IMAGE: u8 = 8;
    /// Everything indexed.
    pub const ALL: u8 = 15;
}

/// A semantic term.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SemPred {
    /// Natural-language text.
    pub text: String,
    /// Kinds searched ([`kinds`]).
    pub kinds: u8,
    /// Similarity threshold in thousandths (`None` = the setting).
    pub min_score: Option<u16>,
    /// Candidates (`None` = the setting).
    pub top: Option<u32>,
}

impl SemPred {
    /// Threshold as a cosine similarity.
    pub fn min_score_f32(&self) -> Option<f32> {
        self.min_score.map(|m| f32::from(m) / 1000.0)
    }
}

/// Kinds searched by a semantic function, `None` for other functions.
pub fn func_kinds(f: FuncId) -> Option<u8> {
    Some(match f {
        FuncId::Semantic => kinds::ALL,
        FuncId::SemName => kinds::NAME,
        FuncId::SemContent => kinds::CONTENT,
        FuncId::SemCode => kinds::CODE,
        FuncId::SemImage => kinds::IMAGE,
        _ => return None,
    })
}

/// `0.6`, `60` or `60%` → 600 thousandths.
pub fn parse_score(s: &str) -> Option<u16> {
    let s = s.trim();
    let (num, pct) = match s.strip_suffix('%') {
        Some(n) => (n, true),
        None => (s, false),
    };
    let v: f32 = num.trim().parse().ok()?;
    let v = if pct || v > 1.0 { v / 100.0 } else { v };
    (0.0..=1.0).contains(&v).then(|| (v * 1000.0).round() as u16)
}

/// Threshold of a value like `>0.6` (only `>`/`>=` make sense for similarity). Text
/// functions keep the operator in their text value.
fn threshold(v: &Value) -> Option<u16> {
    match v {
        Value::Cmp(CmpOp::Gt | CmpOp::Ge, inner) => inner.as_text().and_then(parse_score),
        Value::Text(t) => t.strip_prefix(">=").or_else(|| t.strip_prefix('>')).and_then(parse_score),
        _ => None,
    }
}

/// `sem:>0.6 季度预算`: a threshold-only semantic term takes its text from the plain term
/// right after it (SPEC ch.5 §3.2). Applied to every AND list of the tree.
pub fn merge_thresholds(e: &Expr) -> Expr {
    match e {
        Expr::And(v) => {
            let mut out: Vec<Expr> = Vec::with_capacity(v.len());
            let mut i = 0;
            while i < v.len() {
                let merged = match (&v[i], v.get(i + 1)) {
                    (Expr::Term(t), Some(Expr::Term(next)))
                        if t.func.and_then(func_kinds).is_some()
                            && threshold(&t.value).is_some()
                            && next.func.is_none()
                            && next.value.as_text().is_some() =>
                    {
                        let text = next.value.as_text().unwrap_or_default().to_string();
                        Some(Expr::Term(Term {
                            mods: t.mods,
                            func: t.func,
                            value: Value::Cmp(
                                CmpOp::Ge,
                                Box::new(Value::Text(format!(
                                    "{}\u{1}{text}",
                                    threshold(&t.value).map_or(0.0, |m| f32::from(m) / 1000.0)
                                ))),
                            ),
                            raw: t.raw.start..next.raw.end,
                        }))
                    }
                    _ => None,
                };
                match merged {
                    Some(m) => {
                        out.push(m);
                        i += 2;
                    }
                    None => {
                        out.push(merge_thresholds(&v[i]));
                        i += 1;
                    }
                }
            }
            Expr::And(out)
        }
        Expr::Or(v) => Expr::Or(v.iter().map(merge_thresholds).collect()),
        Expr::Not(inner) => Expr::Not(Box::new(merge_thresholds(inner))),
        Expr::Macro(m, body) => Expr::Macro(m.clone(), Box::new(merge_thresholds(body))),
        other => other.clone(),
    }
}

/// What a semantic function term plans to.
pub enum Planned {
    /// A semantic leaf.
    Leaf(SemPred),
    /// A query-wide threshold (`sem-min-score:`), `sem:>0.6` without text.
    MinScore(u16),
    /// A query-wide candidate count (`sem-top:`).
    Top(u32),
    /// Unusable value.
    Invalid,
}

/// Plans a semantic function term (`f` is one of the `Sem*` functions).
pub fn plan_term(f: FuncId, t: &Term) -> Planned {
    match f {
        FuncId::SemMinScore => match &t.value {
            Value::Text(s) => parse_score(s).map_or(Planned::Invalid, Planned::MinScore),
            v => threshold(v).map_or(Planned::Invalid, Planned::MinScore),
        },
        FuncId::SemTop => match t.value.as_text().and_then(|s| s.trim().parse::<u32>().ok()) {
            Some(n) if n > 0 => Planned::Top(n.min(5000)),
            _ => Planned::Invalid,
        },
        _ => {
            let Some(kinds) = func_kinds(f) else { return Planned::Invalid };
            match &t.value {
                v @ Value::Text(s) if s.starts_with('>') => threshold(v).map_or(Planned::Invalid, Planned::MinScore),
                Value::Text(s) if !s.trim().is_empty() => {
                    Planned::Leaf(SemPred { text: s.trim().to_string(), kinds, min_score: None, top: None })
                }
                // `sem:a;b` — the list is one text.
                Value::List(items) => {
                    let text: Vec<&str> = items.iter().filter_map(Value::as_text).collect();
                    if text.is_empty() {
                        Planned::Invalid
                    } else {
                        Planned::Leaf(SemPred { text: text.join(";"), kinds, min_score: None, top: None })
                    }
                }
                Value::Cmp(CmpOp::Ge | CmpOp::Gt, inner) => {
                    let raw = inner.as_text().unwrap_or_default();
                    match raw.split_once('\u{1}') {
                        // Merged by [`merge_thresholds`].
                        Some((score, text)) => match parse_score(score) {
                            Some(m) => {
                                Planned::Leaf(SemPred { text: text.to_string(), kinds, min_score: Some(m), top: None })
                            }
                            None => Planned::Invalid,
                        },
                        None => parse_score(raw).map_or(Planned::Invalid, Planned::MinScore),
                    }
                }
                _ => Planned::Invalid,
            }
        }
    }
}

/// Applies query-wide thresholds and candidate counts to every semantic leaf without its
/// own.
pub fn apply_globals(n: &mut Node, min_score: Option<u16>, top: Option<u32>) {
    match n {
        Node::And(v) | Node::Or(v) => v.iter_mut().for_each(|c| apply_globals(c, min_score, top)),
        Node::Not(c) => apply_globals(c, min_score, top),
        Node::Leaf(Pred::Semantic(p)) => {
            p.min_score = p.min_score.or(min_score);
            p.top = p.top.or(top);
        }
        _ => {}
    }
}

/// `true` if `n` has a semantic leaf.
pub fn contains_semantic(n: &Node) -> bool {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().any(contains_semantic),
        Node::Not(c) => contains_semantic(c),
        Node::Leaf(Pred::Semantic(_)) => true,
        _ => false,
    }
}

/// Removes an unquoted `?sem:` / `?semantic:` switch from `text`; returns the rest and
/// whether it was present.
pub fn take_switch(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut found = false;
    let mut in_quote = false;
    let mut word_start = true;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '"' {
            in_quote = !in_quote;
        }
        if !in_quote && word_start && c == '?' {
            let rest = &text[i + 1..];
            let lower = rest.get(..9).map(str::to_ascii_lowercase).unwrap_or_else(|| rest.to_ascii_lowercase());
            let len = if lower.starts_with("semantic:") {
                9
            } else if lower.starts_with("sem:") {
                4
            } else {
                0
            };
            let ends_word = rest[len.min(rest.len())..].chars().next().is_none_or(char::is_whitespace);
            if len > 0 && ends_word {
                found = true;
                for _ in 0..len {
                    chars.next();
                }
                continue;
            }
        }
        word_start = c.is_whitespace() && !in_quote;
        out.push(c);
    }
    let cleaned = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (if found { cleaned } else { text.to_string() }, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores() {
        assert_eq!(parse_score("0.6"), Some(600));
        assert_eq!(parse_score("60"), Some(600));
        assert_eq!(parse_score("45%"), Some(450));
        assert_eq!(parse_score("2.5"), Some(25));
        assert_eq!(parse_score("x"), None);
        assert_eq!(parse_score("150"), None);
    }

    #[test]
    fn switch_is_removed() {
        assert_eq!(take_switch("?sem: 上季度的预算表"), ("上季度的预算表".to_string(), true));
        assert_eq!(take_switch("budget ?SEMANTIC: report"), ("budget report".to_string(), true));
        assert_eq!(take_switch("\"?sem:\" x"), ("\"?sem:\" x".to_string(), false));
        assert_eq!(take_switch("a?sem: b"), ("a?sem: b".to_string(), false));
        assert_eq!(take_switch("?semx: b"), ("?semx: b".to_string(), false));
        assert_eq!(take_switch("plain"), ("plain".to_string(), false));
    }

    fn plan_of(q: &str) -> crate::PlannedQuery {
        let parsed = crate::parse(q, &crate::ParseEnv::default());
        crate::plan(&parsed.ast, &crate::PlanEnv::default(), None)
    }

    fn sem_leaves(n: &Node, out: &mut Vec<SemPred>) {
        match n {
            Node::And(v) | Node::Or(v) => v.iter().for_each(|c| sem_leaves(c, out)),
            Node::Not(c) => sem_leaves(c, out),
            Node::Leaf(Pred::Semantic(p)) => out.push(p.clone()),
            _ => {}
        }
    }

    fn leaves(q: &str) -> Vec<SemPred> {
        let mut v = Vec::new();
        sem_leaves(&plan_of(q).root, &mut v);
        v
    }

    #[test]
    fn functions_plan_to_semantic_leaves() {
        let l = leaves(r#"sem:"季度预算表" ext:xlsx"#);
        assert_eq!(l, vec![SemPred { text: "季度预算表".into(), kinds: kinds::ALL, min_score: None, top: None }]);
        assert_eq!(leaves("semantic:budget")[0].kinds, kinds::ALL);
        assert_eq!(leaves("semname:budget")[0].kinds, kinds::NAME);
        assert_eq!(leaves(r#"semcontent:"how to rotate api key""#)[0].text, "how to rotate api key");
        assert_eq!(leaves(r#"semcode:"parse usn record" path:src\"#)[0].kinds, kinds::CODE);
        assert_eq!(leaves(r#"semimage:"sunset on beach""#)[0].kinds, kinds::IMAGE);
        // Thresholds and candidate counts.
        let t = leaves("sem:>0.6 季度预算");
        assert_eq!((t[0].text.as_str(), t[0].min_score), ("季度预算", Some(600)));
        assert_eq!(leaves("sem-min-score:0.7 sem:budget")[0].min_score, Some(700));
        assert_eq!(leaves("sem:budget sem-top:500")[0].top, Some(500));
        assert_eq!(leaves("sem:>=55% invoices")[0].min_score, Some(550));
        // The semantic term runs last, after the cheap filters.
        let root = plan_of("sem:budget ext:xlsx").root;
        let Node::And(v) = &root else { panic!("{root:?}") };
        assert!(matches!(v.last(), Some(Node::Leaf(Pred::Semantic(_)))), "{v:?}");
        // Errors.
        assert!(!plan_of("sem:").diagnostics.is_empty());
        assert!(!plan_of("sem-top:x").diagnostics.is_empty());
    }

    #[test]
    fn semantic_plans_never_reuse_previous_results() {
        let env = crate::PlanEnv::default();
        let a = crate::plan(&crate::parse("sem:budget", &crate::ParseEnv::default()).ast, &env, None);
        let b = crate::plan(&crate::parse("sem:budget ext:xlsx", &crate::ParseEnv::default()).ast, &env, Some(&a));
        assert_eq!(b.input, crate::InputSet::All);
        let c = crate::plan(&crate::parse("ext:xlsx", &crate::ParseEnv::default()).ast, &env, None);
        let d = crate::plan(&crate::parse("ext:xlsx abc", &crate::ParseEnv::default()).ast, &env, Some(&c));
        assert_eq!(d.input, crate::InputSet::Previous);
    }
}

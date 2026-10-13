//! Filters (SPEC ch.2 §7): a named search with its own match options that is AND-ed
//! with the user's search, like `<filter search> <user text>`.
//!
//! Several active filters are combined with OR by default; `and-filter:` /
//! `or-filter:` in the user's search (or [`FilterSet::and`]) choose the mode. A
//! filter's match options (case, whole word, path, diacritics) are defaults for the
//! filter's own terms only — explicit modifiers inside the filter search still win —
//! and `regex` makes the whole filter search one regular expression.

use crate::ast::{Expr, ModSet, Modifier, Term, Value};
use crate::parser::{ParseEnv, UserMacro, parse};

/// One filter (Filters.csv row).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Display name.
    pub name: String,
    /// Search text.
    pub search: String,
    /// Macro name: `name:` in a search expands to this filter's search.
    pub macro_name: Option<String>,
    /// Match case for the filter's terms.
    pub case: bool,
    /// Match whole words.
    pub whole_word: bool,
    /// Match the full path.
    pub path: bool,
    /// Match diacritics.
    pub diacritics: bool,
    /// The search is one regular expression.
    pub regex: bool,
}

impl Filter {
    /// A filter with default options.
    pub fn new(search: impl Into<String>) -> Self {
        Self { search: search.into(), ..Self::default() }
    }

    /// The filter's options as default modifiers (only enabled options are set, so
    /// disabled ones fall back to the global settings).
    pub fn modifiers(&self) -> ModSet {
        let mut m = ModSet::EMPTY;
        for (on, modifier) in [
            (self.case, Modifier::Case),
            (self.whole_word, Modifier::WholeWord),
            (self.path, Modifier::Path),
            (self.diacritics, Modifier::Diacritics),
        ] {
            if on {
                m.set(modifier, true);
            }
        }
        m
    }

    /// The user macro defined by this filter, if it has a macro name. The macro's
    /// expansion is the filter search (`#param:` in it is replaced by the argument).
    pub fn user_macro(&self) -> Option<UserMacro> {
        let name = self.macro_name.as_deref()?.trim().trim_end_matches(':');
        (!name.is_empty()).then(|| UserMacro { name: name.to_string(), search: self.search.clone() })
    }

    /// Parses the filter search with its options applied.
    pub fn expr(&self, env: &ParseEnv<'_>) -> Expr {
        if self.search.trim().is_empty() {
            return Expr::All;
        }
        let mods = self.modifiers();
        if self.regex {
            return Expr::Term(Term {
                mods: mods.with(Modifier::Regex, true),
                func: None,
                value: Value::Text(self.search.clone()),
                raw: 0..self.search.len(),
            });
        }
        with_default_mods(parse(&self.search, env).ast, mods)
    }
}

/// The active filters of a search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterSet {
    /// Active filters (empty = "Everything").
    pub filters: Vec<Filter>,
    /// Combine several filters with AND (`and-filter:`) instead of OR.
    pub and: bool,
}

impl FilterSet {
    /// A set with one filter.
    pub fn single(f: Filter) -> Self {
        Self { filters: vec![f], and: false }
    }

    /// `true` if no filter restricts the search.
    pub fn is_empty(&self) -> bool {
        self.filters.iter().all(|f| f.search.trim().is_empty())
    }

    /// Combined filter expression; `and_override` comes from `and-filter:` /
    /// `or-filter:` in the user's search.
    pub fn expr(&self, env: &ParseEnv<'_>, and_override: Option<bool>) -> Expr {
        let parts: Vec<Expr> = self.filters.iter().map(|f| f.expr(env)).collect();
        match parts.len() {
            0 => Expr::All,
            1 => parts.into_iter().next().unwrap_or(Expr::All),
            _ if and_override.unwrap_or(self.and) => Expr::And(parts),
            // OR with an "Everything" filter matches everything.
            _ if parts.contains(&Expr::All) => Expr::All,
            _ => Expr::Or(parts),
        }
    }
}

impl From<&str> for FilterSet {
    fn from(search: &str) -> Self {
        Self::single(Filter::new(search))
    }
}

impl From<String> for FilterSet {
    fn from(search: String) -> Self {
        Self::single(Filter::new(search))
    }
}

/// Text form of "filter AND user search" (`<filter> <user>`), for display, history and
/// IPC clients that only take a search string. Filter options are not representable
/// here; use [`Filter::expr`] + [`combine`] to keep them.
pub fn combine_filter(filter_search: &str, user_text: &str) -> String {
    let (f, u) = (filter_search.trim(), user_text.trim());
    match (f.is_empty(), u.is_empty()) {
        (true, _) => u.to_string(),
        (false, true) => f.to_string(),
        (false, false) => format!("<{f}> <{u}>"),
    }
}

/// `filter AND text` at the expression level.
pub fn combine(filter: Expr, text: Expr) -> Expr {
    match (filter, text) {
        (Expr::All, t) => t,
        (f, Expr::All) => f,
        (f, t) => Expr::And(vec![f, t]),
    }
}

/// Applies `mods` as defaults to every term of `e` (term modifiers win).
pub fn with_default_mods(e: Expr, mods: ModSet) -> Expr {
    if mods.is_empty() {
        return e;
    }
    match e {
        Expr::All => Expr::All,
        Expr::And(v) => Expr::And(v.into_iter().map(|e| with_default_mods(e, mods)).collect()),
        Expr::Or(v) => Expr::Or(v.into_iter().map(|e| with_default_mods(e, mods)).collect()),
        Expr::Not(e) => Expr::Not(Box::new(with_default_mods(*e, mods))),
        Expr::Term(mut t) => {
            t.mods = mods.overlay(t.mods);
            Expr::Term(t)
        }
        Expr::Macro(mut m, body) => {
            m.mods = mods.overlay(m.mods);
            Expr::Macro(m, Box::new(with_default_mods(*body, mods)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ParseEnv<'static> {
        ParseEnv::default()
    }

    #[test]
    fn text_combination() {
        assert_eq!(combine_filter("", "abc"), "abc");
        assert_eq!(combine_filter("ext:mp3", ""), "ext:mp3");
        assert_eq!(combine_filter("ext:mp3|ext:flac", "a|b"), "<ext:mp3|ext:flac> <a|b>");
        let joined = parse(&combine_filter("ext:mp3|ext:flac", "beatles"), &env()).ast;
        assert!(matches!(joined, Expr::And(ref v) if v.len() == 2));
    }

    #[test]
    fn filter_options_are_term_defaults() {
        let f = Filter { case: true, ..Filter::new("Abc nocase:def") };
        match f.expr(&env()) {
            Expr::And(v) => match (&v[0], &v[1]) {
                (Expr::Term(a), Expr::Term(b)) => {
                    assert_eq!(a.mods.get(Modifier::Case), Some(true));
                    assert_eq!(b.mods.get(Modifier::Case), Some(false));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
        let re = Filter { regex: true, ..Filter::new("^a|b$") }.expr(&env());
        match re {
            Expr::Term(t) => {
                assert_eq!(t.mods.get(Modifier::Regex), Some(true));
                assert_eq!(t.value, Value::Text("^a|b$".into()));
            }
            other => panic!("{other:?}"),
        }
        let path = Filter { path: true, ..Filter::new("audio:") }.expr(&env());
        assert!(matches!(path, Expr::Macro(ref m, _) if m.mods.get(Modifier::Path) == Some(true)));
    }

    #[test]
    fn sets_and_or() {
        let set = FilterSet { filters: vec![Filter::new("ext:mp3"), Filter::new("ext:flac")], and: false };
        assert!(matches!(set.expr(&env(), None), Expr::Or(_)));
        assert!(matches!(set.expr(&env(), Some(true)), Expr::And(_)));
        let with_all = FilterSet { filters: vec![Filter::new(""), Filter::new("ext:flac")], and: false };
        assert_eq!(with_all.expr(&env(), None), Expr::All);
        assert!(FilterSet::from("").is_empty());
        assert!(!FilterSet::from("pic:").is_empty());
        assert_eq!(FilterSet::default().expr(&env(), None), Expr::All);
    }

    #[test]
    fn macros_from_filters() {
        let f = Filter { macro_name: Some("Music:".into()), ..Filter::new("ext:mp3;flac") };
        let m = f.user_macro().unwrap();
        assert_eq!((m.name.as_str(), m.search.as_str()), ("Music", "ext:mp3;flac"));
        assert_eq!(Filter::new("x").user_macro(), None);
        assert_eq!(combine(Expr::All, Expr::All), Expr::All);
    }
}

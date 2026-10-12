//! Text matchers compiled from [`TextPred`]s.
//!
//! The common case — default options matched against the stored fold key — is a
//! SIMD substring search (`memchr::memmem`) over bytes with no allocation. Other
//! options fold the subject into a per-thread scratch buffer.

use fnc_core::fold::{FoldOptions, fold_utf16_with};
use fnc_query::plan::{PatternKind, Target, TextPred};
use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};

/// Compilation error (invalid regex).
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid pattern `{pattern}`: {message}")]
pub struct PatternError {
    /// Pattern text.
    pub pattern: String,
    /// Regex engine message.
    pub message: String,
}

/// Compiled pattern.
// One per search term; boxing the finder would add an indirection on the hot path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Pattern {
    /// Matches everything (empty pattern).
    Any,
    /// Substring.
    Contains(memmem::Finder<'static>),
    /// Prefix.
    Starts(Vec<u8>),
    /// Suffix.
    Ends(Vec<u8>),
    /// Equality.
    Equals(Vec<u8>),
    /// General regex (wildcards are translated to a regex too).
    Regex(Regex),
}

impl Pattern {
    /// `true` if `s` matches.
    #[inline]
    pub fn is_match(&self, s: &[u8]) -> bool {
        match self {
            Pattern::Any => true,
            Pattern::Contains(f) => f.find(s).is_some(),
            Pattern::Starts(p) => s.starts_with(p),
            Pattern::Ends(p) => s.ends_with(p),
            Pattern::Equals(p) => s == p.as_slice(),
            Pattern::Regex(r) => r.is_match(s),
        }
    }

    /// First match range in `s` (for highlighting).
    pub fn find(&self, s: &[u8]) -> Option<(usize, usize)> {
        match self {
            Pattern::Any => None,
            Pattern::Contains(f) => f.find(s).map(|i| (i, i + f.needle().len())),
            Pattern::Starts(p) => s.starts_with(p).then_some((0, p.len())),
            Pattern::Ends(p) => s.ends_with(p).then(|| (s.len() - p.len(), s.len())),
            Pattern::Equals(p) => (s == p.as_slice()).then_some((0, s.len())),
            Pattern::Regex(r) => r.find(s).map(|m| (m.start(), m.end())),
        }
    }
}

/// Removes punctuation and/or whitespace (for `nopunc:` / `nows:`).
pub fn strip_chars(s: &str, punct: bool, ws: bool, out: &mut String) {
    out.clear();
    out.extend(s.chars().filter(|c| {
        !(punct && (c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace())))
            && !(ws && c.is_whitespace())
    }));
}

/// Translates a wildcard pattern into a regex. `*` excludes `\` unless `star_all`;
/// `**` matches anything; `?` is one character; `[...]` sets when `sets` is on.
pub fn wildcard_to_regex(pat: &str, star_all: bool, sets: bool) -> String {
    let mut re = String::with_capacity(pat.len() * 2 + 8);
    re.push_str("^(?s:");
    let chars: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' if chars.get(i + 1) == Some(&'*') => {
                re.push_str(".*");
                i += 2;
                while chars.get(i) == Some(&'*') {
                    i += 1;
                }
                continue;
            }
            '*' => re.push_str(if star_all { ".*" } else { r"[^\\]*" }),
            '?' => re.push('.'),
            '[' if sets => {
                let close = chars[i + 1..].iter().position(|&x| x == ']').map(|p| p + i + 1);
                match close {
                    Some(end) if end > i + 1 => {
                        re.push('[');
                        let mut k = i + 1;
                        if chars[k] == '!' || chars[k] == '^' {
                            re.push('^');
                            k += 1;
                        }
                        for &sc in &chars[k..end] {
                            if matches!(sc, '\\' | '[' | ']' | '^' | '&' | '~') {
                                re.push('\\');
                            }
                            re.push(sc);
                        }
                        re.push(']');
                        i = end + 1;
                        continue;
                    }
                    _ => re.push_str(r"\["),
                }
            }
            c => {
                let mut b = [0u8; 4];
                re.push_str(&regex::escape(c.encode_utf8(&mut b)));
            }
        }
        i += 1;
    }
    re.push_str(")$");
    re
}

/// Compiles the matcher for a text predicate. Patterns are folded like the subject.
pub fn compile_pattern(t: &TextPred) -> Result<Pattern, PatternError> {
    let err = |e: regex::Error| PatternError { pattern: t.pattern.clone(), message: e.to_string() };
    if t.kind == PatternKind::Regex {
        let re = RegexBuilder::new(&t.pattern)
            .case_insensitive(!t.case)
            .dot_matches_new_line(t.regex_flags.0)
            .multi_line(t.regex_flags.1)
            .size_limit(1 << 23)
            .build()
            .map_err(err)?;
        return Ok(Pattern::Regex(re));
    }
    let mut folded = t.folded_pattern();
    if t.ignore_punct || t.ignore_ws {
        let mut s = String::new();
        strip_chars(&folded, t.ignore_punct, t.ignore_ws, &mut s);
        folded = s;
    }
    let word = t.whole_word || t.prefix || t.suffix;
    if word && t.kind == PatternKind::Substring {
        let mut re = String::new();
        if t.whole_word || t.prefix {
            re.push_str(r"(?:^|[^\p{Alphabetic}\p{Nd}])");
        }
        re.push_str(&regex::escape(&folded));
        if t.whole_word || t.suffix {
            re.push_str(r"(?:$|[^\p{Alphabetic}\p{Nd}])");
        }
        return RegexBuilder::new(&re).build().map(Pattern::Regex).map_err(err);
    }
    let bytes = folded.clone().into_bytes();
    Ok(match t.kind {
        PatternKind::Substring if bytes.is_empty() => Pattern::Any,
        PatternKind::Substring => Pattern::Contains(memmem::Finder::new(&bytes).into_owned()),
        PatternKind::StartsWith => Pattern::Starts(bytes),
        PatternKind::EndsWith => Pattern::Ends(bytes),
        PatternKind::Exact => Pattern::Equals(bytes),
        PatternKind::Wildcard => {
            let flat_target = matches!(t.target, Target::Name | Target::Stem | Target::Ext) || t.star_all;
            let inner = &folded;
            let b = inner.as_bytes();
            let simple = |s: &str| !s.contains(['*', '?']) && !(t.wildcard_sets && s.contains('['));
            if flat_target
                && inner.len() > 1
                && inner.starts_with('*')
                && !inner.starts_with("**")
                && simple(&inner[1..])
            {
                Pattern::Ends(b[1..].to_vec())
            } else if flat_target
                && inner.len() > 1
                && inner.ends_with('*')
                && simple(&inner[..inner.len() - 1])
                && !inner.ends_with("**")
            {
                Pattern::Starts(b[..b.len() - 1].to_vec())
            } else if flat_target
                && inner.len() > 2
                && inner.starts_with('*')
                && inner.ends_with('*')
                && simple(&inner[1..inner.len() - 1])
            {
                Pattern::Contains(memmem::Finder::new(&b[1..b.len() - 1]).into_owned())
            } else if inner == "*" || inner == "**" {
                Pattern::Any
            } else {
                let re = wildcard_to_regex(inner, t.star_all, t.wildcard_sets);
                RegexBuilder::new(&re).size_limit(1 << 23).build().map(Pattern::Regex).map_err(err)?
            }
        }
        PatternKind::Regex => unreachable_regex(),
    })
}

#[inline]
fn unreachable_regex() -> Pattern {
    // Handled above; keep the match exhaustive without panicking.
    Pattern::Any
}

/// How the subject string is produced for a predicate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SubjectMode {
    /// Stored default fold key (fast path).
    StoredFold,
    /// Re-fold the UTF-16 name with these options.
    Refold(FoldOptions),
    /// Original text as UTF-8 (regex).
    Original,
}

/// Picks the subject mode of a predicate.
pub fn subject_mode(t: &TextPred) -> SubjectMode {
    if t.kind == PatternKind::Regex {
        SubjectMode::Original
    } else if t.case || t.diacritics {
        SubjectMode::Refold(t.fold_options())
    } else {
        SubjectMode::StoredFold
    }
}

/// Writes the name subject into `out` for non-stored modes.
pub fn name_subject(name: &[u16], mode: SubjectMode, out: &mut Vec<u8>) {
    out.clear();
    match mode {
        SubjectMode::StoredFold => fold_utf16_with(name, FoldOptions::DEFAULT, out),
        SubjectMode::Refold(o) => fold_utf16_with(name, o, out),
        SubjectMode::Original => {
            for c in char::decode_utf16(name.iter().copied()) {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.unwrap_or(char::REPLACEMENT_CHARACTER).encode_utf8(&mut b).as_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnc_query::plan::{Node, Pred};
    use fnc_query::{ParseEnv, PlanEnv, parse, plan};

    fn pred(q: &str) -> TextPred {
        match plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None).root {
            Node::Leaf(Pred::Text(t)) => t,
            other => panic!("{other:?}"),
        }
    }

    fn m(q: &str, subject: &str) -> bool {
        let t = pred(q);
        let p = compile_pattern(&t).unwrap();
        let mut s = Vec::new();
        match subject_mode(&t) {
            SubjectMode::StoredFold => s.extend_from_slice(fnc_core::fold::fold_str(subject).as_bytes()),
            mode => name_subject(&subject.encode_utf16().collect::<Vec<_>>(), mode, &mut s),
        }
        if t.ignore_punct || t.ignore_ws {
            let mut o = String::new();
            strip_chars(std::str::from_utf8(&s).unwrap(), t.ignore_punct, t.ignore_ws, &mut o);
            s = o.into_bytes();
        }
        p.is_match(&s)
    }

    #[test]
    fn substring_and_case() {
        assert!(m("abc", "xxABCxx"));
        assert!(!m("abd", "xxABCxx"));
        assert!(m("cafe", "Café.txt"));
        assert!(m("strasse", "Straße"));
        assert!(m("STRASSE", "straße"));
        assert!(!m("case:abc", "ABC"));
        assert!(m("case:ABC", "xABCx"));
        assert!(!m("diacritics:cafe", "café"));
        assert!(m("diacritics:café", "CAFÉ"));
    }

    #[test]
    fn wildcards() {
        assert!(m("*.jpg", "IMG_0001.JPG"));
        assert!(!m("*.jpg", "IMG_0001.JPG.txt"));
        assert!(m("img*", "IMG_0001.JPG"));
        assert!(!m("img*", "my img"));
        assert!(m("*001*", "IMG_0001.JPG"));
        assert!(m("a?c", "abc"));
        assert!(!m("a?c", "abbc"));
        assert!(m("a?c", "aéc"));
        assert!(m("*.tar.*", "x.tar.gz"));
        assert!(m("wildcards:[ab]x", "bx"));
        assert!(!m("wildcards:[ab]x", "cx"));
        assert!(m("wildcards:[!ab]x", "cx"));
    }

    #[test]
    fn wildcard_star_and_path_separator() {
        let re = Regex::new(&wildcard_to_regex("c:\\windows\\*", false, false)).unwrap();
        assert!(re.is_match(b"c:\\windows\\notepad.exe"));
        assert!(!re.is_match(b"c:\\windows\\system32\\cmd.exe"));
        let re = Regex::new(&wildcard_to_regex("c:\\windows\\**", false, false)).unwrap();
        assert!(re.is_match(b"c:\\windows\\system32\\cmd.exe"));
        let re = Regex::new(&wildcard_to_regex("c:\\windows\\*", true, false)).unwrap();
        assert!(re.is_match(b"c:\\windows\\system32\\cmd.exe"));
        let re = Regex::new(&wildcard_to_regex("a+b(c)", false, false)).unwrap();
        assert!(re.is_match(b"a+b(c)"));
    }

    #[test]
    fn regex_on_original_name() {
        assert!(m("regex:^IMG_\\d{4}", "img_1234.jpg"));
        assert!(m("regex:^IMG_\\d{4}", "IMG_1234.jpg"));
        assert!(!m("case:regex:^IMG_\\d{4}", "img_1234.jpg"));
        assert!(!m("regex:^IMG_\\d{4}", "IMG_12.jpg"));
        assert!(m("regex:é", "café"));
        let bad = pred("regex:(unclosed");
        assert!(compile_pattern(&bad).is_err());
    }

    #[test]
    fn anchors_and_exact() {
        assert!(m("^abc", "abcdef"));
        assert!(!m("^abc", "xabc"));
        assert!(m("def$", "abcdef"));
        assert!(!m("abc$", "abcdef"));
        assert!(m("wfn:a.txt", "A.TXT"));
        assert!(!m("wfn:a.txt", "aa.txt"));
        assert!(m("startwith:ab", "abc"));
        assert!(m("endwith:bc", "abc"));
    }

    #[test]
    fn words_punctuation_whitespace() {
        assert!(m("ww:report", "Q3 report.docx"));
        assert!(!m("ww:report", "reports.docx"));
        assert!(m("ww:report", "report"));
        assert!(m("prefix:rep", "the report"));
        assert!(!m("prefix:port", "the report"));
        assert!(m("suffix:port", "the report x"));
        assert!(m("nopunc:spiderman", "Spider-Man.mkv"));
        assert!(!m("spiderman", "Spider-Man.mkv"));
        assert!(m("nows:helloworld", "hello world.txt"));
    }
}

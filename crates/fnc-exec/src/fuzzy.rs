//! Approximate substring matching for `fuzzy:` (Fennec extension, SPEC ch.2 §3).
//!
//! A name matches if some substring of it is within Levenshtein distance 1 of the
//! pattern (one inserted, deleted or substituted character). Patterns shorter than
//! [`MIN_FUZZY_CHARS`] characters match exactly (a 1–2 character pattern within one
//! edit would match almost everything). Implemented as the bit-parallel Wu–Manber
//! algorithm over characters (patterns up to 64 characters; longer ones match
//! exactly), allocation-free per subject.

use memchr::memmem;

/// Shortest pattern (characters) that is matched approximately.
pub const MIN_FUZZY_CHARS: usize = 3;
/// Longest pattern (characters) that is matched approximately.
pub const MAX_FUZZY_CHARS: usize = 64;

/// Compiled fuzzy pattern.
#[derive(Debug)]
pub struct FuzzyPattern {
    chars: Vec<char>,
    /// Bit `i` set if `chars[i]` is the ASCII character.
    ascii: [u64; 128],
    exact: memmem::Finder<'static>,
}

impl FuzzyPattern {
    /// Compiles `folded` (already folded like the subject).
    pub fn new(folded: &str) -> Self {
        let chars: Vec<char> = folded.chars().collect();
        let mut ascii = [0u64; 128];
        for (i, &c) in chars.iter().enumerate().take(MAX_FUZZY_CHARS) {
            if let Some(slot) = ascii.get_mut(c as usize) {
                *slot |= 1 << i;
            }
        }
        Self { exact: memmem::Finder::new(folded.as_bytes()).into_owned(), chars, ascii }
    }

    /// Pattern text length in bytes (for highlighting fallbacks).
    pub fn needle(&self) -> &[u8] {
        self.exact.needle()
    }

    #[inline]
    fn mask(&self, c: char) -> u64 {
        if let Some(&m) = self.ascii.get(c as usize) {
            return m;
        }
        self.chars.iter().enumerate().filter(|&(_, &p)| p == c).fold(0, |m, (i, _)| m | (1 << i))
    }

    /// `true` if `s` (folded subject, UTF-8) contains the pattern within one edit.
    pub fn is_match(&self, s: &[u8]) -> bool {
        if self.exact.find(s).is_some() {
            return true;
        }
        let m = self.chars.len();
        if !(MIN_FUZZY_CHARS..=MAX_FUZZY_CHARS).contains(&m) {
            return false;
        }
        let Ok(text) = std::str::from_utf8(s) else { return false };
        let goal = 1u64 << (m - 1);
        // r0 bit j: pattern[..=j] matches a suffix of the text read so far exactly;
        // r1: within one edit (bit 0 is always reachable by deleting pattern[0]).
        let mut r0: u64 = 0;
        let mut r1: u64 = 1;
        for c in text.chars() {
            let mk = self.mask(c);
            let n0 = ((r0 << 1) | 1) & mk;
            let n1 = (((r1 << 1) | 1) & mk) // match
                | r0                        // insertion of `c`
                | (r0 << 1) | 1             // substitution by `c`
                | (n0 << 1); // deletion of a pattern character
            if n1 & goal != 0 {
                return true;
            }
            r0 = n0;
            r1 = n1;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Reference: minimal edit distance between `p` and any substring of `t`.
    fn naive(p: &[char], t: &[char]) -> usize {
        let mut prev: Vec<usize> = vec![0; t.len() + 1];
        for (i, &pc) in p.iter().enumerate() {
            let mut cur = vec![i + 1; t.len() + 1];
            for (j, &tc) in t.iter().enumerate() {
                let sub = prev[j] + usize::from(pc != tc);
                cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
            }
            prev = cur;
        }
        prev.into_iter().min().unwrap_or(0)
    }

    fn fm(p: &str, t: &str) -> bool {
        FuzzyPattern::new(p).is_match(t.as_bytes())
    }

    #[test]
    fn one_edit() {
        assert!(fm("report", "annual report.docx"));
        // A transposition is two edits.
        assert!(!fm("reprot", "report"));
        assert!(!fm("repor", "xrexxrx"));
        assert!(fm("rport", "report"));
        assert!(fm("reeport", "report"));
        assert!(fm("rexort", "report"));
        assert!(!fm("rxxort", "report"));
        assert!(fm("cafe", "café"));
        assert!(fm("caf", "cxf"));
        assert!(!fm("ab", "ax"));
        assert!(fm("ab", "xaby"));
        assert!(fm("季度报", "季度报告"));
        assert!(fm("季x报", "季度报告"));
        assert!(!fm("季xx告", "季度报告"));
    }

    proptest! {
        #[test]
        fn agrees_with_naive(p in "[abc]{3,6}", t in "[abcd]{0,12}") {
            let pc: Vec<char> = p.chars().collect();
            let tc: Vec<char> = t.chars().collect();
            prop_assert_eq!(fm(&p, &t), naive(&pc, &tc) <= 1, "{} in {}", p, t);
        }
    }
}

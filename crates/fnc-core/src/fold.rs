//! Fold keys: the normalised form used for case/diacritic-insensitive matching.
//!
//! Default fold (Everything 1.5 "match diacritics" off, "match case" off):
//!
//! 1. NFKD compatibility decomposition (`ｆｕｌｌ` → `full`, `ﬁ` → `fi`, `é` → `e` + U+0301);
//! 2. combining marks dropped (diacritic insensitivity);
//! 3. lowercase mapping plus the full-case-folding specials (`ß`/`ẞ` → `ss`, `ς` → `σ`);
//! 4. letters without a canonical decomposition mapped to their base (`æ` → `ae`, ...).
//!
//! Fold keys are UTF-8 (see ADR-002). ASCII names take a branch-free fast path.

use unicode_normalization::char::{decompose_compatible, is_combining_mark};

/// Which normalisations to apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FoldOptions {
    /// Fold letter case (`false` with the `case:` modifier).
    pub fold_case: bool,
    /// Strip diacritics / decompose (`false` with the `diacritics:` modifier).
    pub strip_diacritics: bool,
}

impl FoldOptions {
    /// Default matching: case- and diacritic-insensitive.
    pub const DEFAULT: FoldOptions = FoldOptions { fold_case: true, strip_diacritics: true };
    /// Exact: no normalisation at all.
    pub const EXACT: FoldOptions = FoldOptions { fold_case: false, strip_diacritics: false };
}

impl Default for FoldOptions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Letters that fold to a multi-letter or base form without having a Unicode
/// decomposition. Applied only when stripping diacritics.
///
/// SPEC-GAP: SPEC §6.2 only pins `ẞ≈ss` and `æ≈ae`; `œ ø đ ł ħ ŧ ı ð þ` follow common
/// "remove accents" practice and need confirmation against Everything 1.5 behaviour.
fn base_letter(c: char) -> Option<&'static str> {
    Some(match c {
        'æ' => "ae",
        'œ' => "oe",
        'ø' => "o",
        'đ' => "d",
        'ð' => "d",
        'ł' => "l",
        'ħ' => "h",
        'ŧ' => "t",
        'ı' => "i",
        'þ' => "th",
        _ => return None,
    })
}

/// Full case folding entries not covered by `char::to_lowercase`.
fn case_special(c: char) -> Option<&'static str> {
    Some(match c {
        'ß' | 'ẞ' => "ss",
        'ς' => "σ",
        _ => return None,
    })
}

/// Folds one source char, calling `emit` for each output char.
#[inline]
fn fold_char(c: char, opts: FoldOptions, emit: &mut impl FnMut(char)) {
    let mut after_decomp = |d: char| {
        if opts.strip_diacritics && is_combining_mark(d) {
            return;
        }
        let mut finish = |l: char| {
            if opts.strip_diacritics && is_combining_mark(l) {
                return;
            }
            if opts.strip_diacritics
                && let Some(rep) = base_letter(l)
            {
                rep.chars().for_each(&mut *emit);
                return;
            }
            emit(l);
        };
        if opts.fold_case {
            if let Some(rep) = case_special(d) {
                rep.chars().for_each(&mut finish);
                return;
            }
            if d.is_ascii() {
                finish(d.to_ascii_lowercase());
            } else {
                d.to_lowercase().for_each(&mut finish);
            }
        } else {
            finish(d);
        }
    };
    if opts.strip_diacritics {
        decompose_compatible(c, &mut after_decomp);
    } else {
        after_decomp(c);
    }
}

/// Appends the fold key of a UTF-16 name to `out` (default options).
pub fn fold_utf16_into(units: &[u16], out: &mut Vec<u8>) {
    fold_utf16_with(units, FoldOptions::DEFAULT, out);
}

/// Appends the fold key of a UTF-16 name to `out`.
pub fn fold_utf16_with(units: &[u16], opts: FoldOptions, out: &mut Vec<u8>) {
    if units.iter().all(|&u| u < 0x80) {
        out.reserve(units.len());
        if opts.fold_case {
            out.extend(units.iter().map(|&u| (u as u8).to_ascii_lowercase()));
        } else {
            out.extend(units.iter().map(|&u| u as u8));
        }
        return;
    }
    let mut buf = [0u8; 4];
    let mut emit = |c: char| out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    for c in char::decode_utf16(units.iter().copied()) {
        fold_char(c.unwrap_or(char::REPLACEMENT_CHARACTER), opts, &mut emit);
    }
}

/// Appends the fold key of `s` to `out`.
pub fn fold_str_into(s: &str, opts: FoldOptions, out: &mut String) {
    if s.is_ascii() {
        if opts.fold_case {
            out.extend(s.bytes().map(|b| b.to_ascii_lowercase() as char));
        } else {
            out.push_str(s);
        }
        return;
    }
    let mut emit = |c: char| out.push(c);
    for c in s.chars() {
        fold_char(c, opts, &mut emit);
    }
}

/// Returns the fold key of `s` with default options.
pub fn fold_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    fold_str_into(s, FoldOptions::DEFAULT, &mut out);
    out
}

/// Folds `s` and records, for every output byte, the byte offset of the source char
/// it came from (`map.len() == out.len() + 1`, last element = `s.len()`).
///
/// Used to map match ranges found in fold space back to the displayed name for
/// highlighting; only called for visible rows.
pub fn fold_str_mapped(s: &str, opts: FoldOptions, out: &mut String, map: &mut Vec<u32>) {
    out.clear();
    map.clear();
    for (pos, c) in s.char_indices() {
        let before = out.len();
        let mut emit = |c: char| out.push(c);
        fold_char(c, opts, &mut emit);
        for _ in before..out.len() {
            map.push(pos as u32);
        }
    }
    map.push(s.len() as u32);
}

/// Returns `true` if `c` is whitespace for query purposes (ASCII space, tab,
/// ideographic space U+3000 and other Unicode spaces).
pub fn is_query_space(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\u{3000}' || (c != '\n' && c != '\r' && c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> String {
        fold_str(s)
    }

    fn f16(s: &str) -> String {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = Vec::new();
        fold_utf16_into(&units, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn ascii_lowercases() {
        assert_eq!(f("IMG_1234.JPG"), "img_1234.jpg");
        assert_eq!(f16("IMG_1234.JPG"), "img_1234.jpg");
    }

    #[test]
    fn sharp_s_and_ligatures() {
        assert_eq!(f("Straße"), "strasse");
        assert_eq!(f("STRAẞE"), "strasse");
        assert_eq!(f("Æsir"), "aesir");
        assert_eq!(f("æble"), "aeble");
        assert_eq!(f("Œuvre"), "oeuvre");
        assert_eq!(f("ﬁle"), "file");
    }

    #[test]
    fn diacritics_are_removed() {
        assert_eq!(f("Café"), "cafe");
        assert_eq!(f("Cafe\u{301}"), "cafe");
        assert_eq!(f("naïve résumé"), "naive resume");
        assert_eq!(f("Ångström"), "angstrom");
        assert_eq!(f("Łódź"), "lodz");
        assert_eq!(f("İstanbul"), "istanbul");
    }

    #[test]
    fn width_and_compat() {
        assert_eq!(f("ＡＢＣ１２３"), "abc123");
        assert_eq!(f("①"), "1");
    }

    #[test]
    fn greek_sigma() {
        assert_eq!(f("ΟΔΟΣ"), f("οδος"));
    }

    #[test]
    fn cjk_unchanged() {
        assert_eq!(f("季度预算表.xlsx"), "季度预算表.xlsx");
        assert_eq!(f16("報告書"), "報告書");
    }

    #[test]
    fn utf16_and_str_agree() {
        for s in ["Straße.txt", "Ünïcödé", "ＦＵＬＬ", "日本語ファイル", "e\u{301}"] {
            assert_eq!(f16(s), f(s), "{s}");
        }
    }

    #[test]
    fn options_case_only_and_diacritics_only() {
        let mut out = String::new();
        fold_str_into("Café", FoldOptions { fold_case: true, strip_diacritics: false }, &mut out);
        assert_eq!(out, "café");
        out.clear();
        fold_str_into("Café", FoldOptions { fold_case: false, strip_diacritics: true }, &mut out);
        assert_eq!(out, "Cafe");
        out.clear();
        fold_str_into("Café", FoldOptions::EXACT, &mut out);
        assert_eq!(out, "Café");
    }

    #[test]
    fn mapped_fold() {
        let mut out = String::new();
        let mut map = Vec::new();
        fold_str_mapped("Aßé", FoldOptions::DEFAULT, &mut out, &mut map);
        assert_eq!(out, "asse");
        // a<-0, s<-1, s<-1, e<-3 (ß is 2 bytes), end = 5
        assert_eq!(map, vec![0, 1, 1, 3, 5]);
    }

    #[test]
    fn query_space() {
        assert!(is_query_space(' '));
        assert!(is_query_space('\u{3000}'));
        assert!(!is_query_space('a'));
    }

    proptest::proptest! {
        #[test]
        fn fold_is_idempotent(s in "\\PC{0,24}") {
            let once = f(&s);
            proptest::prop_assert_eq!(f(&once), once.clone());
        }

        #[test]
        fn utf16_matches_str(s in "\\PC{0,24}") {
            proptest::prop_assert_eq!(f16(&s), f(&s));
        }
    }
}

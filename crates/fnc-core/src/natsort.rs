//! Natural ("logical") ordering of names.
//!
//! Runs of ASCII digits compare by numeric value (`file2` < `file10`); everything
//! else compares by code point of the fold key, so ordering is case- and
//! diacritic-insensitive. Fold keys are UTF-8, whose byte order equals code point
//! order, so the comparator works on bytes and never allocates.
//!
//! Ties (e.g. `a01` vs `a1`, or names differing only in case) are broken by the
//! number of leading zeros and then by [`cmp_utf16`] on the original names, giving a
//! total order.
//!
//! SPEC-GAP: Everything delegates to `CompareStringEx(SORT_DIGITSASNUMBERS)` whose
//! collation is locale dependent (e.g. pinyin order for CJK under zh-CN); Fennec uses
//! code point order for non-digit text. Listed in SPEC ch.2 §9.

use std::cmp::Ordering;

#[inline]
fn digit_run(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    i
}

/// Compares two fold keys naturally. Returns `Equal` only if the keys are equal
/// numerically *and* textually (leading-zero differences are reported as a tie-break
/// via the return value of [`natural_cmp_full`]).
#[inline]
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    natural_cmp_full(a, b).0
}

/// Like [`natural_cmp`] but also returns the leading-zero tie-break (`Less` if `a`
/// had fewer leading zeros in the first differing digit run).
pub fn natural_cmp_full(a: &[u8], b: &[u8]) -> (Ordering, Ordering) {
    let (mut i, mut j) = (0usize, 0usize);
    let mut zeros_tie = Ordering::Equal;
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return (Ordering::Equal, zeros_tie),
            (None, Some(_)) => return (Ordering::Less, zeros_tie),
            (Some(_), None) => return (Ordering::Greater, zeros_tie),
            (Some(&x), Some(&y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let ea = digit_run(a, i);
                    let eb = digit_run(b, j);
                    // Skip leading zeros, but keep at least one digit.
                    let mut sa = i;
                    while sa + 1 < ea && a[sa] == b'0' {
                        sa += 1;
                    }
                    let mut sb = j;
                    while sb + 1 < eb && b[sb] == b'0' {
                        sb += 1;
                    }
                    let (la, lb) = (ea - sa, eb - sb);
                    if la != lb {
                        return (la.cmp(&lb), zeros_tie);
                    }
                    match a[sa..ea].cmp(&b[sb..eb]) {
                        Ordering::Equal => {}
                        o => return (o, zeros_tie),
                    }
                    if zeros_tie == Ordering::Equal {
                        zeros_tie = (sa - i).cmp(&(sb - j));
                    }
                    i = ea;
                    j = eb;
                } else {
                    if x != y {
                        return (x.cmp(&y), zeros_tie);
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
    }
}

/// Ordinal comparison of UTF-16 names by code point (surrogate pairs ordered
/// correctly, unlike a plain `u16` comparison).
pub fn cmp_utf16(a: &[u16], b: &[u16]) -> Ordering {
    #[inline]
    fn key(u: u16) -> u32 {
        // Map surrogates above the BMP private range so pairs sort after U+FFFF.
        if (0xD800..0xE000).contains(&u) {
            u32::from(u) + 0x2000
        } else if u >= 0xE000 {
            u32::from(u) - 0x800
        } else {
            u32::from(u)
        }
    }
    for (&x, &y) in a.iter().zip(b) {
        if x != y {
            return key(x).cmp(&key(y));
        }
    }
    a.len().cmp(&b.len())
}

/// Total natural order for entries: fold key, then leading zeros, then original name.
#[inline]
pub fn name_cmp(fold_a: &[u8], name_a: &[u16], fold_b: &[u8], name_b: &[u16]) -> Ordering {
    let (o, zeros) = natural_cmp_full(fold_a, fold_b);
    o.then(zeros).then_with(|| cmp_utf16(name_a, name_b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fold::fold_str;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<(String, Vec<u16>, String)> =
            names.iter().map(|n| (fold_str(n), n.encode_utf16().collect(), n.to_string())).collect();
        v.sort_by(|a, b| name_cmp(a.0.as_bytes(), &a.1, b.0.as_bytes(), &b.1));
        v.into_iter().map(|t| t.2).collect()
    }

    #[test]
    fn numbers_sort_numerically() {
        assert_eq!(
            sorted(&["file10.txt", "file2.txt", "file1.txt", "file20.txt", "file3.txt"]),
            vec!["file1.txt", "file2.txt", "file3.txt", "file10.txt", "file20.txt"]
        );
    }

    #[test]
    fn leading_zeros_tie_break() {
        assert_eq!(sorted(&["a01", "a1", "a001", "a2"]), vec!["a1", "a01", "a001", "a2"]);
    }

    #[test]
    fn case_insensitive_with_stable_tiebreak() {
        assert_eq!(sorted(&["b", "B", "a", "A"]), vec!["A", "a", "B", "b"]);
    }

    #[test]
    fn diacritics_sort_with_base_letter() {
        assert_eq!(sorted(&["eclair", "écrire", "edit"]), vec!["eclair", "écrire", "edit"]);
    }

    #[test]
    fn long_digit_runs_do_not_overflow() {
        assert_eq!(
            sorted(&["x99999999999999999999999", "x100000000000000000000000", "x5"]),
            vec!["x5", "x99999999999999999999999", "x100000000000000000000000"]
        );
    }

    #[test]
    fn cjk_with_numbers() {
        assert_eq!(
            sorted(&["第12章.txt", "第3章.txt", "第1章.txt", "第100章.txt"]),
            vec!["第1章.txt", "第3章.txt", "第12章.txt", "第100章.txt"]
        );
        assert_eq!(sorted(&["文件10", "文件2", "abc", "文件1"]), vec!["abc", "文件1", "文件2", "文件10"]);
        // Full-width digits fold to ASCII and therefore also sort numerically.
        assert_eq!(sorted(&["写真１０", "写真９"]), vec!["写真９", "写真１０"]);
    }

    #[test]
    fn prefix_sorts_first() {
        assert_eq!(sorted(&["abc", "ab", "abcd"]), vec!["ab", "abc", "abcd"]);
    }

    #[test]
    fn utf16_codepoint_order() {
        let bmp: Vec<u16> = "\u{FFFD}".encode_utf16().collect();
        let astral: Vec<u16> = "\u{1F600}".encode_utf16().collect();
        assert_eq!(cmp_utf16(&bmp, &astral), Ordering::Less);
    }

    proptest::proptest! {
        #[test]
        fn natural_cmp_is_antisymmetric(a in "[a-c0-9]{0,8}", b in "[a-c0-9]{0,8}") {
            let ab = name_cmp(a.as_bytes(), &a.encode_utf16().collect::<Vec<_>>(), b.as_bytes(), &b.encode_utf16().collect::<Vec<_>>());
            let ba = name_cmp(b.as_bytes(), &b.encode_utf16().collect::<Vec<_>>(), a.as_bytes(), &a.encode_utf16().collect::<Vec<_>>());
            proptest::prop_assert_eq!(ab, ba.reverse());
            if ab == Ordering::Equal { proptest::prop_assert_eq!(&a, &b); }
        }

        #[test]
        fn natural_cmp_is_transitive(mut v in proptest::collection::vec("[a-b0-2]{0,5}", 3)) {
            v.sort_by(|a, b| natural_cmp(a.as_bytes(), b.as_bytes()));
            proptest::prop_assert_ne!(natural_cmp(v[0].as_bytes(), v[2].as_bytes()), Ordering::Greater);
        }
    }
}

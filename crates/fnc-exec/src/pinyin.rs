//! Pinyin matching of Chinese names (`pinyin:` / `pinyinquan:` / `pinyinjian:`).
//!
//! The pattern (ASCII letters, optionally mixed with literal characters) is matched
//! as a substring of the name where every Chinese character may be typed as one of its
//! readings (heteronyms: any reading):
//!
//! * [`PinyinMode::Any`] — per character the full syllable, its initial or any prefix
//!   of it, so `jdbg`, `jidubaogao` and `jidubg` all match 季度报告;
//! * [`PinyinMode::Full`] — full syllables only;
//! * [`PinyinMode::Initials`] — initials only (`zh` `ch` `sh` or the first letter).
//!
//! Every other character (ASCII, punctuation, kana, …) and a Chinese character typed
//! as itself matches literally. `ü` may be typed as `v` or `u`.
//!
//! Matching is a left-to-right NFA simulation over the name's characters with the set
//! of reachable pattern positions in a `u128` (patterns up to 127 bytes), so it never
//! allocates. Names without CJK bytes skip the simulation (a plain substring search
//! already decides them).

use memchr::memmem;
use pinyin::{Pinyin, ToPinyinMulti};

use fnc_query::PinyinMode;

/// Longest pattern (bytes) handled by the pinyin simulation; longer patterns match
/// literally only.
pub const MAX_PINYIN_PATTERN: usize = 127;

/// Compiled pinyin pattern.
#[derive(Debug)]
pub struct PinyinPattern {
    needle: Vec<u8>,
    mode: PinyinMode,
    literal: memmem::Finder<'static>,
}

/// First byte of the UTF-8 encoding of U+3000 (CJK symbols and everything above).
const CJK_LEAD: u8 = 0xE3;

/// Writes the ASCII spelling of `reading` into `out` with `ü` replaced by `sub`;
/// returns the length.
fn spell(reading: &str, sub: u8, out: &mut [u8; 16]) -> usize {
    let mut n = 0;
    for c in reading.chars() {
        let b = match c {
            'ü' => sub,
            c if c.is_ascii() => c as u8,
            _ => continue,
        };
        if n == out.len() {
            break;
        }
        out[n] = b;
        n += 1;
    }
    n
}

/// Length of the initial of a reading (`zh`/`ch`/`sh` = 2, else 1).
fn initial_len(r: &[u8]) -> usize {
    if r.len() >= 2 && matches!(r[0], b'z' | b'c' | b's') && r[1] == b'h' { 2 } else { 1.min(r.len()) }
}

impl PinyinPattern {
    /// Compiles `folded` (already folded like the subject).
    pub fn new(folded: &str, mode: PinyinMode) -> Self {
        let needle = folded.as_bytes().to_vec();
        let literal = memmem::Finder::new(&needle).into_owned();
        Self { needle, mode, literal }
    }

    /// Pattern bytes.
    pub fn needle(&self) -> &[u8] {
        &self.needle
    }

    /// Advances the reachable set over one reading; returns `true` if the pattern ends
    /// inside or at the end of this reading.
    #[inline]
    fn step_reading(&self, r: &[u8], cur: u128, next: &mut u128) -> bool {
        let m = self.needle.len();
        let mut bits = cur;
        while bits != 0 {
            let p = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let rest = &self.needle[p..];
            let common = rest.iter().zip(r).take_while(|(a, b)| a == b).count();
            if common == 0 {
                continue;
            }
            match self.mode {
                PinyinMode::Any | PinyinMode::Off => {
                    if common == rest.len() {
                        return true;
                    }
                    // Any non-empty prefix of the reading may stand for the character.
                    for k in 1..=common {
                        *next |= 1u128 << (p + k);
                    }
                }
                PinyinMode::Full => {
                    if common == r.len() {
                        if p + common == m {
                            return true;
                        }
                        *next |= 1u128 << (p + common);
                    }
                }
                PinyinMode::Initials => {
                    let ini = initial_len(r);
                    for k in [1, ini] {
                        if k <= common {
                            if p + k == m {
                                return true;
                            }
                            *next |= 1u128 << (p + k);
                        }
                    }
                }
            }
        }
        false
    }

    fn readings_step(&self, readings: impl Iterator<Item = Pinyin>, cur: u128, next: &mut u128) -> bool {
        let mut buf = [0u8; 16];
        for r in readings {
            let plain = r.plain();
            let n = spell(plain, b'v', &mut buf);
            if self.step_reading(&buf[..n], cur, next) {
                return true;
            }
            if plain.contains('ü') {
                let n = spell(plain, b'u', &mut buf);
                if self.step_reading(&buf[..n], cur, next) {
                    return true;
                }
            }
        }
        false
    }

    /// `true` if `s` (folded subject, UTF-8) matches literally or by pinyin.
    pub fn is_match(&self, s: &[u8]) -> bool {
        if self.literal.find(s).is_some() {
            return true;
        }
        let m = self.needle.len();
        if m == 0 || m > MAX_PINYIN_PATTERN || !s.iter().any(|&b| b >= CJK_LEAD) {
            return false;
        }
        let Ok(text) = std::str::from_utf8(s) else { return false };
        let done = 1u128 << m;
        let mut cur: u128 = 0;
        for (i, c) in text.char_indices() {
            // A match may start at any character.
            cur |= 1;
            let mut next: u128 = 0;
            let cb = &s[i..i + c.len_utf8()];
            // The character typed as itself.
            let mut bits = cur;
            while bits != 0 {
                let p = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if self.needle[p..].starts_with(cb) {
                    next |= 1u128 << (p + cb.len());
                }
            }
            if u32::from(c) >= 0x3000
                && let Some(multi) = c.to_pinyin_multi()
                && self.readings_step(multi.into_iter(), cur, &mut next)
            {
                return true;
            }
            if next & done != 0 {
                return true;
            }
            cur = next & (done - 1);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(mode: PinyinMode, pat: &str, name: &str) -> bool {
        let folded = fnc_core::fold::fold_str(pat);
        PinyinPattern::new(&folded, mode).is_match(fnc_core::fold::fold_str(name).as_bytes())
    }

    fn any(pat: &str, name: &str) -> bool {
        m(PinyinMode::Any, pat, name)
    }

    #[test]
    fn full_initials_and_mixed() {
        let name = "季度报告.docx";
        for pat in ["jdbg", "jidubaogao", "jidubg", "jdbaogao", "jidu", "dubao", "bg", "baog", "jdbg.docx", "gao.d"] {
            assert!(any(pat, name), "{pat}");
        }
        for pat in ["jdbx", "xjdbg", "idu", "jdbgz", "baogaox"] {
            assert!(!any(pat, name), "{pat}");
        }
        assert!(any("季度bg", name));
        assert!(any("docx", name));
        assert!(any("JDBG", name));
    }

    #[test]
    fn quan_and_jian() {
        let name = "季度报告.docx";
        assert!(m(PinyinMode::Full, "jidubaogao", name));
        assert!(m(PinyinMode::Full, "dubao", name));
        assert!(!m(PinyinMode::Full, "jdbg", name));
        assert!(!m(PinyinMode::Full, "jidubg", name));
        assert!(m(PinyinMode::Initials, "jdbg", name));
        assert!(!m(PinyinMode::Initials, "jidubaogao", name));
        assert!(m(PinyinMode::Initials, "zhg", "中国"));
        assert!(m(PinyinMode::Initials, "zg", "中国"));
        assert!(m(PinyinMode::Full, "zhongguo", "中国人"));
    }

    #[test]
    fn heteronyms_and_umlaut() {
        // 行: xing / hang.
        assert!(any("yinhang", "银行"));
        assert!(any("xingdong", "行动"));
        // 绿: lü.
        assert!(any("lvse", "绿色"));
        assert!(any("luse", "绿色"));
        // 重: zhong / chong.
        assert!(any("chongqing", "重庆"));
        assert!(any("cq", "重庆"));
    }

    #[test]
    fn ascii_names_and_limits() {
        assert!(any("abc", "xxabcxx"));
        assert!(!any("jdbg", "plain ascii name"));
        assert!(any("2026nian", "2026年报告"));
        assert!(any("nianbao", "2026年报告"));
        let long = "a".repeat(200);
        assert!(!any(&long, "季度报告"));
        assert!(any("", "x"));
    }
}

//! Size values (SPEC ch.2 §4.3).
//!
//! `size:10mb` uses *granularity*: it matches `[10 MiB, 11 MiB)`; `size:1.5mb` matches
//! `[1.5 MiB, 1.6 MiB)`; plain byte counts are exact. Units are 1024-based (`kb`/`k`,
//! `mb`/`m`, `gb`/`g`, `tb`/`t`, `pb`/`p`, `b`). Constants: `empty`, `tiny`, `small`,
//! `medium`, `large`, `huge`, `gigantic`.

/// Half-open byte range `[lo, hi)`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SizeSpan {
    /// Inclusive lower bound.
    pub lo: u64,
    /// Exclusive upper bound (`u64::MAX` = unbounded).
    pub hi: u64,
}

const KB: u64 = 1024;
const MB: u64 = KB * 1024;

/// Parses a size constant or `number[unit]`.
pub fn parse_size(text: &str) -> Option<SizeSpan> {
    let t = text.trim().to_ascii_lowercase();
    let span = |lo: u64, hi: u64| Some(SizeSpan { lo, hi });
    match t.as_str() {
        "empty" => return span(0, 1),
        "tiny" => return span(1, 10 * KB + 1),
        "small" => return span(10 * KB + 1, 100 * KB + 1),
        "medium" => return span(100 * KB + 1, MB + 1),
        "large" => return span(MB + 1, 16 * MB + 1),
        "huge" => return span(16 * MB + 1, 128 * MB + 1),
        "gigantic" => return span(128 * MB + 1, u64::MAX),
        _ => {}
    }
    let num_end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(t.len());
    let (num, unit) = t.split_at(num_end);
    let num = num.replace(',', "");
    if num.is_empty() {
        return None;
    }
    let mult: u64 = match unit.trim() {
        "" | "b" | "byte" | "bytes" => 1,
        "k" | "kb" | "kib" => KB,
        "m" | "mb" | "mib" => MB,
        "g" | "gb" | "gib" => MB * KB,
        "t" | "tb" | "tib" => MB * MB,
        "p" | "pb" | "pib" => MB * MB * KB,
        _ => return None,
    };
    let decimals = num.split_once('.').map_or(0, |(_, f)| f.len()) as u32;
    let value: f64 = num.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let lo = (value * mult as f64).round();
    if lo >= u64::MAX as f64 {
        return None;
    }
    let lo = lo as u64;
    if mult == 1 {
        return span(lo, lo.saturating_add(1));
    }
    let step = ((mult as f64) / 10f64.powi(decimals as i32)).max(1.0) as u64;
    span(lo, lo.saturating_add(step))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_and_granularity() {
        assert_eq!(parse_size("1kb"), Some(SizeSpan { lo: 1024, hi: 2048 }));
        assert_eq!(parse_size("10MB"), Some(SizeSpan { lo: 10 * MB, hi: 11 * MB }));
        assert_eq!(parse_size("2m"), Some(SizeSpan { lo: 2 * MB, hi: 3 * MB }));
        assert_eq!(parse_size("1.5mb"), Some(SizeSpan { lo: MB * 3 / 2, hi: MB * 3 / 2 + MB / 10 }));
        assert_eq!(parse_size("100"), Some(SizeSpan { lo: 100, hi: 101 }));
        assert_eq!(parse_size("1,000"), Some(SizeSpan { lo: 1000, hi: 1001 }));
        assert_eq!(parse_size("1gb").map(|s| s.lo), Some(1 << 30));
    }

    #[test]
    fn constants() {
        assert_eq!(parse_size("empty"), Some(SizeSpan { lo: 0, hi: 1 }));
        assert_eq!(parse_size("tiny"), Some(SizeSpan { lo: 1, hi: 10 * KB + 1 }));
        assert_eq!(parse_size("gigantic").map(|s| s.hi), Some(u64::MAX));
    }

    #[test]
    fn invalid() {
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("1xb"), None);
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("mb"), None);
    }
}

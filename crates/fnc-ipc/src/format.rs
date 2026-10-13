//! Value formatting shared by exports, `fes` and the HTTP server, following `es.exe`
//! (voidtools ES 1.1): size formats, date formats, attribute letters, digit grouping.

use fnc_core::FileTime;

use crate::proto::UNKNOWN;

/// `-size-format`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum SizeFormat {
    /// Automatic unit with two/three significant digits (`1.23 MB`, `512  B`).
    Auto,
    /// Bytes with digit grouping (es default).
    #[default]
    Bytes,
    /// Kilobytes, rounded up (`12 KB`).
    Kb,
    /// Megabytes, rounded up.
    Mb,
    /// Gigabytes, rounded up.
    Gb,
}

impl SizeFormat {
    /// From the es number (0 = auto, 1 = bytes, 2 = KB, 3 = MB, 4 = GB).
    pub fn from_es(n: u32) -> Option<Self> {
        Some(match n {
            0 => SizeFormat::Auto,
            1 => SizeFormat::Bytes,
            2 => SizeFormat::Kb,
            3 => SizeFormat::Mb,
            4 => SizeFormat::Gb,
            _ => return None,
        })
    }
}

/// `-date-format`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum DateFormat {
    /// Default: formatted local time in CSV/TSV/console, raw FILETIME in EFU/JSON.
    #[default]
    Default,
    /// ISO-8601 local time `2026-07-02T10:52:00`.
    Iso,
    /// FILETIME decimal.
    Filetime,
    /// ISO-8601 UTC `2026-07-02T08:52:00Z`.
    IsoUtc,
    /// "System format" (Fennec: `2026-07-02 10:52:00`, see [`format_filetime`]).
    System,
    /// ISO-8601 local with 100 ns fraction.
    IsoFull,
    /// ISO-8601 UTC with 100 ns fraction.
    IsoUtcFull,
}

impl DateFormat {
    /// From the es number (0..=6).
    pub fn from_es(n: u32) -> Option<Self> {
        Some(match n {
            0 => DateFormat::Default,
            1 => DateFormat::Iso,
            2 => DateFormat::Filetime,
            3 => DateFormat::IsoUtc,
            4 => DateFormat::System,
            5 => DateFormat::IsoFull,
            6 => DateFormat::IsoUtcFull,
            _ => return None,
        })
    }
}

/// `1234567` → `1,234,567` (when `grouping`).
pub fn format_number(n: u64, grouping: bool) -> String {
    let digits = n.to_string();
    if !grouping || digits.len() <= 3 {
        return digits;
    }
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let first = digits.len() % 3;
    for (i, c) in digits.chars().enumerate() {
        if i != 0 && (i + 3 - first).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Formats a size like es (`UNKNOWN` → empty).
pub fn format_size(size: u64, fmt: SizeFormat, grouping: bool) -> String {
    if size == UNKNOWN {
        return String::new();
    }
    match fmt {
        SizeFormat::Bytes => format_number(size, grouping),
        SizeFormat::Kb => format!("{} KB", format_number(size.div_ceil(1024), grouping)),
        SizeFormat::Mb => format!("{} MB", format_number(size.div_ceil(1 << 20), grouping)),
        SizeFormat::Gb => format!("{} GB", format_number(size.div_ceil(1 << 30), grouping)),
        SizeFormat::Auto => format_size_auto(size),
    }
}

/// es "auto" size: `999  B`, `1.23 KB`, `12.3 MB`, `123 GB`.
pub fn format_size_auto(size: u64) -> String {
    if size < 1000 {
        return format!("{size}  B");
    }
    const UNITS: [(&str, u32); 5] = [(" KB", 10), (" MB", 20), (" GB", 30), (" TB", 40), (" PB", 50)];
    let s = size as u128;
    let (suffix, shift) = UNITS.iter().copied().find(|&(_, sh)| s >> sh < 1000).unwrap_or(UNITS[4]);
    let v = (s * 100) >> shift; // hundredths of the unit
    let v = v as u64;
    if v == 0 {
        format!("0{suffix}")
    } else if v < 10 {
        format!("0.0{v}{suffix}")
    } else if v < 100 {
        format!("0.{v}{suffix}")
    } else if v < 1000 {
        format!("{}.{:02}{suffix}", v / 100, v % 100)
    } else if v < 10000 {
        format!("{}.{}{suffix}", v / 100, (v / 10) % 10)
    } else if v < 100000 {
        format!("{}{suffix}", v / 100)
    } else {
        format!("{}{suffix}", format_number(v / 100, true))
    }
}

fn to_filetime(ft: u64) -> Option<FileTime> {
    (ft != UNKNOWN && ft <= i64::MAX as u64).then_some(FileTime(ft as i64))
}

/// Formats a FILETIME (`UNKNOWN` → empty). `Default` and `System` use
/// `YYYY-MM-DD HH:MM:SS` local time.
///
/// SPEC-GAP: es formats `Default`/`System` with the Windows locale (`GetDateFormat`); Fennec
/// uses one fixed, sortable layout on every platform.
pub fn format_filetime(ft: u64, fmt: DateFormat) -> String {
    let Some(t) = to_filetime(ft) else { return String::new() };
    let frac = (ft % 10_000_000) as u32;
    match fmt {
        DateFormat::Filetime => ft.to_string(),
        DateFormat::Default | DateFormat::System => {
            t.to_local().map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
        }
        DateFormat::Iso => t.to_local().map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string()).unwrap_or_default(),
        DateFormat::IsoFull => {
            t.to_local().map(|d| format!("{}.{frac:07}", d.format("%Y-%m-%dT%H:%M:%S"))).unwrap_or_default()
        }
        DateFormat::IsoUtc => t.to_utc().map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default(),
        DateFormat::IsoUtcFull => {
            t.to_utc().map(|d| format!("{}.{frac:07}Z", d.format("%Y-%m-%dT%H:%M:%S"))).unwrap_or_default()
        }
    }
}

/// Attribute letters in es order (`RHSDAVXNTLCOIEUPM`).
pub fn format_attributes(a: u32) -> String {
    const LETTERS: [(u32, char); 17] = [
        (0x1, 'R'),
        (0x2, 'H'),
        (0x4, 'S'),
        (0x10, 'D'),
        (0x20, 'A'),
        (0x8000, 'V'),
        (0x20000, 'X'),
        (0x80, 'N'),
        (0x100, 'T'),
        (0x400, 'L'),
        (0x800, 'C'),
        (0x1000, 'O'),
        (0x2000, 'I'),
        (0x4000, 'E'),
        (0x0010_0000, 'U'),
        (0x0008_0000, 'P'),
        (0x0040_0000, 'M'),
    ];
    LETTERS.iter().filter(|(bit, _)| a & bit != 0).map(|&(_, c)| c).collect()
}

/// Wraps highlighted ranges of `text` in `*` markers (`*` in the text becomes `**`), the
/// Everything SDK highlight syntax. Ranges must be sorted, non-overlapping byte ranges on
/// char boundaries.
pub fn highlight_markup(text: &str, ranges: &[std::ops::Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len() + 2 * ranges.len() + 2);
    let mut pos = 0;
    let push = |s: &str, out: &mut String| {
        for c in s.chars() {
            if c == '*' {
                out.push('*');
            }
            out.push(c);
        }
    };
    for r in ranges {
        let (a, b) = (r.start.min(text.len()), r.end.min(text.len()));
        if a < pos || a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        push(&text[pos..a], &mut out);
        out.push('*');
        push(&text[a..b], &mut out);
        out.push('*');
        pos = b;
    }
    push(&text[pos..], &mut out);
    out
}

/// Splits `*`-markup into `(segment, highlighted)` parts (inverse of [`highlight_markup`]).
pub fn parse_highlight(markup: &str) -> Vec<(String, bool)> {
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut cur = String::new();
    let mut on = false;
    let mut chars = markup.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                cur.push('*');
                continue;
            }
            if !cur.is_empty() {
                parts.push((std::mem::take(&mut cur), on));
            }
            on = !on;
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        parts.push((cur, on));
    }
    parts
}

/// Removes `*` markup.
pub fn strip_highlight(markup: &str) -> String {
    parse_highlight(markup).into_iter().map(|(s, _)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_sizes() {
        assert_eq!(format_number(0, true), "0");
        assert_eq!(format_number(999, true), "999");
        assert_eq!(format_number(1000, true), "1,000");
        assert_eq!(format_number(1234567, true), "1,234,567");
        assert_eq!(format_number(123456, true), "123,456");
        assert_eq!(format_number(1234567, false), "1234567");
        assert_eq!(format_size(1234567, SizeFormat::Bytes, true), "1,234,567");
        assert_eq!(format_size(1, SizeFormat::Kb, true), "1 KB");
        assert_eq!(format_size(2048, SizeFormat::Kb, true), "2 KB");
        assert_eq!(format_size(0, SizeFormat::Kb, true), "0 KB");
        assert_eq!(format_size(3 << 20, SizeFormat::Mb, true), "3 MB");
        assert_eq!(format_size(UNKNOWN, SizeFormat::Bytes, true), "");
        assert_eq!(format_size(512, SizeFormat::Auto, true), "512  B");
        assert_eq!(format_size(1024, SizeFormat::Auto, true), "1.00 KB");
        assert_eq!(format_size(1536, SizeFormat::Auto, true), "1.50 KB");
        assert_eq!(format_size(1000, SizeFormat::Auto, true), "0.97 KB");
        assert_eq!(format_size(20 * 1024, SizeFormat::Auto, true), "20.0 KB");
        assert_eq!(format_size(500 * 1024, SizeFormat::Auto, true), "500 KB");
        assert_eq!(format_size(5 << 30, SizeFormat::Auto, true), "5.00 GB");
    }

    #[test]
    fn dates_and_attributes() {
        // 2024-01-02T03:04:05Z
        let ft = (1_704_164_645i64 * 10_000_000 + fnc_core::time::UNIX_EPOCH_TICKS) as u64 + 1234567;
        assert_eq!(format_filetime(ft, DateFormat::Filetime), ft.to_string());
        assert_eq!(format_filetime(ft, DateFormat::IsoUtc), "2024-01-02T03:04:05Z");
        assert_eq!(format_filetime(ft, DateFormat::IsoUtcFull), "2024-01-02T03:04:05.1234567Z");
        assert_eq!(format_filetime(UNKNOWN, DateFormat::Iso), "");
        assert_eq!(format_filetime(ft, DateFormat::Default).len(), 19);
        assert_eq!(format_attributes(0x21), "RA");
        assert_eq!(format_attributes(0x16), "HSD");
        assert_eq!(format_attributes(0), "");
    }

    #[test]
    fn highlight() {
        assert_eq!(highlight_markup("abcabc", &[0..1, 3..4]), "*a*bc*a*bc");
        let star = 1..2;
        assert_eq!(highlight_markup("a*b", &[star]), "a****b");
        assert_eq!(highlight_markup("a*b", &[]), "a**b");
        assert_eq!(strip_highlight("*a*bc**d"), "abc*d");
        assert_eq!(parse_highlight("x*ab*y"), vec![("x".into(), false), ("ab".into(), true), ("y".into(), false)]);
        let cafe = 3..5;
        assert_eq!(strip_highlight(&highlight_markup("Café*", &[cafe])), "Café*");
    }
}

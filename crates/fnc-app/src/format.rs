//! Cell formatting for the result list and status bar.

use fnc_core::FileTime;

/// Groups digits with `,` (`1234567` → `1,234,567`).
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Human-readable size with binary units.
// SPEC-GAP: Everything's size column format (and locale digit grouping) is
// configurable; Fennec uses automatic B/KB/MB/GB/TB with one decimal.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 { format!("{v:.0} {}", UNITS[u]) } else { format!("{v:.1} {}", UNITS[u]) }
}

/// Local date/time as `YYYY-MM-DD HH:MM`.
// SPEC-GAP: should follow the OS short date/time format.
pub fn date(t: FileTime) -> String {
    t.to_local().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KB");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(300 * 1024 * 1024), "300 MB");
        assert_eq!(size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn dates() {
        assert_eq!(date(FileTime::UNKNOWN), "");
        let t =
            FileTime::from_local(chrono::NaiveDate::from_ymd_opt(2026, 7, 2).unwrap().and_hms_opt(10, 52, 0).unwrap());
        assert_eq!(date(t), "2026-07-02 10:52");
    }
}

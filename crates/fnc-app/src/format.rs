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

/// Size in the configured format.
pub fn size_with(bytes: u64, f: fnc_core::settings::SizeFormat) -> String {
    use fnc_core::settings::SizeFormat as F;
    match f {
        F::Auto => size(bytes),
        F::Bytes => group_digits(bytes),
        F::Kb => format!("{} KB", group_digits(bytes.div_ceil(1024))),
        F::Mb => format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)),
    }
}

/// Date in the configured format; `now` anchors relative dates.
pub fn date_with(t: FileTime, f: fnc_core::settings::DateFormat, now: FileTime) -> String {
    use fnc_core::settings::DateFormat as F;
    let Some(d) = t.to_local() else { return String::new() };
    match f {
        F::Iso => d.format("%Y-%m-%d %H:%M").to_string(),
        F::Us => d.format("%m/%d/%Y %H:%M").to_string(),
        F::Eu => d.format("%d/%m/%Y %H:%M").to_string(),
        F::Relative => {
            // FILETIME ticks are 100 ns.
            let secs = (now.0 - t.0) / 10_000_000;
            match secs {
                s if s < 0 => d.format("%Y-%m-%d %H:%M").to_string(),
                0..60 => crate::i18n::t("time-just-now"),
                60..3600 => crate::i18n::t_args("time-minutes-ago", &[("n", (secs / 60).into())]),
                3600..86_400 => crate::i18n::t_args("time-hours-ago", &[("n", (secs / 3600).into())]),
                86_400..604_800 => crate::i18n::t_args("time-days-ago", &[("n", (secs / 86_400).into())]),
                _ => d.format("%Y-%m-%d %H:%M").to_string(),
            }
        }
    }
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
        let date = |t| date_with(t, fnc_core::settings::DateFormat::Iso, FileTime::now());
        assert_eq!(date(FileTime::UNKNOWN), "");
        let t =
            FileTime::from_local(chrono::NaiveDate::from_ymd_opt(2026, 7, 2).unwrap().and_hms_opt(10, 52, 0).unwrap());
        assert_eq!(date(t), "2026-07-02 10:52");
    }

    #[test]
    fn configured_formats() {
        use fnc_core::settings::{DateFormat, SizeFormat};
        assert_eq!(size_with(1_234_567, SizeFormat::Bytes), "1,234,567");
        assert_eq!(size_with(1025, SizeFormat::Kb), "2 KB");
        assert_eq!(size_with(3 * 1024 * 1024, SizeFormat::Mb), "3.0 MB");
        crate::i18n::init("en");
        let now = FileTime::now();
        assert_eq!(date_with(FileTime(now.0 - 10 * 10_000_000), DateFormat::Relative, now), "just now");
        assert_eq!(date_with(FileTime(now.0 - 3 * 3600 * 10_000_000), DateFormat::Relative, now), "3 hours ago");
        assert_eq!(date_with(FileTime::UNKNOWN, DateFormat::Iso, now), "");
        let iso = date_with(now, DateFormat::Iso, now);
        let us = date_with(now, DateFormat::Us, now);
        assert_eq!(&iso[..4], &us[6..10], "same year in both");
    }
}

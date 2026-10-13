//! Duration values for `length:` / `duration:` (SPEC ch.2 §4.5).
//!
//! Values are half-open millisecond spans with the granularity of what was written,
//! like sizes: `length:1min` matches `[60 s, 120 s)`, `length:1.5hours` matches
//! `[90 min, 96 min)`, `length:3:25` (mm:ss) matches `[205 s, 206 s)`.
//!
//! Accepted forms:
//! * clock forms `hh:mm:ss`, `mm:ss` (optionally with a decimal fraction of a second,
//!   `1:02.5`);
//! * `number[unit]` with units `ms`, `s`/`sec`/`secs`/`second(s)`, `m`/`min`/`mins`/
//!   `minute(s)`, `h`/`hr`/`hrs`/`hour(s)`, `d`/`day(s)`; a decimal number refines the
//!   granularity (`1.5h` steps by 0.1 h);
//! * a bare number is seconds.

/// Half-open span `[lo, hi)` in milliseconds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DurationSpan {
    /// Inclusive lower bound (ms).
    pub lo: i64,
    /// Exclusive upper bound (ms).
    pub hi: i64,
}

const SEC: i64 = 1000;
const MIN: i64 = 60 * SEC;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

fn digits(s: &str) -> Option<i64> {
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `value * mult` with granularity `mult / 10^decimals` (at least 1 ms).
fn scaled(num: &str, mult: i64) -> Option<DurationSpan> {
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let int_v = if int.is_empty() { 0 } else { digits(int)? };
    if !frac.is_empty() && (frac.len() > 6 || !frac.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let scale = 10i64.pow(frac.len() as u32);
    let frac_v = if frac.is_empty() { 0 } else { digits(frac)? };
    let lo = int_v.checked_mul(mult)?.checked_add(frac_v.checked_mul(mult)? / scale)?;
    let step = (mult / scale).max(1);
    Some(DurationSpan { lo, hi: lo.checked_add(step)? })
}

/// `[[h:]m:]s[.fff]` clock form.
fn clock(t: &str) -> Option<DurationSpan> {
    let parts: Vec<&str> = t.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let (h, m, s) = match parts.as_slice() {
        [m, s] => (0, digits(m)?, *s),
        [h, m, s] => (digits(h)?, digits(m)?, *s),
        _ => return None,
    };
    if parts.len() == 3 && m >= 60 {
        return None;
    }
    let sec = scaled(s, SEC)?;
    if sec.lo >= MIN {
        return None;
    }
    let base = h.checked_mul(HOUR)?.checked_add(m.checked_mul(MIN)?)?;
    Some(DurationSpan { lo: base.checked_add(sec.lo)?, hi: base.checked_add(sec.hi)? })
}

/// Parses a duration value. Returns `None` if unrecognised.
pub fn parse_duration(text: &str) -> Option<DurationSpan> {
    let t = text.trim().to_ascii_lowercase();
    if t.is_empty() || !t.is_ascii() {
        return None;
    }
    if t.contains(':') {
        return clock(&t);
    }
    let num_end = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(num_end);
    let mult = match unit.trim() {
        "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => 1,
        // SPEC-GAP: Everything's unit for a bare number is undocumented; seconds here.
        "" | "s" | "sec" | "secs" | "second" | "seconds" => SEC,
        "m" | "min" | "mins" | "minute" | "minutes" => MIN,
        "h" | "hr" | "hrs" | "hour" | "hours" => HOUR,
        "d" | "day" | "days" => DAY,
        _ => return None,
    };
    scaled(num, mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(t: &str) -> (i64, i64) {
        let s = parse_duration(t).unwrap_or_else(|| panic!("{t} did not parse"));
        (s.lo, s.hi)
    }

    #[test]
    fn clock_forms() {
        assert_eq!(d("1:02:03"), (HOUR + 2 * MIN + 3 * SEC, HOUR + 2 * MIN + 4 * SEC));
        assert_eq!(d("3:25"), (205 * SEC, 206 * SEC));
        assert_eq!(d("00:00:59"), (59 * SEC, 60 * SEC));
        assert_eq!(d("90:00"), (90 * MIN, 90 * MIN + SEC));
        assert_eq!(d("1:02.5"), (62_500, 62_600));
        assert_eq!(parse_duration("1:60"), None);
        assert_eq!(parse_duration("1:61:00"), None);
        assert_eq!(parse_duration("1:2:3:4"), None);
        assert_eq!(parse_duration(":"), None);
    }

    #[test]
    fn units_and_granularity() {
        assert_eq!(d("90s"), (90 * SEC, 91 * SEC));
        assert_eq!(d("90"), d("90s"));
        assert_eq!(d("5min"), (5 * MIN, 6 * MIN));
        assert_eq!(d("1min"), (60 * SEC, 120 * SEC));
        assert_eq!(d("2h"), (2 * HOUR, 3 * HOUR));
        assert_eq!(d("1.5hours"), (90 * MIN, 96 * MIN));
        assert_eq!(d("1.25h"), (75 * MIN, 75 * MIN + 36 * SEC));
        assert_eq!(d("250ms"), (250, 251));
        assert_eq!(d("1 day"), (DAY, 2 * DAY));
        assert_eq!(d("2.5S"), (2500, 2600));
        assert_eq!(d(".5h"), (30 * MIN, 36 * MIN));
    }

    #[test]
    fn invalid() {
        for t in ["", "abc", "5 parsecs", "1..2", "1.2.3h", "h", "9999999999999h", "１分"] {
            assert_eq!(parse_duration(t), None, "{t}");
        }
    }
}

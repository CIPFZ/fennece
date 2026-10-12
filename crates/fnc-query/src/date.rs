//! Date values (SPEC ch.2 §4.4).
//!
//! Every date value denotes a half-open local-time span with the granularity of what
//! was written: `2026` is the whole year, `2026-07` the month, `2026-07-02T10` the
//! hour. Comparisons use the span: `>2026` means after the end of 2026.

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use fnc_core::FileTime;

/// Order of numeric date fields in `a/b/c` forms (from the user's locale).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DateOrder {
    /// Month/day/year (en-US).
    Mdy,
    /// Day/month/year (most of Europe).
    Dmy,
    /// Year/month/day (ISO, CJK locales).
    Ymd,
}

/// Inputs needed to resolve relative dates.
#[derive(Copy, Clone, Debug)]
pub struct DateEnv {
    /// Current local time.
    pub now: NaiveDateTime,
    /// Numeric field order.
    pub order: DateOrder,
    /// First day of the week.
    pub week_start: Weekday,
}

impl DateEnv {
    /// Current time with defaults.
    // SPEC-GAP: date order and week start should come from the OS locale
    // (`GetLocaleInfoEx`); Fennec defaults to month/day/year and Monday.
    pub fn now() -> Self {
        let now = FileTime::now().to_local().unwrap_or_default();
        Self { now, order: DateOrder::Mdy, week_start: Weekday::Mon }
    }
}

/// Half-open FILETIME span `[lo, hi)`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DateSpan {
    /// Inclusive start.
    pub lo: i64,
    /// Exclusive end (`i64::MAX` = open).
    pub hi: i64,
}

fn local_span(lo: NaiveDateTime, hi: NaiveDateTime) -> DateSpan {
    DateSpan { lo: FileTime::from_local(lo).0, hi: FileTime::from_local(hi).0 }
}

fn day_span(d: NaiveDate, days: u64) -> Option<DateSpan> {
    let lo = d.and_time(NaiveTime::MIN);
    Some(local_span(lo, lo.checked_add_days(chrono::Days::new(days))?))
}

fn month_span(y: i32, m: u32, months: u32) -> Option<DateSpan> {
    let lo = NaiveDate::from_ymd_opt(y, m, 1)?.and_time(NaiveTime::MIN);
    Some(local_span(lo, lo.checked_add_months(Months::new(months))?))
}

fn week_start(d: NaiveDate, start: Weekday) -> NaiveDate {
    let back = (7 + d.weekday().num_days_from_monday() - start.num_days_from_monday()) % 7;
    d - Duration::days(i64::from(back))
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Mon),
    ("tuesday", Weekday::Tue),
    ("wednesday", Weekday::Wed),
    ("thursday", Weekday::Thu),
    ("friday", Weekday::Fri),
    ("saturday", Weekday::Sat),
    ("sunday", Weekday::Sun),
];

fn constant(t: &str, env: &DateEnv) -> Option<DateSpan> {
    let today = env.now.date();
    let (y, m) = (today.year(), today.month());
    let q_start = (m - 1) / 3 * 3 + 1;
    let now_hi = env.now + Duration::seconds(1);
    let span = match t {
        "today" => day_span(today, 1)?,
        "yesterday" => day_span(today.pred_opt()?, 1)?,
        "tomorrow" => day_span(today.succ_opt()?, 1)?,
        "thisweek" => day_span(week_start(today, env.week_start), 7)?,
        "lastweek" => day_span(week_start(today, env.week_start) - Duration::days(7), 7)?,
        "nextweek" => day_span(week_start(today, env.week_start) + Duration::days(7), 7)?,
        "thismonth" => month_span(y, m, 1)?,
        "lastmonth" => {
            let d = NaiveDate::from_ymd_opt(y, m, 1)?.checked_sub_months(Months::new(1))?;
            month_span(d.year(), d.month(), 1)?
        }
        "nextmonth" => {
            let d = NaiveDate::from_ymd_opt(y, m, 1)?.checked_add_months(Months::new(1))?;
            month_span(d.year(), d.month(), 1)?
        }
        "thisyear" => month_span(y, 1, 12)?,
        "lastyear" => month_span(y - 1, 1, 12)?,
        "nextyear" => month_span(y + 1, 1, 12)?,
        "mtd" => local_span(NaiveDate::from_ymd_opt(y, m, 1)?.and_time(NaiveTime::MIN), now_hi),
        "ytd" => local_span(NaiveDate::from_ymd_opt(y, 1, 1)?.and_time(NaiveTime::MIN), now_hi),
        "qtd" => local_span(NaiveDate::from_ymd_opt(y, q_start, 1)?.and_time(NaiveTime::MIN), now_hi),
        _ => {
            if let Some(i) = MONTHS.iter().position(|full| t == *full || (t.len() == 3 && full.starts_with(t))) {
                // SPEC-GAP: a bare month name is taken to mean that month of the current year.
                return month_span(y, i as u32 + 1, 1);
            }
            if let Some(&(_, wd)) =
                WEEKDAYS.iter().find(|(full, _)| t == *full || (t.len() == 3 && full.starts_with(t)))
            {
                // SPEC-GAP: a bare weekday is the most recent such day (today included).
                let back = (7 + today.weekday().num_days_from_monday() - wd.num_days_from_monday()) % 7;
                return day_span(today - Duration::days(i64::from(back)), 1);
            }
            return None;
        }
    };
    Some(span)
}

/// `3days`, `2weeks`, `past6months`: "within the last N units".
fn relative(t: &str, env: &DateEnv) -> Option<DateSpan> {
    let t = t.strip_prefix("past").unwrap_or(t);
    let digits = t.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 {
        return None;
    }
    let n: u32 = t[..digits].parse().ok()?;
    let unit = &t[digits..];
    let n64 = i64::from(n);
    // All arithmetic is checked: absurd values (`dm:100000000days`) simply don't parse.
    let back = |d: Option<Duration>| d.and_then(|d| env.now.checked_sub_signed(d));
    let start = match unit {
        "sec" | "secs" | "second" | "seconds" | "s" => back(Duration::try_seconds(n64))?,
        "min" | "mins" | "minute" | "minutes" => back(Duration::try_minutes(n64))?,
        "hour" | "hours" | "h" => back(Duration::try_hours(n64))?,
        "day" | "days" | "d" => back(Duration::try_days(n64))?,
        "week" | "weeks" | "w" => back(Duration::try_weeks(n64))?,
        "month" | "months" => env.now.checked_sub_months(Months::new(n))?,
        "year" | "years" | "y" => env.now.checked_sub_months(Months::new(n.checked_mul(12)?))?,
        _ => return None,
    };
    Some(DateSpan { lo: FileTime::from_local(start).0, hi: i64::MAX })
}

fn num(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Parses the date part; returns the start date and the span length (days or months).
enum DateGran {
    Year(i32),
    Month(i32, u32),
    Day(NaiveDate),
}

fn parse_date_part(d: &str, order: DateOrder) -> Option<DateGran> {
    // Every accepted form is ASCII; this also makes the byte slicing below safe.
    if !d.is_ascii() {
        return None;
    }
    let parts: Vec<&str> = d.split(['-', '/', '.']).collect();
    match parts.as_slice() {
        [y] if y.len() == 4 => Some(DateGran::Year(num(y)? as i32)),
        [ymd] if ymd.len() == 8 => {
            let date = NaiveDate::from_ymd_opt(num(&ymd[..4])? as i32, num(&ymd[4..6])?, num(&ymd[6..])?)?;
            Some(DateGran::Day(date))
        }
        [ymd] if ymd.len() == 6 => {
            let date = NaiveDate::from_ymd_opt(2000 + num(&ymd[..2])? as i32, num(&ymd[2..4])?, num(&ymd[4..])?)?;
            Some(DateGran::Day(date))
        }
        [a, b] => {
            let (y, m) = if a.len() == 4 {
                (num(a)?, num(b)?)
            } else if b.len() == 4 {
                (num(b)?, num(a)?)
            } else {
                return None;
            };
            (1..=12).contains(&m).then_some(DateGran::Month(y as i32, m))
        }
        [a, b, c] => {
            let (y, m, dd) = if a.len() == 4 {
                (num(a)?, num(b)?, num(c)?)
            } else {
                let y = num(c)?;
                let y = if c.len() == 2 { 2000 + y } else { y };
                match order {
                    DateOrder::Mdy => (y, num(a)?, num(b)?),
                    DateOrder::Dmy | DateOrder::Ymd => (y, num(b)?, num(a)?),
                }
            };
            Some(DateGran::Day(NaiveDate::from_ymd_opt(y as i32, m, dd)?))
        }
        _ => None,
    }
}

/// Parses an absolute or relative date value. Returns `None` if unrecognised.
pub fn parse_date(text: &str, env: &DateEnv) -> Option<DateSpan> {
    let t = text.trim().to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    if t.len() >= 15 && t.bytes().all(|b| b.is_ascii_digit()) {
        let ft: i64 = t.parse().ok()?;
        return Some(DateSpan { lo: ft, hi: ft.saturating_add(1) });
    }
    if let Some(s) = constant(&t, env) {
        return Some(s);
    }
    if let Some(s) = relative(&t, env) {
        return Some(s);
    }
    let (date_s, time_s) = match t.split_once(['t', ' ']) {
        Some((d, tm)) => (d, Some(tm)),
        None => (t.as_str(), None),
    };
    let gran = parse_date_part(date_s, env.order)?;
    match (gran, time_s) {
        (DateGran::Year(y), None) => month_span(y, 1, 12),
        (DateGran::Month(y, m), None) => month_span(y, m, 1),
        (DateGran::Day(d), None) => day_span(d, 1),
        (DateGran::Day(d), Some(tm)) => {
            let parts: Vec<&str> = tm.split(':').collect();
            let (time, step) = match parts.as_slice() {
                [h] => (NaiveTime::from_hms_opt(num(h)?, 0, 0)?, Duration::hours(1)),
                [h, m] => (NaiveTime::from_hms_opt(num(h)?, num(m)?, 0)?, Duration::minutes(1)),
                [h, m, s] => {
                    let (s, frac) = s.split_once('.').unwrap_or((s, ""));
                    let base = NaiveTime::from_hms_opt(num(h)?, num(m)?, num(s)?)?;
                    if frac.is_empty() {
                        (base, Duration::seconds(1))
                    } else {
                        let nanos = format!("{frac:0<9}").get(..9)?.parse::<i64>().ok()?;
                        (base + Duration::nanoseconds(nanos), Duration::nanoseconds(100))
                    }
                }
                _ => return None,
            };
            let lo = d.and_time(time);
            Some(local_span(lo, lo + step))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> DateEnv {
        // Thursday 2026-10-15 14:30:00 local.
        DateEnv {
            now: NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(14, 30, 0).unwrap(),
            order: DateOrder::Mdy,
            week_start: Weekday::Mon,
        }
    }

    fn ft(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        FileTime::from_local(NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, s).unwrap()).0
    }

    fn span(t: &str) -> DateSpan {
        parse_date(t, &env()).unwrap_or_else(|| panic!("{t} did not parse"))
    }

    #[test]
    fn absolute_granularity() {
        assert_eq!(span("2026"), DateSpan { lo: ft(2026, 1, 1, 0, 0, 0), hi: ft(2027, 1, 1, 0, 0, 0) });
        assert_eq!(span("2026-07"), DateSpan { lo: ft(2026, 7, 1, 0, 0, 0), hi: ft(2026, 8, 1, 0, 0, 0) });
        assert_eq!(span("2026/07"), span("2026-07"));
        assert_eq!(span("07/2026"), span("2026-07"));
        assert_eq!(span("2026-07-02"), DateSpan { lo: ft(2026, 7, 2, 0, 0, 0), hi: ft(2026, 7, 3, 0, 0, 0) });
        assert_eq!(span("20260702"), span("2026-07-02"));
        assert_eq!(span("260702"), span("2026-07-02"));
        assert_eq!(span("07/02/2026"), span("2026-07-02"));
        assert_eq!(
            span("2026-07-02T10:52:00"),
            DateSpan { lo: ft(2026, 7, 2, 10, 52, 0), hi: ft(2026, 7, 2, 10, 52, 1) }
        );
        assert_eq!(span("2026-07-02T10"), DateSpan { lo: ft(2026, 7, 2, 10, 0, 0), hi: ft(2026, 7, 2, 11, 0, 0) });
        assert_eq!(span("2026-07-02t10:52"), DateSpan { lo: ft(2026, 7, 2, 10, 52, 0), hi: ft(2026, 7, 2, 10, 53, 0) });
    }

    #[test]
    fn dmy_locale() {
        let e = DateEnv { order: DateOrder::Dmy, ..env() };
        assert_eq!(parse_date("02/07/2026", &e), Some(span("2026-07-02")));
        assert_eq!(parse_date("02.07.2026", &e), Some(span("2026-07-02")));
    }

    #[test]
    fn constants() {
        assert_eq!(span("today"), DateSpan { lo: ft(2026, 10, 15, 0, 0, 0), hi: ft(2026, 10, 16, 0, 0, 0) });
        assert_eq!(span("yesterday").lo, ft(2026, 10, 14, 0, 0, 0));
        assert_eq!(span("thisweek"), DateSpan { lo: ft(2026, 10, 12, 0, 0, 0), hi: ft(2026, 10, 19, 0, 0, 0) });
        assert_eq!(span("lastweek").lo, ft(2026, 10, 5, 0, 0, 0));
        assert_eq!(span("thismonth"), DateSpan { lo: ft(2026, 10, 1, 0, 0, 0), hi: ft(2026, 11, 1, 0, 0, 0) });
        assert_eq!(span("lastmonth").lo, ft(2026, 9, 1, 0, 0, 0));
        assert_eq!(span("thisyear"), span("2026"));
        assert_eq!(span("lastyear"), span("2025"));
        assert_eq!(span("ytd").lo, ft(2026, 1, 1, 0, 0, 0));
        assert_eq!(span("qtd").lo, ft(2026, 10, 1, 0, 0, 0));
        assert_eq!(span("july"), span("2026-07"));
        assert_eq!(span("jan"), span("2026-01"));
        assert_eq!(span("monday").lo, ft(2026, 10, 12, 0, 0, 0));
        assert_eq!(span("thursday").lo, ft(2026, 10, 15, 0, 0, 0));
    }

    #[test]
    fn relative_ranges() {
        assert_eq!(span("3days").lo, ft(2026, 10, 12, 14, 30, 0));
        assert_eq!(span("past3days"), span("3days"));
        assert_eq!(span("2weeks").lo, ft(2026, 10, 1, 14, 30, 0));
        assert_eq!(span("6months").lo, ft(2026, 4, 15, 14, 30, 0));
        assert_eq!(span("1year").lo, ft(2025, 10, 15, 14, 30, 0));
        assert_eq!(span("4hours").lo, ft(2026, 10, 15, 10, 30, 0));
        assert_eq!(span("30mins").lo, ft(2026, 10, 15, 14, 0, 0));
        assert_eq!(span("10secs").lo, ft(2026, 10, 15, 14, 29, 50));
        assert_eq!(span("3days").hi, i64::MAX);
    }

    #[test]
    fn raw_filetime_and_garbage() {
        assert_eq!(span("134274631200000000"), DateSpan { lo: 134_274_631_200_000_000, hi: 134_274_631_200_000_001 });
        assert_eq!(parse_date("notadate", &env()), None);
        assert_eq!(parse_date("2026-13", &env()), None);
        assert_eq!(parse_date("2026-02-30", &env()), None);
        assert_eq!(parse_date("123", &env()), None);
    }
}

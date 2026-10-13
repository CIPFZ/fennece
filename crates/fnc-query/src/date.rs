//! Date values (SPEC ch.2 §4.4).
//!
//! Every date value denotes a half-open local-time span with the granularity of what
//! was written: `2026` is the whole year, `2026-07` the month, `2026-07-02T10` the
//! hour. Comparisons use the span: `>2026` means after the end of 2026.

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};
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

/// Process-wide defaults for [`DateEnv::now`] (the app sets them from the OS locale or
/// the settings): `order` 0 = M/D/Y, 1 = D/M/Y, 2 = Y/M/D; `week` 0 = Monday … 6 = Sunday.
static ORDER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static WEEK_START: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Sets the date field order and first day of the week used by [`DateEnv::now`].
pub fn set_locale_defaults(order: DateOrder, week_start: Weekday) {
    let o = match order {
        DateOrder::Mdy => 0,
        DateOrder::Dmy => 1,
        DateOrder::Ymd => 2,
    };
    ORDER.store(o, std::sync::atomic::Ordering::Relaxed);
    WEEK_START.store(week_start.num_days_from_monday() as u8, std::sync::atomic::Ordering::Relaxed);
}

impl DateEnv {
    /// Current time with the locale defaults ([`set_locale_defaults`]; M/D/Y and Monday
    /// unless set).
    pub fn now() -> Self {
        let now = FileTime::now().to_local().unwrap_or_default();
        let order = match ORDER.load(std::sync::atomic::Ordering::Relaxed) {
            1 => DateOrder::Dmy,
            2 => DateOrder::Ymd,
            _ => DateOrder::Mdy,
        };
        let w = WEEK_START.load(std::sync::atomic::Ordering::Relaxed);
        let week_start = Weekday::try_from(w).unwrap_or(Weekday::Mon);
        Self { now, order, week_start }
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

/// Calendar unit of the `<prefix>[N]<unit>` constants.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Unit {
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
}

fn unit_of(s: &str) -> Option<Unit> {
    Some(match s {
        "year" | "years" | "y" | "yr" | "yrs" => Unit::Year,
        "quarter" | "quarters" => Unit::Quarter,
        "month" | "months" => Unit::Month,
        "week" | "weeks" | "w" | "wk" | "wks" => Unit::Week,
        "day" | "days" | "d" => Unit::Day,
        "hour" | "hours" | "h" | "hr" | "hrs" => Unit::Hour,
        "minute" | "minutes" | "min" | "mins" => Unit::Minute,
        "second" | "seconds" | "sec" | "secs" | "s" => Unit::Second,
        _ => return None,
    })
}

/// Start of the calendar unit containing `t`.
fn unit_start(t: NaiveDateTime, unit: Unit, env: &DateEnv) -> Option<NaiveDateTime> {
    let d = t.date();
    let midnight = |d: NaiveDate| d.and_time(NaiveTime::MIN);
    Some(match unit {
        Unit::Year => midnight(NaiveDate::from_ymd_opt(d.year(), 1, 1)?),
        Unit::Quarter => midnight(NaiveDate::from_ymd_opt(d.year(), (d.month() - 1) / 3 * 3 + 1, 1)?),
        Unit::Month => midnight(NaiveDate::from_ymd_opt(d.year(), d.month(), 1)?),
        Unit::Week => midnight(week_start(d, env.week_start)),
        Unit::Day => midnight(d),
        Unit::Hour => d.and_hms_opt(t.hour(), 0, 0)?,
        Unit::Minute => d.and_hms_opt(t.hour(), t.minute(), 0)?,
        Unit::Second => d.and_hms_opt(t.hour(), t.minute(), t.second())?,
    })
}

/// `t` moved by `n` units (negative = back). All arithmetic is checked: absurd values
/// (`dm:100000000days`) simply don't parse.
fn add_units(t: NaiveDateTime, unit: Unit, n: i64) -> Option<NaiveDateTime> {
    let months = |m: i64| -> Option<NaiveDateTime> {
        let k = Months::new(u32::try_from(m.unsigned_abs()).ok()?);
        if m >= 0 { t.checked_add_months(k) } else { t.checked_sub_months(k) }
    };
    let dur = |d: Option<Duration>| d.and_then(|d| t.checked_add_signed(d));
    match unit {
        Unit::Year => months(n.checked_mul(12)?),
        Unit::Quarter => months(n.checked_mul(3)?),
        Unit::Month => months(n),
        Unit::Week => dur(Duration::try_weeks(n)),
        Unit::Day => dur(Duration::try_days(n)),
        Unit::Hour => dur(Duration::try_hours(n)),
        Unit::Minute => dur(Duration::try_minutes(n)),
        Unit::Second => dur(Duration::try_seconds(n)),
    }
}

/// Everything's relative constants (Search Syntax → date constants):
///
/// * `<this|current><unit>` — the calendar unit containing now (`thisweek`);
/// * `<last|prev>[N]<unit>` — the N calendar units before the current one
///   (`lastmonth`, `last2weeks`);
/// * `<next|coming>[N]<unit>` — the N calendar units after the current one;
/// * `[past]N<unit>` / `past<unit>` — rolling "within the last N units" (`3days`,
///   `past6months`, `pastweek`); open-ended so future-dated files match too.
fn relative(t: &str, env: &DateEnv) -> Option<DateSpan> {
    const PREFIXES: [&str; 7] = ["current", "this", "last", "prev", "past", "coming", "next"];
    let (prefix, rest) = PREFIXES.iter().find_map(|p| t.strip_prefix(p).map(|r| (*p, r))).unwrap_or(("", t));
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let unit = unit_of(&rest[digits..])?;
    let n: Option<i64> = if digits == 0 { None } else { Some(rest[..digits].parse().ok()?) };
    let now = env.now;
    match (prefix, n) {
        ("this" | "current", None) => {
            let lo = unit_start(now, unit, env)?;
            Some(local_span(lo, add_units(lo, unit, 1)?))
        }
        ("last" | "prev", n) => {
            let hi = unit_start(now, unit, env)?;
            Some(local_span(add_units(hi, unit, -n.unwrap_or(1))?, hi))
        }
        ("next" | "coming", n) => {
            let lo = add_units(unit_start(now, unit, env)?, unit, 1)?;
            Some(local_span(lo, add_units(lo, unit, n.unwrap_or(1))?))
        }
        ("past", n) | ("", n @ Some(_)) => {
            let lo = add_units(now, unit, -n.unwrap_or(1))?;
            Some(DateSpan { lo: FileTime::from_local(lo).0, hi: i64::MAX })
        }
        _ => None,
    }
}

fn constant(t: &str, env: &DateEnv) -> Option<DateSpan> {
    let today = env.now.date();
    let (y, m) = (today.year(), today.month());
    let q_start = (m - 1) / 3 * 3 + 1;
    let now_hi = env.now + Duration::seconds(1);
    let span = match t {
        "today" => day_span(today, 1)?,
        "yesterday" => day_span(today.pred_opt()?, 1)?,
        "tomorrow" => day_span(today.succ_opt()?, 1)?,
        "mtd" => local_span(NaiveDate::from_ymd_opt(y, m, 1)?.and_time(NaiveTime::MIN), now_hi),
        "ytd" => local_span(NaiveDate::from_ymd_opt(y, 1, 1)?.and_time(NaiveTime::MIN), now_hi),
        "qtd" => local_span(NaiveDate::from_ymd_opt(y, q_start, 1)?.and_time(NaiveTime::MIN), now_hi),
        _ => {
            if let Some(i) = month_index(t) {
                // SPEC-GAP: a bare month name is taken to mean that month of the current year.
                return month_span(y, i + 1, 1);
            }
            if let Some(wd) = weekday_of(t) {
                // SPEC-GAP: a bare weekday is the most recent such day (today included).
                let back = (7 + today.weekday().num_days_from_monday() - wd.num_days_from_monday()) % 7;
                return day_span(today - Duration::days(i64::from(back)), 1);
            }
            return relative(t, env);
        }
    };
    Some(span)
}

/// `january`, `jan`, `sept` → 0-based month.
fn month_index(t: &str) -> Option<u32> {
    if t == "sept" {
        return Some(8);
    }
    MONTHS.iter().position(|full| t == *full || (t.len() == 3 && full.starts_with(t))).map(|i| i as u32)
}

/// `monday`, `mon`, `tues`, `thurs` → weekday.
fn weekday_of(t: &str) -> Option<Weekday> {
    let t = match t {
        "tues" => "tue",
        "thur" | "thurs" => "thu",
        "weds" => "wed",
        t => t,
    };
    WEEKDAYS.iter().find(|(full, _)| t == *full || (t.len() == 3 && full.starts_with(t))).map(|&(_, wd)| wd)
}

/// Excel serial day (`46000`, `46000.5`): days since 1899-12-30, local time. Only
/// five-digit integer parts are accepted so the form cannot collide with years
/// (`2026`), `yymmdd` (`260702`) or `yyyymmdd`.
fn excel_serial(t: &str) -> Option<DateSpan> {
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    if int.len() != 5 || !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let base = NaiveDate::from_ymd_opt(1899, 12, 30)?.and_time(NaiveTime::MIN);
    let days: i64 = int.parse().ok()?;
    let day = base.checked_add_signed(Duration::try_days(days)?)?;
    if frac.is_empty() {
        return Some(local_span(day, day.checked_add_signed(Duration::days(1))?));
    }
    // Fractional part: time of day, second granularity.
    let f: f64 = format!("0.{frac}").parse().ok()?;
    let secs = (f * 86_400.0).round() as i64;
    let lo = day.checked_add_signed(Duration::try_seconds(secs)?)?;
    Some(local_span(lo, lo + Duration::seconds(1)))
}

// ------------------------------------------------------------------ wildcards

/// A date with wildcard fields: `*-02` (February of any year),
/// `*-*-*T9:00..*-*-*T17:00` (09:00–17:00 on any day), `2026-*-01` (the first of
/// every month of 2026).
///
/// Only the written fields are compared, lexicographically from year to second, at
/// the granularity of the last written field — so `*-*-*T17:00` covers the whole
/// minute 17:00:00–17:00:59 and a range is inclusive at both ends like other dates.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DateWild {
    /// Which fields take part (bit 0 = year … bit 5 = second).
    pub mask: u8,
    /// Inclusive lower key (`i64::MIN` = open).
    pub lo: i64,
    /// Inclusive upper key (`i64::MAX` = open).
    pub hi: i64,
}

/// Field radices of the packed key (year is unbounded and comes first).
const RADIX: [i64; 5] = [13, 32, 24, 60, 60];

/// Packs the fields selected by `mask`; `pad_max` fills unselected-in-`fields`
/// (`None`) positions with their maximum instead of their minimum.
fn pack(fields: [Option<i64>; 6], mask: u8, pad_max: bool) -> i64 {
    let mut key = 0i64;
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            key = key.saturating_mul(RADIX[i - 1]);
        }
        if mask & (1 << i) == 0 {
            continue;
        }
        let max = if i == 0 { 0 } else { RADIX[i - 1] - 1 };
        let v = f.unwrap_or(if pad_max { max } else { 0 });
        key = key.saturating_add(v);
    }
    key
}

/// Fields of a wildcard date: `None` = `*` (or not written).
type WildFields = ([Option<i64>; 6], u8);

fn wild_field(s: &str) -> Option<Option<i64>> {
    if s == "*" {
        return Some(None);
    }
    num(s).map(|v| Some(i64::from(v)))
}

/// Parses one wildcard date (must contain `*`); returns its fields and the mask of
/// fields written as numbers.
fn parse_wild_fields(t: &str, order: DateOrder) -> Option<WildFields> {
    if !t.is_ascii() || !t.contains('*') {
        return None;
    }
    let (date_s, time_s) = match t.split_once('t') {
        Some((d, tm)) => (d, Some(tm)),
        None => (t, None),
    };
    let mut f: [Option<i64>; 6] = [None; 6];
    let parts: Vec<&str> = date_s.split(['-', '/', '.']).collect();
    let iso = date_s.contains('-');
    match parts.as_slice() {
        [""] if time_s.is_some() => {}
        [y] => f[0] = wild_field(y)?,
        [a, b] => {
            if b.len() == 4 {
                (f[1], f[0]) = (wild_field(a)?, wild_field(b)?);
            } else {
                (f[0], f[1]) = (wild_field(a)?, wild_field(b)?);
            }
        }
        [a, b, c] => {
            if iso || a.len() == 4 {
                (f[0], f[1], f[2]) = (wild_field(a)?, wild_field(b)?, wild_field(c)?);
            } else {
                let y = wild_field(c)?.map(|y| if c.len() == 2 { 2000 + y } else { y });
                let (m, d) = match order {
                    DateOrder::Mdy => (wild_field(a)?, wild_field(b)?),
                    DateOrder::Dmy | DateOrder::Ymd => (wild_field(b)?, wild_field(a)?),
                };
                (f[0], f[1], f[2]) = (y, m, d);
            }
        }
        _ => return None,
    }
    if let Some(tm) = time_s {
        let tp: Vec<&str> = tm.split(':').collect();
        if tp.is_empty() || tp.len() > 3 {
            return None;
        }
        for (k, p) in tp.iter().enumerate() {
            f[3 + k] = wild_field(p)?;
        }
    }
    let mask = f.iter().enumerate().fold(0u8, |m, (i, v)| if v.is_some() { m | (1 << i) } else { m });
    let ok = f[1].is_none_or(|m| (1..=12).contains(&m))
        && f[2].is_none_or(|d| (1..=31).contains(&d))
        && f[3].is_none_or(|h| h < 24)
        && f[4].is_none_or(|m| m < 60)
        && f[5].is_none_or(|s| s < 60);
    (ok && mask != 0).then_some((f, mask))
}

impl DateWild {
    /// Parses a single wildcard date (`*-02`); `None` if `text` has no `*` or is
    /// malformed.
    pub fn parse(text: &str, env: &DateEnv) -> Option<DateWild> {
        let (f, mask) = parse_wild_fields(&text.trim().to_ascii_lowercase(), env.order)?;
        Some(DateWild { mask, lo: pack(f, mask, false), hi: pack(f, mask, true) })
    }

    /// Parses `a..b` where both ends are wildcard dates (an empty end is open).
    pub fn parse_range(a: &str, b: &str, env: &DateEnv) -> Option<DateWild> {
        let fa = (!a.trim().is_empty())
            .then(|| parse_wild_fields(&a.trim().to_ascii_lowercase(), env.order))
            .map_or(Some(None), |f| f.map(Some))?;
        let fb = (!b.trim().is_empty())
            .then(|| parse_wild_fields(&b.trim().to_ascii_lowercase(), env.order))
            .map_or(Some(None), |f| f.map(Some))?;
        let mask = fa.map_or(0, |f| f.1) | fb.map_or(0, |f| f.1);
        if mask == 0 {
            return None;
        }
        Some(DateWild {
            mask,
            lo: fa.map_or(i64::MIN, |(f, _)| pack(f, mask, false)),
            hi: fb.map_or(i64::MAX, |(f, _)| pack(f, mask, true)),
        })
    }

    /// Same fields, keys strictly after this pattern (`>*-*-*T17:00`).
    #[must_use]
    pub fn after(self) -> DateWild {
        DateWild { mask: self.mask, lo: self.hi.saturating_add(1), hi: i64::MAX }
    }

    /// Same fields, keys at or after the start of this pattern.
    #[must_use]
    pub fn from_start(self) -> DateWild {
        DateWild { mask: self.mask, lo: self.lo, hi: i64::MAX }
    }

    /// Same fields, keys strictly before this pattern.
    #[must_use]
    pub fn before(self) -> DateWild {
        DateWild { mask: self.mask, lo: i64::MIN, hi: self.lo.saturating_sub(1) }
    }

    /// Same fields, keys up to the end of this pattern.
    #[must_use]
    pub fn until_end(self) -> DateWild {
        DateWild { mask: self.mask, lo: i64::MIN, hi: self.hi }
    }

    /// `true` if the FILETIME `ft` (UTC) falls in the pattern (in local time).
    pub fn matches(&self, ft: i64) -> bool {
        let Some(t) = FileTime(ft).to_local() else { return false };
        let f = [
            Some(i64::from(t.year())),
            Some(i64::from(t.month())),
            Some(i64::from(t.day())),
            Some(i64::from(t.hour())),
            Some(i64::from(t.minute())),
            Some(i64::from(t.second())),
        ];
        let k = pack(f, self.mask, false);
        k >= self.lo && k <= self.hi
    }
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
    if let Some(s) = excel_serial(&t) {
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
        assert_eq!(parse_date("lastfoo", &env()), None);
        assert_eq!(parse_date("this3weeks", &env()), None);
        assert_eq!(parse_date("next99999999999years", &env()), None);
    }

    #[test]
    fn calendar_prefixes() {
        // Thursday 2026-10-15 14:30 local; weeks start on Monday.
        assert_eq!(span("thisweek"), span("currentweek"));
        assert_eq!(span("nextweek"), DateSpan { lo: ft(2026, 10, 19, 0, 0, 0), hi: ft(2026, 10, 26, 0, 0, 0) });
        assert_eq!(span("comingweek"), span("nextweek"));
        assert_eq!(span("prevweek"), span("lastweek"));
        assert_eq!(span("lastweek").hi, ft(2026, 10, 12, 0, 0, 0));
        assert_eq!(span("last2weeks"), DateSpan { lo: ft(2026, 9, 28, 0, 0, 0), hi: ft(2026, 10, 12, 0, 0, 0) });
        assert_eq!(span("next3months"), DateSpan { lo: ft(2026, 11, 1, 0, 0, 0), hi: ft(2027, 2, 1, 0, 0, 0) });
        assert_eq!(span("nextmonth"), span("2026-11"));
        assert_eq!(span("nextyear"), span("2027"));
        assert_eq!(span("thisquarter"), DateSpan { lo: ft(2026, 10, 1, 0, 0, 0), hi: ft(2027, 1, 1, 0, 0, 0) });
        assert_eq!(span("lastquarter"), DateSpan { lo: ft(2026, 7, 1, 0, 0, 0), hi: ft(2026, 10, 1, 0, 0, 0) });
        assert_eq!(span("thisday"), span("today"));
        assert_eq!(span("lastday"), span("yesterday"));
        assert_eq!(span("thishour"), DateSpan { lo: ft(2026, 10, 15, 14, 0, 0), hi: ft(2026, 10, 15, 15, 0, 0) });
        assert_eq!(span("last2hours"), DateSpan { lo: ft(2026, 10, 15, 12, 0, 0), hi: ft(2026, 10, 15, 14, 0, 0) });
        assert_eq!(span("thisminute").lo, ft(2026, 10, 15, 14, 30, 0));
        assert_eq!(span("pastweek"), DateSpan { lo: ft(2026, 10, 8, 14, 30, 0), hi: i64::MAX });
        assert_eq!(span("pastyear").lo, ft(2025, 10, 15, 14, 30, 0));
        assert_eq!(span("past2years").lo, ft(2024, 10, 15, 14, 30, 0));
        assert_eq!(span("pastmonth").lo, ft(2026, 9, 15, 14, 30, 0));
        assert_eq!(span("past24hours").lo, ft(2026, 10, 14, 14, 30, 0));
        assert_eq!(span("5mins").lo, ft(2026, 10, 15, 14, 25, 0));
        assert_eq!(span("2d"), span("2days"));
        assert_eq!(span("1w"), span("1week"));
        assert_eq!(span("3h"), span("3hours"));
    }

    #[test]
    fn month_and_weekday_names() {
        for (name, m) in [("feb", 2), ("february", 2), ("sep", 9), ("sept", 9), ("december", 12), ("may", 5)] {
            assert_eq!(span(name), month_span(2026, m, 1).unwrap(), "{name}");
        }
        assert_eq!(span("tue").lo, ft(2026, 10, 13, 0, 0, 0));
        assert_eq!(span("tues"), span("tuesday"));
        assert_eq!(span("thurs"), span("thursday"));
        assert_eq!(span("friday").lo, ft(2026, 10, 9, 0, 0, 0));
        assert_eq!(span("sun").lo, ft(2026, 10, 11, 0, 0, 0));
        assert_eq!(span("SAT"), span("saturday"));
    }

    #[test]
    fn to_date_constants() {
        assert_eq!(span("mtd"), DateSpan { lo: ft(2026, 10, 1, 0, 0, 0), hi: ft(2026, 10, 15, 14, 30, 1) });
        assert_eq!(span("ytd").hi, ft(2026, 10, 15, 14, 30, 1));
        assert_eq!(span("qtd").hi, span("mtd").hi);
    }

    #[test]
    fn excel_serial_days() {
        // 46000 = 2025-12-09.
        assert_eq!(span("46000"), span("2025-12-09"));
        assert_eq!(span("46000.5"), DateSpan { lo: ft(2025, 12, 9, 12, 0, 0), hi: ft(2025, 12, 9, 12, 0, 1) });
        assert_eq!(span("45658"), span("2025-01-01"));
        assert_eq!(parse_date("4600", &env()).map(|s| s.lo), Some(ft(4600, 1, 1, 0, 0, 0)));
        assert_eq!(parse_date("460001", &env()), None);
    }

    #[test]
    fn locale_forms() {
        assert_eq!(span("7/2/2026"), span("2026-07-02"));
        assert_eq!(span("07/02/26"), span("2026-07-02"));
        assert_eq!(span("2026.07.02"), span("2026-07-02"));
        let ymd = DateEnv { order: DateOrder::Ymd, ..env() };
        assert_eq!(parse_date("2026/07/02", &ymd), Some(span("2026-07-02")));
        assert_eq!(span("2026-07-02T10:52:00.5").hi - span("2026-07-02T10:52:00.5").lo, 1);
    }

    fn wild(t: &str) -> DateWild {
        DateWild::parse(t, &env()).unwrap_or_else(|| panic!("{t} did not parse"))
    }

    #[test]
    fn wildcard_dates() {
        let feb = wild("*-02");
        assert!(feb.matches(ft(2019, 2, 14, 8, 0, 0)));
        assert!(feb.matches(ft(2026, 2, 28, 23, 59, 59)));
        assert!(!feb.matches(ft(2026, 3, 1, 0, 0, 0)));
        assert!(!feb.matches(i64::MIN));
        let firsts = wild("2026-*-01");
        assert!(firsts.matches(ft(2026, 5, 1, 12, 0, 0)));
        assert!(!firsts.matches(ft(2026, 5, 2, 12, 0, 0)));
        assert!(!firsts.matches(ft(2025, 5, 1, 12, 0, 0)));
        let nine = wild("*-*-*T9:00");
        assert!(nine.matches(ft(2020, 1, 1, 9, 0, 59)));
        assert!(!nine.matches(ft(2020, 1, 1, 9, 1, 0)));
        let us = wild("*/25/*");
        assert!(us.matches(ft(2024, 12, 25, 0, 0, 0)) && !us.matches(ft(2024, 12, 24, 0, 0, 0)));
        let wday = wild("*/*/2025");
        assert!(wday.matches(ft(2025, 6, 1, 0, 0, 0)) && !wday.matches(ft(2024, 6, 1, 0, 0, 0)));
        assert_eq!(DateWild::parse("2026-07-02", &env()), None);
        assert_eq!(DateWild::parse("*-13", &env()), None);
        assert_eq!(DateWild::parse("*-*-*T25:00", &env()), None);
        assert_eq!(DateWild::parse("*-x", &env()), None);
    }

    #[test]
    fn wildcard_ranges_and_comparisons() {
        let office = DateWild::parse_range("*-*-*T9:00", "*-*-*T17:00", &env()).unwrap();
        assert!(office.matches(ft(2026, 3, 3, 9, 0, 0)));
        assert!(office.matches(ft(2026, 3, 3, 12, 34, 56)));
        assert!(office.matches(ft(2026, 3, 3, 17, 0, 59)));
        assert!(!office.matches(ft(2026, 3, 3, 17, 1, 0)));
        assert!(!office.matches(ft(2026, 3, 3, 8, 59, 59)));
        let spring = DateWild::parse_range("*-03", "*-05", &env()).unwrap();
        assert!(spring.matches(ft(2001, 4, 10, 0, 0, 0)) && !spring.matches(ft(2001, 6, 1, 0, 0, 0)));
        let open = DateWild::parse_range("*-*-*T18", "", &env()).unwrap();
        assert!(open.matches(ft(2001, 4, 10, 23, 0, 0)) && !open.matches(ft(2001, 4, 10, 17, 59, 0)));
        let evening = wild("*-*-*T17:00").after();
        assert!(evening.matches(ft(2026, 1, 1, 17, 1, 0)) && !evening.matches(ft(2026, 1, 1, 17, 0, 30)));
        let morning = wild("*-*-*T9").before();
        assert!(morning.matches(ft(2026, 1, 1, 8, 59, 0)) && !morning.matches(ft(2026, 1, 1, 9, 0, 0)));
        assert!(wild("*-*-*T9").until_end().matches(ft(2026, 1, 1, 9, 59, 0)));
        assert!(wild("*-06").from_start().matches(ft(2026, 12, 1, 0, 0, 0)));
        assert_eq!(DateWild::parse_range("", "", &env()), None);
        assert_eq!(DateWild::parse_range("*-02", "today", &env()), None);
    }
}

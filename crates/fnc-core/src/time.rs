//! `FILETIME` timestamps and conversions.
//!
//! Fennec stores every date column as a Windows `FILETIME`: signed 64-bit count of
//! 100 ns ticks since 1601-01-01 00:00:00 UTC. This is what NTFS, USN records and
//! Everything's EFU/IPC/HTTP formats use, so no conversion is needed on the hot path.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};

/// Ticks (100 ns) between 1601-01-01 and 1970-01-01.
pub const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
/// Ticks per second.
pub const TICKS_PER_SECOND: i64 = 10_000_000;
/// Ticks per day.
pub const TICKS_PER_DAY: i64 = TICKS_PER_SECOND * 86_400;

/// A Windows `FILETIME` value (UTC).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[repr(transparent)]
pub struct FileTime(pub i64);

impl FileTime {
    /// Value used for "not indexed / unknown". Sorts before every real date.
    pub const UNKNOWN: FileTime = FileTime(i64::MIN);

    /// `true` if this is not [`FileTime::UNKNOWN`].
    #[inline]
    pub const fn is_known(self) -> bool {
        self.0 != i64::MIN
    }

    /// Converts from Unix seconds + nanoseconds.
    pub const fn from_unix(secs: i64, nanos: u32) -> Self {
        FileTime(UNIX_EPOCH_TICKS + secs * TICKS_PER_SECOND + (nanos / 100) as i64)
    }

    /// Converts a [`std::time::SystemTime`]; times before 1601 clamp to 0.
    pub fn from_system_time(t: std::time::SystemTime) -> Self {
        match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => Self::from_unix(d.as_secs() as i64, d.subsec_nanos()),
            Err(e) => {
                let d = e.duration();
                let ticks =
                    UNIX_EPOCH_TICKS - (d.as_secs() as i64) * TICKS_PER_SECOND - i64::from(d.subsec_nanos() / 100);
                FileTime(ticks.max(0))
            }
        }
    }

    /// Current time.
    pub fn now() -> Self {
        Self::from_system_time(std::time::SystemTime::now())
    }

    /// Unix seconds (floor).
    pub const fn unix_secs(self) -> i64 {
        (self.0 - UNIX_EPOCH_TICKS).div_euclid(TICKS_PER_SECOND)
    }

    /// UTC date-time, `None` if unknown or out of chrono's range.
    pub fn to_utc(self) -> Option<DateTime<Utc>> {
        if !self.is_known() {
            return None;
        }
        let rel = self.0 - UNIX_EPOCH_TICKS;
        let secs = rel.div_euclid(TICKS_PER_SECOND);
        let nanos = (rel.rem_euclid(TICKS_PER_SECOND) * 100) as u32;
        DateTime::from_timestamp(secs, nanos)
    }

    /// Local wall-clock date-time.
    pub fn to_local(self) -> Option<NaiveDateTime> {
        self.to_utc().map(|u| u.with_timezone(&Local).naive_local())
    }

    /// Builds from a UTC date-time.
    pub fn from_utc(dt: DateTime<Utc>) -> Self {
        Self::from_unix(dt.timestamp(), dt.timestamp_subsec_nanos())
    }

    /// Builds from a local wall-clock time. Ambiguous times (DST fold) take the earlier
    /// instant; non-existent times (DST gap) are shifted forward by one hour.
    pub fn from_local(dt: NaiveDateTime) -> Self {
        match Local.from_local_datetime(&dt) {
            chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
                Self::from_utc(t.with_timezone(&Utc))
            }
            chrono::LocalResult::None => {
                let shifted = dt + chrono::Duration::hours(1);
                match Local.from_local_datetime(&shifted) {
                    chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
                        Self::from_utc(t.with_timezone(&Utc))
                    }
                    chrono::LocalResult::None => Self::from_utc(dt.and_utc()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_roundtrip() {
        let ft = FileTime::from_unix(0, 0);
        assert_eq!(ft.0, UNIX_EPOCH_TICKS);
        assert_eq!(ft.unix_secs(), 0);
        let ft = FileTime::from_unix(1_700_000_000, 123_456_700);
        assert_eq!(ft.unix_secs(), 1_700_000_000);
        let utc = ft.to_utc().unwrap();
        assert_eq!(FileTime::from_utc(utc), ft);
    }

    #[test]
    fn unknown_sorts_first_and_has_no_date() {
        assert!(FileTime::UNKNOWN < FileTime(0));
        assert!(FileTime::UNKNOWN.to_utc().is_none());
        assert!(!FileTime::UNKNOWN.is_known());
    }

    #[test]
    fn known_filetime_value() {
        // 2026-07-02T10:52:00Z, cross-checked with Python's datetime.
        let dt = DateTime::parse_from_rfc3339("2026-07-02T10:52:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(FileTime::from_utc(dt).0, 134_274_631_200_000_000);
    }

    #[test]
    fn local_roundtrip() {
        let now = FileTime::from_unix(1_750_000_000, 0);
        let local = now.to_local().unwrap();
        assert_eq!(FileTime::from_local(local), now);
    }
}

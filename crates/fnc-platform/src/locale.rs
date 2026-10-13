//! The user's UI language as a BCP-47 tag (`zh-CN`, `en-US`, ...).

/// UI language of the current user: `GetUserDefaultUILanguage` on Windows, `LC_ALL` /
/// `LC_MESSAGES` / `LANG` elsewhere. `None` if unknown.
pub fn ui_language() -> Option<String> {
    imp::ui_language().filter(|s| !s.is_empty())
}

/// Order of numeric date fields in the user's short date format.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DateFieldOrder {
    /// Month, day, year (en-US).
    Mdy,
    /// Day, month, year.
    Dmy,
    /// Year, month, day (ISO, CJK).
    Ymd,
}

/// Field order of a short date pattern (`M/d/yyyy`, `dd.MM.yyyy`, `yyyy/M/d`, `%d/%m/%Y`).
pub fn order_of_pattern(p: &str) -> Option<DateFieldOrder> {
    let first = |cs: &[char]| p.find(|c: char| cs.contains(&c));
    let (y, m, d) = (first(&['y', 'Y']), first(&['M', 'm', 'b']), first(&['d', 'e']));
    let (y, m, d) = (y?, m?, d?);
    Some(if y < m && m < d {
        DateFieldOrder::Ymd
    } else if d < m {
        DateFieldOrder::Dmy
    } else {
        DateFieldOrder::Mdy
    })
}

/// The user's date field order (Windows: `LOCALE_SSHORTDATE`; elsewhere guessed from
/// `LC_TIME`/`LANG`). `None` if unknown.
pub fn date_order() -> Option<DateFieldOrder> {
    imp::date_order()
}

/// First day of the week, 0 = Monday … 6 = Sunday (Windows: `LOCALE_IFIRSTDAYOFWEEK`).
pub fn first_day_of_week() -> Option<u8> {
    imp::first_day_of_week()
}

/// The system ANSI code page (`GetACP`, e.g. 1252 or 936); 1252 elsewhere.
pub fn ansi_code_page() -> u16 {
    imp::ansi_code_page()
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Globalization::{
        GetACP, GetLocaleInfoEx, GetUserDefaultUILanguage, LCIDToLocaleName, LOCALE_IFIRSTDAYOFWEEK, LOCALE_SSHORTDATE,
    };

    use super::DateFieldOrder;

    fn locale_info(lctype: u32) -> Option<String> {
        let mut buf = [0u16; 128];
        // SAFETY: `None` selects LOCALE_NAME_USER_DEFAULT; `buf` is owned and its length
        // is passed by the wrapper.
        let n = unsafe { GetLocaleInfoEx(windows::core::PCWSTR::null(), lctype, Some(&mut buf)) };
        let n = usize::try_from(n).ok()?.checked_sub(1)?;
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    pub fn date_order() -> Option<DateFieldOrder> {
        super::order_of_pattern(&locale_info(LOCALE_SSHORTDATE)?)
    }

    pub fn first_day_of_week() -> Option<u8> {
        locale_info(LOCALE_IFIRSTDAYOFWEEK)?.trim().parse().ok().filter(|d: &u8| *d < 7)
    }

    pub fn ansi_code_page() -> u16 {
        // SAFETY: no arguments; returns the process's ANSI code page.
        u16::try_from(unsafe { GetACP() }).unwrap_or(1252)
    }

    pub fn ui_language() -> Option<String> {
        // SAFETY: no arguments; returns the user's UI LANGID.
        let langid = unsafe { GetUserDefaultUILanguage() };
        let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
        // SAFETY: `buf` is an owned buffer whose length is passed by the wrapper.
        let n = unsafe { LCIDToLocaleName(u32::from(langid), Some(&mut buf), 0) };
        let n = usize::try_from(n).ok()?.checked_sub(1)?; // drop the NUL
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::DateFieldOrder;

    fn locale() -> String {
        ["LC_ALL", "LC_TIME", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
            .unwrap_or_default()
    }

    /// Region from `ll_RR.charset`.
    fn region(l: &str) -> (String, String) {
        let base = l.split(['.', '@']).next().unwrap_or("");
        let (lang, reg) = base.split_once('_').unwrap_or((base, ""));
        (lang.to_ascii_lowercase(), reg.to_ascii_uppercase())
    }

    pub fn date_order() -> Option<DateFieldOrder> {
        let (lang, reg) = region(&locale());
        if lang.is_empty() {
            return None;
        }
        Some(match (lang.as_str(), reg.as_str()) {
            ("en", "US" | "PH") => DateFieldOrder::Mdy,
            ("zh" | "ja" | "ko" | "hu" | "lt" | "mn", _) | ("en", "CA") | ("sv", _) => DateFieldOrder::Ymd,
            ("en", _) if reg.is_empty() => DateFieldOrder::Mdy,
            _ => DateFieldOrder::Dmy,
        })
    }

    pub fn first_day_of_week() -> Option<u8> {
        let (lang, reg) = region(&locale());
        if lang.is_empty() {
            return None;
        }
        // Sunday-first regions (US, CA, JP, ...); Monday elsewhere (ISO 8601).
        Some(if matches!(reg.as_str(), "US" | "CA" | "JP" | "TW" | "HK" | "IL" | "BR" | "PH" | "MX") { 6 } else { 0 })
    }

    pub fn ansi_code_page() -> u16 {
        1252
    }

    pub fn ui_language() -> Option<String> {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
            .map(|v| v.split(['.', '@']).next().unwrap_or("").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert_eq!(order_of_pattern("M/d/yyyy"), Some(DateFieldOrder::Mdy));
        assert_eq!(order_of_pattern("dd.MM.yyyy"), Some(DateFieldOrder::Dmy));
        assert_eq!(order_of_pattern("yyyy/M/d"), Some(DateFieldOrder::Ymd));
        assert_eq!(order_of_pattern("%d/%m/%Y"), Some(DateFieldOrder::Dmy));
        assert_eq!(order_of_pattern("yyyy'年'M'月'd'日'"), Some(DateFieldOrder::Ymd));
        assert_eq!(order_of_pattern("HH:mm"), None);
    }

    #[cfg(windows)]
    #[test]
    fn system_date_locale() {
        let (o, w) = (date_order(), first_day_of_week());
        eprintln!("date order {o:?}, first day of week {w:?}");
        assert!(o.is_some() && w.is_some());
    }
}

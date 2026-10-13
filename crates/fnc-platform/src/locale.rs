//! The user's UI language as a BCP-47 tag (`zh-CN`, `en-US`, ...).

/// UI language of the current user: `GetUserDefaultUILanguage` on Windows, `LC_ALL` /
/// `LC_MESSAGES` / `LANG` elsewhere. `None` if unknown.
pub fn ui_language() -> Option<String> {
    imp::ui_language().filter(|s| !s.is_empty())
}

/// The system ANSI code page (`GetACP`, e.g. 1252 or 936); 1252 elsewhere.
pub fn ansi_code_page() -> u16 {
    imp::ansi_code_page()
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Globalization::{GetACP, GetUserDefaultUILanguage, LCIDToLocaleName};

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

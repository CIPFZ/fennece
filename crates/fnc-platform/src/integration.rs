//! Desktop integration in the per-user registry (SPEC ch.3 §11, ch.4 §6): start with
//! Windows, Explorer context menu entries ("Search Fennec" on folders, "Explore in
//! Fennec" on files). Everything is written below `HKEY_CURRENT_USER`, so no
//! administrator rights are needed and uninstalling leaves nothing behind.

use std::io;
use std::path::Path;

/// Registry value name of the autostart entry (`…\CurrentVersion\Run`).
pub const RUN_VALUE: &str = "Fennec";

/// `HKCU\Software\Classes` keys of the context menu entries and their command lines;
/// `%1` / `%V` are filled in by Explorer.
pub fn context_menu_keys(exe: &Path, folder: bool, label: &str) -> Vec<(String, String, String)> {
    let exe = exe.display();
    if folder {
        vec![
            (r"Software\Classes\Directory\shell\Fennec".into(), label.into(), format!("\"{exe}\" -path \"%1\"")),
            (
                r"Software\Classes\Directory\Background\shell\Fennec".into(),
                label.into(),
                format!("\"{exe}\" -path \"%V\""),
            ),
            (r"Software\Classes\Drive\shell\Fennec".into(), label.into(), format!("\"{exe}\" -path \"%1\"")),
        ]
    } else {
        vec![(r"Software\Classes\*\shell\FennecExplore".into(), label.into(), format!("\"{exe}\" -filename \"%1\""))]
    }
}

/// Adds (or removes) the autostart entry `"exe" -startup`.
pub fn set_run_on_startup(exe: &Path, enabled: bool) -> io::Result<()> {
    imp::set_run_on_startup(exe, enabled)
}

/// Whether the autostart entry exists.
pub fn run_on_startup() -> bool {
    imp::run_on_startup()
}

/// Installs (or removes) the folder or file context menu entry labelled `label`.
pub fn set_context_menu(exe: &Path, folder: bool, label: &str, enabled: bool) -> io::Result<()> {
    imp::set_context_menu(&context_menu_keys(exe, folder, label), enabled)
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
    };
    use windows::core::PCWSTR;

    use super::*;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn check(e: WIN32_ERROR) -> io::Result<()> {
        if e == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(e.0 as i32)) }
    }

    /// Creates `sub` and sets its value `name` (`None` = default value) to `data`.
    fn set_value(sub: &str, name: Option<&str>, data: &str) -> io::Result<()> {
        let (sub_w, data_w) = (wide(sub), wide(data));
        let name_w = name.map(wide);
        let mut key = HKEY::default();
        // SAFETY: `sub_w` is NUL-terminated; `key` receives an owned handle closed below.
        check(unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub_w.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        })?;
        let bytes: Vec<u8> = data_w.iter().flat_map(|u| u.to_le_bytes()).collect();
        let name_p = name_w.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
        // SAFETY: `key` is open; `bytes` is the NUL-terminated UTF-16 string as bytes.
        let r = check(unsafe { RegSetValueExW(key, name_p, None, REG_SZ, Some(&bytes)) });
        // SAFETY: closes the handle opened above.
        let _ = unsafe { RegCloseKey(key) };
        r
    }

    pub fn set_run_on_startup(exe: &Path, enabled: bool) -> io::Result<()> {
        if enabled {
            return set_value(RUN_KEY, Some(RUN_VALUE), &format!("\"{}\" -startup", exe.display()));
        }
        let (sub, name) = (wide(RUN_KEY), wide(RUN_VALUE));
        let mut key = HKEY::default();
        // SAFETY: as in `set_value`.
        check(unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        })?;
        // SAFETY: `key` is open; `name` is NUL-terminated.
        let e = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
        // SAFETY: closes the handle opened above.
        let _ = unsafe { RegCloseKey(key) };
        if e == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(e) }
    }

    pub fn run_on_startup() -> bool {
        let (sub, name) = (wide(RUN_KEY), wide(RUN_VALUE));
        // SAFETY: only queries the presence/size of the value.
        let e = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            )
        };
        e == ERROR_SUCCESS
    }

    pub fn set_context_menu(keys: &[(String, String, String)], enabled: bool) -> io::Result<()> {
        for (key, label, command) in keys {
            if enabled {
                set_value(key, None, label)?;
                set_value(&format!(r"{key}\command"), None, command)?;
            } else {
                let w = wide(key);
                // SAFETY: deletes the per-user key tree created above; `w` is NUL-terminated.
                let e = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(w.as_ptr())) };
                if e != ERROR_FILE_NOT_FOUND {
                    check(e)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    fn unsupported() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, "desktop integration is only available on Windows")
    }

    pub fn set_run_on_startup(_exe: &Path, _enabled: bool) -> io::Result<()> {
        Err(unsupported())
    }

    pub fn run_on_startup() -> bool {
        false
    }

    pub fn set_context_menu(_keys: &[(String, String, String)], _enabled: bool) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_menu_commands() {
        let k = context_menu_keys(Path::new(r"C:\Program Files\Fennec\fennec.exe"), true, "Search Fennec");
        assert_eq!(k.len(), 3);
        assert_eq!(k[0].2, r#""C:\Program Files\Fennec\fennec.exe" -path "%1""#);
        assert!(k[1].0.contains("Background") && k[1].2.ends_with(r#""%V""#));
        let f = context_menu_keys(Path::new(r"C:\f.exe"), false, "Explore in Fennec");
        assert_eq!(f[0].2, r#""C:\f.exe" -filename "%1""#);
    }
}

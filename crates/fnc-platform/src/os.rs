//! Process-level OS helpers of the crash reports and console tools (REL-01, REL-02).
//!
//! * [`version`] — OS description for `logs/crash-*.log` (Windows: product name,
//!   release, build and revision from the registry; elsewhere the kernel name/release).
//! * [`attach_parent_console`] — lets the GUI-subsystem `fennec.exe` print `-version` /
//!   `-help` into the console it was started from (no window when started from Explorer).
//! * [`install_exception_filter`] — writes a minimal crash report for fatal non-panic
//!   crashes (Windows unhandled structured exceptions such as access violations).

/// Human-readable OS description, e.g. `Windows Server 2022 Datacenter 21H2 (build
/// 20348.2849, x86_64)`. Best effort: never fails.
pub fn version() -> String {
    imp::version()
}

/// Attaches stdout/stderr to the console of the parent process if this process has
/// none (Windows GUI subsystem started from cmd / PowerShell). Returns `true` if output
/// now reaches a console or was already redirected. Never creates a console window.
pub fn attach_parent_console() -> bool {
    imp::attach_parent_console()
}

/// Installs a process-wide unhandled-exception filter that writes a minimal crash
/// report ([`fnc_core::crash::write_exception_report`]) and then lets the previous
/// filter / Windows Error Reporting continue. No-op off Windows.
pub fn install_exception_filter() {
    imp::install_exception_filter();
}

/// Hidden crash-report test (`fes -debug-crash`): raises an unhandled, non-continuable
/// structured exception with the access-violation code (no memory is touched), so the
/// filter of [`install_exception_filter`] runs. Returns without effect off Windows.
pub fn raise_debug_exception() {
    imp::raise_debug_exception();
}

/// Joins the parts of a Windows version description (testable core of [`version`]).
pub fn describe_windows(product: Option<&str>, display: Option<&str>, build: Option<&str>, ubr: Option<u32>) -> String {
    let mut s = product.filter(|p| !p.is_empty()).unwrap_or("Windows").to_string();
    if let Some(d) = display.filter(|d| !d.is_empty()) {
        s.push(' ');
        s.push_str(d);
    }
    match (build, ubr) {
        (Some(b), Some(u)) => s.push_str(&format!(" (build {b}.{u}, {})", std::env::consts::ARCH)),
        (Some(b), None) => s.push_str(&format!(" (build {b}, {})", std::env::consts::ARCH)),
        _ => s.push_str(&format!(" ({})", std::env::consts::ARCH)),
    }
    s
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use windows::Win32::Foundation::NTSTATUS;
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE};
    use windows::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER, RaiseException, SetUnhandledExceptionFilter,
    };
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::PCWSTR;

    const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    /// Code of C++ exceptions (`_CxxThrowException`), which Rust panics also use; panics
    /// are reported by the panic hook.
    const CXX_EXCEPTION: u32 = 0xE06D_7363;
    /// `EXCEPTION_CONTINUE_SEARCH`.
    const CONTINUE_SEARCH: i32 = 0;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn reg_string(name: &str) -> Option<String> {
        let (key, value) = (wide(CURRENT_VERSION), wide(name));
        let mut buf = vec![0u16; 256];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: `key` and `value` are NUL-terminated; `buf` is writable for `bytes`
        // bytes and the function writes at most that many.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(key.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if rc.is_err() {
            return None;
        }
        let n = (bytes as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn reg_dword(name: &str) -> Option<u32> {
        let (key, value) = (wide(CURRENT_VERSION), wide(name));
        let mut data = 0u32;
        let mut bytes = 4u32;
        // SAFETY: as in `reg_string`; `data` is a writable 4-byte DWORD.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(key.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut data).cast()),
                Some(&mut bytes),
            )
        };
        rc.is_ok().then_some(data)
    }

    pub fn version() -> String {
        let display = reg_string("DisplayVersion").or_else(|| reg_string("ReleaseId"));
        super::describe_windows(
            reg_string("ProductName").as_deref(),
            display.as_deref(),
            reg_string("CurrentBuildNumber").as_deref(),
            reg_dword("UBR"),
        )
    }

    pub fn attach_parent_console() -> bool {
        // SAFETY: plain query of this process's standard handle.
        if unsafe { GetStdHandle(STD_ERROR_HANDLE) }.is_ok() {
            // Redirected, or already a console process.
            return true;
        }
        // SAFETY: attaches to the parent's console if it has one; fails harmlessly
        // (started from Explorer) without creating a console.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
    }

    static PREVIOUS: OnceLock<LPTOP_LEVEL_EXCEPTION_FILTER> = OnceLock::new();

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: Windows passes valid exception pointers (or null, checked) for the
        // duration of the call.
        let record = unsafe { info.as_ref() }.and_then(|i| {
            // SAFETY: as above; the record pointer is null-checked by `as_ref`.
            unsafe { i.ExceptionRecord.as_ref() }
        });
        if let Some(r) = record {
            let NTSTATUS(code) = r.ExceptionCode;
            let code = code as u32;
            if code != CXX_EXCEPTION {
                let _ = fnc_core::crash::write_exception_report(code, r.ExceptionAddress as usize);
            }
        }
        match PREVIOUS.get().copied().flatten() {
            // SAFETY: chains to the filter that was installed before ours, with the
            // same arguments Windows passed to us.
            Some(prev) => unsafe { prev(info) },
            None => CONTINUE_SEARCH,
        }
    }

    pub fn install_exception_filter() {
        if PREVIOUS.get().is_some() {
            return;
        }
        // SAFETY: `filter` is a valid `extern "system"` function for the process
        // lifetime; the previous filter is kept and chained.
        let prev = unsafe { SetUnhandledExceptionFilter(Some(filter)) };
        let _ = PREVIOUS.set(prev);
    }

    /// `STATUS_ACCESS_VIOLATION`.
    const ACCESS_VIOLATION: u32 = 0xC000_0005;
    /// `EXCEPTION_NONCONTINUABLE`.
    const NONCONTINUABLE: u32 = 1;

    pub fn raise_debug_exception() {
        // SAFETY: raises a software exception without arguments; nothing is dereferenced.
        // Unhandled, it ends the process through the installed filter (the purpose).
        unsafe { RaiseException(ACCESS_VIOLATION, NONCONTINUABLE, None) };
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn version() -> String {
        let read = |f: &str| std::fs::read_to_string(f).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let pretty = std::fs::read_to_string("/etc/os-release").ok().and_then(|t| {
            t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_string())
        });
        let kernel = match (read("/proc/sys/kernel/ostype"), read("/proc/sys/kernel/osrelease")) {
            (Some(t), Some(r)) => format!("{t} {r}"),
            _ => std::env::consts::OS.to_string(),
        };
        match pretty {
            Some(p) => format!("{p} ({kernel}, {})", std::env::consts::ARCH),
            None => format!("{kernel} ({})", std::env::consts::ARCH),
        }
    }

    pub fn attach_parent_console() -> bool {
        true
    }

    pub fn install_exception_filter() {}

    pub fn raise_debug_exception() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_description() {
        let arch = std::env::consts::ARCH;
        assert_eq!(
            describe_windows(Some("Windows Server 2022 Datacenter"), Some("21H2"), Some("20348"), Some(2849)),
            format!("Windows Server 2022 Datacenter 21H2 (build 20348.2849, {arch})")
        );
        assert_eq!(describe_windows(None, None, Some("19045"), None), format!("Windows (build 19045, {arch})"));
        assert_eq!(describe_windows(Some(""), Some(""), None, Some(1)), format!("Windows ({arch})"));
        let v = version();
        assert!(!v.is_empty() && v.contains(arch), "{v}");
        if cfg!(windows) {
            assert!(v.contains("build "), "{v}");
        }
        assert!(attach_parent_console() || cfg!(windows));
    }
}

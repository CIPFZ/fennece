//! Identity checks for local IPC clients (SPEC ch.4 §3.1: write operations check the
//! client's session id and integrity level).
//!
//! * Windows: terminal-services session id (`ProcessIdToSessionId`) and the mandatory
//!   integrity level of the client's token (`TokenIntegrityLevel`).
//! * Linux: the "session" is the owning user id of `/proc/<pid>`; there are no integrity
//!   levels.
//! * Elsewhere every query returns `None` (callers deny).

/// `SECURITY_MANDATORY_MEDIUM_RID`: a normal user process.
pub const INTEGRITY_MEDIUM: u32 = 0x2000;

/// Session of the current process.
pub fn current_session_id() -> Option<u32> {
    process_session_id(std::process::id())
}

/// Session of process `pid` (`None` if the process is gone or cannot be queried).
pub fn process_session_id(pid: u32) -> Option<u32> {
    imp::process_session_id(pid)
}

/// Mandatory integrity level RID of process `pid` (`0x1000` low, `0x2000` medium,
/// `0x3000` high, `0x4000` system); `None` where the platform has none or the query fails.
pub fn process_integrity(pid: u32) -> Option<u32> {
    imp::process_integrity(pid)
}

/// `true` if `pid` runs in this process's session and (on Windows) with at least medium
/// integrity: the clients allowed to run administrative IPC commands.
pub fn is_trusted_peer(pid: u32) -> bool {
    let same = match (process_session_id(pid), current_session_id()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    same && (!cfg!(windows) || process_integrity(pid).is_some_and(|l| l >= INTEGRITY_MEDIUM))
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
        TokenIntegrityLevel,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

    pub fn process_session_id(pid: u32) -> Option<u32> {
        let mut session = 0u32;
        // SAFETY: `session` is a valid out-pointer for the duration of the call.
        unsafe { ProcessIdToSessionId(pid, &mut session) }.ok()?;
        Some(session)
    }

    pub fn process_integrity(pid: u32) -> Option<u32> {
        // SAFETY: query-only access; the handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut token = HANDLE::default();
        // SAFETY: `process` is a valid process handle; `token` is a valid out-pointer.
        let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.is_ok();
        // SAFETY: `process` was opened above and is closed exactly once.
        let _ = unsafe { CloseHandle(process) };
        if !opened {
            return None;
        }
        let level = token_integrity(token);
        // SAFETY: `token` was opened above and is closed exactly once.
        let _ = unsafe { CloseHandle(token) };
        level
    }

    fn token_integrity(token: HANDLE) -> Option<u32> {
        let mut len = 0u32;
        // SAFETY: size query without a buffer; `len` is a valid out-pointer.
        let _ = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len) };
        if len == 0 || len > 4096 {
            return None;
        }
        // u64 storage: TOKEN_MANDATORY_LABEL holds a pointer and needs pointer alignment.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` provides at least `len` writable, 8-byte aligned bytes.
        unsafe { GetTokenInformation(token, TokenIntegrityLevel, Some(buf.as_mut_ptr().cast()), len, &mut len) }
            .ok()?;
        // SAFETY: on success the buffer starts with a TOKEN_MANDATORY_LABEL whose SID
        // pointer refers to a valid SID inside the same buffer, which outlives these reads.
        unsafe {
            let label = &*(buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>());
            let sid = label.Label.Sid;
            if sid.0.is_null() {
                return None;
            }
            let count = *GetSidSubAuthorityCount(sid);
            if count == 0 {
                return None;
            }
            Some(*GetSidSubAuthority(sid, u32::from(count - 1)))
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::unix::fs::MetadataExt;

    pub fn process_session_id(pid: u32) -> Option<u32> {
        std::fs::metadata(format!("/proc/{pid}")).ok().map(|m| m.uid())
    }

    pub fn process_integrity(_pid: u32) -> Option<u32> {
        None
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn process_session_id(_pid: u32) -> Option<u32> {
        None
    }

    pub fn process_integrity(_pid: u32) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_trusted() {
        if cfg!(any(windows, target_os = "linux")) {
            assert!(current_session_id().is_some());
            assert!(is_trusted_peer(std::process::id()));
        }
        // A pid that cannot exist.
        assert!(!is_trusted_peer(u32::MAX - 1));
        #[cfg(windows)]
        assert!(process_integrity(std::process::id()).is_some_and(|l| l >= INTEGRITY_MEDIUM));
    }
}

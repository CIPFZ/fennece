//! Process privilege queries and thread priority.

/// `true` if the process runs elevated (administrator token). Always `false` on
/// non-Windows platforms.
pub fn is_elevated() -> bool {
    imp::is_elevated()
}

/// Puts the calling thread into background mode (low CPU and I/O priority,
/// `THREAD_MODE_BACKGROUND_BEGIN`) for indexing work (SPEC ch.1 §8.5). `false` if the
/// platform has no such mode or the call failed.
pub fn enter_background_mode() -> bool {
    imp::enter_background_mode()
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub fn is_elevated() -> bool {
        let mut token = HANDLE::default();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing;
        // `token` is a valid out-pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        // SAFETY: the buffer is a properly aligned TOKEN_ELEVATION and its exact size is
        // passed; `token` is a valid token handle opened above.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some(std::ptr::from_mut(&mut elevation).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
        }
        .is_ok();
        // SAFETY: `token` was opened by OpenProcessToken and is closed exactly once.
        let _ = unsafe { CloseHandle(token) };
        ok && elevation.TokenIsElevated != 0
    }

    pub fn enter_background_mode() -> bool {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
        // SAFETY: GetCurrentThread returns a pseudo-handle for the calling thread, valid
        // for the call; background mode only lowers this thread's priorities.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) }.is_ok()
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_elevated() -> bool {
        false
    }

    pub fn enter_background_mode() -> bool {
        false
    }
}

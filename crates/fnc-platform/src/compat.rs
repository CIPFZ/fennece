//! Hidden Everything-compatible IPC window (SPEC ch.4 §3.2).
//!
//! The window plumbing only: a top-level, never shown window of a given class (Everything
//! clients find it with `FindWindow`, which ignores message-only windows) on its own
//! thread, routing the IPC message (`WM_USER`) and `WM_COPYDATA` to a [`CompatHandler`].
//! Queries run on a worker thread (`fennec-compat-query`) that sends the reply
//! `WM_COPYDATA` to the client window itself, so the window thread never waits for a
//! search. The protocol (structures, commands) lives in `fnc_ipc::compat`.
//!
//! ```text
//! let w = CompatWindow::start(&CompatConfig { class, ipc_message: 0x400, announce }, handler)?;
//! drop(w); // or w.stop(): closes the window and joins its thread
//! ```

use std::sync::Arc;

/// A reply to send to the client: `WM_COPYDATA` to `hwnd` with `dwData = copydata_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyDataReply {
    /// Client window.
    pub hwnd: isize,
    /// `dwData` of the reply.
    pub copydata_id: usize,
    /// Reply bytes.
    pub bytes: Vec<u8>,
}

/// Work deferred to the query thread; its reply (if any) is sent from there.
pub type QueryJob = Box<dyn FnOnce() -> Option<CopyDataReply> + Send>;

/// How a `WM_COPYDATA` is answered.
pub enum CopyDataAction {
    /// Return this value from the window procedure.
    Done(isize),
    /// Return `TRUE` now and run the job on the query thread.
    Query(QueryJob),
}

/// Message handler of the compatibility window. Called on the window thread; must not
/// block for long (run counts take the index write lock briefly; searches are deferred
/// with [`CopyDataAction::Query`]).
pub trait CompatHandler: Send + Sync + 'static {
    /// The IPC message (`EVERYTHING_WM_IPC`): command `wparam`, argument `lparam`.
    fn wm_ipc(&self, wparam: usize, lparam: isize) -> isize;
    /// A `WM_COPYDATA` with `dwData = dw_data` (the bytes are a copy of `lpData`).
    fn copydata(&self, dw_data: usize, bytes: Vec<u8>) -> CopyDataAction;
    /// `WM_CLOSE` from a client: Everything exits (es.exe `-exit` sends it and waits for
    /// the process to end). The window itself stays until [`CompatWindow::stop`].
    fn close(&self) {}
}

/// Window settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatConfig {
    /// Window class and title (`EVERYTHING_TASKBAR_NOTIFICATION[_(<instance>)]`).
    pub class: String,
    /// Message id routed to [`CompatHandler::wm_ipc`] (`WM_USER`).
    pub ipc_message: u32,
    /// Registered message broadcast once the window exists (`EVERYTHING_IPC_CREATED`).
    pub announce: Option<String>,
}

/// Start failures.
#[derive(Debug, thiserror::Error)]
pub enum CompatError {
    /// A window of this class already exists in another process (real Everything).
    #[error("another process (pid {0}) owns the Everything IPC window")]
    Conflict(u32),
    /// A Win32 call failed.
    #[error("cannot create the Everything IPC window: {0}")]
    Os(String),
    /// Not on Windows.
    #[error("the Everything IPC window exists on Windows only")]
    Unsupported,
}

/// A running compatibility window; dropping it closes the window and joins its thread.
pub struct CompatWindow {
    #[cfg_attr(not(windows), allow(dead_code))]
    hwnd: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CompatWindow {
    /// Creates the window on a new thread (`fennec-compat`). Fails with
    /// [`CompatError::Conflict`] if another process already has a window of the class.
    pub fn start(config: &CompatConfig, handler: Arc<dyn CompatHandler>) -> Result<Self, CompatError> {
        imp::start(config, handler)
    }

    /// Closes the window and waits for its thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Window handle.
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    fn shutdown(&mut self) {
        if let Some(t) = self.thread.take() {
            imp::close(self.hwnd);
            let _ = t.join();
        }
    }
}

impl Drop for CompatWindow {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Process id owning a top-level window of `class`, if any.
pub fn window_owner(class: &str) -> Option<u32> {
    imp::window_owner(class)
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::sync::Arc;

    use crossbeam_channel::{Receiver, Sender};
    use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW,
        GetMessageW, GetWindowThreadProcessId, HWND_BROADCAST, MSG, MSGFLT_ALLOW, PostMessageW, PostQuitMessage,
        RegisterClassW, RegisterWindowMessageW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, TranslateMessage,
        UnregisterClassW, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COPYDATA, WM_DESTROY, WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::core::PCWSTR;

    use super::{CompatConfig, CompatError, CompatHandler, CompatWindow, CopyDataAction, QueryJob};

    /// Reply time limit (Everything clients wait synchronously).
    const REPLY_TIMEOUT_MS: u32 = 10_000;

    /// Private message that destroys the window ([`CompatWindow::stop`]); `WM_CLOSE`
    /// belongs to clients.
    const WM_STOP: u32 = WM_APP + 0x46;

    struct State {
        handler: Arc<dyn CompatHandler>,
        ipc_message: u32,
        jobs: Sender<(QueryJob, isize)>,
    }

    thread_local! {
        /// Handler of the window owned by this thread (the window procedure always runs
        /// on the creating thread).
        static STATE: RefCell<Option<Arc<State>>> = const { RefCell::new(None) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn window_owner(class: &str) -> Option<u32> {
        let class = wide(class);
        // SAFETY: `class` is NUL-terminated and outlives the call; no title filter.
        let hwnd = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }.ok()?;
        let mut pid = 0u32;
        // SAFETY: `hwnd` came from FindWindowW (a stale handle only makes the call fail);
        // `pid` is a valid out-pointer.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        (pid != 0).then_some(pid)
    }

    pub fn close(hwnd: isize) {
        // SAFETY: posting to a window handle is harmless even if the window is gone.
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_STOP, WPARAM(0), LPARAM(0)) };
    }

    pub fn start(config: &CompatConfig, handler: Arc<dyn CompatHandler>) -> Result<CompatWindow, CompatError> {
        if let Some(pid) = window_owner(&config.class).filter(|&p| p != std::process::id()) {
            return Err(CompatError::Conflict(pid));
        }
        let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<(QueryJob, isize)>();
        std::thread::Builder::new()
            .name("fennec-compat-query".into())
            .spawn(move || run_jobs(jobs_rx))
            .map_err(|e| CompatError::Os(e.to_string()))?;
        let state = Arc::new(State { handler, ipc_message: config.ipc_message, jobs: jobs_tx });
        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<isize, CompatError>>(1);
        let config = config.clone();
        let thread = std::thread::Builder::new()
            .name("fennec-compat".into())
            .spawn(move || window_thread(&config, state, &ready_tx))
            .map_err(|e| CompatError::Os(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(hwnd)) => Ok(CompatWindow { hwnd, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(CompatError::Os("window thread ended".into()))
            }
        }
    }

    fn run_jobs(rx: Receiver<(QueryJob, isize)>) {
        for (job, own) in rx {
            let Ok(Some(reply)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) else { continue };
            let cds = COPYDATASTRUCT {
                dwData: reply.copydata_id,
                cbData: u32::try_from(reply.bytes.len()).unwrap_or(u32::MAX),
                lpData: reply.bytes.as_ptr() as *mut _,
            };
            let mut result = 0usize;
            // SAFETY: `cds` and the bytes it points to outlive this synchronous send; the
            // receiving window copies the data before SendMessageTimeoutW returns. A stale
            // client handle only makes the call fail.
            let sent = unsafe {
                SendMessageTimeoutW(
                    HWND(reply.hwnd as *mut _),
                    WM_COPYDATA,
                    WPARAM(own as usize),
                    LPARAM(&raw const cds as isize),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    REPLY_TIMEOUT_MS,
                    Some(&mut result),
                )
            };
            if sent.0 == 0 {
                tracing::debug!(client = reply.hwnd, "Everything IPC reply not delivered");
            }
        }
    }

    fn window_thread(config: &CompatConfig, state: Arc<State>, ready: &Sender<Result<isize, CompatError>>) {
        STATE.with(|s| *s.borrow_mut() = Some(state));
        let class = wide(&config.class);
        let os = |what: &str| CompatError::Os(format!("{what}: {}", std::io::Error::last_os_error()));
        // SAFETY: class registration and window creation with a NUL-terminated class name
        // that outlives every call below; the message loop runs on this (creating) thread
        // and ends after WM_DESTROY; the class is unregistered after the window is gone.
        unsafe {
            let Ok(hinstance) = GetModuleHandleW(None) else {
                let _ = ready.send(Err(os("GetModuleHandleW")));
                return;
            };
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                let _ = ready.send(Err(os("RegisterClassW")));
                return;
            }
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(class.as_ptr()),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance.into()),
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(hinstance.into()));
                    let _ = ready.send(Err(CompatError::Os(format!("CreateWindowExW: {e}"))));
                    return;
                }
            };
            // Non-elevated clients may talk to an elevated Fennec.
            let ipc = STATE.with(|s| s.borrow().as_ref().map_or(0, |s| s.ipc_message));
            let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
            let _ = ChangeWindowMessageFilterEx(hwnd, ipc, MSGFLT_ALLOW, None);
            let _ = ready.send(Ok(hwnd.0 as isize));
            tracing::info!(class = %config.class, "Everything IPC window created");
            if let Some(name) = &config.announce {
                let name = wide(name);
                let msg = RegisterWindowMessageW(PCWSTR(name.as_ptr()));
                if msg != 0 {
                    let _ = PostMessageW(Some(HWND_BROADCAST), msg, WPARAM(0), LPARAM(0));
                }
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(hinstance.into()));
        }
        // Dropping the state closes the job channel: the query thread ends.
        STATE.with(|s| s.borrow_mut().take());
        tracing::info!(class = %config.class, "Everything IPC window closed");
    }

    /// Copies the `COPYDATASTRUCT` at `lparam`.
    ///
    /// # Safety
    /// `lparam` must be the `lParam` of a `WM_COPYDATA` being handled (valid for the
    /// duration of the window procedure call).
    unsafe fn read_copydata(lparam: LPARAM) -> Option<(usize, Vec<u8>)> {
        if lparam.0 == 0 {
            return None;
        }
        // SAFETY: per the contract, lParam points to a COPYDATASTRUCT whose lpData refers
        // to cbData readable bytes until the window procedure returns; the bytes are copied.
        unsafe {
            let cds = &*(lparam.0 as *const COPYDATASTRUCT);
            let bytes = if cds.lpData.is_null() || cds.cbData == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize).to_vec()
            };
            Some((cds.dwData, bytes))
        }
    }

    /// # Safety
    /// Window procedure: only called by Windows for windows of this class, with the
    /// message parameters the system defines (`WM_COPYDATA` `lparam` = a valid `COPYDATASTRUCT`).
    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        if msg == WM_STOP {
            // SAFETY: `hwnd` is this thread's window.
            let _ = unsafe { DestroyWindow(hwnd) };
            return LRESULT(0);
        }
        let state = STATE.with(|s| s.borrow().clone());
        if let Some(st) = state {
            if msg == WM_CLOSE {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| st.handler.close()));
                return LRESULT(0);
            }
            if msg == st.ipc_message {
                let r =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| st.handler.wm_ipc(wparam.0, lparam.0)));
                return LRESULT(r.unwrap_or(0));
            }
            if msg == WM_COPYDATA {
                // SAFETY: this is the lParam of the WM_COPYDATA being handled.
                let Some((id, bytes)) = (unsafe { read_copydata(lparam) }) else { return LRESULT(0) };
                let action = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| st.handler.copydata(id, bytes)));
                return match action {
                    Ok(CopyDataAction::Done(r)) => LRESULT(r),
                    Ok(CopyDataAction::Query(job)) => {
                        LRESULT(isize::from(st.jobs.send((job, hwnd.0 as isize)).is_ok()))
                    }
                    Err(_) => LRESULT(0),
                };
            }
        }
        // SAFETY: default processing of a message for this window.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::Arc;

    use super::{CompatConfig, CompatError, CompatHandler, CompatWindow};

    pub fn window_owner(_class: &str) -> Option<u32> {
        None
    }

    pub fn close(_hwnd: isize) {}

    pub fn start(_config: &CompatConfig, _handler: Arc<dyn CompatHandler>) -> Result<CompatWindow, CompatError> {
        Err(CompatError::Unsupported)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageW, WM_CLOSE, WM_COPYDATA};
    use windows::core::PCWSTR;

    use super::*;

    struct Echo(AtomicUsize, AtomicUsize);

    impl CompatHandler for Echo {
        fn wm_ipc(&self, wparam: usize, lparam: isize) -> isize {
            self.0.fetch_add(1, Ordering::SeqCst);
            wparam as isize + lparam
        }
        fn copydata(&self, dw_data: usize, bytes: Vec<u8>) -> CopyDataAction {
            CopyDataAction::Done((dw_data + bytes.len()) as isize)
        }
        fn close(&self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn window_routes_messages_and_stops() {
        let class = format!("FENNEC_COMPAT_TEST_{}", std::process::id());
        let cfg = CompatConfig { class: class.clone(), ipc_message: 0x400, announce: None };
        let h = Arc::new(Echo(AtomicUsize::new(0), AtomicUsize::new(0)));
        let w = CompatWindow::start(&cfg, h.clone()).unwrap();
        assert_eq!(window_owner(&class), Some(std::process::id()));
        let wide: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
        // SAFETY (test): plain Win32 calls on a window of this process.
        unsafe {
            let hwnd = FindWindowW(PCWSTR(wide.as_ptr()), PCWSTR::null()).unwrap();
            assert_eq!(SendMessageW(hwnd, 0x400, Some(WPARAM(2)), Some(LPARAM(40))).0, 42);
            let data = [1u8, 2, 3];
            let cds = COPYDATASTRUCT { dwData: 7, cbData: 3, lpData: data.as_ptr() as *mut _ };
            let r = SendMessageW(hwnd, WM_COPYDATA, Some(WPARAM(0)), Some(LPARAM(&raw const cds as isize)));
            assert_eq!(r.0, 10);
            // A client's WM_CLOSE asks the app to exit; the window stays.
            SendMessageW(hwnd, WM_CLOSE, None, None);
        }
        assert_eq!(h.0.load(Ordering::SeqCst), 1);
        assert_eq!(h.1.load(Ordering::SeqCst), 1);
        assert_eq!(window_owner(&class), Some(std::process::id()));
        w.stop();
        assert_eq!(window_owner(&class), None);
        // Restartable after a stop (class unregistered).
        let w = CompatWindow::start(&cfg, h).unwrap();
        drop(w);
    }
}

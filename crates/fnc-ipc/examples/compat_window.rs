//! Everything 1.4 IPC window over a file-list index: manual test harness for official
//! `es.exe`/SDK clients and the reference for the app's Win32 plumbing.
//!
//! ```text
//! cargo run -p fnc-ipc --example compat_window -- <instance> [index.efu]
//! es.exe -instance <instance> -n 10 abc
//! es.exe -instance <instance> -get-everything-version
//! ```
//!
//! The window class is `EVERYTHING_TASKBAR_NOTIFICATION_(<instance>)` (`""` = the default
//! class; do not use that while real Everything runs). Windows only.

// Win32 window procedure and message loop.
#![allow(unsafe_code)]

#[cfg(windows)]
mod win {
    use std::sync::{Arc, OnceLock};

    use fnc_index::{IndexStore, SharedIndex};
    use fnc_ipc::backend::{DefaultHost, IndexBackend};
    use fnc_ipc::compat;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, MSGFLT_ALLOW,
        RegisterClassW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, TranslateMessage, WINDOW_EX_STYLE,
        WM_COPYDATA, WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::core::PCWSTR;

    static BACKEND: OnceLock<Arc<IndexBackend>> = OnceLock::new();

    const FIXTURE: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
        \"C:\\Fennec Test\",,133000000000000000,,16\n\
        \"C:\\Fennec Test\\abc.txt\",11,133000000000000001,132000000000000000,32\n\
        \"C:\\Fennec Test\\abc report.pdf\",2048,133000000000000002,132000000000000000,33\n\
        \"C:\\Fennec Test\\test one.iso\",700000000,133000000000000003,,32\n\
        \"C:\\Fennec Test\\test two.iso\",900000000,133000000000000004,,32\n\
        \"C:\\Fennec Test\\sub\",,133000000000000005,,16\n\
        \"C:\\Fennec Test\\sub\\测试 abc.txt\",5,133000000000000006,,32\n";

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let Some(backend) = BACKEND.get() else {
            // SAFETY: plain forwarding of the window message.
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        };
        if msg == compat::EVERYTHING_WM_IPC {
            return LRESULT(compat::handle_wm_ipc(wparam.0, lparam.0, backend));
        }
        if msg == WM_COPYDATA && lparam.0 != 0 {
            // SAFETY: for WM_COPYDATA, lParam points to a COPYDATASTRUCT that is valid for
            // the duration of this call; lpData points to cbData readable bytes.
            let bytes = unsafe {
                let cds = &*(lparam.0 as *const COPYDATASTRUCT);
                if cds.lpData.is_null() || cds.cbData == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize).to_vec()
                }
            };
            // SAFETY: as above; only dwData is read.
            let id = unsafe { (*(lparam.0 as *const COPYDATASTRUCT)).dwData };
            let Ok(req) = compat::decode_copydata(id, &bytes) else { return LRESULT(0) };
            let out = compat::execute_copydata(&req, backend);
            if let Some(r) = out.reply {
                let reply = COPYDATASTRUCT {
                    dwData: r.copydata_id,
                    cbData: r.bytes.len() as u32,
                    lpData: r.bytes.as_ptr() as *mut _,
                };
                // SAFETY: `reply` and its buffer outlive the synchronous send; the target
                // window copies the data before returning.
                unsafe {
                    SendMessageTimeoutW(
                        HWND(r.hwnd as *mut _),
                        WM_COPYDATA,
                        WPARAM(hwnd.0 as usize),
                        LPARAM(&reply as *const COPYDATASTRUCT as isize),
                        SMTO_ABORTIFHUNG | SMTO_BLOCK,
                        10_000,
                        None,
                    );
                }
            }
            return LRESULT(out.result);
        }
        // SAFETY: plain forwarding of the window message.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub fn main() -> Result<(), Box<dyn std::error::Error>> {
        let mut args = std::env::args().skip(1);
        let instance = args.next().unwrap_or_else(|| "fennec-test".into());
        let mut store = IndexStore::default();
        match args.next() {
            Some(efu) => {
                fnc_index::efu::load_efu_file(&mut store, std::path::Path::new(&efu))?;
            }
            None => {
                fnc_index::efu::load_efu(&mut store, FIXTURE.as_bytes(), "fixture.efu")?;
            }
        }
        store.rebuild_name_order();
        let backend = Arc::new(IndexBackend::new(SharedIndex::new(store), Arc::new(DefaultHost)));
        let _ = BACKEND.set(backend);
        let class: Vec<u16> = compat::window_class(&instance).encode_utf16().chain(Some(0)).collect();
        // SAFETY: standard window class registration and creation with valid,
        // NUL-terminated class names that outlive the calls; the message loop runs on the
        // creating thread.
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                return Err("RegisterClassW failed".into());
            }
            let hwnd = CreateWindowExW(
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
            )?;
            // Let non-elevated clients reach an elevated window.
            let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
            let _ = ChangeWindowMessageFilterEx(hwnd, compat::EVERYTHING_WM_IPC, MSGFLT_ALLOW, None);
            eprintln!("compat window '{}' ready", compat::window_class(&instance));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    win::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("compat_window is Windows only");
}

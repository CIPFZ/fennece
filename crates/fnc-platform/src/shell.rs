//! Shell actions: open a file, reveal it in the file manager.
//!
//! Windows uses `ShellExecuteW` and `explorer /select,`; other platforms use
//! `xdg-open` / `open`. Copy/rename/delete through `IFileOperation` and the shell
//! context menu are M2 (SPEC ch.3 §4.2).

use std::io;
use std::path::Path;
use std::process::Command;

/// Opens `path` with its associated application (Enter / double-click).
pub fn open(path: &Path) -> io::Result<()> {
    imp::open(path)
}

/// Opens the folder containing `path` and selects it (Ctrl+Enter).
pub fn reveal(path: &Path) -> io::Result<()> {
    imp::reveal(path)
}

/// Opens a folder.
pub fn open_folder(path: &Path) -> io::Result<()> {
    imp::open(path)
}

fn spawn(mut c: Command) -> io::Result<()> {
    c.spawn().map(|_| ())
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn open(path: &Path) -> io::Result<()> {
        let file = wide(path.as_os_str());
        let verb = wide(std::ffi::OsStr::new("open"));
        // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call;
        // the remaining pointer arguments are null (optional) as documented.
        let r = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(file.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecute returns a value > 32 on success.
        if r.0 as usize > 32 {
            Ok(())
        } else {
            Err(io::Error::other(format!("ShellExecuteW failed ({})", r.0 as usize)))
        }
    }

    pub fn reveal(path: &Path) -> io::Result<()> {
        // `explorer /select,"C:\a b\c"` needs the quotes verbatim, so pass a raw arg.
        let mut c = Command::new("explorer.exe");
        c.raw_arg(format!("/select,\"{}\"", path.display()));
        spawn(c)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub fn open(path: &Path) -> io::Result<()> {
        let mut c = Command::new("open");
        c.arg(path);
        spawn(c)
    }

    pub fn reveal(path: &Path) -> io::Result<()> {
        let mut c = Command::new("open");
        c.arg("-R").arg(path);
        spawn(c)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::*;

    pub fn open(path: &Path) -> io::Result<()> {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        spawn(c)
    }

    pub fn reveal(path: &Path) -> io::Result<()> {
        // SPEC-GAP: selecting the item needs the FileManager1 D-Bus interface; open the
        // parent folder instead.
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        open(dir)
    }
}

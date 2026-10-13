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

/// Type description of files with extension `ext` (no dot; empty = none) or of a
/// folder, as Explorer shows it ("Text Document"); `None` where the OS has none.
pub fn type_name(ext: &str, is_dir: bool) -> Option<String> {
    imp::type_name(ext, is_dir)
}

/// Mouse pointer position in screen pixels (Windows), for native popup menus.
pub fn cursor_pos() -> Option<(i32, i32)> {
    imp::cursor_pos()
}

/// Starts `exe args` with administrator rights (UAC prompt on Windows); does not wait.
pub fn run_elevated(exe: &Path, args: &str) -> io::Result<()> {
    imp::run_elevated(exe, args)
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

    pub fn cursor_pos() -> Option<(i32, i32)> {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT::default();
        // SAFETY: `p` is a valid out-parameter.
        unsafe { GetCursorPos(&mut p) }.ok().map(|()| (p.x, p.y))
    }

    pub fn type_name(ext: &str, is_dir: bool) -> Option<String> {
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
        use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW};
        // With SHGFI_USEFILEATTRIBUTES the name need not exist; only its extension counts.
        let name = if is_dir { "folder".to_string() } else { format!("file.{ext}") };
        let w = wide(std::ffi::OsStr::new(&name));
        let attrs = if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
        let mut info = SHFILEINFOW::default();
        // SAFETY: `w` is NUL-terminated; `info` is an owned out-structure of the size passed.
        let r = unsafe {
            SHGetFileInfoW(
                PCWSTR(w.as_ptr()),
                attrs,
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
            )
        };
        let n = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(0);
        (r != 0 && n > 0).then(|| String::from_utf16_lossy(&info.szTypeName[..n]))
    }

    pub fn run_elevated(exe: &Path, args: &str) -> io::Result<()> {
        let file = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(args));
        let verb = wide(std::ffi::OsStr::new("runas"));
        // SAFETY: all strings are NUL-terminated UTF-16 buffers that outlive the call.
        let r = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(file.as_ptr()),
                PCWSTR(params.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if r.0 as usize > 32 {
            Ok(())
        } else {
            Err(io::Error::other(format!("ShellExecuteW(runas) failed ({})", r.0 as usize)))
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

    pub fn type_name(_ext: &str, _is_dir: bool) -> Option<String> {
        None
    }

    pub fn cursor_pos() -> Option<(i32, i32)> {
        None
    }

    pub fn run_elevated(exe: &Path, args: &str) -> io::Result<()> {
        let _ = (exe, args);
        Err(io::Error::new(io::ErrorKind::Unsupported, "elevation is only supported on Windows"))
    }

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

    pub fn type_name(_ext: &str, _is_dir: bool) -> Option<String> {
        None
    }

    pub fn cursor_pos() -> Option<(i32, i32)> {
        None
    }

    pub fn run_elevated(exe: &Path, args: &str) -> io::Result<()> {
        let _ = (exe, args);
        Err(io::Error::new(io::ErrorKind::Unsupported, "elevation is only supported on Windows"))
    }

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

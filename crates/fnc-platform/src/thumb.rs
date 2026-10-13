//! Shell images: file icons and thumbnails (SPEC ch.3 §4.1, §5). On Windows
//! `IShellItemImageFactory` renders the icon or thumbnail any installed handler
//! provides (pictures, videos, PDF, Office, …); other platforms report `Unsupported`
//! and the app falls back to decoding images itself.

use std::io;
use std::path::Path;

/// An RGBA image (premultiplied alpha, row-major, no padding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// What to render.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ShellImage {
    /// The file's icon (fast; the same for all files of a type, except `.exe`/`.lnk`/`.ico`).
    Icon,
    /// A thumbnail if a handler exists, otherwise the icon.
    Thumbnail,
}

/// Renders `path` at `size` × `size` pixels (fits inside, keeps the aspect ratio).
/// Call from worker threads; each initialises COM for itself.
pub fn shell_image(path: &Path, size: u32, kind: ShellImage) -> io::Result<Rgba> {
    imp::shell_image(path, size.clamp(8, 1024), kind)
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
        GetDIBits, GetObjectW, HGDIOBJ,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT,
    };
    use windows::core::PCWSTR;

    use super::*;

    thread_local! {
        // COM stays initialised for the life of the worker thread.
        static COM: () = {
            // SAFETY: initialises COM for this thread; never uninitialised (thread-lifetime).
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        };
    }

    pub fn shell_image(path: &Path, size: u32, kind: ShellImage) -> io::Result<Rgba> {
        COM.with(|()| {});
        let w: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: `w` is NUL-terminated and outlives the call.
        let factory: IShellItemImageFactory =
            unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) }.map_err(io::Error::other)?;
        let flags = match kind {
            ShellImage::Icon => SIIGBF_ICONONLY,
            ShellImage::Thumbnail => SIIGBF_RESIZETOFIT | SIIGBF_BIGGERSIZEOK,
        };
        let px = size as i32;
        // SAFETY: valid factory; the returned HBITMAP is owned here and deleted below.
        let hbm = unsafe { factory.GetImage(SIZE { cx: px, cy: px }, flags) }.map_err(io::Error::other)?;
        let result = (|| {
            let mut bm = BITMAP::default();
            // SAFETY: `bm` is a valid out-structure of the size passed; `hbm` is a bitmap.
            let n = unsafe {
                GetObjectW(
                    HGDIOBJ(hbm.0),
                    std::mem::size_of::<BITMAP>() as i32,
                    Some(std::ptr::from_mut(&mut bm).cast()),
                )
            };
            if n == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
                return Err(io::Error::other("empty shell image"));
            }
            let (width, height) = (bm.bmWidth as u32, bm.bmHeight.unsigned_abs());
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    // Negative height: top-down rows.
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = vec![0u8; width as usize * height as usize * 4];
            // SAFETY: a memory DC for GetDIBits; deleted below.
            let dc = unsafe { CreateCompatibleDC(None) };
            // SAFETY: `pixels` holds exactly width*height 32-bit pixels as described by
            // `info`; `dc` and `hbm` are valid.
            let lines =
                unsafe { GetDIBits(dc, hbm, 0, height, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS) };
            // SAFETY: deletes the DC created above.
            let _ = unsafe { DeleteDC(dc) };
            if lines == 0 {
                return Err(io::Error::other("GetDIBits failed"));
            }
            // BGRA → RGBA. Bitmaps without alpha (all zero) are opaque.
            let has_alpha = pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
            for p in pixels.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
                if !has_alpha {
                    p[3] = 255;
                }
            }
            Ok(Rgba { width, height, pixels })
        })();
        // SAFETY: the bitmap returned by GetImage is owned by the caller.
        let _ = unsafe { DeleteObject(HGDIOBJ(hbm.0)) };
        result
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn shell_image(_path: &Path, _size: u32, _kind: ShellImage) -> io::Result<Rgba> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "shell images are only available on Windows"))
    }
}

//! File operations and shell integration for the result list (SPEC ch.3 §4.2, §4.4):
//! the Explorer context menu, `CF_HDROP` clipboard, dragging files out, and
//! copy/move/rename/delete through `IFileOperation` (undoable, recycle bin; AGENT.md
//! iron rule 6).
//!
//! Other platforms get plain `std::fs` equivalents where they exist (trash via the
//! freedesktop trash folder) and `Unsupported` for the rest.

use std::io;
use std::path::{Path, PathBuf};

/// Native owner window (`HWND` on Windows, 0 = none).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Owner(pub isize);

/// An application command added to the shell context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// Returned in [`MenuOutcome::App`] when chosen.
    pub id: u32,
    /// Label (`&` marks the access key).
    pub label: String,
    /// Draw a separator after this item.
    pub separator_after: bool,
    /// Greyed out.
    pub disabled: bool,
}

/// What the user chose in [`context_menu`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuOutcome {
    /// Menu dismissed.
    None,
    /// A shell verb was invoked (`open`, `properties`, a third-party command, ...).
    Invoked(String),
    /// The shell `rename` verb: the caller renames in place.
    Rename,
    /// An application item.
    App(u32),
}

/// What a drag ended with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DragResult {
    /// Dropped and copied/linked.
    Copied,
    /// Dropped and moved.
    Moved,
    /// Cancelled or not accepted.
    Cancelled,
}

/// Shows the shell context menu for `paths` (all in one folder) at screen position
/// `at`, with `extra` application items on top. Modal (runs the menu loop).
pub fn context_menu(owner: Owner, paths: &[PathBuf], at: (i32, i32), extra: &[MenuItem]) -> io::Result<MenuOutcome> {
    imp::context_menu(owner, paths, at, extra)
}

/// Puts `paths` on the clipboard as files (`CF_HDROP`); `cut` marks them for moving.
pub fn set_clipboard_files(paths: &[PathBuf], cut: bool) -> io::Result<()> {
    imp::set_clipboard_files(paths, cut)
}

/// Pastes files from the clipboard into `dest` (copy, or move after a cut).
pub fn paste_files(owner: Owner, dest: &Path) -> io::Result<()> {
    imp::paste_files(owner, dest)
}

/// Starts dragging `paths` out of the window (call while the mouse button is down).
/// Modal until the drop.
pub fn drag_files(paths: &[PathBuf]) -> io::Result<DragResult> {
    imp::drag_files(paths)
}

/// Deletes `paths`: to the recycle bin unless `permanent` (callers confirm permanent
/// deletion themselves). Returns `false` if the user cancelled a shell prompt.
pub fn delete(owner: Owner, paths: &[PathBuf], permanent: bool) -> io::Result<bool> {
    if paths.is_empty() {
        return Ok(true);
    }
    imp::delete(owner, paths, permanent)
}

/// An item moved to the recycle bin: where it was and where the bin keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recycled {
    /// Path before deletion.
    pub original: PathBuf,
    /// The item inside the recycle bin (Windows: `…\$Recycle.Bin\<SID>\$R…`).
    pub bin_item: PathBuf,
}

/// Moves `paths` to the recycle bin and reports where each went (for undo). Items the
/// shell did not recycle (cancelled, too large) are missing from the result.
pub fn delete_to_recycle_bin(owner: Owner, paths: &[PathBuf]) -> io::Result<Vec<Recycled>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    imp::delete_to_recycle_bin(owner, paths)
}

/// Restores recycled items to their original places (undo of a delete).
pub fn restore_from_recycle_bin(owner: Owner, items: &[Recycled]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    imp::restore_from_recycle_bin(owner, items)
}

/// Renames `path` to `new_name` (a name, not a path).
pub fn rename(owner: Owner, path: &Path, new_name: &str) -> io::Result<()> {
    if new_name.is_empty() || new_name.contains(['\\', '/']) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"));
    }
    imp::rename(owner, path, new_name)
}

/// Copies (`move_items = false`) or moves `paths` into folder `dest`.
pub fn transfer(owner: Owner, paths: &[PathBuf], dest: &Path, move_items: bool) -> io::Result<()> {
    imp::transfer(owner, paths, dest, move_items)
}

/// Creates folder `name` in `parent`.
pub fn new_folder(owner: Owner, parent: &Path, name: &str) -> io::Result<()> {
    imp::new_folder(owner, parent, name)
}

/// Shows the properties dialog of `path`.
pub fn properties(owner: Owner, path: &Path) -> io::Result<()> {
    imp::properties(owner, path)
}

/// Shows the "Open with" dialog for `path`.
pub fn open_with(owner: Owner, path: &Path) -> io::Result<()> {
    imp::open_with(owner, path)
}

/// Opens `path` with administrator rights.
pub fn run_as_admin(path: &Path) -> io::Result<()> {
    crate::shell::run_elevated(path, "")
}

#[cfg(windows)]
#[allow(non_snake_case)]
mod imp {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Foundation::{
        DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HGLOBAL, HWND, LPARAM, LRESULT, POINT,
        RPC_E_CHANGED_MODE, S_OK, WPARAM,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, DVASPECT_CONTENT,
        FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
    };
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::{
        DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DoDragDrop, IDropSource, IDropSource_Impl,
        OleFlushClipboard, OleGetClipboard, OleSetClipboard, ReleaseStgMedium,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        BHID_DataObject, BHID_SFUIObject, CMF_CANRENAME, CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO,
        CMINVOKECOMMANDINFOEX, DefSubclassProc, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR,
        FOF_RENAMEONCOLLISION, FOFX_ADDUNDORECORD, FOFX_RECYCLEONDELETE, FileOperation, GCS_VERBW, IContextMenu,
        IContextMenu2, IContextMenu3, IFileOperation, ILFree, IShellItem, IShellItemArray, OAIF_EXEC,
        OAIF_HIDE_REGISTRATION, OPENASINFO, RemoveWindowSubclass, SHCreateItemFromParsingName,
        SHCreateShellItemArrayFromIDLists, SHOP_FILEPATH, SHObjectProperties, SHOpenWithDialog, SHParseDisplayName,
        SetWindowSubclass,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, MF_GRAYED, MF_SEPARATOR, MF_STRING, SW_SHOWNORMAL,
        SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM, WM_INITMENUPOPUP,
        WM_MEASUREITEM, WM_MENUCHAR,
    };
    use windows::core::{BOOL, HRESULT, Interface, PCSTR, PCWSTR, PSTR, implement};

    use super::*;

    const CMIC_MASK_UNICODE: u32 = 0x4000;
    /// Shell command ids are `1..APP_BASE`; application items start at `APP_BASE`.
    const APP_BASE: u32 = 0x8000;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn hwnd(o: Owner) -> HWND {
        HWND(o.0 as *mut core::ffi::c_void)
    }

    fn to_io(e: windows::core::Error) -> io::Error {
        io::Error::other(e)
    }

    /// COM for the calling thread (the UI thread is normally already an OLE STA).
    struct Com(bool);

    impl Com {
        fn init() -> Self {
            // SAFETY: plain COM initialisation of the current thread; balanced in `Drop`
            // when it succeeded (S_OK / S_FALSE), skipped if another mode is active.
            let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            Com(hr.is_ok() && hr != RPC_E_CHANGED_MODE)
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances the successful CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Absolute PIDL, freed on drop.
    struct Pidl(*mut ITEMIDLIST);

    impl Drop for Pidl {
        fn drop(&mut self) {
            // SAFETY: allocated by SHParseDisplayName, freed exactly once.
            unsafe { ILFree(Some(self.0)) };
        }
    }

    fn item_array(paths: &[PathBuf]) -> io::Result<IShellItemArray> {
        let mut pidls = Vec::with_capacity(paths.len());
        for p in paths {
            let w = wide(p.as_os_str());
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            // SAFETY: `w` is NUL-terminated and outlives the call; `pidl` receives an
            // owned PIDL (wrapped immediately).
            unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }.map_err(to_io)?;
            pidls.push(Pidl(pidl));
        }
        let ptrs: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| p.0.cast_const()).collect();
        // SAFETY: every pointer is a valid absolute PIDL alive until `pidls` drops
        // (after the call; the array keeps its own copies).
        unsafe { SHCreateShellItemArrayFromIDLists(&ptrs) }.map_err(to_io)
    }

    fn shell_item(path: &Path) -> io::Result<IShellItem> {
        let w = wide(path.as_os_str());
        // SAFETY: `w` is NUL-terminated and outlives the call.
        unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) }.map_err(to_io)
    }

    /// Holds the menu interfaces for the subclass procedure during `TrackPopupMenuEx`.
    struct MenuHost {
        cm2: Option<IContextMenu2>,
        cm3: Option<IContextMenu3>,
    }

    /// Forwards owner-draw and submenu-initialisation messages to the shell menu, so
    /// "Send to", "Open with" and third-party submenus fill in (IContextMenu2/3).
    unsafe extern "system" fn menu_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
            // SAFETY: `data` is the address of the `MenuHost` on `context_menu`'s stack,
            // which outlives the subclass (removed before it returns).
            let host = unsafe { &*(data as *const MenuHost) };
            let mut result = LRESULT(0);
            if let Some(cm3) = &host.cm3 {
                // SAFETY: valid interface; `result` is a live out-parameter.
                if unsafe { cm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)) }.is_ok() {
                    return result;
                }
            } else if let Some(cm2) = &host.cm2 {
                // SAFETY: valid interface.
                if unsafe { cm2.HandleMenuMsg(msg, wparam, lparam) }.is_ok() {
                    return LRESULT(0);
                }
            }
        }
        // SAFETY: standard chaining to the original window procedure.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    pub fn context_menu(
        owner: Owner,
        paths: &[PathBuf],
        at: (i32, i32),
        extra: &[MenuItem],
    ) -> io::Result<MenuOutcome> {
        let _com = Com::init();
        let items = item_array(paths)?;
        // SAFETY: valid array; requests the folder's context menu handler for it.
        let cm: IContextMenu = unsafe { items.BindToHandler(None, &BHID_SFUIObject) }.map_err(to_io)?;
        // SAFETY: creates an empty popup menu owned here (destroyed below).
        let menu = unsafe { CreatePopupMenu() }.map_err(to_io)?;
        let result = (|| {
            for e in extra {
                let label = wide(std::ffi::OsStr::new(&e.label));
                let flags = if e.disabled { MF_STRING | MF_GRAYED } else { MF_STRING };
                // SAFETY: `menu` is a valid menu; `label` outlives the call.
                unsafe { AppendMenuW(menu, flags, (APP_BASE + e.id) as usize, PCWSTR(label.as_ptr())) }
                    .map_err(to_io)?;
                if e.separator_after {
                    // SAFETY: as above.
                    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) }.map_err(to_io)?;
                }
            }
            let index = extra.len() as u32 + extra.iter().filter(|e| e.separator_after).count() as u32;
            // SAFETY: the shell appends its items after ours, ids in 1..APP_BASE.
            unsafe { cm.QueryContextMenu(menu, index, 1, APP_BASE - 1, CMF_NORMAL | CMF_CANRENAME) }
                .ok()
                .map_err(to_io)?;
            let host = MenuHost { cm2: cm.cast().ok(), cm3: cm.cast().ok() };
            let owner_hwnd = hwnd(owner);
            // SAFETY: `host` outlives the subclass, which is removed right after the menu
            // loop; `menu_proc` matches SUBCLASSPROC.
            let subclassed =
                unsafe { SetWindowSubclass(owner_hwnd, Some(menu_proc), 1, std::ptr::from_ref(&host) as usize) }
                    .as_bool();
            // SAFETY: the owner must be foreground for the menu to dismiss correctly.
            let _ = unsafe { SetForegroundWindow(owner_hwnd) };
            // SAFETY: valid menu and owner; TPM_RETURNCMD returns the chosen id.
            let cmd =
                unsafe { TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, at.0, at.1, owner_hwnd, None) }.0
                    as u32;
            if subclassed {
                // SAFETY: removes the subclass installed above.
                let _ = unsafe { RemoveWindowSubclass(owner_hwnd, Some(menu_proc), 1) };
            }
            if cmd == 0 {
                return Ok(MenuOutcome::None);
            }
            if cmd >= APP_BASE {
                return Ok(MenuOutcome::App(cmd - APP_BASE));
            }
            let offset = (cmd - 1) as usize;
            let mut verb = [0u16; 128];
            // SAFETY: GCS_VERBW writes at most `cchmax` UTF-16 units into `verb`.
            let verb =
                match unsafe { cm.GetCommandString(offset, GCS_VERBW, None, PSTR(verb.as_mut_ptr().cast()), 128) } {
                    Ok(()) => String::from_utf16_lossy(&verb[..verb.iter().position(|&c| c == 0).unwrap_or(0)]),
                    Err(_) => String::new(),
                };
            // The shell's rename only works inside Explorer views.
            if verb.eq_ignore_ascii_case("rename") {
                return Ok(MenuOutcome::Rename);
            }
            let info = CMINVOKECOMMANDINFOEX {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE,
                hwnd: owner_hwnd,
                // MAKEINTRESOURCE(offset).
                lpVerb: PCSTR(offset as *const u8),
                lpVerbW: PCWSTR(offset as *const u16),
                nShow: SW_SHOWNORMAL.0,
                ptInvoke: POINT { x: at.0, y: at.1 },
                ..Default::default()
            };
            // SAFETY: `info` is a fully initialised CMINVOKECOMMANDINFOEX whose size is set,
            // so the shell reads it as such.
            unsafe { cm.InvokeCommand(std::ptr::from_ref(&info).cast::<CMINVOKECOMMANDINFO>()) }.map_err(to_io)?;
            Ok(MenuOutcome::Invoked(verb))
        })();
        // SAFETY: destroys the menu created above.
        let _ = unsafe { DestroyMenu(menu) };
        result
    }

    fn data_object(paths: &[PathBuf]) -> io::Result<IDataObject> {
        let items = item_array(paths)?;
        // SAFETY: valid array; asks the shell for its IDataObject (CF_HDROP, shell IDs).
        unsafe { items.BindToHandler(None, &BHID_DataObject) }.map_err(to_io)
    }

    fn preferred_effect_format() -> u16 {
        let name = wide(std::ffi::OsStr::new("Preferred DropEffect"));
        // SAFETY: `name` is NUL-terminated.
        unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) as u16 }
    }

    fn hglobal_format(cf: u16) -> FORMATETC {
        FORMATETC {
            cfFormat: cf,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    pub fn set_clipboard_files(paths: &[PathBuf], cut: bool) -> io::Result<()> {
        let _com = Com::init();
        let data = data_object(paths)?;
        let effect = if cut { DROPEFFECT_MOVE.0 } else { DROPEFFECT_COPY.0 | DROPEFFECT_LINK.0 };
        // SAFETY: allocates a movable 4-byte block, written while locked; ownership
        // passes to the data object (fRelease = TRUE).
        unsafe {
            let h: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, 4).map_err(to_io)?;
            let p = GlobalLock(h).cast::<u32>();
            if !p.is_null() {
                p.write_unaligned(effect);
            }
            let _ = GlobalUnlock(h);
            let fmt = hglobal_format(preferred_effect_format());
            let medium = STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: h },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            };
            data.SetData(&fmt, &medium, true).map_err(to_io)?;
            OleSetClipboard(&data).map_err(to_io)?;
            // Keep the data available after Fennec exits.
            OleFlushClipboard().map_err(to_io)?;
        }
        Ok(())
    }

    pub fn paste_files(owner: Owner, dest: &Path) -> io::Result<()> {
        let _com = Com::init();
        // SAFETY: returns the clipboard's data object (owned).
        let data = unsafe { OleGetClipboard() }.map_err(to_io)?;
        let hdrop = hglobal_format(windows::Win32::System::Ole::CF_HDROP.0);
        // SAFETY: valid data object and format.
        if unsafe { data.QueryGetData(&hdrop) } != S_OK {
            return Err(io::Error::new(io::ErrorKind::NotFound, "the clipboard holds no files"));
        }
        let mut moving = false;
        // SAFETY: reads the optional preferred effect; the medium is released after use.
        unsafe {
            if let Ok(mut m) = data.GetData(&hglobal_format(preferred_effect_format())) {
                let p = GlobalLock(m.u.hGlobal).cast::<u32>();
                if !p.is_null() {
                    moving = p.read_unaligned() & DROPEFFECT_MOVE.0 != 0;
                }
                let _ = GlobalUnlock(m.u.hGlobal);
                ReleaseStgMedium(&mut m);
            }
        }
        // Pasting into the source folder makes "name - Copy" like Explorer.
        let flags = FOF_ALLOWUNDO.0 | FOFX_ADDUNDORECORD.0 | FOF_NOCONFIRMMKDIR.0 | FOF_RENAMEONCOLLISION.0;
        let op = file_operation(owner, flags)?;
        let dest = shell_item(dest)?;
        // SAFETY: valid operation, data object and destination folder.
        unsafe {
            if moving {
                op.MoveItems(&data, &dest).map_err(to_io)?;
            } else {
                op.CopyItems(&data, &dest).map_err(to_io)?;
            }
        }
        perform(&op).map(|_| ())
    }

    #[implement(IDropSource)]
    struct DropSource;

    impl IDropSource_Impl for DropSource_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() {
                DRAGDROP_S_CANCEL
            } else if keys.0 & MK_LBUTTON.0 == 0 {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    pub fn drag_files(paths: &[PathBuf]) -> io::Result<DragResult> {
        let _com = Com::init();
        let data = data_object(paths)?;
        let source: IDropSource = DropSource.into();
        let mut effect = DROPEFFECT(0);
        let allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
        // SAFETY: valid data object and drop source; `effect` is a live out-parameter.
        // Runs the OLE drag loop on this (STA) thread until the drop.
        let hr = unsafe { DoDragDrop(&data, &source, allowed, &mut effect) };
        Ok(if hr == DRAGDROP_S_DROP {
            if effect.0 & DROPEFFECT_MOVE.0 != 0 { DragResult::Moved } else { DragResult::Copied }
        } else {
            DragResult::Cancelled
        })
    }

    fn file_operation(owner: Owner, flags: u32) -> io::Result<IFileOperation> {
        // SAFETY: creates the in-process FileOperation object.
        let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.map_err(to_io)?;
        // SAFETY: valid object; plain setters.
        unsafe {
            if owner.0 != 0 {
                op.SetOwnerWindow(hwnd(owner)).map_err(to_io)?;
            }
            op.SetOperationFlags(windows::Win32::UI::Shell::FILEOPERATION_FLAGS(flags)).map_err(to_io)?;
        }
        Ok(op)
    }

    /// Runs the queued operations; `false` if the user cancelled.
    fn perform(op: &IFileOperation) -> io::Result<bool> {
        // SAFETY: valid object; synchronous (shows progress/confirmation UI as needed).
        unsafe {
            op.PerformOperations().map_err(to_io)?;
            Ok(!op.GetAnyOperationsAborted().map_err(to_io)?.as_bool())
        }
    }

    pub fn delete(owner: Owner, paths: &[PathBuf], permanent: bool) -> io::Result<bool> {
        let _com = Com::init();
        // The caller confirms permanent deletion itself (one prompt, not two).
        let flags = if permanent {
            FOF_NOCONFIRMATION.0
        } else {
            FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0 | FOFX_ADDUNDORECORD.0
        };
        let op = file_operation(owner, flags)?;
        let items = item_array(paths)?;
        // SAFETY: valid operation and item array.
        unsafe { op.DeleteItems(&items) }.map_err(to_io)?;
        perform(&op)
    }

    /// Records where deleted items went (`PostDeleteItem`'s newly created item).
    #[implement(windows::Win32::UI::Shell::IFileOperationProgressSink)]
    struct DeleteSink {
        recycled: std::rc::Rc<std::cell::RefCell<Vec<Recycled>>>,
    }

    fn item_path(item: &IShellItem) -> Option<PathBuf> {
        // SAFETY: valid item; the returned string is CoTaskMem-allocated and freed here.
        unsafe {
            let p = item.GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH).ok()?;
            let s = p.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
            s.map(PathBuf::from)
        }
    }

    #[allow(clippy::too_many_arguments)]
    impl windows::Win32::UI::Shell::IFileOperationProgressSink_Impl for DeleteSink_Impl {
        fn StartOperations(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn FinishOperations(&self, _hr: HRESULT) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreRenameItem(&self, _f: u32, _i: windows::core::Ref<IShellItem>, _n: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostRenameItem(
            &self,
            _f: u32,
            _i: windows::core::Ref<IShellItem>,
            _n: &PCWSTR,
            _hr: HRESULT,
            _new: windows::core::Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreMoveItem(
            &self,
            _f: u32,
            _i: windows::core::Ref<IShellItem>,
            _d: windows::core::Ref<IShellItem>,
            _n: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostMoveItem(
            &self,
            _f: u32,
            _i: windows::core::Ref<IShellItem>,
            _d: windows::core::Ref<IShellItem>,
            _n: &PCWSTR,
            _hr: HRESULT,
            _new: windows::core::Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreCopyItem(
            &self,
            _f: u32,
            _i: windows::core::Ref<IShellItem>,
            _d: windows::core::Ref<IShellItem>,
            _n: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostCopyItem(
            &self,
            _f: u32,
            _i: windows::core::Ref<IShellItem>,
            _d: windows::core::Ref<IShellItem>,
            _n: &PCWSTR,
            _hr: HRESULT,
            _new: windows::core::Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreDeleteItem(&self, _f: u32, _i: windows::core::Ref<IShellItem>) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostDeleteItem(
            &self,
            _f: u32,
            item: windows::core::Ref<IShellItem>,
            hr: HRESULT,
            new: windows::core::Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            if hr.is_ok()
                && let (Some(item), Some(new)) = (item.as_ref(), new.as_ref())
                && let (Some(original), Some(bin_item)) = (item_path(item), item_path(new))
            {
                self.recycled.borrow_mut().push(Recycled { original, bin_item });
            }
            Ok(())
        }
        fn PreNewItem(&self, _f: u32, _d: windows::core::Ref<IShellItem>, _n: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostNewItem(
            &self,
            _f: u32,
            _d: windows::core::Ref<IShellItem>,
            _n: &PCWSTR,
            _t: &PCWSTR,
            _a: u32,
            _hr: HRESULT,
            _new: windows::core::Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn UpdateProgress(&self, _total: u32, _done: u32) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
    }

    pub fn delete_to_recycle_bin(owner: Owner, paths: &[PathBuf]) -> io::Result<Vec<Recycled>> {
        let _com = Com::init();
        let op = file_operation(owner, FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0 | FOFX_ADDUNDORECORD.0)?;
        let recycled = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink: windows::Win32::UI::Shell::IFileOperationProgressSink =
            DeleteSink { recycled: recycled.clone() }.into();
        let items = item_array(paths)?;
        // SAFETY: valid operation, sink and items; the sink is unadvised before it drops.
        let cookie = unsafe { op.Advise(&sink) }.map_err(to_io)?;
        // SAFETY: as above.
        let r = unsafe { op.DeleteItems(&items) }.map_err(to_io).and_then(|()| perform(&op));
        // SAFETY: `cookie` came from `Advise` on this operation.
        let _ = unsafe { op.Unadvise(cookie) };
        r?;
        Ok(recycled.borrow().clone())
    }

    pub fn restore_from_recycle_bin(owner: Owner, items: &[Recycled]) -> io::Result<()> {
        use windows::Win32::UI::Shell::{
            BHID_EnumItems, FOLDERID_RecycleBinFolder, IEnumShellItems, KF_FLAG_DEFAULT, SHGetKnownFolderItem,
        };
        let _com = Com::init();
        let want: std::collections::HashSet<String> =
            items.iter().map(|r| r.bin_item.to_string_lossy().to_lowercase()).collect();
        // SAFETY: known-folder lookup without a token (current user).
        let bin: IShellItem =
            unsafe { SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None) }.map_err(to_io)?;
        // SAFETY: valid item; enumerator of its children.
        let en: IEnumShellItems = unsafe { bin.BindToHandler(None, &BHID_EnumItems) }.map_err(to_io)?;
        let mut found = Vec::new();
        loop {
            let mut got = [None];
            let mut n = 0u32;
            // SAFETY: one slot, `n` receives the count.
            if unsafe { en.Next(&mut got, Some(&mut n)) }.is_err() || n == 0 {
                break;
            }
            if let Some(item) = got[0].take()
                && item_path(&item).is_some_and(|p| want.contains(&p.to_string_lossy().to_lowercase()))
            {
                found.push(item);
            }
        }
        if found.len() < want.len() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "items are no longer in the recycle bin"));
        }
        for item in found {
            // SAFETY: valid recycle-bin item; its shell context menu implements "undelete".
            let cm: IContextMenu = unsafe { item.BindToHandler(None, &BHID_SFUIObject) }.map_err(to_io)?;
            let verb = c"undelete";
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                hwnd: hwnd(owner),
                lpVerb: PCSTR(verb.as_ptr().cast()),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            // SAFETY: valid menu; `info` and the verb outlive the call.
            unsafe { cm.InvokeCommand(&info) }.map_err(to_io)?;
        }
        Ok(())
    }

    pub fn rename(owner: Owner, path: &Path, new_name: &str) -> io::Result<()> {
        let _com = Com::init();
        let op = file_operation(owner, FOF_ALLOWUNDO.0 | FOFX_ADDUNDORECORD.0)?;
        let item = shell_item(path)?;
        let name = wide(std::ffi::OsStr::new(new_name));
        // SAFETY: valid operation and item; `name` outlives the call.
        unsafe { op.RenameItem(&item, PCWSTR(name.as_ptr()), None) }.map_err(to_io)?;
        perform(&op).map(|_| ())
    }

    pub fn transfer(owner: Owner, paths: &[PathBuf], dest: &Path, move_items: bool) -> io::Result<()> {
        let _com = Com::init();
        let op = file_operation(owner, FOF_ALLOWUNDO.0 | FOFX_ADDUNDORECORD.0 | FOF_NOCONFIRMMKDIR.0)?;
        let items = item_array(paths)?;
        let dest = shell_item(dest)?;
        // SAFETY: valid operation, items and destination.
        unsafe {
            if move_items {
                op.MoveItems(&items, &dest).map_err(to_io)?;
            } else {
                op.CopyItems(&items, &dest).map_err(to_io)?;
            }
        }
        perform(&op).map(|_| ())
    }

    pub fn new_folder(owner: Owner, parent: &Path, name: &str) -> io::Result<()> {
        let _com = Com::init();
        let op = file_operation(owner, FOF_ALLOWUNDO.0 | FOFX_ADDUNDORECORD.0)?;
        let dest = shell_item(parent)?;
        let name = wide(std::ffi::OsStr::new(name));
        // SAFETY: valid operation and folder; `name` outlives the call.
        unsafe {
            op.NewItem(
                &dest,
                windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY.0,
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                None,
            )
        }
        .map_err(to_io)?;
        perform(&op).map(|_| ())
    }

    pub fn properties(owner: Owner, path: &Path) -> io::Result<()> {
        let w = wide(path.as_os_str());
        // SAFETY: `w` is NUL-terminated and outlives the call (the dialog is modeless
        // and copies the path).
        let ok = unsafe { SHObjectProperties(Some(hwnd(owner)), SHOP_FILEPATH, PCWSTR(w.as_ptr()), PCWSTR::null()) };
        if ok.as_bool() { Ok(()) } else { Err(io::Error::other("SHObjectProperties failed")) }
    }

    pub fn open_with(owner: Owner, path: &Path) -> io::Result<()> {
        let w = wide(path.as_os_str());
        let info = OPENASINFO {
            pcszFile: PCWSTR(w.as_ptr()),
            pcszClass: PCWSTR::null(),
            oaifInFlags: OAIF_EXEC | OAIF_HIDE_REGISTRATION,
        };
        // SAFETY: `info` and `w` outlive the (modal) call.
        unsafe { SHOpenWithDialog(Some(hwnd(owner)), &info) }.map_err(to_io)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    fn unsupported<T>(what: &str) -> io::Result<T> {
        Err(io::Error::new(io::ErrorKind::Unsupported, format!("{what} is only available on Windows")))
    }

    pub fn context_menu(_o: Owner, _p: &[PathBuf], _at: (i32, i32), _e: &[MenuItem]) -> io::Result<MenuOutcome> {
        unsupported("the shell context menu")
    }

    pub fn set_clipboard_files(_p: &[PathBuf], _cut: bool) -> io::Result<()> {
        unsupported("copying files to the clipboard")
    }

    pub fn paste_files(_o: Owner, _dest: &Path) -> io::Result<()> {
        unsupported("pasting files")
    }

    pub fn drag_files(_p: &[PathBuf]) -> io::Result<DragResult> {
        unsupported("dragging files out")
    }

    /// Freedesktop trash (`$XDG_DATA_HOME/Trash`) or permanent removal.
    pub fn delete(_o: Owner, paths: &[PathBuf], permanent: bool) -> io::Result<bool> {
        for p in paths {
            if permanent {
                if p.is_dir() { std::fs::remove_dir_all(p)? } else { std::fs::remove_file(p)? }
            } else {
                trash(p)?;
            }
        }
        Ok(true)
    }

    pub fn delete_to_recycle_bin(_o: Owner, paths: &[PathBuf]) -> io::Result<Vec<Recycled>> {
        let mut out = Vec::new();
        for p in paths {
            let original = std::fs::canonicalize(p)?;
            let bin_item = trash(p)?;
            out.push(Recycled { original, bin_item });
        }
        Ok(out)
    }

    pub fn restore_from_recycle_bin(_o: Owner, items: &[Recycled]) -> io::Result<()> {
        for r in items {
            if r.original.exists() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", r.original.display())));
            }
            move_any(&r.bin_item, &r.original)?;
            // `Trash/files/x` ↔ `Trash/info/x.trashinfo`.
            if let (Some(files), Some(name)) = (r.bin_item.parent(), r.bin_item.file_name())
                && let Some(base) = files.parent()
            {
                let _ = std::fs::remove_file(base.join("info").join(format!("{}.trashinfo", name.to_string_lossy())));
            }
        }
        Ok(())
    }

    fn trash(p: &Path) -> io::Result<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder"))?
            .join("Trash");
        let files = base.join("files");
        let info = base.join("info");
        std::fs::create_dir_all(&files)?;
        std::fs::create_dir_all(&info)?;
        let name = p.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
        let mut target = files.join(name);
        let mut n = 1;
        while target.exists() {
            target = files.join(format!("{}.{n}", name.to_string_lossy()));
            n += 1;
        }
        let abs = std::fs::canonicalize(p)?;
        let stamp = chrono_like_now();
        let info_file =
            info.join(format!("{}.trashinfo", target.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
        std::fs::write(info_file, format!("[Trash Info]\nPath={}\nDeletionDate={stamp}\n", abs.display()))?;
        move_any(p, &target)?;
        Ok(target)
    }

    /// `rename`, or copy + remove when source and target are on different file systems.
    fn move_any(from: &Path, to: &Path) -> io::Result<()> {
        match std::fs::rename(from, to) {
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                copy_tree(from, to)?;
                if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) }
            }
            r => r,
        }
    }

    fn chrono_like_now() -> String {
        fnc_core::FileTime::now().to_local().map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string()).unwrap_or_default()
    }

    pub fn rename(_o: Owner, path: &Path, new_name: &str) -> io::Result<()> {
        let to = path.with_file_name(new_name);
        if to.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", to.display())));
        }
        std::fs::rename(path, to)
    }

    fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
        if from.is_dir() {
            std::fs::create_dir_all(to)?;
            for e in std::fs::read_dir(from)? {
                let e = e?;
                copy_tree(&e.path(), &to.join(e.file_name()))?;
            }
            Ok(())
        } else {
            std::fs::copy(from, to).map(|_| ())
        }
    }

    pub fn transfer(_o: Owner, paths: &[PathBuf], dest: &Path, move_items: bool) -> io::Result<()> {
        for p in paths {
            let name = p.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
            let to = dest.join(name);
            if move_items && std::fs::rename(p, &to).is_ok() {
                continue;
            }
            copy_tree(p, &to)?;
            if move_items {
                if p.is_dir() { std::fs::remove_dir_all(p)? } else { std::fs::remove_file(p)? }
            }
        }
        Ok(())
    }

    pub fn new_folder(_o: Owner, parent: &Path, name: &str) -> io::Result<()> {
        std::fs::create_dir(parent.join(name))
    }

    pub fn properties(_o: Owner, _p: &Path) -> io::Result<()> {
        unsupported("the properties dialog")
    }

    pub fn open_with(_o: Owner, _p: &Path) -> io::Result<()> {
        unsupported("the Open With dialog")
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn rename_transfer_new_folder_and_trash() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.txt");
        std::fs::write(&a, b"x").unwrap();
        rename(Owner::default(), &a, "b.txt").unwrap();
        assert!(d.path().join("b.txt").exists());
        assert!(rename(Owner::default(), &d.path().join("b.txt"), "x/y").is_err());
        new_folder(Owner::default(), d.path(), "sub").unwrap();
        transfer(Owner::default(), &[d.path().join("b.txt")], &d.path().join("sub"), false).unwrap();
        assert!(d.path().join("sub/b.txt").exists() && d.path().join("b.txt").exists());
        assert!(delete(Owner::default(), &[d.path().join("sub/b.txt")], true).unwrap());
        assert!(!d.path().join("sub/b.txt").exists());
    }
}

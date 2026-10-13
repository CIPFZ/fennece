//! Windows Search system index (`si:` / `system-index:`, SPEC ch.2 §5).
//!
//! A query is Advanced Query Syntax (AQS) text, as typed in Explorer's search box. It is
//! turned into Windows Search SQL by `ISearchQueryHelper::GenerateSQLFromUserQuery` of
//! the `SystemIndex` catalog and executed through the Windows Search OLE DB provider
//! (`Search.CollatorDSO`, opened from the helper's connection string with
//! `IDataInitialize`): `IDBInitialize` → `IDBCreateSession` → `ICommandText` →
//! `IRowset`. The result is the display path (`System.ItemPathDisplay`) of every
//! matching file-system item (`SCOPE='file:'`; mail and other non-file items are
//! skipped), at most `max` of them.
//!
//! The COM work runs on a short-lived thread of its own (multithreaded apartment), so
//! the caller's apartment does not matter; the caller polls for cancellation and the
//! timeout and abandons the thread (which stops at its next batch of rows) when either
//! fires. Windows Search filters results by the calling user's access rights, so a
//! service account sees other items than the interactive user.
//!
//! Without the Windows Search service (not installed, disabled, stopped) every query
//! fails with [`Error::Unavailable`]; on other platforms always.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;

/// Paths matched by a query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hits {
    /// Display paths of the matching file-system items (`C:\dir\file.txt`, UNC paths).
    pub paths: Vec<String>,
    /// More than `max` items matched; `paths` holds the first `max`.
    pub truncated: bool,
}

/// Why a query produced no result.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The Windows Search service is not installed or not running (or the platform has
    /// no Windows Search).
    #[error("Windows Search is not available: {0}")]
    Unavailable(String),
    /// The service answered but the query failed.
    #[error("Windows Search query failed: {0}")]
    Failed(String),
    /// The query did not finish in time.
    #[error("Windows Search query timed out after {} ms", .0.as_millis())]
    TimedOut(Duration),
    /// The caller cancelled the query.
    #[error("cancelled")]
    Cancelled,
}

/// Runs the AQS query `aqs` against the system index of the current user: at most `max`
/// paths, waiting at most `timeout`; `cancel` is polled every few milliseconds.
pub fn search(aqs: &str, max: usize, timeout: Duration, cancel: &dyn Fn() -> bool) -> Result<Hits, Error> {
    if !cfg!(windows) {
        return Err(Error::Unavailable("Windows Search exists only on Windows".into()));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::bounded(1);
    let (text, flag) = (aqs.to_string(), stop.clone());
    std::thread::Builder::new()
        .name("fennec-winsearch".into())
        .spawn(move || {
            let _ = tx.send(imp::query(&text, max, &flag));
        })
        .map_err(|e| Error::Failed(format!("cannot start the query thread: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        if cancel() {
            stop.store(true, Ordering::Relaxed);
            return Err(Error::Cancelled);
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(r) => return r,
            Err(RecvTimeoutError::Disconnected) => return Err(Error::Failed("the query thread ended".into())),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= deadline {
            stop.store(true, Ordering::Relaxed);
            return Err(Error::TimedOut(timeout));
        }
    }
}

/// `true` if the Windows Search service answers (its search manager can be created).
pub fn available() -> bool {
    imp::indexer_version().is_ok()
}

/// Version of the indexer (`"7.0.20348.1"`); fails when the service is unavailable.
pub fn indexer_version() -> Result<String, Error> {
    imp::indexer_version()
}

/// Adds `folder` (and everything below it) to the crawl scope of the system index as a
/// user scope rule (needs administrator rights), and asks the indexer to crawl it.
pub fn include_folder(folder: &Path) -> Result<(), Error> {
    imp::scope_rule(folder, true)
}

/// Removes the user scope rule [`include_folder`] added for `folder`.
pub fn remove_folder(folder: &Path) -> Result<(), Error> {
    imp::scope_rule(folder, false)
}

/// Items waiting to be indexed (incremental, notification and high-priority queues).
pub fn pending_items() -> Result<u64, Error> {
    imp::pending_items()
}

/// `file:///` URL of a folder for crawl scope rules (`C:\a` → `file:///C:\a\`).
pub fn folder_url(folder: &Path) -> String {
    let mut s = folder.to_string_lossy().replace('/', "\\");
    if !s.ends_with('\\') {
        s.push('\\');
    }
    format!("file:///{s}")
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    fn unavailable<T>() -> Result<T, Error> {
        Err(Error::Unavailable("Windows Search exists only on Windows".into()))
    }

    pub(super) fn query(_aqs: &str, _max: usize, _stop: &AtomicBool) -> Result<Hits, Error> {
        unavailable()
    }

    pub(super) fn indexer_version() -> Result<String, Error> {
        unavailable()
    }

    pub(super) fn scope_rule(_folder: &Path, _add: bool) -> Result<(), Error> {
        unavailable()
    }

    pub(super) fn pending_items() -> Result<u64, Error> {
        unavailable()
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::System::Search::{
        CSearchManager, DB_S_ENDOFROWSET, DBACCESSOR_ROWDATA, DBBINDING, DBMEMOWNER_CLIENTOWNED, DBPARAMIO_NOTPARAM,
        DBPART_LENGTH, DBPART_STATUS, DBPART_VALUE, DBSTATUS_S_OK, DBTYPE_WSTR, HACCESSOR, IAccessor, ICommand,
        ICommandText, IDBCreateCommand, IDBCreateSession, IDBInitialize, IDataInitialize, IRowset,
        ISearchCatalogManager, ISearchManager, MSDAINITIALIZE,
    };
    use windows::core::{GUID, IUnknown, Interface, PCWSTR, PWSTR, w};

    use super::*;

    /// `DBGUID_DEFAULT` — the provider's default command dialect (oledb.h).
    const DBGUID_DEFAULT: GUID = GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);
    /// Rows fetched per `IRowset::GetNextRows` call (the stop flag is checked in between).
    const BATCH: usize = 256;
    /// UTF-16 units of the path buffer of a row (longer paths are skipped).
    const MAX_PATH_UNITS: usize = 32 * 1024;
    /// Row buffer layout: status (u32) at 0, length (usize) at 8, value at 16.
    const OB_STATUS: usize = 0;
    const OB_LENGTH: usize = 8;
    const OB_VALUE: usize = 16;
    const VALUE_BYTES: usize = (MAX_PATH_UNITS + 1) * 2;

    /// COM for the calling thread (multithreaded apartment unless the thread already
    /// has another one, which works just as well for these out-of-process objects).
    struct Com(bool);

    impl Com {
        fn init() -> Self {
            // SAFETY: plain COM initialisation of the current thread; balanced in `Drop`
            // when it succeeded (S_OK / S_FALSE), skipped if another mode is active.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Com(hr.is_ok() && hr != RPC_E_CHANGED_MODE)
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances the successful CoInitializeEx above; every interface
                // created on this thread has been released by now (they live in inner
                // scopes of the functions that create a `Com`).
                unsafe { CoUninitialize() };
            }
        }
    }

    fn describe(e: &windows::core::Error) -> String {
        let msg = e.message();
        let msg = msg.trim();
        if msg.is_empty() {
            format!("0x{:08X}", e.code().0 as u32)
        } else {
            format!("{msg} (0x{:08X})", e.code().0 as u32)
        }
    }

    fn unavailable(what: &str) -> impl Fn(windows::core::Error) -> Error + '_ {
        move |e| Error::Unavailable(format!("{what}: {}", describe(&e)))
    }

    fn failed(what: &str) -> impl Fn(windows::core::Error) -> Error + '_ {
        move |e| Error::Failed(format!("{what}: {}", describe(&e)))
    }

    /// Copies and frees a string returned by COM (`CoTaskMemAlloc`).
    fn take_string(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: `p` is a NUL-terminated string allocated by the callee with
        // CoTaskMemAlloc; it is read once and freed exactly once here.
        unsafe {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
            s
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn catalog() -> Result<ISearchCatalogManager, Error> {
        // SAFETY: COM is initialised on this thread (callers hold a `Com`); the class is
        // the Windows Search manager, served by the indexer service.
        let manager: ISearchManager = unsafe { CoCreateInstance(&CSearchManager, None, CLSCTX_LOCAL_SERVER) }
            .map_err(unavailable("search manager"))?;
        // SAFETY: valid interface; the literal is NUL-terminated and static.
        unsafe { manager.GetCatalog(w!("SystemIndex")) }.map_err(unavailable("SystemIndex catalog"))
    }

    pub(super) fn indexer_version() -> Result<String, Error> {
        let _com = Com::init();
        // SAFETY: as in `catalog`.
        let manager: ISearchManager = unsafe { CoCreateInstance(&CSearchManager, None, CLSCTX_LOCAL_SERVER) }
            .map_err(unavailable("search manager"))?;
        // SAFETY: valid interface; the returned string is owned and freed by `take_string`.
        unsafe { manager.GetIndexerVersionStr() }.map(take_string).map_err(unavailable("indexer version"))
    }

    pub(super) fn pending_items() -> Result<u64, Error> {
        let _com = Com::init();
        let cat = catalog()?;
        let (mut a, mut b, mut c) = (0i32, 0i32, 0i32);
        // SAFETY: valid interface; the three out-pointers are live locals.
        unsafe { cat.NumberOfItemsToIndex(&mut a, &mut b, &mut c) }.map_err(failed("NumberOfItemsToIndex"))?;
        Ok([a, b, c].iter().map(|&n| u64::try_from(n).unwrap_or(0)).sum())
    }

    pub(super) fn scope_rule(folder: &Path, add: bool) -> Result<(), Error> {
        let _com = Com::init();
        let cat = catalog()?;
        // SAFETY: valid interface.
        let scopes = unsafe { cat.GetCrawlScopeManager() }.map_err(failed("crawl scope manager"))?;
        let url = wide(&folder_url(folder));
        let url = PCWSTR(url.as_ptr());
        // SAFETY: `url` is NUL-terminated and outlives the calls; 0 = FF_INDEXCOMPLEXURLS
        // not requested (default follow flags).
        unsafe {
            if add {
                scopes.AddUserScopeRule(url, true, false, 0).map_err(failed("AddUserScopeRule"))?;
            } else {
                scopes.RemoveScopeRule(url).map_err(failed("RemoveScopeRule"))?;
            }
            scopes.SaveAll().map_err(failed("SaveAll"))?;
            if add {
                // A new rule is crawled on the next pass; ask for it now.
                let _ = cat.ReindexSearchRoot(url);
            }
        }
        Ok(())
    }

    pub(super) fn query(aqs: &str, max: usize, stop: &AtomicBool) -> Result<Hits, Error> {
        let _com = Com::init();
        run(aqs, max, stop)
    }

    /// SQL of `aqs` and the provider connection string.
    fn generate_sql(aqs: &str, max: usize) -> Result<(String, String), Error> {
        let cat = catalog()?;
        // SAFETY: valid interfaces; every string argument is NUL-terminated and outlives
        // its call; returned strings are owned and freed by `take_string`.
        unsafe {
            let helper = cat.GetQueryHelper().map_err(unavailable("query helper"))?;
            helper.SetQuerySelectColumns(w!("System.ItemPathDisplay")).map_err(failed("select columns"))?;
            // File-system items only (no mail, no other protocol handlers).
            helper.SetQueryWhereRestrictions(w!("AND SCOPE='file:'")).map_err(failed("restrictions"))?;
            // One more than wanted tells whether the result was cut.
            let limit = i32::try_from(max.saturating_add(1)).unwrap_or(i32::MAX);
            helper.SetQueryMaxResults(limit).map_err(failed("max results"))?;
            let conn = take_string(helper.ConnectionString().map_err(unavailable("connection string"))?);
            let text = wide(aqs);
            let sql = take_string(
                helper.GenerateSQLFromUserQuery(PCWSTR(text.as_ptr())).map_err(failed("GenerateSQLFromUserQuery"))?,
            );
            Ok((sql, conn))
        }
    }

    fn run(aqs: &str, max: usize, stop: &AtomicBool) -> Result<Hits, Error> {
        let (sql, conn) = generate_sql(aqs, max)?;
        tracing::debug!(%sql, "system index query");
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let conn = wide(&conn);
        let sql = wide(&sql);
        // SAFETY: COM is initialised (caller holds a `Com`); every interface below is
        // obtained from a successful call and released when dropped; the strings are
        // NUL-terminated and outlive the calls that read them.
        let rowset: IRowset = unsafe {
            let init: IDataInitialize = CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER)
                .map_err(unavailable("OLE DB service components"))?;
            let mut source: Option<IUnknown> = None;
            init.GetDataSource(None, CLSCTX_INPROC_SERVER.0, PCWSTR(conn.as_ptr()), &IDBInitialize::IID, &mut source)
                .map_err(unavailable("Search.CollatorDSO"))?;
            let source: IDBInitialize = source
                .ok_or_else(|| Error::Unavailable("Search.CollatorDSO: no data source".into()))?
                .cast()
                .map_err(unavailable("IDBInitialize"))?;
            source.Initialize().map_err(unavailable("IDBInitialize::Initialize"))?;
            let session: IDBCreateSession = source.cast().map_err(failed("IDBCreateSession"))?;
            let creator: IDBCreateCommand = session
                .CreateSession(None, &IDBCreateCommand::IID)
                .map_err(failed("CreateSession"))?
                .cast()
                .map_err(failed("IDBCreateCommand"))?;
            let command: ICommandText = creator
                .CreateCommand(None, &ICommandText::IID)
                .map_err(failed("CreateCommand"))?
                .cast()
                .map_err(failed("ICommandText"))?;
            command.SetCommandText(&DBGUID_DEFAULT, PCWSTR(sql.as_ptr())).map_err(failed("SetCommandText"))?;
            let mut rows: Option<IUnknown> = None;
            command
                .cast::<ICommand>()
                .map_err(failed("ICommand"))?
                .Execute(None, &IRowset::IID, None, None, Some(&mut rows))
                .map_err(failed("Execute"))?;
            rows.ok_or_else(|| Error::Failed("Execute returned no rowset".into()))?.cast().map_err(failed("IRowset"))?
        };
        read_rows(&rowset, max, stop)
    }

    /// Accessor of column 1 as a client-owned UTF-16 string with length and status.
    struct Accessor {
        accessor: IAccessor,
        handle: HACCESSOR,
    }

    impl Accessor {
        fn new(rowset: &IRowset) -> Result<Self, Error> {
            let accessor: IAccessor = rowset.cast().map_err(failed("IAccessor"))?;
            let binding = DBBINDING {
                iOrdinal: 1,
                obValue: OB_VALUE,
                obLength: OB_LENGTH,
                obStatus: OB_STATUS,
                dwPart: (DBPART_VALUE.0 | DBPART_LENGTH.0 | DBPART_STATUS.0) as u32,
                dwMemOwner: DBMEMOWNER_CLIENTOWNED.0 as u32,
                eParamIO: DBPARAMIO_NOTPARAM.0 as u32,
                cbMaxLen: VALUE_BYTES,
                wType: DBTYPE_WSTR.0 as u16,
                ..DBBINDING::default()
            };
            let mut handle = HACCESSOR::default();
            // SAFETY: one binding read during the call; `handle` receives the accessor,
            // released in `Drop`.
            unsafe {
                accessor.CreateAccessor(
                    DBACCESSOR_ROWDATA.0 as u32,
                    1,
                    &binding,
                    OB_VALUE + VALUE_BYTES,
                    &mut handle,
                    None,
                )
            }
            .map_err(failed("CreateAccessor"))?;
            Ok(Self { accessor, handle })
        }
    }

    impl Drop for Accessor {
        fn drop(&mut self) {
            // SAFETY: releases the accessor created in `new`, once.
            let _ = unsafe { self.accessor.ReleaseAccessor(self.handle, None) };
        }
    }

    fn read_rows(rowset: &IRowset, max: usize, stop: &AtomicBool) -> Result<Hits, Error> {
        let acc = Accessor::new(rowset)?;
        // u64 storage keeps the length field (usize at offset 8) aligned.
        let mut buf = vec![0u64; (OB_VALUE + VALUE_BYTES).div_ceil(8)];
        let mut hits = Hits::default();
        let mut hrows = [0usize; BATCH];
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let mut got = 0usize;
            let mut out: *mut usize = hrows.as_mut_ptr();
            // SAFETY: `out` points to our array of BATCH row handles, so the provider
            // fills it instead of allocating; `got` receives the count (≤ BATCH). The
            // vtable call is used because the generated wrapper passes the array
            // pointer differently from the documented `HROW**` contract.
            let hr = unsafe {
                (Interface::vtable(rowset).GetNextRows)(
                    Interface::as_raw(rowset),
                    0,
                    0,
                    BATCH as isize,
                    &mut got,
                    &mut out,
                )
            };
            if hr.is_err() {
                return Err(Error::Failed(format!("GetNextRows: {}", describe(&hr.into()))));
            }
            let got = got.min(BATCH);
            for &row in &hrows[..got] {
                if hits.paths.len() >= max {
                    hits.truncated = true;
                    break;
                }
                // SAFETY: `row` was just returned by GetNextRows; `buf` is large enough
                // for the accessor's row layout (OB_VALUE + VALUE_BYTES bytes).
                if unsafe { rowset.GetData(row, acc.handle, buf.as_mut_ptr().cast()) }.is_err() {
                    continue;
                }
                if let Some(p) = row_path(&buf) {
                    hits.paths.push(p);
                }
            }
            if got > 0 {
                // SAFETY: releases exactly the `got` handles obtained above.
                let _ = unsafe {
                    rowset.ReleaseRows(
                        got,
                        hrows.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
            }
            if hits.truncated || got == 0 || hr == DB_S_ENDOFROWSET {
                return Ok(hits);
            }
        }
    }

    /// The path in a row buffer filled by `GetData` (`None`: null, truncated, empty).
    fn row_path(buf: &[u64]) -> Option<String> {
        // SAFETY: `buf` spans OB_VALUE + VALUE_BYTES bytes of initialised u64 storage;
        // the reads below stay inside it and are suitably aligned (u32 at 0, usize at 8,
        // u16 from 16).
        let (status, len, units) = unsafe {
            let base = buf.as_ptr().cast::<u8>();
            let status = base.add(OB_STATUS).cast::<u32>().read();
            let len = base.add(OB_LENGTH).cast::<usize>().read();
            let units = std::slice::from_raw_parts(base.add(OB_VALUE).cast::<u16>(), MAX_PATH_UNITS);
            (status, len, units)
        };
        if status != DBSTATUS_S_OK.0 as u32 || len == 0 || len / 2 > MAX_PATH_UNITS {
            return None;
        }
        let units = &units[..len / 2];
        let s = String::from_utf16_lossy(units);
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_urls() {
        assert_eq!(folder_url(Path::new("C:\\data\\docs")), "file:///C:\\data\\docs\\");
        assert_eq!(folder_url(Path::new("D:\\x\\")), "file:///D:\\x\\");
    }

    #[cfg(not(windows))]
    #[test]
    fn unavailable_elsewhere() {
        assert!(matches!(search("x", 10, Duration::from_secs(1), &|| false), Err(Error::Unavailable(_))));
        assert!(!available());
    }
}

//! C ABI mirroring the Everything SDK (`Everything_*` → `fnc_*`, see
//! `include/fennec_sdk.h`). State is process-global and guarded by a mutex, like the
//! Everything SDK's critical section; string pointers returned by `fnc_GetResult*` stay
//! valid until the next `fnc_Query*`, `fnc_Reset` or `fnc_CleanUp`.
//!
//! Differences from the Everything SDK: queries are always synchronous (`bWait` is
//! ignored; no reply window), `A` functions use UTF-8, and total file/folder counts are
//! only available for the returned page.

#![allow(unsafe_code)]

use std::ffi::{CStr, c_char};
use std::time::Duration;

use fnc_ipc::compat::{decode_sort, encode_sort, request};
use fnc_ipc::proto::{Column, FLAG_FOLDER, FLAG_ROOT, Mods, Query, UNKNOWN};
use fnc_ipc::rows::{Row, join_path};
use fnc_ipc::{Client, ClientError, Op, Payload};
use parking_lot::Mutex;

/// `BOOL`.
pub type Bool = i32;
/// `DWORD`.
pub type Dword = u32;
/// UTF-16 code unit (`WCHAR`).
pub type Wchar = u16;

/// No error.
pub const FNC_OK: Dword = 0;
/// Out of memory.
pub const FNC_ERROR_MEMORY: Dword = 1;
/// Fennec is not running / IPC failed.
pub const FNC_ERROR_IPC: Dword = 2;
/// Unused (Everything: RegisterClassEx failed).
pub const FNC_ERROR_REGISTERCLASSEX: Dword = 3;
/// Unused (Everything: CreateWindow failed).
pub const FNC_ERROR_CREATEWINDOW: Dword = 4;
/// Unused (Everything: CreateThread failed).
pub const FNC_ERROR_CREATETHREAD: Dword = 5;
/// Result index out of range.
pub const FNC_ERROR_INVALIDINDEX: Dword = 6;
/// Call not valid now (e.g. no query yet).
pub const FNC_ERROR_INVALIDCALL: Dword = 7;
/// Data not requested (see `fnc_SetRequestFlags`).
pub const FNC_ERROR_INVALIDREQUEST: Dword = 8;
/// Bad parameter (NULL pointer, invalid sort, ...).
pub const FNC_ERROR_INVALIDPARAMETER: Dword = 9;

const DEFAULT_FLAGS: Dword = request::NAME | request::PATH;
const MAX_STRING: usize = 1 << 20;

struct WideRow {
    name: Vec<Wchar>,
    path: Vec<Wchar>,
    full: Vec<Wchar>,
    ext: Vec<Wchar>,
    file_list: Vec<Wchar>,
    hl_name: Vec<Wchar>,
    hl_path: Vec<Wchar>,
    hl_full: Vec<Wchar>,
}

struct NarrowRow {
    name: Vec<u8>,
    path: Vec<u8>,
    ext: Vec<u8>,
    file_list: Vec<u8>,
    hl_name: Vec<u8>,
    hl_path: Vec<u8>,
    hl_full: Vec<u8>,
}

struct Page {
    total: u64,
    rows: Vec<Row>,
    wide: Vec<WideRow>,
    narrow: Vec<NarrowRow>,
    request_flags: Dword,
    sort: Dword,
}

struct State {
    search: String,
    search_w: Vec<Wchar>,
    search_a: Vec<u8>,
    mods: Mods,
    max: Dword,
    offset: Dword,
    sort: Dword,
    request_flags: Dword,
    instance: String,
    last_error: Dword,
    client: Option<Client>,
    page: Option<Page>,
}

impl State {
    const fn new() -> Self {
        Self {
            search: String::new(),
            search_w: Vec::new(),
            search_a: Vec::new(),
            mods: Mods {
                case: false,
                whole_word: false,
                path: false,
                regex: false,
                diacritics: false,
                prefix: false,
                suffix: false,
                ignore_punctuation: false,
                ignore_whitespace: false,
            },
            max: u32::MAX,
            offset: 0,
            sort: 1,
            request_flags: DEFAULT_FLAGS,
            instance: String::new(),
            last_error: FNC_OK,
            client: None,
            page: None,
        }
    }

    fn set_search(&mut self, s: String) {
        self.search_w = s.encode_utf16().chain(std::iter::once(0)).collect();
        self.search_a = s.as_bytes().iter().copied().chain(std::iter::once(0)).collect();
        self.search = s;
    }

    fn client(&mut self) -> Option<&mut Client> {
        if self.client.is_none() {
            match Client::connect(&self.instance, Duration::from_secs(1)) {
                Ok(mut c) => {
                    c.set_timeout(Some(Duration::from_secs(120)));
                    self.client = Some(c);
                }
                Err(_) => {
                    self.last_error = FNC_ERROR_IPC;
                    return None;
                }
            }
        }
        self.client.as_mut()
    }

    /// Runs `op`, reconnecting once if the connection was lost.
    fn call(&mut self, op: Op) -> Option<Payload> {
        for attempt in 0..2 {
            let c = self.client()?;
            match c.call(op.clone()) {
                Ok(p) => {
                    self.last_error = FNC_OK;
                    return Some(p);
                }
                Err(ClientError::Server { .. }) => {
                    self.last_error = FNC_ERROR_INVALIDCALL;
                    return None;
                }
                Err(_) if attempt == 0 => self.client = None,
                Err(_) => {
                    self.client = None;
                    self.last_error = FNC_ERROR_IPC;
                    return None;
                }
            }
        }
        None
    }
}

static STATE: Mutex<State> = Mutex::new(State::new());

fn wide(s: &str) -> Vec<Wchar> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn narrow(s: &str) -> Vec<u8> {
    s.as_bytes().iter().copied().chain(std::iter::once(0)).collect()
}

fn columns_for(flags: Dword) -> (Vec<Column>, bool) {
    let mut cols = Vec::new();
    let mut add = |c: Column| {
        if !cols.contains(&c) {
            cols.push(c);
        }
    };
    let pairs = [
        (request::NAME | request::HIGHLIGHTED_NAME, Column::Name),
        (request::PATH | request::HIGHLIGHTED_PATH, Column::Path),
        (request::FULL_PATH_AND_NAME | request::HIGHLIGHTED_FULL_PATH_AND_NAME, Column::FullPath),
        (request::EXTENSION, Column::Extension),
        (request::SIZE, Column::Size),
        (request::DATE_CREATED, Column::DateCreated),
        (request::DATE_MODIFIED, Column::DateModified),
        (request::DATE_ACCESSED, Column::DateAccessed),
        (request::ATTRIBUTES, Column::Attributes),
        (request::FILE_LIST_FILE_NAME, Column::FileListName),
        (request::RUN_COUNT, Column::RunCount),
        (request::DATE_RUN, Column::DateRun),
        (request::DATE_RECENTLY_CHANGED, Column::DateRecentlyChanged),
    ];
    for (bits, c) in pairs {
        if flags & bits != 0 {
            add(c);
        }
    }
    let hl =
        flags & (request::HIGHLIGHTED_NAME | request::HIGHLIGHTED_PATH | request::HIGHLIGHTED_FULL_PATH_AND_NAME) != 0;
    (cols, hl)
}

fn run_query() -> Bool {
    let mut st = STATE.lock();
    let Some((col, asc)) = decode_sort(st.sort) else {
        st.last_error = FNC_ERROR_INVALIDPARAMETER;
        return 0;
    };
    let flags = st.request_flags & request::ALL;
    let (columns, highlight) = columns_for(flags);
    let q = Query {
        search: st.search.clone(),
        mods: st.mods,
        sort: vec![(col, asc)],
        offset: u64::from(st.offset),
        limit: if st.max == u32::MAX { u64::MAX } else { u64::from(st.max) },
        columns,
        highlight,
        timeout_ms: 0,
    };
    st.page = None;
    let mut results = None;
    for attempt in 0..2 {
        let Some(c) = st.client() else { return 0 };
        match c.query_all(&q) {
            Ok(r) => {
                results = Some(r);
                break;
            }
            Err(ClientError::Server { .. }) => {
                st.last_error = FNC_ERROR_INVALIDCALL;
                return 0;
            }
            Err(_) => {
                st.client = None;
                if attempt == 1 {
                    st.last_error = FNC_ERROR_IPC;
                    return 0;
                }
            }
        }
    }
    let Some(r) = results else {
        st.last_error = FNC_ERROR_IPC;
        return 0;
    };
    let rows: Vec<Row> = (0..r.len())
        .map(|i| {
            let mut row = r.row(i);
            if row.full_path.is_empty() && !row.name.is_empty() {
                row.full_path = join_path(&row.path, &row.name);
            }
            row
        })
        .collect();
    let hl = |h: &Option<String>, plain: &str| h.clone().unwrap_or_else(|| plain.to_string());
    let wide_rows = rows
        .iter()
        .map(|r| WideRow {
            name: wide(&r.name),
            path: wide(&r.path),
            full: wide(&r.full_path),
            ext: wide(&r.extension),
            file_list: wide(&r.file_list_name),
            hl_name: wide(&hl(&r.highlighted_name, &r.name)),
            hl_path: wide(&hl(&r.highlighted_path, &r.path)),
            hl_full: wide(&hl(&r.highlighted_full_path, &r.full_path)),
        })
        .collect();
    let narrow_rows = rows
        .iter()
        .map(|r| NarrowRow {
            name: narrow(&r.name),
            path: narrow(&r.path),
            ext: narrow(&r.extension),
            file_list: narrow(&r.file_list_name),
            hl_name: narrow(&hl(&r.highlighted_name, &r.name)),
            hl_path: narrow(&hl(&r.highlighted_path, &r.path)),
            hl_full: narrow(&hl(&r.highlighted_full_path, &r.full_path)),
        })
        .collect();
    st.page = Some(Page {
        total: r.total,
        rows,
        wide: wide_rows,
        narrow: narrow_rows,
        request_flags: flags,
        sort: encode_sort(col, asc),
    });
    st.last_error = FNC_OK;
    1
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated UTF-16 string.
unsafe fn read_wide(p: *const Wchar) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    // SAFETY: the caller guarantees a NUL terminator; we stop at it (or at MAX_STRING
    // units, never reading past the terminator).
    unsafe {
        while len < MAX_STRING && *p.add(len) != 0 {
            len += 1;
        }
    }
    // SAFETY: `len` units starting at `p` were just read successfully.
    let units = unsafe { std::slice::from_raw_parts(p, len) };
    Some(String::from_utf16_lossy(units))
}

/// Reads a NUL-terminated UTF-8 string.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated byte string.
unsafe fn read_narrow(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

fn with_row<T>(index: Dword, default: T, f: impl FnOnce(&mut State, usize) -> T) -> T {
    let mut st = STATE.lock();
    let Some(page) = &st.page else {
        st.last_error = FNC_ERROR_INVALIDCALL;
        return default;
    };
    let i = index as usize;
    if i >= page.rows.len() {
        st.last_error = FNC_ERROR_INVALIDINDEX;
        return default;
    }
    f(&mut st, i)
}

fn requested(st: &mut State, bits: Dword) -> bool {
    let ok = st.page.as_ref().is_some_and(|p| p.request_flags & bits != 0);
    if !ok {
        st.last_error = FNC_ERROR_INVALIDREQUEST;
    }
    ok
}

fn wide_ptr(index: Dword, bits: Dword, pick: fn(&WideRow) -> &Vec<Wchar>) -> *const Wchar {
    with_row(index, std::ptr::null(), |st, i| {
        if !requested(st, bits) {
            return std::ptr::null();
        }
        st.page.as_ref().and_then(|p| p.wide.get(i)).map_or(std::ptr::null(), |w| pick(w).as_ptr())
    })
}

fn narrow_ptr(index: Dword, bits: Dword, pick: fn(&NarrowRow) -> &Vec<u8>) -> *const c_char {
    with_row(index, std::ptr::null(), |st, i| {
        if !requested(st, bits) {
            return std::ptr::null();
        }
        st.page.as_ref().and_then(|p| p.narrow.get(i)).map_or(std::ptr::null(), |w| pick(w).as_ptr().cast())
    })
}

fn row_value<T>(index: Dword, bits: Dword, default: T, pick: impl FnOnce(&Row) -> T) -> T {
    with_row(index, default, |st, i| {
        if !requested(st, bits) {
            // Unrequested data: same default as an unknown value.
            return pick(&Row::default());
        }
        match st.page.as_ref().and_then(|p| p.rows.get(i)) {
            Some(r) => pick(r),
            None => pick(&Row::default()),
        }
    })
}

/// Copies `src` (no terminator) into `buf` (capacity `max` units, terminated); returns the
/// number of units copied, or the full length when `buf` is NULL.
///
/// # Safety
/// `buf` must be NULL or valid for `max` writes.
unsafe fn copy_out<T: Copy + Default>(src: &[T], buf: *mut T, max: Dword) -> Dword {
    if buf.is_null() {
        return src.len() as Dword;
    }
    if max == 0 {
        return 0;
    }
    let n = src.len().min(max as usize - 1);
    // SAFETY: the caller guarantees `buf` holds `max` elements; we write `n + 1 <= max`.
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), buf, n);
        *buf.add(n) = T::default();
    }
    n as Dword
}

fn set_flag(f: impl FnOnce(&mut Mods, bool), v: Bool) {
    let mut st = STATE.lock();
    f(&mut st.mods, v != 0);
}

// ---------------------------------------------------------------- search state

/// Sets the search text (UTF-16). NULL clears it.
///
/// # Safety
/// `search` must be NULL or a NUL-terminated UTF-16 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_SetSearchW(search: *const Wchar) {
    // SAFETY: forwarded caller contract.
    let s = unsafe { read_wide(search) }.unwrap_or_default();
    STATE.lock().set_search(s);
}

/// Sets the search text (UTF-8). NULL clears it.
///
/// # Safety
/// `search` must be NULL or a NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_SetSearchA(search: *const c_char) {
    // SAFETY: forwarded caller contract.
    let s = unsafe { read_narrow(search) }.unwrap_or_default();
    STATE.lock().set_search(s);
}

/// Current search (UTF-16), valid until the next `fnc_SetSearch*`.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetSearchW() -> *const Wchar {
    let mut st = STATE.lock();
    if st.search_w.is_empty() {
        st.search_w = vec![0];
    }
    st.search_w.as_ptr()
}

/// Current search (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetSearchA() -> *const c_char {
    let mut st = STATE.lock();
    if st.search_a.is_empty() {
        st.search_a = vec![0];
    }
    st.search_a.as_ptr().cast()
}

/// Selects the instance (`-instance <name>`; NULL or "" = default).
///
/// # Safety
/// `instance` must be NULL or a NUL-terminated UTF-16 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_SetInstanceW(instance: *const Wchar) {
    // SAFETY: forwarded caller contract.
    let s = unsafe { read_wide(instance) }.unwrap_or_default();
    let mut st = STATE.lock();
    if st.instance != s {
        st.instance = s;
        st.client = None;
    }
}

/// Match path.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetMatchPath(enable: Bool) {
    set_flag(|m, v| m.path = v, enable);
}

/// Match case.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetMatchCase(enable: Bool) {
    set_flag(|m, v| m.case = v, enable);
}

/// Match whole words.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetMatchWholeWord(enable: Bool) {
    set_flag(|m, v| m.whole_word = v, enable);
}

/// Regex.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetRegex(enable: Bool) {
    set_flag(|m, v| m.regex = v, enable);
}

/// Match diacritics.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetMatchDiacritics(enable: Bool) {
    set_flag(|m, v| m.diacritics = v, enable);
}

/// Most results (`0xFFFFFFFF` = all).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetMax(max: Dword) {
    STATE.lock().max = max;
}

/// First result.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetOffset(offset: Dword) {
    STATE.lock().offset = offset;
}

/// Sort (`EVERYTHING_SORT_*` values, 1..=26).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetSort(sort: Dword) {
    STATE.lock().sort = sort;
}

/// Requested data (`EVERYTHING_REQUEST_*` bits; default name | path).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetRequestFlags(flags: Dword) {
    STATE.lock().request_flags = flags;
}

/// Accepted for source compatibility; replies are synchronous.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetReplyWindow(_hwnd: isize) {}

/// Accepted for source compatibility.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SetReplyID(_id: Dword) {}

/// Match path?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetMatchPath() -> Bool {
    Bool::from(STATE.lock().mods.path)
}

/// Match case?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetMatchCase() -> Bool {
    Bool::from(STATE.lock().mods.case)
}

/// Match whole word?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetMatchWholeWord() -> Bool {
    Bool::from(STATE.lock().mods.whole_word)
}

/// Regex?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetRegex() -> Bool {
    Bool::from(STATE.lock().mods.regex)
}

/// Max.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetMax() -> Dword {
    STATE.lock().max
}

/// Offset.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetOffset() -> Dword {
    STATE.lock().offset
}

/// Sort.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetSort() -> Dword {
    STATE.lock().sort
}

/// Request flags.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetRequestFlags() -> Dword {
    STATE.lock().request_flags
}

/// Last error (`FNC_ERROR_*`).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetLastError() -> Dword {
    STATE.lock().last_error
}

// ---------------------------------------------------------------- query

/// Runs the query (synchronously; `wait` is ignored). Returns TRUE on success.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_QueryW(_wait: Bool) -> Bool {
    run_query()
}

/// Same as [`fnc_QueryW`] (strings are UTF-8 on the `A` getters).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_QueryA(_wait: Bool) -> Bool {
    run_query()
}

fn permute<T>(v: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut old: Vec<Option<T>> = v.into_iter().map(Some).collect();
    order.iter().filter_map(|&i| old.get_mut(i).and_then(Option::take)).collect()
}

/// Sorts the current results by path, then name (local, like the Everything SDK).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SortResultsByPath() {
    let mut st = STATE.lock();
    let Some(p) = st.page.as_mut() else {
        st.last_error = FNC_ERROR_INVALIDCALL;
        return;
    };
    let mut order: Vec<usize> = (0..p.rows.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&p.rows[a], &p.rows[b]);
        x.path
            .to_lowercase()
            .cmp(&y.path.to_lowercase())
            .then_with(|| x.name.to_lowercase().cmp(&y.name.to_lowercase()))
    });
    p.rows = permute(std::mem::take(&mut p.rows), &order);
    p.wide = permute(std::mem::take(&mut p.wide), &order);
    p.narrow = permute(std::mem::take(&mut p.narrow), &order);
    p.sort = 3;
}

/// Sort of the current results.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultListSort() -> Dword {
    STATE.lock().page.as_ref().map_or(0, |p| p.sort)
}

/// Request flags of the current results.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultListRequestFlags() -> Dword {
    STATE.lock().page.as_ref().map_or(0, |p| p.request_flags)
}

fn count(f: impl FnOnce(&Page) -> u64) -> Dword {
    let mut st = STATE.lock();
    match st.page.as_ref() {
        Some(p) => f(p).min(u64::from(u32::MAX)) as Dword,
        None => {
            st.last_error = FNC_ERROR_INVALIDCALL;
            0
        }
    }
}

/// Results returned.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetNumResults() -> Dword {
    count(|p| p.rows.len() as u64)
}

/// Files among the returned results.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetNumFileResults() -> Dword {
    count(|p| p.rows.iter().filter(|r| !r.is_folder()).count() as u64)
}

/// Folders among the returned results.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetNumFolderResults() -> Dword {
    count(|p| p.rows.iter().filter(|r| r.is_folder()).count() as u64)
}

/// Results of the whole query.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetTotResults() -> Dword {
    count(|p| p.total)
}

/// Is result `index` a folder?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_IsFolderResult(index: Dword) -> Bool {
    with_row(index, 0, |st, i| Bool::from(st.page.as_ref().is_some_and(|p| p.rows[i].flags & FLAG_FOLDER != 0)))
}

/// Is result `index` a file?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_IsFileResult(index: Dword) -> Bool {
    with_row(index, 0, |st, i| Bool::from(st.page.as_ref().is_some_and(|p| p.rows[i].flags & FLAG_FOLDER == 0)))
}

/// Is result `index` a volume root?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_IsVolumeResult(index: Dword) -> Bool {
    with_row(index, 0, |st, i| Bool::from(st.page.as_ref().is_some_and(|p| p.rows[i].flags & FLAG_ROOT != 0)))
}

/// File name of result `index` (NULL on error).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultFileNameW(index: Dword) -> *const Wchar {
    wide_ptr(index, request::NAME, |w| &w.name)
}

/// File name (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultFileNameA(index: Dword) -> *const c_char {
    narrow_ptr(index, request::NAME, |w| &w.name)
}

/// Parent path of result `index`.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultPathW(index: Dword) -> *const Wchar {
    wide_ptr(index, request::PATH, |w| &w.path)
}

/// Parent path (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultPathA(index: Dword) -> *const c_char {
    narrow_ptr(index, request::PATH, |w| &w.path)
}

/// Extension.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultExtensionW(index: Dword) -> *const Wchar {
    wide_ptr(index, request::EXTENSION, |w| &w.ext)
}

/// Extension (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultExtensionA(index: Dword) -> *const c_char {
    narrow_ptr(index, request::EXTENSION, |w| &w.ext)
}

/// File list file name.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultFileListFileNameW(index: Dword) -> *const Wchar {
    wide_ptr(index, request::FILE_LIST_FILE_NAME, |w| &w.file_list)
}

/// File list file name (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultFileListFileNameA(index: Dword) -> *const c_char {
    narrow_ptr(index, request::FILE_LIST_FILE_NAME, |w| &w.file_list)
}

/// Highlighted file name (`*` toggles, `**` = `*`).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultHighlightedFileNameW(index: Dword) -> *const Wchar {
    wide_ptr(index, request::HIGHLIGHTED_NAME, |w| &w.hl_name)
}

/// Highlighted file name (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultHighlightedFileNameA(index: Dword) -> *const c_char {
    narrow_ptr(index, request::HIGHLIGHTED_NAME, |w| &w.hl_name)
}

/// Highlighted path.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultHighlightedPathW(index: Dword) -> *const Wchar {
    wide_ptr(index, request::HIGHLIGHTED_PATH, |w| &w.hl_path)
}

/// Highlighted path (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultHighlightedPathA(index: Dword) -> *const c_char {
    narrow_ptr(index, request::HIGHLIGHTED_PATH, |w| &w.hl_path)
}

/// Highlighted full path and name.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultHighlightedFullPathAndFileNameW(index: Dword) -> *const Wchar {
    wide_ptr(index, request::HIGHLIGHTED_FULL_PATH_AND_NAME, |w| &w.hl_full)
}

/// Highlighted full path and name (UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultHighlightedFullPathAndFileNameA(index: Dword) -> *const c_char {
    narrow_ptr(index, request::HIGHLIGHTED_FULL_PATH_AND_NAME, |w| &w.hl_full)
}

/// Copies the full path of result `index` into `buf` (`max` units incl. the terminator).
/// Returns the units copied; with `buf` NULL, the length needed (without terminator).
/// Needs `FULL_PATH_AND_NAME` or `NAME | PATH` in the request flags.
///
/// # Safety
/// `buf` must be NULL or valid for `max` UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_GetResultFullPathNameW(index: Dword, buf: *mut Wchar, max: Dword) -> Dword {
    let full: Option<Vec<Wchar>> = with_row(index, None, |st, i| {
        let have = st.page.as_ref().is_some_and(|p| {
            p.request_flags & request::FULL_PATH_AND_NAME != 0
                || p.request_flags & (request::NAME | request::PATH) == request::NAME | request::PATH
        });
        if !have {
            st.last_error = FNC_ERROR_INVALIDREQUEST;
            return None;
        }
        st.page.as_ref().and_then(|p| p.wide.get(i)).map(|w| w.full[..w.full.len() - 1].to_vec())
    });
    let src = full.unwrap_or_default();
    // SAFETY: forwarded caller contract on `buf`/`max`.
    unsafe { copy_out(&src, buf, max) }
}

/// UTF-8 variant of [`fnc_GetResultFullPathNameW`] (`max` in bytes).
///
/// # Safety
/// `buf` must be NULL or valid for `max` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_GetResultFullPathNameA(index: Dword, buf: *mut c_char, max: Dword) -> Dword {
    let full: Option<Vec<c_char>> = with_row(index, None, |st, i| {
        st.page.as_ref().and_then(|p| p.rows.get(i)).map(|r| r.full_path.bytes().map(|b| b as c_char).collect())
    });
    let src = full.unwrap_or_default();
    // SAFETY: forwarded caller contract on `buf`/`max`.
    unsafe { copy_out(&src, buf, max) }
}

/// Writes a 64-bit value through `out`; FALSE if unknown or not requested.
///
/// # Safety
/// `out` must be NULL or valid for one write.
unsafe fn put_u64(out: *mut u64, v: u64) -> Bool {
    if out.is_null() {
        return 0;
    }
    // SAFETY: caller guarantees `out` is writable.
    unsafe { out.write_unaligned(v) };
    Bool::from(v != UNKNOWN)
}

/// Size of result `index` (`LARGE_INTEGER`; -1 for folders/unknown). Returns FALSE if
/// unknown or not requested.
///
/// # Safety
/// `size` must be NULL or valid for one 64-bit write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_GetResultSize(index: Dword, size: *mut u64) -> Bool {
    let v = row_value(index, request::SIZE, UNKNOWN, |r| r.size);
    // SAFETY: forwarded caller contract.
    unsafe { put_u64(size, v) }
}

macro_rules! date_getter {
    ($(#[$doc:meta])* $name:ident, $bit:expr, $field:ident) => {
        $(#[$doc])*
        ///
        /// # Safety
        /// `filetime` must be NULL or valid for one 64-bit (`FILETIME`) write.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(index: Dword, filetime: *mut u64) -> Bool {
            let v = row_value(index, $bit, UNKNOWN, |r| r.$field);
            // SAFETY: forwarded caller contract.
            unsafe { put_u64(filetime, v) }
        }
    };
}

date_getter!(
    /// Date created of result `index`.
    fnc_GetResultDateCreated, request::DATE_CREATED, date_created
);
date_getter!(
    /// Date modified of result `index`.
    fnc_GetResultDateModified, request::DATE_MODIFIED, date_modified
);
date_getter!(
    /// Date accessed of result `index`.
    fnc_GetResultDateAccessed, request::DATE_ACCESSED, date_accessed
);
date_getter!(
    /// Date run of result `index`.
    fnc_GetResultDateRun, request::DATE_RUN, date_run
);
date_getter!(
    /// Date recently changed of result `index`.
    fnc_GetResultDateRecentlyChanged, request::DATE_RECENTLY_CHANGED, date_recently_changed
);

/// Attributes of result `index` (`INVALID_FILE_ATTRIBUTES` on error).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultAttributes(index: Dword) -> Dword {
    row_value(index, request::ATTRIBUTES, u32::MAX, |r| r.attributes)
}

/// Run count of result `index`.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetResultRunCount(index: Dword) -> Dword {
    row_value(index, request::RUN_COUNT, 0, |r| r.run_count)
}

// ---------------------------------------------------------------- control

/// Resets the search state and frees results.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_Reset() {
    let mut st = STATE.lock();
    let client = st.client.take();
    let instance = std::mem::take(&mut st.instance);
    *st = State::new();
    st.client = client;
    st.instance = instance;
}

/// Frees everything, including the connection.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_CleanUp() {
    *STATE.lock() = State::new();
}

fn version_part(i: usize) -> Dword {
    let mut st = STATE.lock();
    match st.call(Op::GetVersion) {
        Some(Payload::Version(v)) => v.everything[i],
        _ => 0,
    }
}

/// Major version reported to Everything clients (0 if Fennec is not running).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetMajorVersion() -> Dword {
    version_part(0)
}

/// Minor version.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetMinorVersion() -> Dword {
    version_part(1)
}

/// Revision.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetRevision() -> Dword {
    version_part(2)
}

/// Build number.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetBuildNumber() -> Dword {
    version_part(3)
}

/// `EVERYTHING_IPC_TARGET_MACHINE_*` of this SDK build.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_GetTargetMachine() -> Dword {
    if cfg!(target_arch = "x86_64") {
        2
    } else if cfg!(target_arch = "aarch64") {
        4
    } else if cfg!(target_arch = "x86") {
        1
    } else {
        0
    }
}

/// Database loaded?
#[unsafe(no_mangle)]
pub extern "C" fn fnc_IsDBLoaded() -> Bool {
    let mut st = STATE.lock();
    Bool::from(matches!(st.call(Op::IsDbLoaded), Some(Payload::Bool(true))))
}

fn admin(op: Op) -> Bool {
    let mut st = STATE.lock();
    Bool::from(st.call(op).is_some())
}

/// Rebuilds the index.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_RebuildDB() -> Bool {
    admin(Op::Rebuild)
}

/// Rescans folder indexes.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_UpdateAllFolderIndexes() -> Bool {
    admin(Op::Update)
}

/// Saves the database.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SaveDB() -> Bool {
    admin(Op::SaveDb)
}

/// Saves the run history.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_SaveRunHistory() -> Bool {
    admin(Op::SaveRunHistory)
}

/// Deletes the run history.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_DeleteRunHistory() -> Bool {
    admin(Op::DeleteRunHistory)
}

/// Exits Fennec.
#[unsafe(no_mangle)]
pub extern "C" fn fnc_Exit() -> Bool {
    admin(Op::Exit)
}

/// All sorts are fast in Fennec (in-memory columns).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_IsFastSort(sort: Dword) -> Bool {
    Bool::from(decode_sort(sort).is_some())
}

/// File info indexed? (`EVERYTHING_IPC_FILE_INFO_*`: sizes, dates and attributes are;
/// folder sizes and date accessed are not reported as indexed by the SDK).
#[unsafe(no_mangle)]
pub extern "C" fn fnc_IsFileInfoIndexed(info: Dword) -> Bool {
    Bool::from(matches!(info, 1 | 3 | 4 | 6))
}

/// Run count of a file by name (0 if unknown).
///
/// # Safety
/// `file_name` must be NULL or a NUL-terminated UTF-16 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_GetRunCountFromFileNameW(file_name: *const Wchar) -> Dword {
    // SAFETY: forwarded caller contract.
    let Some(path) = (unsafe { read_wide(file_name) }) else {
        STATE.lock().last_error = FNC_ERROR_INVALIDPARAMETER;
        return 0;
    };
    let mut st = STATE.lock();
    match st.call(Op::GetRunCount { path }) {
        Some(Payload::RunCount(n)) => n,
        _ => 0,
    }
}

/// Sets the run count of a file by name.
///
/// # Safety
/// `file_name` must be NULL or a NUL-terminated UTF-16 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_SetRunCountFromFileNameW(file_name: *const Wchar, count: Dword) -> Bool {
    // SAFETY: forwarded caller contract.
    let Some(path) = (unsafe { read_wide(file_name) }) else {
        STATE.lock().last_error = FNC_ERROR_INVALIDPARAMETER;
        return 0;
    };
    admin(Op::SetRunCount { path, count })
}

/// Increments the run count of a file by name; returns the new count (0 on error).
///
/// # Safety
/// `file_name` must be NULL or a NUL-terminated UTF-16 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fnc_IncRunCountFromFileNameW(file_name: *const Wchar) -> Dword {
    // SAFETY: forwarded caller contract.
    let Some(path) = (unsafe { read_wide(file_name) }) else {
        STATE.lock().last_error = FNC_ERROR_INVALIDPARAMETER;
        return 0;
    };
    let mut st = STATE.lock();
    match st.call(Op::IncRunCount { path }) {
        Some(Payload::RunCount(n)) => n,
        _ => 0,
    }
}

//! SDK against a real IPC server: Rust API and the C ABI.

// Test-only helpers outside `#[test]` functions; the C ABI is unsafe by nature.
#![allow(clippy::unwrap_used, unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use fennec_sdk::ffi::*;
use fennec_sdk::{Column, Fennec};
use fnc_index::{IndexStore, SharedIndex};
use fnc_ipc::backend::{DefaultHost, IndexBackend};
use fnc_ipc::{ServerConfig, ServerHandle};

fn server(tag: &str) -> (String, ServerHandle) {
    let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
               \"C:\\Music\",,,,16\n\
               \"C:\\Music\\abc one.mp3\",300,133000000000000003,,32\n\
               \"C:\\Music\\abc two.mp3\",100,133000000000000001,,32\n\
               \"C:\\Music\\abc three.flac\",200,133000000000000002,,32\n\
               \"C:\\Other\\zzz.txt\",1,,,32\n";
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
    s.rebuild_name_order();
    let backend = Arc::new(IndexBackend::new(SharedIndex::new(s), Arc::new(DefaultHost)));
    let inst = format!("sdk-{tag}-{}", std::process::id());
    let h = fnc_ipc::spawn(&inst, backend, ServerConfig::default()).unwrap();
    (inst, h)
}

#[test]
fn rust_api() {
    let (inst, _h) = server("rust");
    let r = Fennec::connect_instance(&inst, Duration::from_secs(2))
        .unwrap()
        .search("abc")
        .sort(Column::Size, false)
        .limit(2)
        .run()
        .unwrap();
    assert_eq!(r.total, 3);
    let names: Vec<&str> = r.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["abc one.mp3", "abc three.flac"]);
    assert_eq!(r.get(0).unwrap().full_path, "C:\\Music\\abc one.mp3");
    assert_eq!(r.get(0).unwrap().size, 300);
    let mut f = Fennec::connect_instance(&inst, Duration::from_secs(2)).unwrap();
    assert_eq!(f.result_count("mp3").unwrap(), 2);
    assert_eq!(f.total_size("abc").unwrap(), 600);
    let r = f.search("ABC").case(true).run().unwrap();
    assert!(r.is_empty());
    let r = f.search("^abc t").regex(true).columns(&[Column::Name]).highlight(true).run().unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r.get(0).unwrap().highlighted_name.as_deref(), Some("*abc t*hree.flac"));
    assert_eq!(f.version().unwrap().protocol, fnc_ipc::proto::PROTOCOL_VERSION);
    assert!(Fennec::connect_instance("sdk-nobody", Duration::from_millis(50)).is_err());
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// # Safety
/// `p` must point to a NUL-terminated UTF-16 string.
unsafe fn from_w(p: *const u16) -> String {
    assert!(!p.is_null());
    let mut n = 0;
    // SAFETY: the SDK returns NUL-terminated strings.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

#[test]
fn c_abi() {
    let (inst, _h) = server("ffi");
    // SAFETY: all pointers passed are valid NUL-terminated buffers owned by this test.
    unsafe {
        fnc_SetInstanceW(w(&inst).as_ptr());
        fnc_SetSearchW(w("abc").as_ptr());
        assert_eq!(from_w(fnc_GetSearchW()), "abc");
        fnc_SetSort(6); // size descending
        fnc_SetRequestFlags(0x1 | 0x2 | 0x10 | 0x40 | 0x100);
        assert_eq!(fnc_QueryW(1), 1, "error {}", fnc_GetLastError());
        assert_eq!(fnc_GetNumResults(), 3);
        assert_eq!(fnc_GetTotResults(), 3);
        assert_eq!(fnc_GetNumFileResults(), 3);
        assert_eq!(fnc_GetResultListSort(), 6);
        assert_eq!(from_w(fnc_GetResultFileNameW(0)), "abc one.mp3");
        assert_eq!(from_w(fnc_GetResultPathW(0)), "C:\\Music");
        let mut buf = [0u16; 260];
        let n = fnc_GetResultFullPathNameW(0, buf.as_mut_ptr(), buf.len() as u32);
        assert_eq!(String::from_utf16_lossy(&buf[..n as usize]), "C:\\Music\\abc one.mp3");
        assert_eq!(fnc_GetResultFullPathNameW(0, std::ptr::null_mut(), 0), 20);
        let mut small = [0u16; 4];
        assert_eq!(fnc_GetResultFullPathNameW(0, small.as_mut_ptr(), 4), 3);
        assert_eq!(&small, &[b'C' as u16, b':' as u16, b'\\' as u16, 0]);
        let mut size = 0u64;
        assert_eq!(fnc_GetResultSize(0, &mut size), 1);
        assert_eq!(size, 300);
        let mut dm = 0u64;
        assert_eq!(fnc_GetResultDateModified(1, &mut dm), 1);
        assert_eq!(dm, 133000000000000002);
        assert_eq!(fnc_GetResultAttributes(0), 32);
        assert_eq!(fnc_IsFileResult(0), 1);
        assert_eq!(fnc_IsFolderResult(0), 0);
        // Not requested → NULL + INVALIDREQUEST; bad index → INVALIDINDEX.
        assert!(fnc_GetResultExtensionW(0).is_null());
        assert_eq!(fnc_GetLastError(), FNC_ERROR_INVALIDREQUEST);
        assert!(fnc_GetResultFileNameW(99).is_null());
        assert_eq!(fnc_GetLastError(), FNC_ERROR_INVALIDINDEX);
        // UTF-8 variants and paging.
        fnc_SetSearchA(c"mp3".as_ptr());
        fnc_SetSort(1);
        fnc_SetMax(1);
        fnc_SetOffset(1);
        assert_eq!(fnc_QueryA(1), 1);
        assert_eq!((fnc_GetNumResults(), fnc_GetTotResults()), (1, 2));
        let name = std::ffi::CStr::from_ptr(fnc_GetResultFileNameA(0)).to_str().unwrap().to_string();
        assert_eq!(name, "abc two.mp3");
        // Run counts, version, admin.
        assert_eq!(fnc_IncRunCountFromFileNameW(w("C:\\Music\\abc two.mp3").as_ptr()), 1);
        assert_eq!(fnc_GetRunCountFromFileNameW(w("c:\\music\\ABC TWO.mp3").as_ptr()), 1);
        assert_eq!(fnc_SetRunCountFromFileNameW(w("C:\\Music\\abc two.mp3").as_ptr(), 9), 1);
        assert_eq!(fnc_GetRunCountFromFileNameW(w("C:\\Music\\abc two.mp3").as_ptr()), 9);
        assert_eq!(fnc_GetMajorVersion(), 1);
        assert_eq!(fnc_GetMinorVersion(), 4);
        assert_eq!(fnc_IsDBLoaded(), 1);
        assert_eq!(fnc_RebuildDB(), 0, "default host does not rebuild");
        assert_eq!(fnc_IsFastSort(6), 1);
        // Reset keeps the instance, drops results.
        fnc_Reset();
        assert_eq!(fnc_GetMax(), u32::MAX);
        assert_eq!(fnc_GetNumResults(), 0);
        assert_eq!(fnc_GetLastError(), FNC_ERROR_INVALIDCALL);
        fnc_SetSearchW(w("zzz").as_ptr());
        assert_eq!(fnc_QueryW(0), 1);
        assert_eq!(fnc_GetNumResults(), 1);
        fnc_CleanUp();
        // Wrong instance → IPC error.
        fnc_SetInstanceW(w("sdk-nobody").as_ptr());
        assert_eq!(fnc_QueryW(1), 0);
        assert_eq!(fnc_GetLastError(), FNC_ERROR_IPC);
        fnc_CleanUp();
    }
}

#[test]
fn header_matches_exports() {
    let header = include_str!("../include/fennec_sdk.h");
    let ffi = include_str!("../src/ffi.rs");
    let mut declared: Vec<&str> = header
        .lines()
        .filter(|l| l.starts_with("FNC_API"))
        .map(|l| l.split('(').next().unwrap().rsplit([' ', '*']).next().unwrap())
        .collect();
    // `extern "C" fn fnc_X(` plus the `date_getter!` invocations (`    fnc_X, request::...`).
    let mut exported: Vec<&str> = ffi
        .split("extern \"C\" fn ")
        .skip(1)
        .map(|rest| rest.split('(').next().unwrap())
        .filter(|name| name.starts_with("fnc_"))
        .chain(
            ffi.lines()
                .map(str::trim)
                .filter(|l| l.starts_with("fnc_") && l.contains(", request::"))
                .map(|l| l.split(',').next().unwrap()),
        )
        .collect();
    declared.sort_unstable();
    exported.sort_unstable();
    assert_eq!(declared, exported);
    assert!(declared.len() > 60);
}

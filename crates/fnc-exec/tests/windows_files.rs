//! Windows check of the file-reading functions on real files (not run by default):
//! `.lnk` shortcuts, junctions, symbolic links, file signatures and recycle bin items.
//!
//! The caller prepares `FENNEC_WIN_FILES` (a folder with `notepad.lnk` → notepad.exe,
//! `jdir` = junction and `sdir` = directory symlink to `target-dir`, `sfile` = file
//! symlink to `target-dir\target.txt`, `site.url`, `photo.dat` = a JPEG, `exe.dat` = a PE
//! executable, `lib.dat` = a DLL, `arch.dat` = a ZIP) and deletes one file to the recycle
//! bin, passing its original path in `FENNEC_WIN_RECYCLED`; then
//! `cargo test -p fnc-exec --test windows_files -- --ignored --nocapture`.

#![cfg(windows)]
// Test-only helpers outside `#[test]` functions.
#![allow(clippy::unwrap_used)]

use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use fnc_core::FileTime;
use fnc_exec::{Caches, CancelToken, ExecCtx, ExtCtx, execute_ext};
use fnc_index::IndexStore;
use fnc_index::efu::EfuWriter;
use fnc_props::ondemand::{DefaultOnDemand, OnDemand};
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

/// Writes `dir` and everything below it (links are not followed) as EFU records.
fn walk(dir: &Path, out: &mut EfuWriter<&mut Vec<u8>>, depth: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
        let attrs = m.file_attributes();
        let is_dir = attrs & 0x10 != 0;
        let size = (!is_dir).then(|| m.file_size());
        let dm = FileTime(i64::try_from(m.last_write_time()).unwrap_or(0));
        out.write(&p.to_string_lossy(), size, dm, FileTime(0), attrs).unwrap();
        if is_dir && attrs & 0x400 == 0 && depth < 8 {
            walk(&p, out, depth + 1);
        }
    }
}

#[test]
#[ignore = "needs prepared files on Windows (see the module docs)"]
fn real_links_signatures_and_recycle_bin() {
    let dir = std::env::var("FENNEC_WIN_FILES").expect("FENNEC_WIN_FILES");
    let recycled = std::env::var("FENNEC_WIN_RECYCLED").expect("FENNEC_WIN_RECYCLED");
    let mut efu = Vec::new();
    {
        let mut w = EfuWriter::new(&mut efu).unwrap();
        let m = std::fs::metadata(&dir).unwrap();
        w.write(&dir, None, FileTime(0), FileTime(0), m.file_attributes()).unwrap();
        walk(Path::new(&dir), &mut w, 0);
        let bin = format!("{}\\$Recycle.Bin", &dir[..2]);
        w.write(&bin, None, FileTime(0), FileTime(0), 0x16).unwrap();
        walk(Path::new(&bin), &mut w, 0);
    }
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, efu.as_slice(), "win.efu").unwrap();
    s.rebuild_name_order();
    let od: Arc<dyn OnDemand> = Arc::new(DefaultOnDemand::default());
    let caches = Caches::new();
    let names = |q: &str| -> Vec<String> {
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let cancel = CancelToken::new();
        let ext = ExtCtx { on_demand: Some(&od), ..ExtCtx::default() };
        let rs = execute_ext(&p, &s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }, &ext).unwrap();
        let mut v: Vec<String> = rs.ids.iter().map(|&i| s.name_string(i)).collect();
        v.sort();
        println!("{q:<50} -> {v:?} {:?}", rs.stats.warnings);
        v
    };
    let has = |v: &[String], n: &str| v.iter().any(|x| x.eq_ignore_ascii_case(n));
    let all_links = names("target:");
    for n in ["notepad.lnk", "jdir", "sdir", "sfile", "site.url"] {
        assert!(has(&all_links, n), "{n} in {all_links:?}");
    }
    assert!(has(&names("target:notepad.exe"), "notepad.lnk"));
    assert!(has(&names("target:*.exe"), "notepad.lnk"));
    let t = names(&format!("wfn:target:\"{dir}\\target-dir\""));
    assert!(has(&t, "jdir") && has(&t, "sdir"), "{t:?}");
    assert_eq!(names("target:target.txt"), ["sfile"]);
    assert!(has(&names("target:example.com"), "site.url"));
    assert!(has(&names("fsi:jpg"), "photo.dat"));
    assert!(has(&names("fsi:exe"), "exe.dat"));
    assert!(has(&names("fsi:dll"), "lib.dat"));
    assert!(has(&names("fsi:zip !ext:zip"), "arch.dat"));
    assert!(has(&names("fsi:lnk"), "notepad.lnk"));
    assert!(!names("fsi:").iter().any(|n| n == "photo.dat"));
    let bin = names("recycled:");
    assert!(bin.iter().any(|n| n.starts_with("$R")), "{bin:?}");
    assert!(!bin.iter().any(|n| n.starts_with("$I")), "{bin:?}");
    let orig = names(&format!("wfn:recycled:\"{recycled}\""));
    assert_eq!(orig.len(), 1, "{orig:?}");
    assert!(orig[0].starts_with("$R"));
    let file_name = Path::new(&recycled).file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(names(&format!("recycled:\"{file_name}\"")), orig);
}

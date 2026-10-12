//! Executor tests against a small synthetic index.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fnc_core::settings::FoldersFirst;
use fnc_index::{Change, EntryRef, IndexStore, Meta, SharedIndex, efu};
use fnc_query::plan::InputSet;
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

use crate::*;

const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
\"C:\\Windows\",,,,16\n\
\"C:\\Windows\\notepad.exe\",200000,133500000000000000,133500000000000000,32\n\
\"C:\\Windows\\System32\",,,,16\n\
\"C:\\Windows\\System32\\cmd.exe\",300000,133600000000000000,133500000000000000,32\n\
\"C:\\Windows\\System32\\drivers\",,,,16\n\
\"C:\\Users\\bob\\Photos\\IMG_0001.JPG\",3000000,134000000000000000,133500000000000000,32\n\
\"C:\\Users\\bob\\Photos\\IMG_0002.jpg\",4000000,134000000000000001,133500000000000000,32\n\
\"C:\\Users\\bob\\Photos\\holiday.png\",500,134000000000000000,133500000000000000,34\n\
\"C:\\Users\\bob\\Documents\\Report Q3.docx\",12000,134100000000000000,133500000000000000,32\n\
\"C:\\Users\\bob\\Documents\\café menu.txt\",0,134100000000000000,133500000000000000,33\n\
\"C:\\Users\\bob\\Documents\\季度预算表.xlsx\",1048576,134100000000000000,133500000000000000,32\n\
\"C:\\Users\\bob\\Empty\",,,,16\n\
\"C:\\Users\\bob\\file2.txt\",10,,,32\n\
\"C:\\Users\\bob\\file10.txt\",20,,,32\n\
\"C:\\Users\\bob\\README\",30,,,32\n\
\"D:\\Music\\track.flac\",9000000,,,32\n";

fn store() -> IndexStore {
    let mut s = IndexStore::default();
    efu::load_efu(&mut s, EFU.as_bytes(), "t.efu").unwrap();
    s.set_volume_online(fnc_core::VolumeId(0), true);
    s.rebuild_name_order();
    s
}

fn run_on(s: &IndexStore, q: &str, sort: Option<SortSpec>) -> Vec<String> {
    let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let mut rs = execute(&p, s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }).unwrap();
    if let Some(spec) = sort {
        sort_ids(&mut rs.ids, &spec, s, &caches, &cancel).unwrap();
    }
    let mut buf = String::new();
    rs.ids
        .iter()
        .map(|&id| {
            s.full_path(id, &mut buf);
            buf.clone()
        })
        .collect()
}

fn leaf_name(p: &str) -> String {
    p.rsplit('\\').next().unwrap_or("").to_string()
}

fn names(q: &str) -> BTreeSet<String> {
    let s = store();
    run_on(&s, q, None).iter().map(|p| leaf_name(p)).collect()
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

const ALL_FOLDERS: &[&str] =
    &["C:", "D:", "Windows", "System32", "drivers", "Users", "bob", "Photos", "Documents", "Empty", "Music"];

#[test]
fn literal_wildcard_regex() {
    assert_eq!(names("img"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("*.jpg"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("*.jpg | *.png"), set(&["IMG_0001.JPG", "IMG_0002.jpg", "holiday.png"]));
    assert_eq!(names("regex:^IMG_\\d{4}"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("case:IMG_0002.jpg"), set(&["IMG_0002.jpg"]));
    assert_eq!(names("case:img"), set(&[]));
    assert_eq!(names("cafe"), set(&["café menu.txt"]));
    assert_eq!(names("diacritics:cafe"), set(&[]));
    assert_eq!(names("预算"), set(&["季度预算表.xlsx"]));
    assert_eq!(names("report q3"), set(&["Report Q3.docx"]));
    assert_eq!(names("\"report q3\""), set(&["Report Q3.docx"]));
    assert_eq!(names("ww:report"), set(&["Report Q3.docx"]));
    assert_eq!(names("ww:repor"), set(&[]));
    assert_eq!(names("wfn:readme"), set(&["README"]));
    assert_eq!(names("regex:(bad"), set(&[]));
}

#[test]
fn boolean_operators() {
    assert_eq!(names("img !0001"), set(&["IMG_0002.jpg"]));
    assert_eq!(names("file !10"), set(&["file2.txt"]));
    assert_eq!(names("<img|holiday> !png"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
}

#[test]
fn functions() {
    assert_eq!(names("ext:jpg;png"), set(&["IMG_0001.JPG", "IMG_0002.jpg", "holiday.png"]));
    assert_eq!(names("ext:"), set(&["README"]));
    assert_eq!(names("ext:x*"), set(&["季度预算表.xlsx"]));
    assert_eq!(names("size:>1mb"), set(&["IMG_0001.JPG", "IMG_0002.jpg", "track.flac"]));
    assert_eq!(names("size:1mb"), set(&["季度预算表.xlsx"]));
    assert_eq!(names("size:empty"), set(&["café menu.txt"]));
    assert_eq!(names("size:10..20"), set(&["file2.txt", "file10.txt"]));
    assert_eq!(names("folder:"), set(ALL_FOLDERS));
    assert_eq!(names("folder:sys"), set(&["System32"]));
    assert_eq!(names("file:txt"), set(&["café menu.txt", "file2.txt", "file10.txt"]));
    assert_eq!(names("\\ sys"), set(&["System32"]));
    assert_eq!(names("empty:"), set(&["drivers", "Empty", "café menu.txt"]));
    assert_eq!(names("attrib:h"), set(&["holiday.png"]));
    assert_eq!(names("attrib:r"), set(&["café menu.txt"]));
    assert_eq!(names("!attrib:a"), set(ALL_FOLDERS));
    assert_eq!(names("root:"), set(&["C:", "D:"]));
    assert_eq!(names("depth:1 folder:"), set(&["Windows", "Users", "Music"]));
    assert_eq!(names("childcount:2"), set(&["C:", "Windows", "System32"]));
    assert_eq!(names("childfilecount:>2"), set(&["bob", "Photos", "Documents"]));
    assert_eq!(names("childfoldercount:3"), set(&["bob"]));
    assert_eq!(
        names("dm:>133600000000000000"),
        set(&["IMG_0001.JPG", "IMG_0002.jpg", "holiday.png", "Report Q3.docx", "café menu.txt", "季度预算表.xlsx"])
    );
    assert_eq!(names("namelength:3"), set(&["bob"]));
    assert_eq!(names("audio:"), set(&["track.flac"]));
    assert_eq!(names("pic: !png"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("kind:folder empty"), set(&["Empty"]));
    assert_eq!(names("volume:d"), set(&["D:", "Music", "track.flac"]));
    assert_eq!(names("volume-kind:filelist ext:flac"), set(&["track.flac"]));
    assert_eq!(names("width:>100"), set(&[]));
}

#[test]
fn path_matching() {
    assert_eq!(names("windows\\"), set(&["notepad.exe", "System32", "cmd.exe", "drivers"]));
    assert_eq!(names("c:\\windows\\*"), set(&["notepad.exe", "System32"]));
    assert_eq!(names("c:\\windows\\**"), set(&["notepad.exe", "System32", "cmd.exe", "drivers"]));
    assert_eq!(names("system32\\cmd"), set(&["cmd.exe"]));
    assert_eq!(names("path:photos jpg"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    // "documents\café" straddles the separator.
    assert_eq!(names("s\\c"), set(&["café menu.txt"]));
    assert_eq!(names("parent:c:\\windows"), set(&["notepad.exe", "System32"]));
    assert_eq!(names("parent:\"c:\\windows\\\""), set(&["notepad.exe", "System32"]));
    assert_eq!(names("infolder:c:\\users\\bob\\photos ext:png"), set(&["holiday.png"]));
    assert_eq!(names("ancestor:c:\\windows file:"), set(&["notepad.exe", "cmd.exe"]));
    assert_eq!(names("child:*.flac"), set(&["Music"]));
    assert_eq!(names("folder: child:*.jpg"), set(&["Photos"]));
    assert_eq!(names("pathpart:system32"), set(&["cmd.exe", "drivers"]));
    assert_eq!(names("fullpath:\"bob\\file2\""), set(&["file2.txt"]));
    assert_eq!(names("case:path:Windows\\System32"), set(&["System32", "cmd.exe", "drivers"]));
    assert_eq!(names("case:path:windows\\system32"), set(&[]));
    assert_eq!(names("c:/windows/system32/"), set(&["cmd.exe", "drivers"]));
}

#[test]
fn sorting() {
    let s = store();
    let name = SortSpec { column: SortColumn::Name, ascending: true, folders_first: FoldersFirst::Never };
    let r = run_on(&s, "file", Some(name));
    assert_eq!(r.iter().map(|p| leaf_name(p)).collect::<Vec<_>>(), vec!["file2.txt", "file10.txt"]);
    let desc = SortSpec { ascending: false, ..name };
    let r = run_on(&s, "file", Some(desc));
    assert_eq!(r.iter().map(|p| leaf_name(p)).collect::<Vec<_>>(), vec!["file10.txt", "file2.txt"]);
    let size = SortSpec { column: SortColumn::Size, ascending: false, folders_first: FoldersFirst::Never };
    let r = run_on(&s, "ext:jpg;png;flac", Some(size));
    assert_eq!(
        r.iter().map(|p| leaf_name(p)).collect::<Vec<_>>(),
        vec!["track.flac", "IMG_0002.jpg", "IMG_0001.JPG", "holiday.png"]
    );
    let ff = SortSpec { column: SortColumn::Name, ascending: true, folders_first: FoldersFirst::Ascending };
    let r = run_on(&s, "bob\\", Some(ff));
    let first_file = r.iter().position(|p| p.contains('.') || p.ends_with("README")).unwrap();
    assert!(r[..first_file].iter().all(|p| !p.contains('.')), "{r:?}");
    let path = SortSpec { column: SortColumn::Path, ascending: true, folders_first: FoldersFirst::Never };
    let r = run_on(&s, "file:", Some(path));
    assert_eq!(r.first().map(String::as_str), Some("C:\\Users\\bob\\file2.txt"));
    assert_eq!(r.last().map(String::as_str), Some("D:\\Music\\track.flac"));
    // Renamed entries (dirty name ranks) still sort correctly.
    let mut s2 = store();
    let id = (0..s2.len() as u32).map(fnc_core::EntryId).find(|&i| s2.name_string(i) == "file2.txt").unwrap();
    s2.apply(vec![Change::Rename {
        target: EntryRef::Id(id),
        new_name: "file99.txt".encode_utf16().collect(),
        new_parent: EntryRef::Id(s2.parent(id)),
    }]);
    let r = run_on(&s2, "file", Some(name));
    assert_eq!(r.iter().map(|p| leaf_name(p)).collect::<Vec<_>>(), vec!["file10.txt", "file99.txt"]);
}

#[test]
fn reuse_previous_results() {
    let s = store();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let env = PlanEnv::default();
    let pe = ParseEnv::default();
    let p1 = plan(&parse("i", &pe).ast, &env, None);
    let r1 = execute(&p1, &s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }).unwrap();
    assert!(!r1.stats.reused);
    let p2 = plan(&parse("img", &pe).ast, &env, Some(&p1));
    assert_eq!(p2.input, InputSet::Previous);
    let r2 = execute(&p2, &s, &ExecCtx { cancel: &cancel, prev: Some(&r1), caches: &caches }).unwrap();
    assert!(r2.stats.reused);
    assert_eq!(r2.stats.examined, r1.len());
    assert_eq!(r2.len(), 2);
    // Stale previous results (index changed) are not reused.
    let mut s3 = s.clone();
    s3.record_run(r2.ids[0]);
    let r3 = execute(&p2, &s3, &ExecCtx { cancel: &cancel, prev: Some(&r1), caches: &caches }).unwrap();
    assert!(!r3.stats.reused);
}

#[test]
fn cancellation() {
    let s = store();
    let cancel = CancelToken::new();
    cancel.cancel();
    let p = plan(&parse("a", &ParseEnv::default()).ast, &PlanEnv::default(), None);
    assert!(execute(&p, &s, &ExecCtx { cancel: &cancel, prev: None, caches: &Caches::new() }).is_err());
}

#[test]
fn deleted_entries_are_skipped() {
    let mut s = store();
    let photos = (0..s.len() as u32).map(fnc_core::EntryId).find(|&i| s.name_string(i) == "Photos").unwrap();
    s.apply(vec![Change::Delete(EntryRef::Id(photos))]);
    assert_eq!(run_on(&s, "*.jpg", None), Vec::<String>::new());
    let users = (0..s.len() as u32).map(fnc_core::EntryId).find(|&i| s.name_string(i) == "Users").unwrap();
    s.apply(vec![Change::Create {
        volume: fnc_core::VolumeId(0),
        frn: 9999,
        parent: EntryRef::Id(users),
        name: "new.jpg".encode_utf16().collect(),
        meta: Meta::attrs(32),
    }]);
    assert_eq!(run_on(&s, "*.jpg", None), vec!["C:\\Users\\new.jpg".to_string()]);
    assert_eq!(run_on(&s, "users\\new", None), vec!["C:\\Users\\new.jpg".to_string()]);
}

fn engine_with(store: IndexStore) -> (SearchEngine, crossbeam_channel::Receiver<EngineEvent>, SharedIndex) {
    let shared = SharedIndex::new(store);
    let (tx, rx) = crossbeam_channel::unbounded();
    (SearchEngine::spawn(shared.clone(), tx, Box::new(|| {})), rx, shared)
}

fn req(id: u64, text: &str) -> SearchRequest {
    SearchRequest {
        id,
        text: text.into(),
        filter: None,
        sort: SortSpec::default(),
        options: Arc::new(SearchOptions::default()),
    }
}

#[test]
fn engine_latest_query_wins() {
    // UI-02: typing `a` then `bc` quickly; intermediate queries may be cancelled but the
    // last outcome must be for `abc`... here `img` after `i`, `im`.
    let (engine, rx, _) = engine_with(store());
    engine.search(req(1, "i"));
    engine.search(req(2, "im"));
    engine.search(req(3, "img"));
    let mut last = None;
    while let Ok(EngineEvent::Done(o)) = rx.recv_timeout(Duration::from_secs(5)) {
        let done = o.id == 3;
        last = Some(o);
        if done {
            break;
        }
    }
    let last = last.unwrap();
    assert_eq!(last.id, 3);
    assert_eq!(last.results.len(), 2);
}

#[test]
fn engine_refresh_after_index_change() {
    let (engine, rx, shared) = engine_with(store());
    engine.search(req(1, "*.jpg"));
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(o.results.len(), 2);
    {
        let mut w = shared.write();
        let users = (0..w.len() as u32).map(fnc_core::EntryId).find(|&i| w.name_string(i) == "Users").unwrap();
        w.apply(vec![Change::Create {
            volume: fnc_core::VolumeId(0),
            frn: 777,
            parent: EntryRef::Id(users),
            name: "a.jpg".encode_utf16().collect(),
            meta: Meta::attrs(32),
        }]);
    }
    engine.refresh();
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(o.id, 1);
    assert_eq!(o.results.len(), 3);
    // Sort-only change reuses ids.
    let mut r = req(2, "*.jpg");
    r.sort = SortSpec { column: SortColumn::Size, ascending: false, folders_first: FoldersFirst::Never };
    engine.search(r);
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(o.results.len(), 3);
    assert!(o.results.stats.reused);
    assert_eq!(o.results.stats.examined, 0);
}

#[test]
fn engine_concurrent_search_and_refresh_never_drop_the_last_search() {
    let (engine, rx, _) = engine_with(store());
    let handle = engine.handle();
    let refresher = std::thread::spawn(move || {
        for _ in 0..500 {
            handle.refresh();
        }
    });
    for i in 1..=200 {
        engine.search(req(i, if i % 2 == 0 { "img" } else { "txt" }));
    }
    refresher.join().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(10)).expect("final search lost");
        if o.id == 200 && o.text == "img" {
            assert_eq!(o.results.len(), 2);
            break;
        }
        assert!(std::time::Instant::now() < deadline);
    }
}

#[test]
fn engine_filter_and_regex_all() {
    let (engine, rx, _) = engine_with(store());
    let mut r = req(1, "0001");
    r.filter = Some("pic:".into());
    engine.search(r);
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(o.results.len(), 1);
    let mut r = req(2, "^file\\d+\\.txt$");
    r.options = Arc::new(SearchOptions { regex_all: true, ..SearchOptions::default() });
    engine.search(r);
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(o.results.len(), 2);
}

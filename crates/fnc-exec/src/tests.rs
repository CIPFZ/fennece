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
    let name = SortSpec {
        column: SortColumn::Name,
        ascending: true,
        folders_first: FoldersFirst::Never,
        then: [None; 2],
        props: [None; 3],
        custom: [None; 3],
        files: [None; 3],
    };
    let r = run_on(&s, "file", Some(name));
    assert_eq!(r.iter().map(|p| leaf_name(p)).collect::<Vec<_>>(), vec!["file2.txt", "file10.txt"]);
    let desc = SortSpec { ascending: false, ..name };
    let r = run_on(&s, "file", Some(desc));
    assert_eq!(r.iter().map(|p| leaf_name(p)).collect::<Vec<_>>(), vec!["file10.txt", "file2.txt"]);
    let size = SortSpec {
        column: SortColumn::Size,
        ascending: false,
        folders_first: FoldersFirst::Never,
        then: [None; 2],
        props: [None; 3],
        custom: [None; 3],
        files: [None; 3],
    };
    let r = run_on(&s, "ext:jpg;png;flac", Some(size));
    assert_eq!(
        r.iter().map(|p| leaf_name(p)).collect::<Vec<_>>(),
        vec!["track.flac", "IMG_0002.jpg", "IMG_0001.JPG", "holiday.png"]
    );
    let ff = SortSpec {
        column: SortColumn::Name,
        ascending: true,
        folders_first: FoldersFirst::Ascending,
        then: [None; 2],
        props: [None; 3],
        custom: [None; 3],
        files: [None; 3],
    };
    let r = run_on(&s, "bob\\", Some(ff));
    let first_file = r.iter().position(|p| p.contains('.') || p.ends_with("README")).unwrap();
    assert!(r[..first_file].iter().all(|p| !p.contains('.')), "{r:?}");
    let path = SortSpec {
        column: SortColumn::Path,
        ascending: true,
        folders_first: FoldersFirst::Never,
        then: [None; 2],
        props: [None; 3],
        custom: [None; 3],
        files: [None; 3],
    };
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
    r.sort = SortSpec {
        column: SortColumn::Size,
        ascending: false,
        folders_first: FoldersFirst::Never,
        then: [None; 2],
        props: [None; 3],
        custom: [None; 3],
        files: [None; 3],
    };
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

// ------------------------------------------------------------------ M2 functions

fn names_on(s: &IndexStore, q: &str, env: &PlanEnv, caches: &Caches) -> BTreeSet<String> {
    let p = plan(&parse(q, &ParseEnv::default()).ast, env, None);
    let cancel = CancelToken::new();
    let rs = execute(&p, s, &ExecCtx { cancel: &cancel, prev: None, caches }).unwrap();
    rs.ids.iter().map(|&id| s.name_string(id)).collect()
}

fn names_with(q: &str, env: &PlanEnv, caches: &Caches) -> BTreeSet<String> {
    names_on(&store(), q, env, caches)
}

const PHOTOS: &[&str] = &["IMG_0001.JPG", "IMG_0002.jpg", "holiday.png"];
const BOB_CHILDREN: &[&str] = &["Photos", "Documents", "Empty", "file2.txt", "file10.txt", "README"];

#[test]
fn descendants() {
    assert_eq!(names("descendant:*.jpg"), set(&["C:", "Users", "bob", "Photos"]));
    assert_eq!(names("descendant:cmd"), set(&["C:", "Windows", "System32"]));
    assert_eq!(names("descendant:nothing-like-this"), set(&[]));
    assert_eq!(names("descendantcount:19"), set(&["C:"]));
    assert_eq!(names("descendantcount:0 folder:"), set(&["drivers", "Empty"]));
    assert_eq!(names("descendantfilecount:>5"), set(&["C:", "Users", "bob"]));
    assert_eq!(names("descendantfoldercount:2"), set(&["Windows"]));
    assert_eq!(names("descendantfoldercount:8"), set(&["C:"]));
    // Files have no descendants (unknown), unless asked for.
    assert_eq!(names("descendantcount:unknown ext:flac"), set(&["track.flac"]));
}

#[test]
fn folder_sizes() {
    assert_eq!(names("foldersize:7000500"), set(&["Photos"]));
    assert_eq!(names("fs:1060576"), set(&["Documents"]));
    assert_eq!(names("folder: foldersize:>8mb"), set(&["C:", "D:", "Music"]));
    // Files: their own size.
    assert_eq!(names("foldersize:9000000"), set(&["D:", "Music", "track.flac"]));
    assert_eq!(names("folder-size:0"), set(&["drivers", "Empty", "café menu.txt"]));
}

#[test]
fn siblings() {
    assert_eq!(names("sibling:readme"), set(&BOB_CHILDREN[..5]));
    assert_eq!(names("sibling:*.png"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("sibling:img"), set(PHOTOS));
    assert_eq!(
        names("siblingcount:2"),
        set(&[PHOTOS, &["Report Q3.docx", "café menu.txt", "季度预算表.xlsx"]].concat())
    );
    assert_eq!(names("siblingcount:5"), set(BOB_CHILDREN));
    assert_eq!(names("parent-sibling:documents"), set(PHOTOS));
    assert_eq!(names("ancestor-sibling:users"), set(&["notepad.exe", "System32", "cmd.exe", "drivers"]));
    assert_eq!(names("sibling:"), names(""));
}

#[test]
fn hardlinks_and_name_parts() {
    assert_eq!(names("hardlinkcount:1 ext:flac"), set(&["track.flac"]));
    assert_eq!(names("hardlinks:>1"), set(&[]));
    assert_eq!(names("nameparts:2"), set(&["IMG_0001.JPG", "IMG_0002.jpg", "Report Q3.docx", "café menu.txt"]));
    assert_eq!(names("nameparts:0"), set(&[]));
}

#[test]
fn type_descriptions() {
    assert_eq!(names("type:\"jpg file\""), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("type:\"txt file\""), set(&["café menu.txt", "file2.txt", "file10.txt"]));
    assert_eq!(names("type:folder"), set(ALL_FOLDERS));
    assert_eq!(names("wfn:type:file"), set(&["README"]));
    // An injected provider (the registry on Windows) replaces the fallback names.
    let caches = Caches::new();
    caches.set_type_names(Some(TypeNames::new(|e| (e == "txt").then(|| "Text Document".to_string()))));
    let env = PlanEnv::default();
    assert_eq!(names_with("type:\"text document\"", &env, &caches), set(&["café menu.txt", "file2.txt", "file10.txt"]));
    assert_eq!(names_with("type:\"png file\"", &env, &caches), set(&["holiday.png"]));
    caches.set_type_names(None);
    assert_eq!(names_with("type:\"text document\"", &env, &caches), set(&[]));
}

#[test]
fn file_lists() {
    assert_eq!(names("filelist:\"readme|C:\\Windows\\notepad.exe\""), set(&["README", "notepad.exe"]));
    assert_eq!(names("filelist:\"c:/users/bob/photos/\";track.flac"), set(&["Photos", "track.flac"]));
    assert_eq!(names("filelist*:IMG_0001.JPG|nope.txt"), set(&["IMG_0001.JPG"]));
    assert_eq!(names("filelist:\"c:\\no\\such\\file\""), set(&[]));
}

#[test]
fn existence_checks() {
    assert_eq!(names("fileexists:readme"), set(BOB_CHILDREN));
    assert_eq!(names("fileexists:$stem:.txt"), set(&["café menu.txt", "file2.txt", "file10.txt"]));
    assert_eq!(names("fileexists:..\\notepad.exe"), set(&["cmd.exe", "drivers"]));
    assert_eq!(names("fileexists:c:\\windows\\notepad.exe ext:flac"), set(&["track.flac"]));
    assert_eq!(names("fileexists:c:\\nope.txt"), set(&[]));
    assert_eq!(names("folderexists:$path:\\Photos"), set(BOB_CHILDREN));
    assert_eq!(names("folderexists:$fullpath:\\photos"), set(&["bob"]));
    assert_eq!(names("folderexists:$name: file:"), set(&[]));
    // A file of that name is not a folder.
    assert_eq!(names("folderexists:readme"), set(&[]));
    assert_eq!(names("fileexists:$parent:.txt"), set(&[]));
    assert_eq!(names("ext:png fileexists:$name:;nope"), set(&["holiday.png"]));
}

#[test]
fn omitted_results() {
    let s = store();
    let readme = (0..s.len() as u32).map(fnc_core::EntryId).find(|&i| s.name_string(i) == "README").unwrap();
    let omit = fnc_query::OmitSet { instance: s.instance(), ids: vec![readme].into() };
    let env = PlanEnv { omit: Some(omit.clone()), ..PlanEnv::default() };
    let caches = Caches::new();
    assert!(!names_on(&s, "", &env, &caches).contains("README"));
    assert_eq!(names_on(&s, "readme", &env, &caches), set(&[]));
    assert_eq!(names_on(&s, "omit:", &env, &caches), set(&["README"]));
    assert_eq!(names_on(&s, "!omit: parent:c:\\users\\bob", &env, &caches), set(&BOB_CHILDREN[..5]));
    // Without an omit set `omit:` changes nothing; a foreign instance omits nothing.
    assert_eq!(names("omit: readme"), set(&["README"]));
    let other = PlanEnv { omit: Some(fnc_query::OmitSet { instance: u64::MAX, ..omit }), ..PlanEnv::default() };
    assert_eq!(names_on(&s, "readme", &other, &caches), set(&["README"]));
}

#[test]
fn wildcard_dates() {
    let local = fnc_core::FileTime(134_000_000_000_000_000).to_local().unwrap();
    let (y, m) = (local.format("%Y").to_string(), local.format("%m").to_string());
    let other = format!("{:02}", m.parse::<u32>().unwrap() % 12 + 1);
    assert_eq!(names(&format!("dm:*-{m} ext:jpg;png")), set(PHOTOS));
    assert_eq!(names(&format!("dm:*-{other} ext:jpg;png")), set(&[]));
    assert_eq!(names(&format!("dm:!*-{other} ext:jpg;png")), set(PHOTOS));
    assert_eq!(names(&format!("dm:{y}-*-* ext:jpg;png")), set(PHOTOS));
    assert_eq!(names("dm:*-*-*T0:00..*-*-*T23:59 ext:jpg"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("dm:*-13"), set(&[]));
}

#[test]
fn pinyin_ascii_fuzzy_modifiers() {
    for q in ["py:jdys", "pinyin:jiduyusuan", "py:jiduysb", "pinyinjian:jdysb", "pinyinquan:yusuanbiao", "py:ysb.xlsx"]
    {
        assert_eq!(names(q), set(&["季度预算表.xlsx"]), "{q}");
    }
    assert_eq!(names("pinyinquan:jdys"), set(&[]));
    assert_eq!(names("pinyinjian:jiduyusuan"), set(&[]));
    assert_eq!(names("jdys"), set(&[]));
    assert_eq!(names("py:holiday"), set(&["holiday.png"]));
    // Global default (settings) with per-term opt-out.
    let env = PlanEnv { pinyin_default: true, ..PlanEnv::default() };
    let caches = Caches::new();
    assert_eq!(names_with("jdys", &env, &caches), set(&["季度预算表.xlsx"]));
    assert_eq!(names_with("nopinyin:jdys", &env, &caches), set(&[]));
    assert_eq!(names_with("img", &env, &caches), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    // ascii: ASCII-only case folding, everything else exact.
    assert_eq!(names("ascii:cafe"), set(&[]));
    assert_eq!(names("ascii:CAFÉ"), set(&[]));
    assert_eq!(names("ascii:Café"), set(&["café menu.txt"]));
    assert_eq!(names("ascii:readme"), set(&["README"]));
    // fuzzy: one edit.
    assert_eq!(names("fuzzy:holidy"), set(&["holiday.png"]));
    assert_eq!(names("fuzzy:hollday"), set(&["holiday.png"]));
    assert_eq!(names("fuzzy:holidya"), set(&["holiday.png"]));
    assert_eq!(names("fuzzy:hloiday"), set(&[]));
    assert_eq!(names("fuzzy:rport"), set(&["Report Q3.docx"]));
}

#[test]
fn start_end_with_and_weight() {
    assert_eq!(names("startwith:file"), set(&["file2.txt", "file10.txt"]));
    assert_eq!(names("sw:img_000"), set(&["IMG_0001.JPG", "IMG_0002.jpg"]));
    assert_eq!(names("endwith:.txt"), set(&["café menu.txt", "file2.txt", "file10.txt"]));
    assert_eq!(names("ew:flac"), set(&["track.flac"]));
    assert_eq!(names("weight:10 holiday"), set(&["holiday.png"]));
    assert_eq!(names("len:3"), set(&["bob"]));
    assert_eq!(names("len:>13 ext:docx;xlsx"), set(&["Report Q3.docx"]));
    assert_eq!(names("len:<3"), set(&["C:", "D:"]));
}

#[test]
fn engine_filters_with_options() {
    use fnc_query::{Filter, FilterSet};
    let (engine, rx, _) = engine_with(store());
    let run = |id: u64, text: &str, filter: Option<FilterSet>| {
        let mut r = req(id, text);
        r.filter = filter;
        engine.search(r);
        let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        o.results.len()
    };
    let case = |search: &str| FilterSet::single(Filter { case: true, ..Filter::new(search) });
    assert_eq!(run(1, "", Some(case("IMG"))), 2);
    assert_eq!(run(2, "", Some(case("img"))), 0);
    // Filter options only apply to the filter's own terms.
    assert_eq!(run(3, "jpg", Some(case("IMG"))), 2);
    let two = FilterSet { filters: vec![Filter::new("ext:png"), Filter::new("ext:flac")], and: false };
    assert_eq!(run(4, "", Some(two.clone())), 2);
    assert_eq!(run(5, "and-filter:", Some(two.clone())), 0);
    assert_eq!(run(6, "", Some(FilterSet { and: true, ..two.clone() })), 0);
    assert_eq!(run(7, "or-filter:", Some(FilterSet { and: true, ..two })), 2);
    let regex = FilterSet::single(Filter { regex: true, ..Filter::new(r"^file\d+\.txt$") });
    assert_eq!(run(8, "", Some(regex)), 2);
    let ww = FilterSet::single(Filter { whole_word: true, ..Filter::new("report") });
    assert_eq!(run(9, "", Some(ww)), 1);
    let ww_part = FilterSet::single(Filter { whole_word: true, ..Filter::new("repor") });
    assert_eq!(run(10, "", Some(ww_part)), 0);
    let path = FilterSet::single(Filter { path: true, ..Filter::new("photos") });
    assert_eq!(run(11, "", Some(path)), 4);
}

#[test]
fn compiled_nodes_stay_small() {
    // Compiled nodes are walked for every entry; keep them within a cache line.
    assert!(std::mem::size_of::<eval::Leaf>() <= 64, "Leaf is {} bytes", std::mem::size_of::<eval::Leaf>());
    assert!(std::mem::size_of::<eval::CNode>() <= 64, "CNode is {} bytes", std::mem::size_of::<eval::CNode>());
}

#[test]
fn engine_pinyin_default_and_type_names() {
    let (engine, rx, _) = engine_with(store());
    let mut r = req(1, "jdys");
    r.options = Arc::new(SearchOptions { pinyin_default: true, ..SearchOptions::default() });
    engine.search(r);
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(o.results.len(), 1);
    let mut r = req(2, "type:spreadsheet");
    let types = TypeNames::new(|e| (e == "xlsx").then(|| "Microsoft Excel Worksheet (spreadsheet)".to_string()));
    r.options = Arc::new(SearchOptions { type_names: Some(types), ..SearchOptions::default() });
    engine.search(r);
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(o.results.len(), 1);
}

#[test]
fn name_substring_fast_path_matches_the_tree_walk() {
    let mut s = store();
    // A deleted entry must not appear on either path.
    let victim = (0..s.len() as u32).map(fnc_core::EntryId).find(|&i| s.name_string(i) == "file2.txt").unwrap();
    s.apply(vec![Change::Delete(EntryRef::Id(victim))]);
    for q in ["file", "e", "img 000", "REPORT", "季度", "o w", "zzz"] {
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let (caches, cancel) = (Caches::new(), CancelToken::new());
        let c = crate::eval::Compiled::new(&p.root, &s, &caches, &cancel).unwrap();
        assert!(c.name_substrings().is_some(), "{q} takes the fast path");
        let mut sc = crate::eval::Scratch::default();
        let slow: Vec<_> = (0..s.len() as u32)
            .map(fnc_core::EntryId)
            .filter(|&id| !s.flags(id).contains(fnc_core::EntryFlags::DELETED) && c.matches(&s, id, &mut sc))
            .collect();
        let fast = execute(&p, &s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }).unwrap().ids;
        assert_eq!(fast, slow, "{q}");
    }
    // Options keep the general evaluator.
    for q in ["ww:file", "path:users", "*.txt", "pinyin:jd", "case:File"] {
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let (caches, cancel) = (Caches::new(), CancelToken::new());
        let c = crate::eval::Compiled::new(&p.root, &s, &caches, &cancel).unwrap();
        assert!(c.name_substrings().is_none(), "{q}");
    }
}

#[test]
fn cloud_placeholders_are_not_links() {
    // 0x400 reparse point; 0x400400 + recall on data access (OneDrive); 0x600 + sparse (dedup).
    let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
        \"C:\\L\\link.txt\",0,,,1056\n\
        \"C:\\L\\onedrive.docx\",10,,,4195360\n\
        \"C:\\L\\dedup.vhd\",10,,,1568\n\
        \"C:\\L\\junction\",,,,1040\n\
        \"C:\\L\\cloud folder\",,,,4195344\n";
    let mut s = IndexStore::default();
    efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
    s.set_volume_online(fnc_core::VolumeId(0), true);
    s.rebuild_name_order();
    let names = |q: &str| run_on(&s, q, None).iter().map(|p| leaf_name(p)).collect::<BTreeSet<_>>();
    assert_eq!(names("symlink:"), set(&["link.txt"]));
    assert_eq!(names("junction:"), set(&["junction"]));
    assert_eq!(names("attrib:L").len(), 5);
}

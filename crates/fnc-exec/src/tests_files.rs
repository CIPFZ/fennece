//! Executor tests of the file-reading functions without an index (`target:`, `fsi:`,
//! `recycled:`) and the index-only `filelistfilename:` / `parent+N:`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fnc_index::IndexStore;
use fnc_props::content::TextEncoding;
use fnc_props::link::LinkKind;
use fnc_props::ondemand::{DefaultOnDemand, OnDemand};
use fnc_props::{PropertyId, Value};
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

use crate::exec::{ExecCtx, ResultSet, execute_ext};
use crate::ext::{ExecWarning, ExtCtx, SlowPolicy};
use crate::{Caches, CancelToken};

/// Counts the reads of an inner source (or serves fixed bytes by path).
struct Counting {
    inner: Option<DefaultOnDemand>,
    files: HashMap<String, Vec<u8>>,
    links: AtomicUsize,
    heads: AtomicUsize,
}

impl Counting {
    fn real() -> Arc<Self> {
        Arc::new(Self {
            inner: Some(DefaultOnDemand::default()),
            files: HashMap::new(),
            links: AtomicUsize::new(0),
            heads: AtomicUsize::new(0),
        })
    }

    fn fake(files: &[(&str, Vec<u8>)]) -> Arc<Self> {
        Arc::new(Self {
            inner: None,
            files: files.iter().map(|(p, b)| (p.to_string(), b.clone())).collect(),
            links: AtomicUsize::new(0),
            heads: AtomicUsize::new(0),
        })
    }
}

impl OnDemand for Counting {
    fn properties(&self, _: &Path, _: &str, _: &[PropertyId]) -> Vec<(PropertyId, Value)> {
        Vec::new()
    }
    fn content(&self, _: &Path, _: &str) -> Option<String> {
        None
    }
    fn text_as(&self, _: &Path, _: TextEncoding) -> Option<String> {
        None
    }
    fn bytes(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.get(path.to_str()?).cloned()
    }
    fn head(&self, path: &Path, max: usize) -> Option<Vec<u8>> {
        self.heads.fetch_add(1, Ordering::Relaxed);
        match &self.inner {
            Some(i) => i.head(path, max),
            None => self.bytes(path).map(|mut b| {
                b.truncate(max);
                b
            }),
        }
    }
    fn read_at(&self, path: &Path, offset: u64, len: usize) -> Option<Vec<u8>> {
        self.inner.as_ref()?.read_at(path, offset, len)
    }
    fn link_target(&self, path: &Path, kind: LinkKind) -> Option<String> {
        self.links.fetch_add(1, Ordering::Relaxed);
        self.inner.as_ref()?.link_target(path, kind)
    }
}

/// A shell link whose LinkInfo LocalBasePath is `local`, or with only a StringData
/// RELATIVE_PATH `rel`.
fn lnk(local: Option<&str>, rel: Option<&str>) -> Vec<u8> {
    const CLSID: [u8; 16] = [1, 0x14, 2, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    let mut flags = 0x80u32; // IsUnicode
    if local.is_some() {
        flags |= 0x02;
    }
    if rel.is_some() {
        flags |= 0x08;
    }
    let mut v = 0x4Cu32.to_le_bytes().to_vec();
    v.extend_from_slice(&CLSID);
    v.extend_from_slice(&flags.to_le_bytes());
    v.resize(0x4C, 0);
    if let Some(local) = local {
        let lbp_off = 0x1Cu32;
        let cps_off = lbp_off + local.len() as u32 + 1;
        let size = cps_off + 1;
        for x in [size, 0x1C, 1, 0, lbp_off, 0, cps_off] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(local.as_bytes());
        v.extend_from_slice(&[0, 0]);
    }
    if let Some(rel) = rel {
        let units: Vec<u16> = rel.encode_utf16().collect();
        v.extend_from_slice(&(units.len() as u16).to_le_bytes());
        v.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    }
    v
}

/// `$I` record (version 2).
fn info(path: &str) -> Vec<u8> {
    let units: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut v = 2u64.to_le_bytes().to_vec();
    v.extend_from_slice(&10u64.to_le_bytes());
    v.extend_from_slice(&133_000_000_000_000_000u64.to_le_bytes());
    v.extend_from_slice(&(units.len() as u32).to_le_bytes());
    v.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    v
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: IndexStore,
}

/// Real files (links, signatures) indexed through an EFU list.
fn files_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    let zip = {
        let mut z = b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0\0\0\0\0\x05\0\0\0\x05\0\0\0\x05\0\0\0".to_vec();
        z.extend_from_slice(b"a.txthello");
        z
    };
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("docs/notepad.lnk", lnk(Some("C:\\Windows\\notepad.exe"), None)),
        ("docs/rel.lnk", lnk(None, Some("..\\data\\report.txt"))),
        ("docs/site.url", b"[InternetShortcut]\r\nURL=https://example.com/page.html\r\n".to_vec()),
        ("docs/broken.lnk", b"not a link".to_vec()),
        ("data/report.txt", b"quarterly report".to_vec()),
        ("data/photo.jpg", b"\xFF\xD8\xFF\xE0\0\x10JFIF\0".to_vec()),
        ("data/fake.zip", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec()),
        ("data/real.zip", zip.clone()),
        ("data/renamed.txt", zip),
        ("data/empty.bin", Vec::new()),
    ];
    let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
    for d in ["docs", "data"] {
        efu.push_str(&format!("\"{}\",,,,16\n", root.join(d).display()));
    }
    for (name, bytes) in &files {
        let p = root.join(name);
        std::fs::write(&p, bytes).unwrap();
        efu.push_str(&format!("\"{}\",{},7,,32\n", p.display(), bytes.len()));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("data/report.txt"), root.join("docs/sym")).unwrap();
        std::os::unix::fs::symlink("../data", root.join("docs/dirlink")).unwrap();
        // Reparse-point attribute (0x400): file symlink and folder link.
        efu.push_str(&format!("\"{}\",0,7,,1056\n", root.join("docs/sym").display()));
        efu.push_str(&format!("\"{}\",,7,,1040\n", root.join("docs/dirlink").display()));
    }
    let mut store = IndexStore::default();
    fnc_index::efu::load_efu(&mut store, efu.as_bytes(), "t.efu").unwrap();
    store.rebuild_name_order();
    Fixture { _dir: dir, root, store }
}

fn run(s: &IndexStore, q: &str, od: Option<Arc<dyn OnDemand>>, policy: SlowPolicy, caches: &Caches) -> ResultSet {
    let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
    let cancel = CancelToken::new();
    let ext = ExtCtx { on_demand: od.as_ref(), policy, ..ExtCtx::default() };
    execute_ext(&p, s, &ExecCtx { cancel: &cancel, prev: None, caches }, &ext).unwrap()
}

fn sorted(s: &IndexStore, rs: &ResultSet) -> Vec<String> {
    let mut v: Vec<String> = rs.ids.iter().map(|&i| s.name_string(i)).collect();
    v.sort();
    v
}

#[test]
fn link_targets() {
    let f = files_fixture();
    let s = &f.store;
    let src = Counting::real();
    let od: Arc<dyn OnDemand> = src.clone();
    let caches = Caches::new();
    let names = |q: &str| sorted(s, &run(s, q, Some(od.clone()), SlowPolicy::default(), &caches));
    assert_eq!(names("target:notepad.exe"), ["notepad.lnk"]);
    assert_eq!(names("target:*.exe"), ["notepad.lnk"]);
    assert_eq!(names("target:c:\\windows\\*"), ["notepad.lnk"]);
    assert_eq!(names("shortcuttarget:example.com"), ["site.url"]);
    assert_eq!(names("linktarget:*.html"), ["site.url"]);
    assert_eq!(names("target:https://*"), ["site.url"]);
    let mut links = vec!["notepad.lnk", "rel.lnk", "site.url"];
    let mut report = vec!["rel.lnk"];
    if cfg!(unix) {
        links.extend(["dirlink", "sym"]);
        report.push("sym");
    }
    links.sort_unstable();
    report.sort_unstable();
    assert_eq!(names("target:report.txt"), report, "relative targets are resolved");
    assert_eq!(names("target:"), links);
    let full = f.root.join("data").join("report.txt").display().to_string();
    assert_eq!(names(&format!("wfn:target:\"{full}\"")), report);
    if cfg!(unix) {
        assert_eq!(names("target:*data"), ["dirlink"], "folder links resolve too");
    }
    assert!(names("target:quarterly").is_empty(), "contents are not targets");
    // Only links are read: the plain files and folders cost nothing.
    let reads = src.links.load(Ordering::Relaxed);
    assert_eq!(reads, links.len() + 1, "one read per link (+ broken.lnk), cached across queries");
    names("target:x");
    assert_eq!(src.links.load(Ordering::Relaxed), reads, "cached");
    assert!(names("!target: ext:txt").contains(&"report.txt".to_string()));
    // Budget and missing source.
    let rs = run(s, "target:", Some(od.clone()), SlowPolicy { max_files: 1, ..SlowPolicy::default() }, &Caches::new());
    assert!(rs.stats.warnings.contains(&ExecWarning::SlowLimit(1)), "{:?}", rs.stats.warnings);
    assert_eq!(rs.len(), 1);
    let rs = run(s, "target:x", None, SlowPolicy::default(), &Caches::new());
    assert!(rs.is_empty());
    assert_eq!(rs.stats.warnings, [ExecWarning::NoFileAccess("target")]);
}

#[test]
fn file_signatures() {
    let f = files_fixture();
    let s = &f.store;
    let src = Counting::real();
    let od: Arc<dyn OnDemand> = src.clone();
    let caches = Caches::new();
    let names = |q: &str| sorted(s, &run(s, q, Some(od.clone()), SlowPolicy::default(), &caches));
    assert_eq!(names("fsi:jpg"), ["photo.jpg"]);
    assert_eq!(names("fsi:jpeg"), ["photo.jpg"]);
    assert_eq!(names("fsi:png"), ["fake.zip"]);
    assert_eq!(names("fsi:zip"), ["real.zip", "renamed.txt"]);
    assert_eq!(names("fsi:zip !ext:zip"), ["renamed.txt"], "content/extension mismatch");
    assert_eq!(names("ext:zip !fsi:zip"), ["fake.zip"]);
    assert_eq!(names("fsi:jpg;png"), ["fake.zip", "photo.jpg"]);
    assert_eq!(names("fsi:lnk"), ["notepad.lnk", "rel.lnk"]);
    let unknown = names("filesignature:");
    for n in ["report.txt", "empty.bin", "site.url", "broken.lnk"] {
        assert!(unknown.contains(&n.to_string()), "{n} in {unknown:?}");
    }
    assert!(!unknown.iter().any(|n| n == "photo.jpg" || n == "docs" || n == "data"), "{unknown:?}");
    assert!(names("folder: fsi:").is_empty(), "folders have no signature");
    let heads = src.heads.load(Ordering::Relaxed);
    names("fsi:pdf");
    assert_eq!(src.heads.load(Ordering::Relaxed), heads, "signatures cached across queries");
    let p = plan(&parse("fsi:nonsense", &ParseEnv::default()).ast, &PlanEnv::default(), None);
    assert!(!p.diagnostics.is_empty());
    let rs = run(s, "fsi:jpg", None, SlowPolicy::default(), &Caches::new());
    assert_eq!(rs.stats.warnings, [ExecWarning::NoFileAccess("fsi")]);
}

/// A recycle bin (paths are never read from disk: a fake source serves the `$I` records).
fn bin_store() -> IndexStore {
    let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
               \"C:\\$Recycle.Bin\",,,,22\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\",,,,22\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\desktop.ini\",129,1,,38\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\$RAB12.txt\",10,1,,32\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\$IAB12.txt\",80,1,,32\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\$RDIR9\",,1,,16\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\$IDIR9\",70,1,,32\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\$RDIR9\\sub\",,1,,16\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\$RDIR9\\sub\\x.txt\",3,1,,32\n\
               \"C:\\$Recycle.Bin\\S-1-5-21-1\\$RGONE.txt\",3,1,,32\n\
               \"C:\\Users\\me\\$Rnot-in-bin.txt\",3,1,,32\n\
               \"C:\\Users\\me\\report.txt\",3,1,,32\n";
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "bin.efu").unwrap();
    s.rebuild_name_order();
    s
}

#[test]
fn recycle_bin() {
    let s = bin_store();
    let src = Counting::fake(&[
        ("C:\\$Recycle.Bin\\S-1-5-21-1\\$IAB12.txt", info("C:\\Users\\me\\Docs\\Old report.txt")),
        ("C:\\$Recycle.Bin\\S-1-5-21-1\\$IDIR9", info("D:\\Projects\\old")),
    ]);
    let od: Arc<dyn OnDemand> = src.clone();
    let caches = Caches::new();
    let names = |q: &str| sorted(&s, &run(&s, q, Some(od.clone()), SlowPolicy::default(), &caches));
    // Index only: the `$R` items and everything below them.
    assert_eq!(names("recycled:"), ["$RAB12.txt", "$RDIR9", "$RGONE.txt", "sub", "x.txt"]);
    assert_eq!(names("recyclebinoriginalpath:"), names("recycled:"));
    assert_eq!(src.heads.load(Ordering::Relaxed), 0, "the boolean form reads nothing");
    assert_eq!(names("!recycled: ext:txt"), ["$IAB12.txt", "$Rnot-in-bin.txt", "report.txt"]);
    // Original paths from the `$I` records (descendants: folder path + suffix).
    assert_eq!(names("recycled:report"), ["$RAB12.txt"]);
    assert_eq!(names("recycled:\"c:\\users\\me\\docs\\old report.txt\""), ["$RAB12.txt"]);
    assert_eq!(names("recycled:d:\\projects\\old"), ["$RDIR9", "sub", "x.txt"]);
    assert_eq!(names("wfn:recycled:d:\\projects\\old\\sub\\x.txt"), ["x.txt"]);
    assert_eq!(names("recycled:d:\\projects\\old\\*"), ["sub"], "`*` stops at `\\`");
    assert_eq!(names("recycled:d:/projects/**"), ["$RDIR9", "sub", "x.txt"]);
    assert_eq!(names("recycled:x.txt file:"), ["x.txt"]);
    assert!(names("recycled:gone").is_empty(), "no `$I` record: unknown original path");
    // One `$I` read per top-level item, cached across queries.
    let reads = src.heads.load(Ordering::Relaxed);
    assert_eq!(reads, 3);
    names("recycled:zzz");
    assert_eq!(src.heads.load(Ordering::Relaxed), reads);
    let rs = run(&s, "recycled:x", None, SlowPolicy::default(), &Caches::new());
    assert_eq!(rs.stats.warnings, [ExecWarning::NoFileAccess("recycled")]);
    assert_eq!(sorted(&s, &run(&s, "recycled:", None, SlowPolicy::default(), &Caches::new())).len(), 5);
}

#[test]
fn file_list_names_and_parent_n() {
    let mut s = IndexStore::default();
    let a = "Filename,Size,Date Modified,Date Created,Attributes\n\"C:\\a\\b\\c\\d.txt\",1,1,,32\n\"C:\\a\\x.txt\",1,1,,32\n";
    let b = "Filename,Size,Date Modified,Date Created,Attributes\n\"E:\\e\\f.txt\",1,1,,32\n";
    fnc_index::efu::load_efu(&mut s, a.as_bytes(), "D:\\lists\\backup.efu").unwrap();
    fnc_index::efu::load_efu(&mut s, b.as_bytes(), "D:\\other\\Photos 2024.EFU").unwrap();
    s.rebuild_name_order();
    let caches = Caches::new();
    let names = |q: &str| sorted(&s, &run(&s, q, None, SlowPolicy::default(), &caches));
    assert_eq!(names("filelistfilename:backup.efu file:"), ["d.txt", "x.txt"]);
    assert_eq!(names("filelistfilename:photos file:"), ["f.txt"]);
    assert_eq!(names("filelistfilename:*.efu file:"), ["d.txt", "f.txt", "x.txt"]);
    assert_eq!(names("filelistfilename:d:\\other\\* file:"), ["f.txt"]);
    assert_eq!(names("filelistfilename:\"d:\\lists\" file:"), ["d.txt", "x.txt"]);
    assert_eq!(names("filelistfilename: file:"), ["d.txt", "f.txt", "x.txt"]);
    assert!(names("filelistfilename:nothing").is_empty());
    assert!(names("case:filelistfilename:PHOTOS").is_empty());
    // parent+N: exactly N+1 levels below the folder.
    assert_eq!(names("parent+0:c:\\a\\b\\c"), ["d.txt"]);
    assert_eq!(names("parent:c:\\a\\b\\c"), ["d.txt"]);
    assert_eq!(names("parent+1:c:\\a\\b"), ["d.txt"]);
    assert_eq!(names("parent+2:c:\\a"), ["d.txt"]);
    assert_eq!(names("parent+1:c:\\a"), ["c"]);
    assert_eq!(names("parent+1:c:\\a\\"), ["c"]);
    assert!(names("parent+3:c:\\a").is_empty());
    assert!(names("parent+255:c:\\a").is_empty());
    assert_eq!(names("parent+1:c:\\a\\*"), ["d.txt"]);
    assert_eq!(names("parent+1:c:\\a;c:\\a\\b"), ["c", "d.txt"]);
    assert_eq!(names("!parent+2:c:\\a ext:txt"), ["f.txt", "x.txt"]);
}

#[test]
fn engine_sorts_by_custom_formulas_and_type() {
    use std::time::Duration;

    use fnc_core::settings::FoldersFirst;
    use fnc_query::formula::Formula;

    use crate::engine::{EngineEvent, SearchEngine, SearchOptions, SearchRequest};
    use crate::ext::Shared;
    use crate::sort::{SortColumn, SortSpec};

    let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
               \"C:\\d\\bb.txt\",30,1,,32\n\
               \"C:\\d\\a.zip\",5,2,,32\n\
               \"C:\\d\\cccc.jpg\",100,3,,32\n";
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
    s.rebuild_name_order();
    let shared = fnc_index::SharedIndex::new(s);
    let (tx, rx) = crossbeam_channel::unbounded();
    let eng = SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
    let formulas = vec![Some(Shared::new(Arc::new(Formula::parse("LEN($stem:)").unwrap())))];
    let types = crate::TypeNames::new(|e| (e == "jpg").then(|| "AAA picture".to_string()));
    let opts = Arc::new(SearchOptions { formulas, type_names: Some(types), ..SearchOptions::default() });
    let wait = |id| loop {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            EngineEvent::Done(o) if o.id == id => return o.results,
            _ => {}
        }
    };
    let snap = shared.snapshot();
    let names = |rs: &ResultSet| rs.ids.iter().map(|&i| snap.name_string(i)).collect::<Vec<_>>();
    let req = |id, sort| SearchRequest { id, text: "file:".into(), filter: None, sort, options: opts.clone() };
    eng.search(req(1, SortSpec::by_custom(0, false, FoldersFirst::Ascending)));
    assert_eq!(names(&wait(1)), ["cccc.jpg", "bb.txt", "a.zip"]);
    eng.search(req(2, SortSpec::by_custom(0, true, FoldersFirst::Ascending)));
    let rs = wait(2);
    assert!(rs.stats.reused, "a sort change reuses the results");
    assert_eq!(names(&rs), ["a.zip", "bb.txt", "cccc.jpg"]);
    eng.search(req(3, SortSpec::by(SortColumn::Type, true, FoldersFirst::Ascending)));
    // "AAA picture" < "TXT File" < "ZIP File".
    assert_eq!(names(&wait(3)), ["cccc.jpg", "bb.txt", "a.zip"]);
    drop(eng);
}

//! Tests of property, content, hash and set functions through the executor and engine.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fnc_core::EntryId;
use fnc_core::settings::FoldersFirst;
use fnc_index::{IndexStore, SharedIndex};
use fnc_props::content::{ContentIndex, ContentStore};
use fnc_props::hash::HashCache;
use fnc_props::ondemand::{DefaultOnDemand, OnDemand};
use fnc_props::store::{PropertyIndex, PropertyStore};
use fnc_props::{PropertyId as P, Value};
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

use crate::engine::{EngineEvent, SearchEngine, SearchOptions, SearchRequest};
use crate::exec::{ExecCtx, ResultSet, execute_ext};
use crate::ext::{ExecWarning, ExtCtx, Shared, SlowPolicy, content_highlights};
use crate::sort::{SortColumn, SortSpec, sort_ids_ext};
use crate::{Caches, CancelToken};

/// Files in a temp dir indexed through an EFU list (real paths, so the slow paths can
/// read them).
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: IndexStore,
}

fn png(w: u32, h: u32) -> Vec<u8> {
    // Minimal valid PNG header + IHDR (the header reader never decodes pixels).
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(&ihdr);
    v.extend_from_slice(&crc32fast_hash(&ihdr).to_be_bytes());
    // The header reader stops at the first IDAT chunk; its data is never inflated.
    let idat = b"IDAT\x78\x9c\x03\x00\x00\x00\x00\x01".to_vec();
    v.extend_from_slice(&((idat.len() - 4) as u32).to_be_bytes());
    v.extend_from_slice(&idat);
    v.extend_from_slice(&crc32fast_hash(&idat).to_be_bytes());
    let iend = b"IEND".to_vec();
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&iend);
    v.extend_from_slice(&crc32fast_hash(&iend).to_be_bytes());
    v
}

fn crc32fast_hash(b: &[u8]) -> u32 {
    let r = fnc_props::hash::hash_reader(&mut std::io::Cursor::new(b), &[fnc_props::hash::HashAlgo::Crc32], &|| false)
        .unwrap()
        .unwrap();
    u32::from_be_bytes(r[0].1[..4].try_into().unwrap())
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("big.png", png(5000, 3000)),
        ("tall.png", png(600, 900)),
        ("hd.png", png(1920, 1080)),
        ("notes.txt", "The quarterly plan.\nSecond line, plan again.".as_bytes().to_vec()),
        ("other.txt", b"nothing to see".to_vec()),
        ("copy1.bin", b"same bytes".to_vec()),
        ("copy2.bin", b"same bytes".to_vec()),
        ("diff.bin", b"other bytes".to_vec()),
        ("u16.txt", [0xff, 0xfe].into_iter().chain("utf16 季度".encode_utf16().flat_map(u16::to_le_bytes)).collect()),
        ("song.mp3", vec![0u8; 10]),
    ];
    let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
    efu.push_str(&format!("\"{}\",,,,16\n", root.join("sub").display()));
    for (name, bytes) in &files {
        let p = root.join(name);
        std::fs::write(&p, bytes).unwrap();
        efu.push_str(&format!("\"{}\",{},5,,32\n", p.display(), bytes.len()));
    }
    let mut store = IndexStore::default();
    fnc_index::efu::load_efu(&mut store, efu.as_bytes(), "t.efu").unwrap();
    store.rebuild_name_order();
    Fixture { _dir: dir, root, store }
}

fn id(s: &IndexStore, name: &str) -> EntryId {
    (0..s.len() as u32).map(EntryId).find(|&i| s.name_string(i) == name).unwrap()
}

fn props_for(s: &IndexStore) -> PropertyStore {
    let mut p = PropertyStore::new(s.instance());
    p.set_indexed(&[P::WIDTH, P::HEIGHT, P::ARTIST, P::LENGTH, P::TAGS, P::CAMERA_MODEL, P::PAGE_COUNT, P::F_STOP]);
    p.insert(
        id(s, "big.png"),
        vec![
            (P::WIDTH, Value::Int(5000)),
            (P::HEIGHT, Value::Int(3000)),
            (P::CAMERA_MODEL, Value::Text("EOS R5".into())),
            (P::TAGS, Value::multi(["sea", "sun"]).unwrap()),
            (P::F_STOP, Value::Float(2.8)),
        ],
    );
    p.insert(id(s, "tall.png"), vec![(P::WIDTH, Value::Int(600)), (P::HEIGHT, Value::Int(900))]);
    p.insert(
        id(s, "song.mp3"),
        vec![(P::ARTIST, Value::Text("John Doe".into())), (P::LENGTH, Value::Duration(185_000))],
    );
    // hd.png is not extracted yet: unknown.
    p
}

fn content_for(s: &IndexStore) -> ContentStore {
    let mut c = ContentStore::new(s.instance());
    c.insert(id(s, "notes.txt"), "The quarterly plan.\nSecond line, plan again.");
    c.insert(id(s, "other.txt"), "nothing to see");
    c
}

struct Run<'a> {
    s: &'a IndexStore,
    props: Option<&'a PropertyStore>,
    content: Option<&'a ContentStore>,
    on_demand: Option<Arc<dyn OnDemand>>,
    hashes: Arc<HashCache>,
    policy: SlowPolicy,
}

impl<'a> Run<'a> {
    fn new(s: &'a IndexStore) -> Self {
        Self {
            s,
            props: None,
            content: None,
            on_demand: None,
            hashes: Arc::new(HashCache::new()),
            policy: SlowPolicy::default(),
        }
    }

    fn rs(&self, q: &str) -> ResultSet {
        let env = PlanEnv {
            indexed_props: self.props.map(|p| p.indexed().to_vec()).unwrap_or_default(),
            ..PlanEnv::default()
        };
        let p = plan(&parse(q, &ParseEnv::default()).ast, &env, None);
        let caches = Caches::new();
        let cancel = CancelToken::new();
        let ext = ExtCtx {
            props: self.props,
            content: self.content,
            hashes: Some(&self.hashes),
            on_demand: self.on_demand.as_ref(),
            policy: self.policy,
        };
        execute_ext(&p, self.s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }, &ext).unwrap()
    }

    fn names(&self, q: &str) -> Vec<String> {
        let mut v: Vec<String> = self.rs(q).ids.iter().map(|&i| self.s.name_string(i)).collect();
        v.sort();
        v
    }
}

#[test]
fn indexed_properties() {
    let f = fixture();
    let props = props_for(&f.store);
    let r = Run { props: Some(&props), ..Run::new(&f.store) };
    assert_eq!(r.names("width:>4000"), ["big.png"]);
    assert_eq!(r.names("width:600;5000"), ["big.png", "tall.png"]);
    assert_eq!(r.names("height:>=900 width:<1000"), ["tall.png"]);
    assert_eq!(r.names("dimensions:5000x3000"), ["big.png"]);
    assert_eq!(r.names("dimensions:>=600x900"), ["big.png", "tall.png"]);
    assert_eq!(r.names("aspect-ratio:landscape"), ["big.png"]);
    assert_eq!(r.names("aspect-ratio:portrait"), ["tall.png"]);
    assert_eq!(r.names("artist:\"John Doe\""), ["song.mp3"]);
    assert_eq!(r.names("artist:doe"), ["song.mp3"]);
    assert_eq!(r.names("wfn:artist:doe"), Vec::<String>::new());
    assert_eq!(r.names("length:1min..5min"), ["song.mp3"]);
    assert_eq!(r.names("length:3:05"), ["song.mp3"]);
    assert_eq!(r.names("length:>10min"), Vec::<String>::new());
    assert_eq!(r.names("camera-model:"), ["big.png"]);
    assert_eq!(r.names("cameramodel:eos"), ["big.png"]);
    assert_eq!(r.names("tags:sun"), ["big.png"]);
    assert_eq!(r.names("count:tags:2"), ["big.png"]);
    assert_eq!(r.names("len:artist:8"), ["song.mp3"]);
    assert_eq!(r.names("f-stop:2.8"), ["big.png"]);
    assert_eq!(r.names("f-stop:>2"), ["big.png"]);
    assert_eq!(r.names("!width:>4000 ext:png"), ["hd.png", "tall.png"]);
    // Not extracted (hd.png) or files outside the store are unknown: only `unknown` matches.
    let unknown = r.names("width:unknown");
    assert!(unknown.contains(&"hd.png".to_string()) && unknown.contains(&"notes.txt".to_string()));
    assert!(!unknown.contains(&"big.png".to_string()));
    // `unknown` = no value: not extracted, or extracted without the property.
    assert!(r.names("artist:unknown").contains(&"big.png".to_string()));
    assert!(r.names("artist:unknown").contains(&"hd.png".to_string()));
    assert!(!r.names("artist:unknown").contains(&"song.mp3".to_string()));
    assert_eq!(r.names("page-count:>1"), Vec::<String>::new());
    // Without an on-demand source, reading is impossible: warning.
    let w = r.rs("bit-rate:>100").stats.warnings;
    assert_eq!(w, [ExecWarning::NotIndexed("bit-rate")]);
}

#[test]
fn property_store_of_another_instance_is_ignored() {
    let f = fixture();
    let other = f.store.clone();
    let props = props_for(&other);
    let r = Run { props: Some(&props), ..Run::new(&f.store) };
    assert!(r.names("width:>4000").is_empty());
}

#[test]
fn on_demand_properties_and_notindexed() {
    let f = fixture();
    let od: Arc<dyn OnDemand> = Arc::new(DefaultOnDemand::default());
    let r = Run { on_demand: Some(od.clone()), ..Run::new(&f.store) };
    assert_eq!(r.names("width:>1000"), ["big.png", "hd.png"]);
    assert_eq!(r.names("dimensions:1920x1080"), ["hd.png"]);
    assert_eq!(r.names("ext:png height:<1000 width:<1000"), ["tall.png"]);
    // Slow reads disabled: only `notindexed:` reads.
    let off = Run {
        on_demand: Some(od.clone()),
        policy: SlowPolicy { props: false, ..SlowPolicy::default() },
        ..Run::new(&f.store)
    };
    assert!(off.names("width:>1000").is_empty());
    assert_eq!(off.names("notindexed:width:>1000"), ["big.png", "hd.png"]);
    // Indexed values win; missing entries are read.
    let props = props_for(&f.store);
    let mixed = Run { props: Some(&props), on_demand: Some(od.clone()), ..Run::new(&f.store) };
    assert_eq!(mixed.names("width:>1000"), ["big.png", "hd.png"]);
    // Budget.
    let tight =
        Run { on_demand: Some(od), policy: SlowPolicy { max_files: 1, ..SlowPolicy::default() }, ..Run::new(&f.store) };
    let rs = tight.rs("width:>1");
    assert!(rs.stats.warnings.contains(&ExecWarning::SlowLimit(1)), "{:?}", rs.stats.warnings);
    assert!(rs.len() <= 1);
}

#[test]
fn indexed_and_unindexed_content() {
    let f = fixture();
    let content = content_for(&f.store);
    let r = Run { content: Some(&content), ..Run::new(&f.store) };
    assert_eq!(r.names("content:\"quarterly plan\""), ["notes.txt"]);
    assert_eq!(r.names("content:plan"), ["notes.txt"]);
    assert_eq!(r.names("content:plan ext:txt"), ["notes.txt"]);
    assert_eq!(r.names("!content:plan ext:txt"), ["other.txt", "u16.txt"]);
    assert_eq!(r.names("case:content:Quarterly"), Vec::<String>::new());
    assert_eq!(r.names("ww:content:plan"), ["notes.txt"]);
    assert_eq!(r.names("regex:content:^Second"), Vec::<String>::new());
    assert_eq!(r.names("multiline:regex:content:^Second"), ["notes.txt"]);
    assert_eq!(r.names("content:plan contenthits:2"), ["notes.txt"]);
    assert_eq!(r.names("content:plan contenthits:>2"), Vec::<String>::new());
    assert_eq!(r.names("line-count:2"), ["notes.txt"]);
    assert_eq!(r.names("word-count:3"), ["other.txt"]);
    assert_eq!(r.names("char-count:14"), ["other.txt"]);
    let rs = r.rs("content:plan");
    let hit = rs.content_hit(id(&f.store, "notes.txt")).unwrap();
    assert_eq!((hit.offset, hit.hits), (14, 2));
    let p = plan(&parse("content:plan", &ParseEnv::default()).ast, &PlanEnv::default(), None);
    let text = content.text(id(&f.store, "notes.txt")).unwrap();
    assert_eq!(content_highlights(&p, &text, 10), [14..18, 33..37]);
    // Unindexed files without an on-demand source: warning, no match.
    let none = Run::new(&f.store);
    assert!(none.names("content:plan").is_empty());
    assert_eq!(none.rs("content:plan").stats.warnings, [ExecWarning::NotIndexed("content")]);
    // On demand.
    let od: Arc<dyn OnDemand> = Arc::new(DefaultOnDemand::default());
    let r = Run { on_demand: Some(od), ..Run::new(&f.store) };
    assert_eq!(r.names("content:\"quarterly plan\""), ["notes.txt"]);
    assert_eq!(r.names("content:季度"), ["u16.txt"]);
    assert_eq!(r.names("utf16content:季度"), ["u16.txt"]);
    assert_eq!(r.names("utf8content:\"quarterly plan\""), ["notes.txt"]);
    assert_eq!(r.names("ansicontent:nothing"), ["other.txt"]);
    assert_eq!(r.names("textplain:quarterly"), ["notes.txt"]);
    assert_eq!(r.names("binary:content:\"same\\x20bytes\""), ["copy1.bin", "copy2.bin"]);
    assert_eq!(r.names("line-count:2"), ["notes.txt"]);
    let rs = r.rs("content:again");
    assert_eq!(rs.content_hit(id(&f.store, "notes.txt")).map(|h| h.offset), Some(38));
    assert!(r.names("folder: content:x").is_empty());
}

#[test]
fn hashes_are_computed_cached_and_prefix_matched() {
    let f = fixture();
    let r = Run::new(&f.store);
    assert!(r.names("md5:unknown").len() >= 10);
    // md5("same bytes") = 2a1f7ba8b9d6d3cc8ef0c3b0cd5ce9a2? Compute instead of hard-coding.
    let digest = fnc_props::hash::hash_file(&f.root.join("copy1.bin"), &[fnc_props::hash::HashAlgo::Md5], &|| false)
        .unwrap()
        .unwrap();
    let hex = fnc_props::hash::to_hex(&digest[0].1);
    assert_eq!(r.names(&format!("md5:{}", &hex[..6])), ["copy1.bin", "copy2.bin"]);
    assert_eq!(r.names(&format!("md5:{}", hex.to_uppercase())), ["copy1.bin", "copy2.bin"]);
    assert!(r.hashes.len() >= 10, "every candidate hashed once");
    assert!(!r.names("md5:unknown").contains(&"copy1.bin".to_string()));
    assert_eq!(r.names("ext:bin sha256:"), ["copy1.bin", "copy2.bin", "diff.bin"]);
    assert_eq!(r.names("crc32:0000;ffffffff"), Vec::<String>::new());
    let off = Run { policy: SlowPolicy { hashes: false, ..SlowPolicy::default() }, ..Run::new(&f.store) };
    assert!(off.names("sha1:a").is_empty());
}

#[test]
fn set_functions_through_the_executor() {
    let f = fixture();
    let props = props_for(&f.store);
    let r = Run { props: Some(&props), ..Run::new(&f.store) };
    assert_eq!(r.names("ext:bin dupe:size;sha256"), ["copy1.bin", "copy2.bin"]);
    assert_eq!(r.names("ext:bin sizedupe:"), ["copy1.bin", "copy2.bin"]);
    assert_eq!(r.names("ext:bin dupe:size"), ["copy1.bin", "copy2.bin"]);
    assert_eq!(r.names("ext:bin unique:size;md5"), ["diff.bin"]);
    assert_eq!(r.names("ext:png distinct:width").len(), 3, "unknown widths are their own groups");
    let rs = r.rs("ext:bin dupe:size;sha256");
    assert_eq!(rs.dupe_group(id(&f.store, "copy1.bin")), Some(1));
    assert_eq!(rs.dupe_group(id(&f.store, "copy2.bin")), Some(1));
    assert_eq!(rs.dupe_group(id(&f.store, "diff.bin")), None);
}

#[test]
fn property_sort_columns() {
    let f = fixture();
    let props = props_for(&f.store);
    let mut ids: Vec<EntryId> = ["hd.png", "big.png", "tall.png"].iter().map(|n| id(&f.store, n)).collect();
    let spec = SortSpec::by_property(P::WIDTH, true, FoldersFirst::Never);
    sort_ids_ext(&mut ids, &spec, &f.store, &Caches::new(), &CancelToken::new(), Some(&props)).unwrap();
    let names: Vec<String> = ids.iter().map(|&i| f.store.name_string(i)).collect();
    assert_eq!(names, ["hd.png", "tall.png", "big.png"], "unknown first, then by value");
    let mut spec = SortSpec::by(SortColumn::Extension, true, FoldersFirst::Never);
    spec.push_property_key(P::WIDTH, false);
    sort_ids_ext(&mut ids, &spec, &f.store, &Caches::new(), &CancelToken::new(), Some(&props)).unwrap();
    let names: Vec<String> = ids.iter().map(|&i| f.store.name_string(i)).collect();
    assert_eq!(names, ["big.png", "tall.png", "hd.png"]);
    assert_eq!(spec.sort_keys().count(), 2);
    // Text columns sort by natural order of the values.
    let mut ids: Vec<EntryId> = ["song.mp3", "big.png", "tall.png"].iter().map(|n| id(&f.store, n)).collect();
    let spec = SortSpec::by_property(P::ARTIST, false, FoldersFirst::Never);
    sort_ids_ext(&mut ids, &spec, &f.store, &Caches::new(), &CancelToken::new(), Some(&props)).unwrap();
    assert_eq!(f.store.name_string(ids[0]), "song.mp3");
}

fn wait(rx: &crossbeam_channel::Receiver<EngineEvent>, id: u64) -> Arc<ResultSet> {
    loop {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            EngineEvent::Done(o) if o.id == id => return o.results,
            _ => {}
        }
    }
}

#[test]
fn engine_wires_stores_sort_and_sets() {
    let f = fixture();
    let shared = SharedIndex::new(f.store);
    let snap = shared.snapshot();
    let s2: &IndexStore = &snap;
    let mut p = props_for(s2);
    p.insert(id(s2, "hd.png"), vec![(P::WIDTH, Value::Int(1920)), (P::HEIGHT, Value::Int(1080))]);
    let props2 = PropertyIndex::new(p);
    let content2 = ContentIndex::new(content_for(s2));
    let names_of = |rs: &ResultSet| rs.ids.iter().map(|&i| s2.name_string(i)).collect::<Vec<_>>();
    let (tx, rx) = crossbeam_channel::unbounded();
    let eng = SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
    let opts = Arc::new(SearchOptions {
        props: Some(props2.clone()),
        content: Some(content2.clone()),
        hashes: Some(Shared::new(Arc::new(HashCache::new()))),
        ..SearchOptions::default()
    });
    let req = |id, text: &str, sort: SortSpec| SearchRequest {
        id,
        text: text.into(),
        filter: None,
        sort,
        options: opts.clone(),
    };
    let by_name = SortSpec::by(SortColumn::Name, true, FoldersFirst::Never);
    eng.search(req(1, "width:>1000", by_name));
    assert_eq!(names_of(&wait(&rx, 1)), ["big.png", "hd.png"]);
    let by_width = SortSpec::by_property(P::WIDTH, false, FoldersFirst::Never);
    eng.search(req(2, "width:>1000", by_width));
    let rs = wait(&rx, 2);
    assert!(rs.stats.reused, "sort-only change reuses the ids");
    assert_eq!(names_of(&rs), ["big.png", "hd.png"]);
    eng.search(req(3, "content:plan", by_name));
    assert_eq!(names_of(&wait(&rx, 3)), ["notes.txt"]);
    // Store updates invalidate cached results.
    content2.write().insert(id(s2, "other.txt"), "a plan");
    eng.refresh();
    let rs = wait(&rx, 3);
    assert_eq!(names_of(&rs), ["notes.txt", "other.txt"]);
    assert!(!rs.stats.reused);
    // Set functions run after sorting: `distinct:` keeps the first in display order.
    let desc = SortSpec::by(SortColumn::Name, false, FoldersFirst::Never);
    eng.search(req(4, "ext:bin distinct:size", desc));
    assert_eq!(names_of(&wait(&rx, 4)), ["diff.bin", "copy2.bin"]);
    eng.search(req(5, "ext:bin distinct:size", by_name));
    let rs = wait(&rx, 5);
    assert!(rs.stats.reused);
    assert_eq!(names_of(&rs), ["copy1.bin", "diff.bin"]);
    eng.search(req(6, "ext:bin dupe:size;sha256", by_name));
    let rs = wait(&rx, 6);
    assert_eq!(names_of(&rs), ["copy1.bin", "copy2.bin"]);
    assert_eq!(rs.dupe_group(rs.ids[0]), Some(1));
    drop(eng);
}

#[test]
fn literal_queries_do_not_build_extension_state() {
    let f = fixture();
    let p = plan(&parse("plan *.txt size:>1", &ParseEnv::default()).ast, &PlanEnv::default(), None);
    let c = crate::eval::Compiled::new(&p.root, &f.store, &Caches::new(), &CancelToken::new()).unwrap();
    assert!(c.slow.is_none());
    assert!(c.warnings().is_empty());
    let _ = Path::new("");
}

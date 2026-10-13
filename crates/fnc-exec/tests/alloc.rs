//! Hot paths must not allocate (AGENT.md §5): per-entry matching with default options,
//! full-path joining into a reused buffer, and the name comparator.
//!
//! Uses dhat's counting allocator in testing mode (one test per binary).

// Test-only helpers outside `#[test]` functions.
#![allow(clippy::unwrap_used)]

use fnc_core::EntryId;
use fnc_exec::Caches;
use fnc_exec::eval::{Compiled, Scratch};
use fnc_index::IndexStore;
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn sample() -> IndexStore {
    let mut text = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
    for d in 0..50 {
        text.push_str(&format!("\"C:\\Dir{d}\\Sub Folder\",,,,16\n"));
        for f in 0..40 {
            text.push_str(&format!(
                "\"C:\\Dir{d}\\Sub Folder\\IMG_{f:04}.JPG\",{},{},,32\n",
                f * 1000,
                134_000_000_000_000_000u64 + f as u64 * 864_000_000_000
            ));
            text.push_str(&format!("\"C:\\Dir{d}\\report-{f}.txt\",10,,,32\n"));
            text.push_str(&format!("\"C:\\Dir{d}\\Café 季度 {f}.docx\",10,,,32\n"));
        }
    }
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, text.as_bytes(), "a.efu").unwrap();
    s.rebuild_name_order();
    s
}

#[test]
fn hot_paths_do_not_allocate() {
    let _profiler = dhat::Profiler::builder().testing().build();
    let s = sample();
    let caches = Caches::new();
    let cancel = fnc_exec::CancelToken::new();
    let ids: Vec<EntryId> = (0..s.len() as u32).map(EntryId).collect();
    let mut sc = Scratch::default();
    let mut path = String::with_capacity(1024);
    let mut hits = 0usize;

    for q in [
        "img",
        "*.jpg",
        "ext:jpg size:>1kb",
        "dir1\\sub",
        "café",
        "季度",
        "report !txt",
        "folder:",
        // M2: modifiers and functions evaluated per entry.
        "py:jidu",
        "pinyinjian:jd",
        "fuzzy:reprot",
        "ascii:img",
        "descendantcount:>1",
        "foldersize:>1kb",
        "siblingcount:>2",
        "nameparts:2",
        "type:jpg",
        "sibling:img",
        "hardlinkcount:1",
        "fileexists:$stem:.txt",
        "filelist:\"img_0001.jpg|c:\\dir1\\report-1.txt\"",
        "dm:*-02",
    ] {
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let c = Compiled::new(&p.root, &s, &caches, &cancel).unwrap();
        // Warm-up pass grows the scratch buffers once.
        for &id in &ids {
            hits += usize::from(c.matches(&s, id, &mut sc));
        }
        let before = dhat::HeapStats::get();
        for &id in &ids {
            hits += usize::from(c.matches(&s, id, &mut sc));
        }
        let after = dhat::HeapStats::get();
        dhat::assert_eq!(after.total_blocks, before.total_blocks, "query {q:?} allocated while matching");
    }
    assert!(hits > 0);
    hits = 0;

    // M3: property/content/hash leaves without stores and with an indexed property store
    // evaluate with bit tests only.
    let mut props = fnc_props::PropertyStore::new(s.instance());
    props.set_indexed(&[fnc_props::PropertyId::WIDTH, fnc_props::PropertyId::HEIGHT, fnc_props::PropertyId::ARTIST]);
    for &id in ids.iter().step_by(3) {
        props.insert(
            id,
            vec![
                (fnc_props::PropertyId::WIDTH, fnc_props::Value::Int(i64::from(id.0))),
                (fnc_props::PropertyId::ARTIST, fnc_props::Value::Text("John Doe".into())),
            ],
        );
    }
    let ext = fnc_exec::ExtCtx { props: Some(&props), ..fnc_exec::ExtCtx::default() };
    for q in
        ["width:>100", "width:unknown", "artist:doe", "dimensions:1x1", "content:x", "md5:unknown", "ext:jpg width:>10"]
    {
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let c = Compiled::with_ext(&p.root, &s, &caches, &cancel, &ext).unwrap();
        for &id in &ids {
            hits += usize::from(c.matches(&s, id, &mut sc));
        }
        let before = dhat::HeapStats::get();
        for &id in &ids {
            hits += usize::from(c.matches(&s, id, &mut sc));
        }
        let after = dhat::HeapStats::get();
        dhat::assert_eq!(after.total_blocks, before.total_blocks, "query {q:?} allocated while matching");
    }
    assert!(hits > 0, "property queries matched");

    for &id in &ids {
        s.full_path(id, &mut path);
    }
    let before = dhat::HeapStats::get();
    for &id in &ids {
        s.full_path(id, &mut path);
    }
    let mut ord = 0i32;
    for w in ids.windows(2) {
        ord += s.cmp_names(w[0], w[1]) as i32;
    }
    let after = dhat::HeapStats::get();
    dhat::assert_eq!(after.total_blocks, before.total_blocks, "path join / name compare allocated");
    assert!(ord != i32::MIN);
}

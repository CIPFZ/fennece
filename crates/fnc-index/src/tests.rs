use std::collections::BTreeSet;

use fnc_core::flags::attr;
use fnc_core::{EntryId, VolumeId};

use crate::*;

fn u16s(s: &str) -> Box<[u16]> {
    s.encode_utf16().collect()
}

fn dir() -> Meta {
    Meta::attrs(attr::DIRECTORY)
}

fn file(size: u64) -> Meta {
    Meta { attributes: Some(attr::ARCHIVE), size: Some(size), ..Meta::default() }
}

/// C:\ (5) ── Windows (10) ── System32 (11) ── cmd.exe (12)
///         └─ Users (20) ── a.txt (21)
/// Records are pushed child-first to exercise deferred parent resolution.
fn sample() -> (IndexStore, VolumeId) {
    let mut s = IndexStore::default();
    let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
    let mut b = s.bulk_load(vol);
    b.push(12, 11, &u16s("cmd.exe"), &file(300_000)).unwrap();
    b.push(21, 20, &u16s("a.txt"), &file(10)).unwrap();
    b.push(11, 10, &u16s("System32"), &dir()).unwrap();
    b.push(10, 5, &u16s("Windows"), &dir()).unwrap();
    b.push(20, 5, &u16s("Users"), &dir()).unwrap();
    b.push(5, 5, &u16s("."), &dir()).unwrap(); // the root record itself
    let stats = b.finish().unwrap();
    assert_eq!(stats, BulkStats { entries: 5, orphans: 0 });
    (s, vol)
}

fn find(s: &IndexStore, name: &str) -> EntryId {
    (0..s.len() as u32)
        .map(EntryId)
        .find(|&id| s.is_live(id) && s.name_string(id) == name)
        .unwrap_or_else(|| panic!("{name} not found"))
}

fn path(s: &IndexStore, id: EntryId) -> String {
    let mut p = String::new();
    s.full_path(id, &mut p);
    p
}

fn all_paths(s: &IndexStore) -> BTreeSet<String> {
    (0..s.len() as u32).map(EntryId).filter(|&id| s.is_live(id)).map(|id| path(s, id)).collect()
}

#[test]
fn bulk_build_resolves_parents_and_paths() {
    let (s, _) = sample();
    assert_eq!(s.live_count(), 6);
    assert_eq!(path(&s, find(&s, "cmd.exe")), "C:\\Windows\\System32\\cmd.exe");
    assert_eq!(path(&s, find(&s, "C:")), "C:");
    assert_eq!(s.depth(find(&s, "cmd.exe")), 3);
    let win = find(&s, "Windows");
    let kids: Vec<_> = s.children(win).map(|c| s.name_string(c)).collect();
    assert_eq!(kids, vec!["System32"]);
    assert_eq!(s.size(find(&s, "cmd.exe")), Some(300_000));
    assert_eq!(s.ext_dict().name(s.ext_id(find(&s, "cmd.exe"))), b"exe");
    assert_eq!(s.fold(find(&s, "System32")), b"system32");
    let mut fold = Vec::new();
    s.full_path_fold(find(&s, "a.txt"), &mut fold);
    assert_eq!(fold, b"c:\\users\\a.txt");
}

#[test]
fn orphans_go_to_hidden_folder() {
    let mut s = IndexStore::default();
    let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "D:\\"), Some(5)).unwrap();
    let mut b = s.bulk_load(vol);
    b.push(30, 999, &u16s("lost.bin"), &file(1)).unwrap();
    let stats = b.finish().unwrap();
    assert_eq!(stats.orphans, 1);
    let lost = find(&s, "lost.bin");
    assert_eq!(path(&s, lost), "D:\\$Orphans\\lost.bin");
    assert!(s.flags(s.parent(lost)).contains(fnc_core::EntryFlags::HIDDEN));
}

#[test]
fn parent_cycles_are_broken() {
    let mut s = IndexStore::default();
    let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "E:\\"), Some(5)).unwrap();
    let mut b = s.bulk_load(vol);
    b.push(40, 41, &u16s("x"), &dir()).unwrap();
    b.push(41, 40, &u16s("y"), &dir()).unwrap();
    let stats = b.finish().unwrap();
    assert_eq!(stats.orphans, 2);
    assert!(path(&s, find(&s, "x")).starts_with("E:\\$Orphans"));
}

#[test]
fn create_delete_rename_move() {
    let (mut s, vol) = sample();
    let users = find(&s, "Users");
    let g0 = s.generation();
    // IDX-02: create C:\t\a.txt
    let st = s.apply(vec![
        Change::Create { volume: vol, frn: 50, parent: EntryRef::Frn(vol, 5), name: u16s("t"), meta: dir() },
        Change::Create { volume: vol, frn: 51, parent: EntryRef::Frn(vol, 50), name: u16s("a.txt"), meta: file(1) },
    ]);
    assert_eq!(st.created, 2);
    assert!(s.generation() > g0);
    let new_a = s.resolve(EntryRef::Frn(vol, 51)).unwrap();
    assert_eq!(path(&s, new_a), "C:\\t\\a.txt");

    // Rename + move a.txt into Users as b.txt.
    let st = s.apply(vec![Change::Rename {
        target: EntryRef::Frn(vol, 51),
        new_name: u16s("b.TXT"),
        new_parent: EntryRef::Id(users),
    }]);
    assert_eq!(st.renamed, 1);
    assert_eq!(path(&s, new_a), "C:\\Users\\b.TXT");
    assert_eq!(s.fold(new_a), b"b.txt");
    let kids: BTreeSet<_> = s.children(users).map(|c| s.name_string(c)).collect();
    assert_eq!(kids, ["a.txt", "b.TXT"].iter().map(|s| s.to_string()).collect());
    assert_eq!(s.children(s.resolve(EntryRef::Frn(vol, 50)).unwrap()).count(), 0);

    // Delete Windows subtree.
    let before = s.live_count();
    let st = s.apply(vec![Change::Delete(EntryRef::Frn(vol, 10))]);
    assert_eq!(st.deleted, 3);
    assert_eq!(s.live_count(), before - 3);
    assert!(s.resolve(EntryRef::Frn(vol, 12)).is_none());
    assert!(!all_paths(&s).iter().any(|p| p.contains("Windows")));

    // Meta update.
    let st = s.apply(vec![Change::Meta { target: EntryRef::Id(new_a), meta: file(77) }]);
    assert_eq!(st.updated, 1);
    assert_eq!(s.size(new_a), Some(77));
    assert!(s.date_recently_changed(new_a).is_known());

    // Unresolvable changes are counted, not applied.
    let st = s.apply(vec![Change::Delete(EntryRef::Frn(vol, 12345))]);
    assert_eq!(st.unresolved, 1);
}

#[test]
fn moving_folder_into_itself_is_rejected() {
    let (mut s, vol) = sample();
    let st = s.apply(vec![Change::Rename {
        target: EntryRef::Frn(vol, 10),
        new_name: u16s("Windows"),
        new_parent: EntryRef::Frn(vol, 11),
    }]);
    assert_eq!(st.unresolved, 1);
    assert_eq!(path(&s, find(&s, "cmd.exe")), "C:\\Windows\\System32\\cmd.exe");
}

#[test]
fn batch_order_does_not_matter_for_parents() {
    // A file moved into a folder created later in the same batch, and a create whose
    // parent appears later: both resolve on the retry pass.
    let (mut s, vol) = sample();
    let st = s.apply(vec![
        Change::Create { volume: vol, frn: 61, parent: EntryRef::Frn(vol, 60), name: u16s("late.txt"), meta: file(1) },
        Change::Rename { target: EntryRef::Frn(vol, 21), new_name: u16s("a.txt"), new_parent: EntryRef::Frn(vol, 60) },
        Change::Create { volume: vol, frn: 60, parent: EntryRef::Frn(vol, 5), name: u16s("new"), meta: dir() },
        Change::Delete(EntryRef::Frn(vol, 99_999)),
    ]);
    assert_eq!((st.created, st.renamed, st.unresolved), (2, 1, 1));
    assert_eq!(path(&s, s.resolve(EntryRef::Frn(vol, 61)).unwrap()), "C:\\new\\late.txt");
    assert_eq!(path(&s, s.resolve(EntryRef::Frn(vol, 21)).unwrap()), "C:\\new\\a.txt");
}

#[test]
fn compact_keeps_dense_frn_maps_with_orphans() {
    let mut s = IndexStore::default();
    let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
    let mut b = s.bulk_load(vol);
    for i in 0..1000u64 {
        b.push(100 + i, 5, &u16s(&format!("f{i}")), &file(1)).unwrap();
    }
    b.push(2_000, 1_999, &u16s("orphan"), &file(1)).unwrap();
    b.finish().unwrap();
    s.compact();
    assert!(matches!(s.frn_maps[vol.idx()], crate::frnmap::FrnMap::Dense { .. }));
    assert!(s.resolve(EntryRef::Frn(vol, 500)).is_some());
    assert!(s.resolve(EntryRef::Frn(vol, crate::store::ORPHANS_FRN)).is_some());
    let c = s.clone();
    assert_ne!(c.instance(), s.instance(), "clones are separate id spaces");
}

#[test]
fn idx03_rename_folder_with_100k_children() {
    let mut s = IndexStore::default();
    let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
    let mut b = s.bulk_load(vol);
    b.push(6, 5, &u16s("t"), &dir()).unwrap();
    let mut name = Vec::new();
    for i in 0..100_000u64 {
        name.clear();
        name.extend(format!("file{i}.dat").encode_utf16());
        b.push(100 + i, 6, &name, &file(i)).unwrap();
    }
    b.finish().unwrap();
    let t0 = std::time::Instant::now();
    let st = s.apply(vec![Change::Rename {
        target: EntryRef::Frn(vol, 6),
        new_name: u16s("u"),
        new_parent: EntryRef::Frn(vol, 5),
    }]);
    let elapsed = t0.elapsed();
    assert_eq!(st.renamed, 1);
    assert!(elapsed.as_millis() < 50, "rename took {elapsed:?}");
    let child = s.resolve(EntryRef::Frn(vol, 100 + 99_999)).unwrap();
    assert_eq!(path(&s, child), "C:\\u\\file99999.dat");
    assert_eq!(s.children(s.resolve(EntryRef::Frn(vol, 6)).unwrap()).count(), 100_000);
}

#[test]
fn compact_preserves_content() {
    let (mut s, vol) = sample();
    s.apply(vec![Change::Delete(EntryRef::Frn(vol, 20))]);
    let before = all_paths(&s);
    s.record_run(find(&s, "cmd.exe"));
    let remap = s.compact();
    assert_eq!(remap.iter().filter(|&&r| r == u32::MAX).count(), 2);
    assert_eq!(s.len(), s.live_count());
    assert_eq!(all_paths(&s), before);
    assert_eq!(s.run_info(find(&s, "cmd.exe")).count, 1);
    assert_eq!(path(&s, s.resolve(EntryRef::Frn(vol, 12)).unwrap()), "C:\\Windows\\System32\\cmd.exe");
}

#[test]
fn name_order_ranks_and_dirty_tracking() {
    let (mut s, vol) = sample();
    s.rebuild_name_order();
    let a = find(&s, "a.txt");
    let cmd = find(&s, "cmd.exe");
    assert!(s.name_order().is_clean(a));
    assert!(s.name_order().rank(a) < s.name_order().rank(cmd));
    s.apply(vec![Change::Rename {
        target: EntryRef::Id(a),
        new_name: u16s("z.txt"),
        new_parent: EntryRef::Frn(vol, 20),
    }]);
    assert!(!s.name_order().is_clean(a));
    assert!(s.name_order().is_clean(cmd));
}

#[test]
fn efu_roundtrip() {
    let (s, _) = sample();
    let mut out = Vec::new();
    let mut w = efu::EfuWriter::new(&mut out).unwrap();
    let mut buf = String::new();
    for id in (0..s.len() as u32).map(EntryId).filter(|&id| s.is_live(id)) {
        w.write_entry(&s, id, &mut buf).unwrap();
    }
    w.finish().unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.starts_with("Filename,Size,Date Modified,Date Created,Attributes\r\n"));
    assert!(text.contains("\"C:\\Windows\\System32\\cmd.exe\",300000,,,32\r\n"));

    let mut t = IndexStore::default();
    let (vol, stats) = efu::load_efu(&mut t, &out[..], "test.efu").unwrap();
    assert_eq!(stats.orphans, 0);
    assert_eq!(all_paths(&t), all_paths(&s));
    let cmd = find(&t, "cmd.exe");
    assert_eq!(t.size(cmd), Some(300_000));
    assert!(t.flags(cmd).contains(fnc_core::EntryFlags::OFFLINE));
    assert_eq!(t.volume_info(vol).unwrap().info.kind, VolumeKind::FileList);
}

#[test]
fn efu_implicit_folders_quotes_bom_and_iso_dates() {
    let text = "\u{feff}Filename,Size,Date Modified,Date Created,Attributes\n\
                \"D:\\x\\y \"\"q\"\".txt\",5,2026-07-02T10:52:00Z,134274631200000000,32\n\
                \"\\\\srv\\share\\doc.pdf\",,,,\n\
                \"D:\\x\\\",,,,16\n";
    let mut t = IndexStore::default();
    efu::load_efu(&mut t, text.as_bytes(), "x.efu").unwrap();
    let q = find(&t, "y \"q\".txt");
    assert_eq!(path(&t, q), "D:\\x\\y \"q\".txt");
    assert_eq!(t.date_modified(q).0, 134_274_631_200_000_000);
    assert_eq!(t.date_created(q).0, 134_274_631_200_000_000);
    assert_eq!(path(&t, find(&t, "doc.pdf")), "\\\\srv\\share\\doc.pdf");
    assert_eq!(t.children(find(&t, "D:")).count(), 1, "explicit folder line must not duplicate x");
}

#[test]
fn csv_split() {
    let mut v = Vec::new();
    efu::split_csv_line("\"a,b\",1,,\"c\"\"d\"", &mut v);
    assert_eq!(v, vec!["a,b", "1", "", "c\"d"]);
}

#[test]
fn shared_snapshot_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let shared = SharedIndex::new(sample().0);
    let snap = shared.snapshot();
    assert_send(&snap);
    assert_eq!(snap.live_count(), 6);
    drop(snap);
    shared.write().record_run(EntryId(1));
}

mod model {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Create { parent: usize, name: u8, is_dir: bool },
        Delete(usize),
        Rename { target: usize, parent: usize, name: u8 },
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (0usize..64, 0u8..6, any::<bool>()).prop_map(|(parent, name, is_dir)| Op::Create { parent, name, is_dir }),
            1 => (0usize..64).prop_map(Op::Delete),
            2 => (0usize..64, 0usize..64, 0u8..6).prop_map(|(target, parent, name)| Op::Rename { target, parent, name }),
        ]
    }

    proptest! {
        /// Applying random change sequences equals rebuilding from the resulting tree.
        #[test]
        fn incremental_equals_rebuild(ops in proptest::collection::vec(op(), 1..60)) {
            let mut s = IndexStore::default();
            let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
            let mut frns: Vec<u64> = vec![5];
            let mut next = 100u64;
            for op in ops {
                let live: Vec<u64> = frns.iter().copied().filter(|&f| s.resolve(EntryRef::Frn(vol, f)).is_some()).collect();
                let dirs: Vec<u64> = live.iter().copied().filter(|&f| s.is_dir(s.resolve(EntryRef::Frn(vol, f)).unwrap())).collect();
                match op {
                    Op::Create { parent, name, is_dir } => {
                        let p = dirs[parent % dirs.len()];
                        s.apply(vec![Change::Create { volume: vol, frn: next, parent: EntryRef::Frn(vol, p), name: u16s(&format!("n{name}")), meta: if is_dir { dir() } else { file(1) } }]);
                        frns.push(next);
                        next += 1;
                    }
                    Op::Delete(t) => {
                        let f = live[t % live.len()];
                        if f != 5 { s.apply(vec![Change::Delete(EntryRef::Frn(vol, f))]); }
                    }
                    Op::Rename { target, parent, name } => {
                        let f = live[target % live.len()];
                        let p = dirs[parent % dirs.len()];
                        s.apply(vec![Change::Rename { target: EntryRef::Frn(vol, f), new_name: u16s(&format!("r{name}")), new_parent: EntryRef::Frn(vol, p) }]);
                    }
                }
            }
            // Invariants: children index agrees with the parent column.
            for id in (0..s.len() as u32).map(EntryId).filter(|&id| s.is_live(id)) {
                let p = s.parent(id);
                if p.is_some() {
                    prop_assert!(s.is_live(p), "live entry with dead parent");
                    prop_assert_eq!(s.children(p).filter(|&c| c == id).count(), 1);
                }
            }
            let paths = all_paths(&s);
            let mut c = s.clone();
            c.compact();
            prop_assert_eq!(all_paths(&c), paths.clone());
            let mut r = c.clone();
            r.rebuild_children();
            for id in (0..r.len() as u32).map(EntryId) {
                let a: BTreeSet<_> = r.children(id).collect();
                let b: BTreeSet<_> = c.children(id).collect();
                prop_assert_eq!(a, b);
            }
        }
    }
}

#[test]
fn hard_links_become_aliases() {
    let (mut s, vol) = sample();
    let cmd = s.resolve(EntryRef::Frn(vol, 12)).unwrap();
    let users = s.resolve(EntryRef::Frn(vol, 20)).unwrap();
    // A listing of C:\Users shows the same file (FRN 12) as "cmd-link.exe".
    let touch = |name: &str, parent: u64, size: u64| Change::Touch {
        volume: vol,
        frn: 12,
        parent: EntryRef::Frn(vol, parent),
        name: u16s(name),
        meta: file(size),
    };
    let st = s.apply(vec![touch("cmd-link.exe", 20, 300_000)]);
    assert_eq!(st.created, 1);
    assert_eq!(s.hardlink_count(cmd), 2);
    let alias = s.children(users).find(|&c| s.name_string(c) == "cmd-link.exe").unwrap();
    assert!(s.flags(alias).contains(fnc_core::EntryFlags::HARDLINK_ALIAS));
    assert_eq!(s.size(alias), Some(300_000));
    // Seeing a known name again only refreshes metadata (no duplicate, no rename).
    let st = s.apply(vec![touch("cmd-link.exe", 20, 1), touch("cmd.exe", 11, 1)]);
    assert_eq!((st.created, st.renamed, st.updated), (0, 0, 2));
    assert_eq!(path(&s, cmd), "C:\\Windows\\System32\\cmd.exe");
    assert_eq!((s.size(cmd), s.size(alias)), (Some(1), Some(1)));
    // A USN create/rename record naming the alias does not move the primary.
    s.apply(vec![Change::Create {
        volume: vol,
        frn: 12,
        parent: EntryRef::Frn(vol, 20),
        name: u16s("cmd-link.exe"),
        meta: file(2),
    }]);
    assert_eq!(path(&s, cmd), "C:\\Windows\\System32\\cmd.exe");
    // Metadata by FRN reaches every name.
    s.apply(vec![Change::Meta { target: EntryRef::Frn(vol, 12), meta: file(5) }]);
    assert_eq!(s.size(alias), Some(5));
    // Compaction keeps the FRN on the primary and the alias list.
    s.compact();
    let cmd = s.resolve(EntryRef::Frn(vol, 12)).unwrap();
    assert_eq!(s.name_string(cmd), "cmd.exe");
    assert_eq!(s.hardlink_count(cmd), 2);
    // Deleting the primary's folder hands the FRN to the alias.
    s.apply(vec![Change::Delete(EntryRef::Frn(vol, 11))]);
    let heir = s.resolve(EntryRef::Frn(vol, 12)).unwrap();
    assert_eq!(path(&s, heir), "C:\\Users\\cmd-link.exe");
    assert!(!s.flags(heir).contains(fnc_core::EntryFlags::HARDLINK_ALIAS));
    // Deleting the file removes every remaining name.
    s.apply(vec![touch("third.exe", 10, 5)]);
    assert_eq!(s.hardlink_count(heir), 2);
    let st = s.apply(vec![Change::Delete(EntryRef::Frn(vol, 12))]);
    assert_eq!(st.deleted, 2);
    assert!(!all_paths(&s).iter().any(|p| p.ends_with(".exe")));
}

#[test]
fn touch_inserts_unknown_files_and_renames_folders() {
    let (mut s, vol) = sample();
    let st = s.apply(vec![Change::Touch {
        volume: vol,
        frn: 90,
        parent: EntryRef::Frn(vol, 20),
        name: u16s("missed.txt"),
        meta: file(3),
    }]);
    assert_eq!(st.created, 1);
    assert_eq!(path(&s, s.resolve(EntryRef::Frn(vol, 90)).unwrap()), "C:\\Users\\missed.txt");
    // Folders cannot be hard-linked: a different name is a rename.
    s.apply(vec![Change::Touch {
        volume: vol,
        frn: 20,
        parent: EntryRef::Frn(vol, 5),
        name: u16s("Users2"),
        meta: dir(),
    }]);
    assert_eq!(path(&s, s.resolve(EntryRef::Frn(vol, 90)).unwrap()), "C:\\Users2\\missed.txt");
}

#[test]
fn initial_metadata_is_not_a_recent_change() {
    let (mut s, vol) = sample();
    let cmd = s.resolve(EntryRef::Frn(vol, 12)).unwrap();
    s.apply_initial(vec![Change::Meta { target: EntryRef::Id(cmd), meta: file(9) }]);
    assert_eq!(s.size(cmd), Some(9));
    assert!(!s.date_recently_changed(cmd).is_known());
    s.apply(vec![Change::Meta { target: EntryRef::Id(cmd), meta: file(10) }]);
    assert!(s.date_recently_changed(cmd).is_known());
}

#[test]
fn absorb_appends_a_privately_built_volume() {
    let (mut s, _vol) = sample();
    let cmd = find(&s, "cmd.exe");
    let before = all_paths(&s);
    let mut other = IndexStore::default();
    let v = other.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "T:\\"), Some(5)).unwrap();
    other.set_journal_position(v, 9, 99);
    let mut b = other.bulk_load(v);
    b.push(30, 5, &u16s("新建文件夹"), &dir()).unwrap();
    b.push(31, 30, &u16s("notes.md"), &file(7)).unwrap();
    b.push(32, 5, &u16s("gone.tmp"), &file(1)).unwrap();
    b.finish().unwrap();
    other.apply(vec![Change::Delete(EntryRef::Frn(v, 32))]);
    let ids = s.absorb(other).unwrap();
    let t = ids[0];
    assert_eq!(s.volume_info(t).unwrap().next_usn, 99);
    let notes = s.resolve(EntryRef::Frn(t, 31)).unwrap();
    assert_eq!(path(&s, notes), "T:\\新建文件夹\\notes.md");
    assert_eq!(s.size(notes), Some(7));
    assert_eq!(s.ext_dict().name(s.ext_id(notes)), b"md");
    // Old ids are untouched; the new volume works incrementally.
    assert_eq!(s.name_string(cmd), "cmd.exe");
    let mut after = all_paths(&s);
    after.retain(|p| !p.starts_with("T:"));
    assert_eq!(after, before);
    s.apply(vec![Change::Rename {
        target: EntryRef::Frn(t, 31),
        new_name: u16s("n.md"),
        new_parent: EntryRef::Frn(t, 5),
    }]);
    assert_eq!(path(&s, notes), "T:\\n.md");
    assert!(!all_paths(&s).iter().any(|p| p.contains("gone.tmp")));
}

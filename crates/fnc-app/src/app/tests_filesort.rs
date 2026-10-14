//! Sorting by the file-reading columns in the window (REL-06): the header click counts the
//! whole result's unread files and asks; Cancel and a new search leave the order alone;
//! Continue reads in the background and then sorts; a second click with everything cached
//! sorts without asking.

use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use fnc_index::SharedIndex;
use fnc_props::filecache::FileField;

use super::tests::{deps_for, harness, index_folder, wait_query, wait_until};
use super::*;

/// 300 files whose signatures (gif / pdf / png, by `i % 3`) disagree with name order.
fn fixture(dir: &std::path::Path) {
    for i in 0..300 {
        let data: &[u8] = match i % 3 {
            0 => b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR",
            1 => b"GIF89a\x01\0\x01\0",
            _ => b"%PDF-1.4\n",
        };
        std::fs::write(dir.join(format!("f{i:03}.bin")), data).unwrap();
    }
}

fn names(a: &FennecApp) -> Vec<String> {
    let s = a.shared.snapshot();
    a.v.results.ids.iter().map(|&id| s.name_string(id)).collect()
}

fn settled(a: &FennecApp) -> bool {
    a.v.latest_shown + 1 == a.v.next_id
}

fn click_header(h: &mut egui_kittest::Harness<'_, FennecApp>) {
    h.query_by_label_contains(&crate::columns::ColumnId::FileSignature.title()).unwrap().click();
    h.run_steps(2);
}

#[test]
fn file_signature_sort_asks_reads_and_sorts() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let shared = SharedIndex::default();
    index_folder(&shared, dir.path());
    let mut deps = deps_for(None, shared.clone());
    deps.settings.columns = ["name:200", "filesignature:120"].into_iter().map(String::from).collect();
    deps.stores = crate::stores::Stores::in_memory(shared.clone(), &deps.settings);
    let mut h = harness(deps);
    wait_query(&mut h, "ext:bin", 300);
    let by_name = names(h.state());
    // The visible rows are read lazily; wait until they are cached.
    for _ in 0..5 {
        h.state().stores.cells.wait_idle();
        h.run_steps(2);
    }
    let expected = {
        let a = h.state();
        let s = a.shared.snapshot();
        a.stores.cells.missing(&s, &a.v.results.ids, &[FileField::Signature], &Default::default()).len()
    };
    assert!(expected >= 250, "the whole result counts, not the visible rows: {expected}");

    // Click → dialog with N; Cancel leaves the order unchanged.
    click_header(&mut h);
    wait_until(&mut h, "dialog", |a| matches!(a.dialog, Some(dialogs::Dialog::ReadFiles { .. })));
    h.run_steps(2);
    assert!(matches!(h.state().dialog, Some(dialogs::Dialog::ReadFiles { count, .. }) if count == expected));
    assert!(h.query_by_label_contains(&format!("{expected} files need to be read")).is_some());
    h.query_by_label(&t("dialog-cancel")).unwrap().click();
    h.run_steps(3);
    assert!(h.state().dialog.is_none() && h.state().v.file_sort.is_none());
    assert_eq!(h.state().v.sort.files, [None; 3], "sort unchanged");
    assert!(settled(h.state()));
    assert_eq!(names(h.state()), by_name, "order unchanged");

    // A new search while the dialog is open cancels too.
    click_header(&mut h);
    wait_until(&mut h, "dialog again", |a| matches!(a.dialog, Some(dialogs::Dialog::ReadFiles { .. })));
    wait_query(&mut h, "f00", 10);
    assert!(h.state().dialog.is_none() && h.state().v.file_sort.is_none());
    assert_eq!(h.state().v.sort.files, [None; 3]);
    wait_query(&mut h, "ext:bin", 300);

    // Continue → background read → sorted (signature ascending: gif, pdf, png).
    click_header(&mut h);
    wait_until(&mut h, "dialog for reading", |a| matches!(a.dialog, Some(dialogs::Dialog::ReadFiles { .. })));
    h.query_by_label(&t("dialog-continue")).unwrap().click();
    wait_until(&mut h, "sorted by signature", |a| a.v.sort.files[0] == Some(FileField::Signature) && settled(a));
    wait_until(&mut h, "results", |a| a.v.results.len() == 300);
    let sorted = names(h.state());
    let kind = |n: &str| n[1..4].parse::<u32>().unwrap() % 3;
    let mut want = by_name.clone();
    // gif (1), pdf (2), png (0); ties by name.
    want.sort_by_key(|n| ([1, 2, 0].iter().position(|&k| k == kind(n)).unwrap(), n.clone()));
    assert_eq!(sorted, want);
    assert!(
        h.state()
            .stores
            .cells
            .missing(
                &h.state().shared.snapshot(),
                &h.state().v.results.ids,
                &[FileField::Signature],
                &Default::default()
            )
            .is_empty()
    );
    assert_eq!(
        crate::columns::sort_setting(&h.state().v.sort),
        "filesignature-ascending",
        "persisted like other columns"
    );

    // Everything cached: the next click flips the order at once, without a dialog.
    click_header(&mut h);
    let t0 = Instant::now();
    while !(h.state().v.sort.files[0] == Some(FileField::Signature)
        && !h.state().v.sort.ascending
        && settled(h.state()))
    {
        assert!(h.state().dialog.is_none(), "no dialog when everything is cached");
        assert!(t0.elapsed() < Duration::from_secs(20), "descending sort never arrived");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(h.state().dialog.is_none());
    let mut rev = want;
    rev.reverse();
    assert_eq!(names(h.state()), rev);
}

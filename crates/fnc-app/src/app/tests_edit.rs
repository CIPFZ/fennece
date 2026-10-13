//! Advanced rename and undo/redo on real files (SPEC ch.3 §9–10).

use std::time::Duration;

use egui_kittest::kittest::Queryable;
use fnc_index::SharedIndex;

use super::tests::{deps_for, harness, wait_until};
use super::*;

#[test]
#[cfg(not(windows))]
fn advanced_rename_then_undo_and_redo() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("photos");
    std::fs::create_dir_all(&root).unwrap();
    for n in ["IMG_0001.JPG", "IMG_0002.JPG"] {
        std::fs::write(root.join(n), n).unwrap();
    }
    let efu = dir.path().join("t.efu");
    let mut list = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
    list.push_str(&format!("\"{}\",,,,16\n", root.display()));
    for n in ["IMG_0001.JPG", "IMG_0002.JPG"] {
        list.push_str(&format!("\"{}\",4,,,32\n", root.join(n).display()));
    }
    std::fs::write(&efu, list).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && !a.v.results.is_empty());
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("ext:jpg");
    wait_until(&mut h, "two pictures", |a| a.v.sent_query.as_deref() == Some("ext:jpg") && a.v.results.len() == 2);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::SelectAll);
    // F2 with two selected opens advanced rename.
    h.state_mut().run(&ctx, Command::Rename);
    h.run();
    assert!(h.state().edit.rename.is_some());
    if let Some(r) = h.state_mut().edit.rename.as_mut() {
        r.rule.template = "Holiday $n.$ext:".into();
        r.rule.digits = 2;
        r.rule.keep_ext = false;
        r.rule.case = crate::rename::CaseMode::Lower;
    }
    h.run();
    assert!(h.query_by_label("holiday 01.jpg").is_some(), "preview shows the new name");
    h.get_by_label("Rename 2 items").click();
    let renamed = |root: &std::path::Path| root.join("holiday 01.jpg").exists() && root.join("holiday 02.jpg").exists();
    wait_until(&mut h, "renamed on disk", |_| renamed(&root));
    wait_until(&mut h, "undo recorded", |a| a.can_undo(false));
    assert!(!root.join("IMG_0001.JPG").exists());
    h.state_mut().run(&ctx, Command::Undo);
    wait_until(&mut h, "undone", |a| a.can_undo(true));
    assert!(root.join("IMG_0001.JPG").exists() && root.join("IMG_0002.JPG").exists() && !renamed(&root));
    h.state_mut().run(&ctx, Command::Redo);
    wait_until(&mut h, "redone", |a| a.can_undo(false) && !a.can_undo(true));
    assert!(renamed(&root));
    // Delete to the recycle bin, then undo restores the file.
    let victim = root.join("holiday 01.jpg");
    h.state_mut().recycle_paths(vec![victim.clone()]);
    wait_until(&mut h, "delete recorded", |a| a.undo_label(false).contains("delete") && !victim.exists());
    h.state_mut().run(&ctx, Command::Undo);
    let t = std::time::Instant::now();
    while !victim.exists() && t.elapsed() < Duration::from_secs(10) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "IMG_0001.JPG");
}

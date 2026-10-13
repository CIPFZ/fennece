//! Semantic search in the window: the toggle, the status bar without a model (SEM-07),
//! and nothing semantic while it is off (SEM-06).

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use fnc_index::SharedIndex;

use super::tests::{deps_for, harness, wait_until};
use super::*;

const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
\"C:\\Docs\\report.docx\",12000,,,32\n\
\"C:\\Docs\\季度预算表.xlsx\",1048576,,,32\n";

#[test]
fn sem07_missing_model_reports_and_does_not_crash() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut deps = deps_for(Some(efu), SharedIndex::default());
    deps.settings_path = dir.path().join("data").join("settings.toml");
    deps.settings.semantic_enabled = true;
    deps.settings.semantic_folders = vec!["C:\\Docs".into()];
    let mut h = harness(deps);
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() >= 2);
    assert!(h.state().v.search_opts.semantic.is_some());
    // The toggle is shown and the status bar says the model is missing.
    assert!(h.query_by_label("Semantic").is_some());
    wait_until(&mut h, "no-model status", |a| a.semantic_status().is_some_and(|s| s.contains("model missing")));
    h.get_by_role(Role::TextInput).type_text("sem:budget");
    wait_until(&mut h, "semantic warning", |a| {
        a.v.sent_query.as_deref() == Some("sem:budget") && a.v.diagnostics.iter().any(|d| d.contains("model file"))
    });
    assert!(h.state().v.results.is_empty());
    // Plain searches still work.
    h.key_press(egui::Key::Escape);
    h.get_by_role(Role::TextInput).type_text("report");
    wait_until(&mut h, "literal results", |a| a.v.sent_query.as_deref() == Some("report") && a.v.results.len() == 1);
    // Clicking the status opens the setup window with a download button.
    h.state_mut().semantic_open_setup();
    h.run();
    assert!(h.query_by_label_contains("Download the model").is_some());
}

#[test]
fn sem06_off_means_no_semantic_source() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }));
    assert!(h.state().v.search_opts.semantic.is_none());
    assert!(h.state().semantic_status().is_none());
    assert!(h.query_by_label("Semantic").is_none());
}

#[test]
fn semantic_searches_sort_by_relevance_and_restore_the_sort() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }));
    let before = h.state().v.sort;
    h.state_mut().set_query("sem:budget".into());
    assert_eq!(h.state().v.sort.column, fnc_exec::SortColumn::Relevance);
    assert!(!h.state().v.sort.ascending);
    h.state_mut().set_query("report".into());
    assert_eq!(h.state().v.sort, before);
}

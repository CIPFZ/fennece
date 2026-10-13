//! Tabs and sessions (SPEC ch.3 §7).

use egui_kittest::kittest::Queryable;
use fnc_index::SharedIndex;

use super::tests::{deps_for, harness, wait_until};
use super::*;

const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
\"C:\\Photos\\IMG_0001.JPG\",3000000,,,32\n\
\"C:\\Photos\\IMG_0002.jpg\",4000000,,,32\n\
\"C:\\Docs\\report.docx\",12000,,,32\n";

#[test]
fn tabs_have_their_own_searches_and_sessions_restore_them() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let settings_path = dir.path().join("data").join("settings.toml");
    let mut deps = deps_for(Some(efu.clone()), SharedIndex::default());
    deps.settings_path = settings_path.clone();
    deps.settings.restore_session = true;
    let mut h = harness(deps);
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && !a.v.results.is_empty());
    let ctx = h.ctx.clone();
    h.state_mut().v.query = "img".into();
    h.state_mut().send_search();
    wait_until(&mut h, "img", |a| a.v.results.len() == 2);
    // New tab: empty search, own results; the tab bar appears.
    h.state_mut().run(&ctx, Command::NewTab);
    h.run();
    assert_eq!(h.state().tabs.active(), 1);
    assert!(h.query_by_label("img").is_some(), "tab bar shows the first tab");
    h.state_mut().v.query = "report".into();
    h.state_mut().send_search();
    wait_until(&mut h, "report", |a| a.v.sent_query.as_deref() == Some("report") && a.v.results.len() == 1);
    // Ctrl+Tab back: the first tab kept its search and results.
    h.state_mut().run(&ctx, Command::NextTab);
    h.run();
    assert_eq!((h.state().tabs.active(), h.state().v.query.as_str()), (0, "img"));
    wait_until(&mut h, "img results kept", |a| a.v.results.len() == 2);
    // Save the session, close the second tab.
    h.state_mut().save_session();
    h.state_mut().run(&ctx, Command::NextTab);
    h.state_mut().run(&ctx, Command::CloseTab);
    h.run();
    assert_eq!(h.state().tabs.queries().len(), 1);
    assert_eq!(h.state().v.query, "img");
    // The last tab: Ctrl+W clears it but keeps the window.
    h.state_mut().run(&ctx, Command::CloseTab);
    assert!(h.state().v.query.is_empty());
    drop(h);

    // A new start reopens both tabs.
    let mut deps = deps_for(Some(efu), SharedIndex::default());
    deps.settings_path = settings_path;
    deps.settings.restore_session = true;
    let mut h = harness(deps);
    wait_until(&mut h, "restored", |a| a.tabs.queries().len() == 2);
    assert_eq!(h.state().tabs.active(), 0);
    assert_eq!(h.state().v.query, "img");
    assert_eq!(h.state().tabs.view(1).map(|v| v.query.as_str()), Some("report"));
}

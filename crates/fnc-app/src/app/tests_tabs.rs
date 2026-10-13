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

#[test]
fn find_bar_jumps_without_changing_results() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut deps = deps_for(Some(efu), SharedIndex::default());
    deps.settings.findbar_ctrl_f = true;
    let mut h = harness(deps);
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() >= 3);
    // Let late refreshes settle (a new result set re-maps the cursor).
    for _ in 0..20 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let ctx = h.ctx.clone();
    let n = h.state().v.results.len();
    h.state_mut().v.focus = Focus::Results;
    h.state_mut().run(&ctx, Command::FindInResults);
    h.run();
    assert!(h.state().v.find.open);
    assert!(h.query_by_label("Find:").is_some());
    // Names of the selected row (ids are stable across result sets, row indices are not).
    let sel = |a: &FennecApp| a.v.selected_id.map(|id| a.shared.snapshot().name_string(id).to_lowercase());
    h.state_mut().v.find.text = "img_000".into();
    assert!(h.state_mut().find_step(true));
    let first = sel(h.state()).unwrap();
    assert!(first.starts_with("img_000"), "{first}");
    h.state_mut().run(&ctx, Command::FindNext);
    let second = sel(h.state()).unwrap();
    assert!(second.starts_with("img_000") && second != first, "{second}");
    // Wraps around; the result set is unchanged.
    h.state_mut().run(&ctx, Command::FindNext);
    assert_eq!(sel(h.state()).as_deref(), Some(first.as_str()));
    h.state_mut().run(&ctx, Command::FindPrevious);
    assert_eq!(sel(h.state()).as_deref(), Some(second.as_str()));
    assert_eq!(h.state().v.results.len(), n);
    // Case-sensitive: no match, the selection stays.
    h.state_mut().v.find.case = true;
    h.state_mut().run(&ctx, Command::FindNext);
    assert_eq!(sel(h.state()).as_deref(), Some(second.as_str()));
}

#[test]
fn sidebar_folder_click_toolbar_and_back_forward() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut deps = deps_for(Some(efu), SharedIndex::default());
    deps.settings.show_sidebar = true;
    deps.settings.show_toolbar = true;
    let mut h = harness(deps);
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && !a.v.results.is_empty());
    h.run();
    // The tree starts at the volume root; expand it and click a folder.
    assert!(h.query_by_label("Folders").is_some());
    let root = h.state().shared.snapshot().volumes()[0].roots[0];
    let root_label = {
        let s = h.state().shared.snapshot();
        let mut p = String::new();
        s.full_path(root, &mut p);
        p
    };
    // Only the sidebar shows the root's name once the results are narrowed.
    h.state_mut().set_query("ext:docx".into());
    wait_until(&mut h, "docx", |a| a.v.results.len() == 1);
    h.run();
    h.get_by_label(&root_label).click();
    h.run();
    wait_until(&mut h, "folder search", |a| a.v.query.starts_with('"') && a.v.query.ends_with("\\\""));
    // Back/forward through used searches.
    let ctx = h.ctx.clone();
    for q in ["img", "report"] {
        h.state_mut().set_query(q.to_string());
        h.state_mut().record_history();
    }
    h.state_mut().run(&ctx, Command::NavigateBack);
    assert_eq!(h.state().v.query, "img");
    h.state_mut().run(&ctx, Command::NavigateForward);
    assert_eq!(h.state().v.query, "report");
    // Toolbar buttons are there (back is enabled now).
    h.run();
    assert!(h.query_by_label("◀").is_some() && h.query_by_label("⟳").is_some());
}

#[test]
fn high_contrast_theme() {
    let mut h = harness(deps_for(None, SharedIndex::default()));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::ThemeHighContrast);
    h.run();
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    assert_eq!(h.ctx.style_of(egui::Theme::Dark).visuals.panel_fill, egui::Color32::BLACK);
    h.state_mut().run(&ctx, Command::ThemeDark);
    h.run();
    assert_ne!(h.ctx.style_of(egui::Theme::Dark).visuals.panel_fill, egui::Color32::BLACK, "plain dark restored");
}

#[test]
fn custom_formula_columns() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut deps = deps_for(Some(efu), SharedIndex::default());
    deps.settings.custom_columns = vec!["Big=IF($size:>3mb,\"yes\",\"no\")".into(), "UPPER($ext:)".into()];
    deps.settings.columns = vec!["name:300".into(), "custom1:80".into(), "custom2:80".into()];
    let mut h = harness(deps);
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && !a.v.results.is_empty());
    h.state_mut().set_query("ext:jpg".into());
    wait_until(&mut h, "jpg", |a| a.v.results.len() == 2);
    h.run();
    // Titles from the setting (or "Custom n"), values from the formulas.
    assert!(h.query_by_label("Big").is_some());
    assert!(h.query_by_label("Custom 2").is_some());
    assert!(h.query_by_label("yes").is_some(), "4 MB photo");
    assert!(h.query_by_label("no").is_some(), "3 MB photo");
    assert!(!h.query_all_by_label("JPG").collect::<Vec<_>>().is_empty());
}

#[test]
fn other_tabs_follow_index_changes() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && !a.v.results.is_empty());
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::NewTab);
    h.state_mut().set_query("ext:jpg".into());
    wait_until(&mut h, "two pictures", |a| a.v.sent_query.as_deref() == Some("ext:jpg") && a.v.results.len() == 2);
    // The indexer only notifies the first view's engine; this tab must still follow.
    let victim = h.state().v.results.ids[0];
    h.state().shared.write().apply(vec![fnc_index::Change::Delete(fnc_index::EntryRef::Id(victim))]);
    wait_until(&mut h, "tab refreshed", |a| a.v.results.len() == 1);
}

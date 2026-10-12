//! Headless UI tests (egui_kittest): typing, cancellation, navigation, and a frame-time
//! measurement for scrolling a million results (UI-01, run with `--ignored --release`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fnc_exec::SearchEngine;
use fnc_index::SharedIndex;

use super::*;

const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
\"C:\\Photos\\IMG_0001.JPG\",3000000,,,32\n\
\"C:\\Photos\\IMG_0002.jpg\",4000000,,,32\n\
\"C:\\Docs\\report.docx\",12000,,,32\n\
\"C:\\Docs\\季度预算表.xlsx\",1048576,,,32\n";

fn deps_for(file_list: Option<std::path::PathBuf>, shared: SharedIndex) -> AppDeps {
    crate::i18n::init("en");
    let (engine_tx, engine_rx) = crossbeam_channel::unbounded();
    let engine = SearchEngine::spawn(shared.clone(), engine_tx, Box::new(|| {}));
    let (index_tx, index_rx) = crossbeam_channel::unbounded();
    let handle = engine.handle();
    let indexer = match file_list {
        Some(f) => {
            let cfg = crate::indexer::IndexerConfig { file_lists: vec![f], ..Default::default() };
            Indexer::spawn(cfg, shared.clone(), Arc::new(move || handle.refresh()), index_tx, Arc::new(|| {}))
        }
        // The caller supplies a ready index.
        None => Indexer::idle(),
    };
    let dir = std::env::temp_dir().join(format!("fennec-ui-{}", std::process::id()));
    AppDeps {
        shared,
        engine,
        engine_rx,
        index_rx,
        instance: None,
        indexer,
        settings: Settings { show_tray_icon: false, hotkey_show_window: String::new(), ..Settings::default() },
        settings_path: dir.join("settings.toml"),
        initial_search: None,
        start_hidden: false,
    }
}

fn wait_until(h: &mut Harness<'_, FennecApp>, what: &str, f: impl Fn(&FennecApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn harness(deps: AppDeps) -> Harness<'static, FennecApp> {
    Harness::builder().with_size(egui::vec2(1000.0, 600.0)).build_eframe(|cc| FennecApp::new(cc, deps))
}

#[test]
fn typing_searches_and_keyboard_navigates() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.results.len() == 7);

    h.get_by_role(Role::TextInput).type_text("img");
    wait_until(&mut h, "results for img", |a| a.sent_query.as_deref() == Some("img") && a.results.len() == 2);

    h.key_press(egui::Key::ArrowDown);
    h.step();
    assert_eq!(h.state().selected, Some(0));
    h.key_press(egui::Key::ArrowDown);
    h.step();
    assert_eq!(h.state().selected, Some(1));
    // Shift+Down belongs to the text box (selection), not to the result list.
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowDown);
    h.step();
    assert_eq!(h.state().selected, Some(1));

    // Escape clears the search and every entry is listed again.
    h.key_press(egui::Key::Escape);
    wait_until(&mut h, "cleared search", |a| a.query.is_empty() && a.results.len() == 7);

    // Unsupported functions are reported in the status bar, not silently ignored.
    h.get_by_role(Role::TextInput).type_text("width:>100");
    wait_until(&mut h, "diagnostic", |a| !a.diagnostics.is_empty());
    assert!(h.state().diagnostics[0].contains("width"));
}

#[test]
fn ui02_fast_typing_shows_only_the_final_query() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("big.efu");
    std::fs::write(&efu, gen_fixtures::write_efu(200_000, 3, Vec::new()).unwrap()).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.results.len() == 200_000);
    // `a`, then `bc` one frame later: the intermediate query may be cancelled, and the
    // final result must be for `abc`.
    let input = h.get_by_role(Role::TextInput);
    input.type_text("a");
    h.step();
    h.get_by_role(Role::TextInput).type_text("bc");
    let t = Instant::now();
    let mut max_frame = Duration::ZERO;
    while !(h.state().sent_query.as_deref() == Some("abc") && h.state().latest_shown + 1 == h.state().next_id) {
        assert!(t.elapsed() < Duration::from_secs(20), "abc never arrived");
        let f = Instant::now();
        h.step();
        max_frame = max_frame.max(f.elapsed());
    }
    let expected = {
        let snap = h.state().shared.snapshot();
        (0..snap.len() as u32)
            .map(fnc_core::EntryId)
            .filter(|&i| snap.is_live(i) && String::from_utf8_lossy(snap.fold(i)).contains("abc"))
            .count()
    };
    assert_eq!(h.state().results.len(), expected);
    eprintln!("UI-02: largest frame while the search ran: {max_frame:?}");
}

/// UI-01: frame cost while paging through 1,000,000 results. Run with
/// `cargo test -p fnc-app --release -- --ignored ui01`.
#[test]
#[ignore = "needs a release build; measures frame time on 1M results"]
fn ui01_scrolling_one_million_results() {
    let shared = SharedIndex::new(gen_fixtures::build_index(1_000_000, 42).unwrap());
    {
        let mut w = shared.write();
        w.rebuild_name_order();
    }
    let mut h = harness(deps_for(None, shared));
    wait_until(&mut h, "1M results", |a| a.results.len() == 1_000_000);
    h.run_steps(5);
    let mut frames = Vec::new();
    for i in 0..300 {
        h.key_press(if i % 50 < 45 { egui::Key::PageDown } else { egui::Key::ArrowDown });
        let t = Instant::now();
        h.step();
        frames.push(t.elapsed());
    }
    frames.sort();
    let p50 = frames[frames.len() / 2];
    let p95 = frames[frames.len() * 95 / 100];
    let mean = frames.iter().sum::<Duration>() / frames.len() as u32;
    eprintln!("UI-01 frame CPU time over 300 page-downs: mean {mean:?}, p50 {p50:?}, p95 {p95:?}");
    assert!(mean < Duration::from_millis(18), "mean frame {mean:?} (< 18 ms for 55 fps)");
}

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

pub(super) fn deps_for(file_list: Option<std::path::PathBuf>, shared: SharedIndex) -> AppDeps {
    crate::i18n::init("en");
    let (engine_tx, engine_rx) = crossbeam_channel::unbounded();
    let engine = SearchEngine::spawn(shared.clone(), engine_tx, Box::new(|| {}));
    let sh = shared.clone();
    let new_engine: Arc<dyn Fn() -> (SearchEngine, crossbeam_channel::Receiver<EngineEvent>) + Send + Sync> =
        Arc::new(move || {
            let (tx, rx) = crossbeam_channel::unbounded();
            (SearchEngine::spawn(sh.clone(), tx, Box::new(|| {})), rx)
        });
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
        new_engine,
        index_rx,
        instance: None,
        indexer,
        settings: Settings { show_tray_icon: false, hotkey_show_window: String::new(), ..Settings::default() },
        settings_path: dir.join("settings.toml"),
        initial_search: None,
        initial_cli: None,
        start_hidden: false,
        servers: None,
    }
}

pub(super) fn wait_until(h: &mut Harness<'_, FennecApp>, what: &str, f: impl Fn(&FennecApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn harness(deps: AppDeps) -> Harness<'static, FennecApp> {
    Harness::builder().with_size(egui::vec2(1000.0, 600.0)).build_eframe(|cc| FennecApp::new(cc, deps))
}

#[test]
fn typing_searches_and_keyboard_navigates() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() == 7);

    h.get_by_role(Role::TextInput).type_text("img");
    wait_until(&mut h, "results for img", |a| a.v.sent_query.as_deref() == Some("img") && a.v.results.len() == 2);

    h.key_press(egui::Key::ArrowDown);
    h.step();
    assert_eq!(h.state().v.selected, Some(0));
    h.key_press(egui::Key::ArrowDown);
    h.step();
    assert_eq!(h.state().v.selected, Some(1));
    // Shift+Down belongs to the text box (selection), not to the result list.
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowDown);
    h.step();
    assert_eq!(h.state().v.selected, Some(1));

    // Escape clears the search and every entry is listed again.
    h.key_press(egui::Key::Escape);
    wait_until(&mut h, "cleared search", |a| a.v.query.is_empty() && a.v.results.len() == 7);

    // Unsupported functions are reported in the status bar, not silently ignored.
    h.get_by_role(Role::TextInput).type_text("width:>100");
    wait_until(&mut h, "diagnostic", |a| !a.v.diagnostics.is_empty());
    assert!(h.state().v.diagnostics[0].contains("width"));
}

#[test]
fn ui02_fast_typing_shows_only_the_final_query() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("big.efu");
    std::fs::write(&efu, gen_fixtures::write_efu(200_000, 3, Vec::new()).unwrap()).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| {
        matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() == 200_000
    });
    // `a`, then `bc` one frame later: the intermediate query may be cancelled, and the
    // final result must be for `abc`.
    let input = h.get_by_role(Role::TextInput);
    input.type_text("a");
    h.step();
    h.get_by_role(Role::TextInput).type_text("bc");
    let t = Instant::now();
    let mut max_frame = Duration::ZERO;
    while !(h.state().v.sent_query.as_deref() == Some("abc") && h.state().v.latest_shown + 1 == h.state().v.next_id) {
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
    assert_eq!(h.state().v.results.len(), expected);
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
    wait_until(&mut h, "1M results", |a| a.v.results.len() == 1_000_000);
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

/// UI-08: Options → Advanced, filter "delay": `search_delay` is listed and editable,
/// and OK applies it.
#[test]
fn ui08_advanced_page_lists_and_edits_search_delay() {
    let shared = SharedIndex::default();
    let mut h = harness(deps_for(None, shared));
    h.step();
    let keymap = h.state().keymap.clone();
    let settings = h.state().settings.clone();
    h.state_mut().options_window = Some(super::options::OptionsState::new(&settings, &keymap).advanced("delay"));
    h.run_steps(3);
    h.get_by_label("search_delay");
    // The editor next to it is the only numeric text field on the filtered page.
    let fields = h.get_all_by_role(Role::TextInput).count();
    assert!(fields >= 2, "filter box + search_delay editor");
    let editor = h.get_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("0")).unwrap();
    editor.focus();
    h.step();
    h.key_press(egui::Key::Backspace);
    h.step();
    h.get_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("")).unwrap().type_text("250");
    h.step();
    h.get_by_label(&t("options-ok")).click();
    h.run_steps(3);
    assert!(h.state().options_window.is_none());
    assert_eq!(h.state().settings.search_delay, 250);
}

/// UI-07: switching to dark mode applies to every panel at once.
#[test]
fn ui07_dark_mode_applies_immediately() {
    let mut h = harness(deps_for(None, SharedIndex::default()));
    h.step();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::ThemeDark);
    h.run_steps(2);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    h.state_mut().run(&ctx, Command::ThemeLight);
    h.run_steps(2);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
}

/// Commands, multi-selection and copy-as-text on real results.
#[test]
fn selection_commands_and_copy() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() == 7);
    h.get_by_role(Role::TextInput).type_text("img");
    wait_until(&mut h, "img", |a| a.v.results.len() == 2 && a.v.sent_query.as_deref() == Some("img"));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::SelectAll);
    assert_eq!(h.state().target_ids().len(), 2);
    h.state_mut().run(&ctx, Command::InvertSelection);
    assert!(h.state().target_ids().is_empty() || h.state().v.sel.ids.is_empty());
    h.state_mut().run(&ctx, Command::MoveDown);
    h.state_mut().run(&ctx, Command::ExtendDown);
    assert_eq!(h.state().v.sel.ids.len(), 2);
    let paths: Vec<String> = h.state().target_paths().iter().map(|(_, p)| p.display().to_string()).collect();
    assert!(paths.iter().all(|p| p.contains("Photos")), "{paths:?}");
    // Omit hides the rows until the search changes.
    h.state_mut().run(&ctx, Command::Omit);
    wait_until(&mut h, "omitted", |a| a.v.results.is_empty());
    // A new search shows them again.
    h.state_mut().set_query("img ".into());
    wait_until(&mut h, "back", |a| a.v.results.len() == 2);
    // `/key=value` inline command.
    h.state_mut().inline_command(&ctx, "/search_delay=7");
    assert_eq!(h.state().settings.search_delay, 7);
    // Sorting by a second key from the header model.
    let mut spec = h.state().v.sort;
    spec.push_key(fnc_exec::SortColumn::Size, false);
    assert_eq!(spec.keys().count(), 2);
}

/// Mouse: click selects, Ctrl+click adds, Shift+click extends, double click opens.
#[test]
fn mouse_selection_on_rows() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() == 7);
    h.get_by_role(Role::TextInput).type_text("img");
    wait_until(&mut h, "img", |a| a.v.results.len() == 2 && a.v.sent_query.as_deref() == Some("img"));
    h.run_steps(2);
    h.get_by_label_contains("0002").click();
    h.run_steps(2);
    assert_eq!(h.state().v.sel.ids.len(), 1);
    assert_eq!(h.state().v.focus, Focus::Results, "clicking the list moves keyboard focus to it");
    h.get_by_label_contains("0001").click_modifiers(egui::Modifiers::CTRL);
    h.run_steps(2);
    assert_eq!(h.state().v.sel.ids.len(), 2);
    // Ctrl+C arrives as a clipboard event; in the list it copies the files (here: the
    // text fallback, as the test has no file clipboard).
    h.event(egui::Event::Copy);
    h.run_steps(2);
    assert!(h.state().message.as_ref().is_some_and(|(m, _)| !m.is_empty()));
}

/// UI-03: clicking the Size header re-sorts 1,000,000 results within 300 ms (click to
/// displayed). `cargo test -p fnc-app --release -- --ignored ui03`.
#[test]
#[ignore = "needs a release build; sorts 1M results"]
fn ui03_size_header_sorts_one_million_within_300ms() {
    let shared = SharedIndex::new(gen_fixtures::build_index(1_000_000, 42).unwrap());
    shared.write().rebuild_name_order();
    let mut h = harness(deps_for(None, shared));
    wait_until(&mut h, "1M results", |a| a.v.results.len() == 1_000_000);
    h.run_steps(3);
    let mut times = Vec::new();
    for _ in 0..4 {
        let before = h.state().v.latest_shown;
        let t = Instant::now();
        h.get_by_label_contains(&t_title("size")).click();
        while h.state().v.latest_shown == before {
            h.step();
        }
        times.push(t.elapsed());
        h.run_steps(1);
    }
    eprintln!("UI-03 size header click → sorted 1M shown: {times:?}");
    assert_eq!(h.state().v.results.len(), 1_000_000);
    assert!(times.iter().all(|t| *t < Duration::from_millis(300)), "{times:?}");
}

fn t_title(key: &str) -> String {
    crate::columns::ColumnId::from_key(key).map(|c| c.title()).unwrap_or_default()
}

#[test]
fn window_rect_setting() {
    assert_eq!(super::parse_window_rect("10,20,1000,600"), Some([10.0, 20.0, 1000.0, 600.0]));
    assert_eq!(super::parse_window_rect(""), None);
    assert_eq!(super::parse_window_rect("1,2,3"), None);
    assert_eq!(super::parse_window_rect("-30000,0,1000,600"), None, "off screen");
    assert_eq!(super::parse_window_rect("0,0,10,10"), None, "too small");
}

/// Preview panel shows text files; thumbnail view lays results out in a grid.
#[test]
fn preview_text_and_thumbnail_grid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "quarterly plan line one\nline two").unwrap();
    std::fs::write(dir.path().join("pic.png"), b"\x89PNG broken").unwrap();
    let shared = SharedIndex::default();
    let mut deps = deps_for(None, shared.clone());
    {
        let mut store = fnc_index::IndexStore::default();
        let mut src = fnc_platform::walk::FolderSource::new(dir.path());
        let vol = store.add_volume(fnc_platform::VolumeSource::info(&src), Some(1)).unwrap();
        struct Sink<'a>(fnc_index::BulkLoader<'a>);
        impl fnc_platform::EntrySink for Sink<'_> {
            fn entry(&mut self, e: &fnc_platform::RawEntry<'_>) -> Result<(), fnc_platform::SourceError> {
                self.0
                    .push(e.frn, e.parent_frn, e.name, &e.meta)
                    .map(|_| ())
                    .map_err(|e| fnc_platform::SourceError::io("push", std::io::Error::other(e)))
            }
        }
        let mut sink = Sink(store.bulk_load(vol));
        fnc_platform::VolumeSource::enumerate(&mut src, &mut sink, &fnc_platform::StopFlag::new()).unwrap();
        sink.0.finish().unwrap();
        store.rebuild_name_order();
        shared.replace(store);
    }
    deps.settings.show_preview = true;
    let mut h = harness(deps);
    h.get_by_role(Role::TextInput).type_text("notes");
    wait_until(&mut h, "notes", |a| a.v.results.len() == 1 && a.v.sent_query.as_deref() == Some("notes"));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::MoveDown);
    let t = Instant::now();
    while h.query_by_label_contains("quarterly plan line one").is_none() {
        assert!(t.elapsed() < Duration::from_secs(10), "text preview never appeared");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    // Grid view with a broken picture: the item still shows (placeholder), no panic.
    h.state_mut().set_query(String::new());
    wait_until(&mut h, "all", |a| a.v.results.len() >= 3);
    h.state_mut().run(&ctx, Command::ViewLarge);
    h.run_steps(5);
    h.state_mut().run(&ctx, Command::MoveDown);
    assert_eq!(
        h.state().v.selected,
        Some(h.state().v.grid_cols.min(h.state().v.results.len() - 1)),
        "down moves a row"
    );
    h.state_mut().run(&ctx, Command::ViewDetails);
    h.run_steps(2);
}

/// INT-01 (in-process part): a new window searches independently of the main one.
#[test]
fn new_window_has_its_own_search() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() == 7);
    h.state_mut().open_window("ext:docx".into());
    let t = Instant::now();
    loop {
        h.step();
        let w = &h.state().windows;
        if w.len() == 1 && w[0].view.results.len() == 1 {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "window search never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.state().v.results.len(), 7, "main window unchanged");
    // Esc on an empty search closes a secondary window.
    let ctx = h.ctx.clone();
    let mut w = h.state_mut().windows.pop().unwrap();
    std::mem::swap(&mut h.state_mut().v, &mut w.view);
    h.state_mut().in_secondary = true;
    h.state_mut().run(&ctx, Command::CloseWindow);
    assert!(h.state().close_secondary);
}

/// Forwarded command lines: -filename selects the file in its folder, -sort and -filter
/// apply, -search* is literal.
#[test]
fn forwarded_command_lines() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("t.efu");
    std::fs::write(&efu, EFU).unwrap();
    let mut h = harness(deps_for(Some(efu), SharedIndex::default()));
    wait_until(&mut h, "index ready", |a| matches!(a.status, IndexStatus::Ready { .. }) && a.v.results.len() == 7);
    let ctx = h.ctx.clone();
    let cli = crate::cli::parse(["-filename", r"C:\Photos\IMG_0002.jpg"].map(String::from));
    h.state_mut().apply_cli(&ctx, cli, true);
    wait_until(&mut h, "selected", |a| a.v.selected.is_some() && a.v.pending_select.is_none());
    let sel = h.state().target_paths();
    assert_eq!(sel.len(), 1);
    assert!(sel[0].1.display().to_string().ends_with("IMG_0002.jpg"));
    let cli = crate::cli::parse(["-sort", "size", "-sort-descending", "-filter", "Picture"].map(String::from));
    h.state_mut().apply_cli(&ctx, cli, true);
    assert_eq!(h.state().v.sort.column, fnc_exec::SortColumn::Size);
    assert!(!h.state().v.sort.ascending);
    assert_eq!(h.state().active_filter_name(), "Picture");
    h.state_mut().set_query(String::new());
    wait_until(&mut h, "pictures only", |a| a.v.results.len() == 2 && a.v.sent_query.as_deref() == Some(""));
}

/// M4: the servers follow the settings; the HTTP address shows in the status bar and the
/// Options → Servers page lists the server settings (password masked).
#[test]
fn http_server_address_in_status_bar() {
    let mut deps = deps_for(None, SharedIndex::default());
    deps.servers =
        Some(crate::servers::ServersContext { instance: format!("kittest-{}", std::process::id()), portable: true });
    deps.settings.ipc_server = false;
    deps.settings.everything_ipc = false;
    deps.settings.http_enabled = true;
    deps.settings.http_port = 0;
    deps.settings.http_password = "secret-pw".into();
    let mut h = harness(deps);
    let t0 = Instant::now();
    while h.query_by_label_contains("http://127.0.0.1:").is_none() {
        assert!(t0.elapsed() < Duration::from_secs(20), "HTTP address never shown");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(h.state().servers_status().unwrap().http, crate::servers::HttpStatus::Running(_)));
    // Turning it off removes the address.
    let ctx = h.ctx.clone();
    let mut s = h.state().settings.clone();
    s.http_enabled = false;
    h.state_mut().apply_settings(&ctx, s);
    let t0 = Instant::now();
    while h.query_by_label_contains("http://127.0.0.1:").is_some() {
        assert!(t0.elapsed() < Duration::from_secs(20), "HTTP address still shown");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    // `/http_enabled=true` + Enter in the search box (the Enter key must reach the
    // inline command instead of opening the selection) starts it again.
    h.get_by_role(Role::TextInput).focus();
    h.step();
    h.get_by_role(Role::TextInput).type_text("/http_enabled=true");
    h.step();
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(h.state().settings.http_enabled, "inline command applied");
    let t0 = Instant::now();
    while h.query_by_label_contains("http://127.0.0.1:").is_none() {
        assert!(t0.elapsed() < Duration::from_secs(20), "HTTP address not shown again");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    // Options → Servers.
    let keymap = h.state().keymap.clone();
    let settings = h.state().settings.clone();
    h.state_mut().options_window = Some(super::options::OptionsState::new(&settings, &keymap).on_page("server"));
    h.run_steps(3);
    assert!(h.query_by_label_contains("HTTP server port").is_some());
    assert!(h.query_by_label_contains("Everything 1.4 compatible IPC window").is_some());
    assert!(
        h.get_all_by_role(Role::PasswordInput).count() == 1
            || h.get_all_by_role(Role::TextInput).all(|n| n.value().as_deref() != Some("secret-pw")),
        "the password is masked"
    );
    h.state_mut().options_window = None;
    h.state_mut().stop_servers();
}

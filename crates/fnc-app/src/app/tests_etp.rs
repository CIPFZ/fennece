//! ETP client in the window (SPEC ch.4 §5, ADR-036): `-connect` switches the view to a
//! server's results, searches and sorting go to the server, file-changing commands are
//! refused, downloads open the local copy, and disconnecting returns to the local index.

use std::sync::Arc;
use std::time::Duration;

use egui_kittest::kittest::Queryable;
use fnc_index::SharedIndex;

use super::tests::{deps_for, harness, wait_until};
use super::*;

const LOCAL: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
\"C:\\Local\\only-here.txt\",1,,,32\n";

fn remote_server() -> (tempfile::TempDir, fnc_etp::EtpHandle) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("remote report.txt"), b"remote bytes").unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let mut s = fnc_index::IndexStore::default();
    let vol = s.add_volume(fnc_index::VolumeInfo::new(fnc_index::VolumeKind::Folder, root), Some(1)).unwrap();
    let mut b = s.bulk_load(vol);
    let meta = |size| fnc_index::Meta { attributes: Some(32), size: Some(size), ..Default::default() };
    b.push(2, 1, &"remote report.txt".encode_utf16().collect::<Vec<_>>(), &meta(12)).unwrap();
    for i in 0..30u64 {
        b.push(10 + i, 1, &format!("server{i:02}.dat").encode_utf16().collect::<Vec<_>>(), &meta(i)).unwrap();
    }
    b.finish().unwrap();
    s.rebuild_name_order();
    let backend = Arc::new(fnc_ipc::IndexBackend::new(SharedIndex::new(s), Arc::new(fnc_ipc::DefaultHost)));
    let cfg = fnc_etp::EtpConfig { port: 0, username: "u".into(), password: "pw".into(), ..Default::default() };
    (dir, fnc_etp::start(cfg, backend).unwrap())
}

#[test]
fn connect_search_refuse_download_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let efu = dir.path().join("local.efu");
    std::fs::write(&efu, LOCAL).unwrap();
    let (_remote_dir, server) = remote_server();
    let port = server.local_addr().port();
    let mut deps = deps_for(Some(efu), SharedIndex::default());
    deps.settings_path = dir.path().join("data").join("settings.toml");
    deps.initial_cli = Some(crate::cli::parse(["-connect".to_string(), format!("u:pw@127.0.0.1:{port}")]));
    let mut h = harness(deps);
    // The view shows the server's index (31 entries + its root), not the local one.
    wait_until(&mut h, "remote results", |a| a.is_remote() && a.v.results.len() == 32);
    h.run_steps(3);
    assert!(h.query_by_label_contains("server07.dat").is_some());
    assert!(h.query_by_label_contains("only-here.txt").is_none());
    assert!(h.query_by_label_contains(&format!("ETP: 127.0.0.1:{port}")).is_some(), "status bar names the server");
    // The last server is remembered (not the password).
    assert_eq!((h.state().settings.etp_client_host.as_str(), h.state().settings.etp_client_port), ("127.0.0.1", port));
    let saved = std::fs::read_to_string(dir.path().join("data").join("settings.toml")).unwrap();
    assert!(!saved.contains("pw\""), "{saved}");
    // Searches and sorting run on the server.
    h.state_mut().v.query = "server1".into();
    h.state_mut().send_search();
    wait_until(&mut h, "remote search", |a| a.v.sent_query.as_deref() == Some("server1") && a.v.results.len() == 10);
    h.state_mut().v.sort = SortSpec::by(SortColumn::Size, false, fnc_core::settings::FoldersFirst::Ascending);
    h.state_mut().send_search();
    wait_until(&mut h, "sorted by size", |a| {
        a.view_index()
            .try_snapshot_for(Duration::from_millis(50))
            .zip(a.v.results.ids.first())
            .is_some_and(|(s, &id)| s.name_string(id) == "server19.dat")
    });
    // Read-only: deleting is refused with a message.
    let ctx = h.ctx.clone();
    h.state_mut().select_only(0);
    h.state_mut().run(&ctx, Command::Delete);
    assert_eq!(h.state().message.as_ref().map(|m| m.0.as_str()), Some(t("msg-etp-not-available").as_str()));
    // Opening downloads through the server (link type ftp) into the temporary folder.
    h.state_mut().v.query = "remote report".into();
    h.state_mut().send_search();
    wait_until(&mut h, "report", |a| a.v.sent_query.as_deref() == Some("remote report") && a.v.results.len() == 1);
    h.state_mut().select_only(0);
    let target = h.state().v.remote.as_ref().unwrap().session.target().clone();
    let path = h.state().target_paths()[0].1.display().to_string();
    let local = crate::remote::local_copy_path(&crate::remote::download_dir(&target), &path).unwrap();
    let _ = std::fs::remove_file(&local);
    h.state_mut().v.remote.as_ref().unwrap().session.download(path, true);
    let t0 = std::time::Instant::now();
    while std::fs::read(&local).ok().as_deref() != Some(&b"remote bytes"[..]) {
        assert!(t0.elapsed() < Duration::from_secs(20), "download never arrived");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(&local);
    // The connect dialog offers the last server.
    h.state_mut().run(&ctx, Command::ConnectEtp);
    h.run_steps(2);
    assert!(h.query_by_label(&t("etp-connect")).is_some());
    assert!(h.query_by_label_contains(&t("etp-plain-text-warning")).is_some());
    h.state_mut().dialog = None;
    // Back to the local index.
    h.state_mut().run(&ctx, Command::DisconnectEtp);
    h.state_mut().v.query = String::new();
    h.state_mut().send_search();
    wait_until(&mut h, "local results", |a| !a.is_remote() && a.v.results.len() == 3);
    // A server that refuses the login ends the remote mode with a message.
    let bad = crate::remote::RemoteTarget {
        target: fnc_etp::Target { password: "wrong".into(), ..target.target },
        link: target.link,
    };
    h.state_mut().connect_etp(bad);
    wait_until(&mut h, "login refused", |a| !a.is_remote());
    assert!(h.state().message.as_ref().is_some_and(|m| m.0.contains("530")), "{:?}", h.state().message);
    drop(server);
}

//! `fes` against a real IPC server (in-process `fnc_ipc::spawn` on a test instance).

// Test-only helpers outside `#[test]` functions.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use fnc_fes::{EXIT_NO_IPC, EXIT_OK, Env};
use fnc_index::{IndexStore, SharedIndex};
use fnc_ipc::backend::{AdminCommand, Host, HostError, IndexBackend};
use fnc_ipc::{ServerConfig, ServerHandle};

struct SlowHost {
    loaded: std::sync::atomic::AtomicBool,
}

impl Host for SlowHost {
    fn is_db_loaded(&self) -> bool {
        self.loaded.swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    fn command(&self, cmd: AdminCommand) -> Result<(), HostError> {
        if cmd == AdminCommand::SaveDb { Ok(()) } else { Err(HostError::Unsupported) }
    }
}

fn server(tag: &str) -> (String, ServerHandle) {
    let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
    for (i, size) in [5u64, 50, 500, 5000, 50000, 7, 70].iter().enumerate() {
        efu.push_str(&format!("\"D:\\Images\\disk{i}.iso\",{size},133000000000000000,,32\n"));
    }
    efu.push_str("\"D:\\Images\\notes.txt\",1,,,32\n");
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
    s.rebuild_name_order();
    let host = Arc::new(SlowHost { loaded: false.into() });
    let backend = Arc::new(IndexBackend::new(SharedIndex::new(s), host));
    let inst = format!("fes-{tag}-{}", std::process::id());
    let h = fnc_ipc::spawn(&inst, backend, ServerConfig::default()).unwrap();
    (inst, h)
}

fn fes(inst: &str, args: &[&str]) -> (u8, String, String) {
    let mut all = vec!["-instance".to_string(), inst.to_string()];
    all.extend(args.iter().map(|s| s.to_string()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = fnc_fes::run(all, &Env::default(), &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

#[test]
fn int02_over_ipc() {
    let (inst, _server) = server("int02");
    let (code, out, err) = fes(&inst, &["-n", "5", "-sort", "size-descending", "*.iso", "-csv"]);
    assert_eq!(code, EXIT_OK, "{err}");
    let lines: Vec<&str> = out.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 6, "header + 5 rows: {out}");
    assert_eq!(lines[0], "Filename");
    assert_eq!(lines[1], "\"D:\\Images\\disk4.iso\"");
    assert_eq!(lines[4], "\"D:\\Images\\disk6.iso\"");
    assert_eq!(lines[5], "\"D:\\Images\\disk1.iso\"");
}

#[test]
fn options_over_ipc() {
    let (inst, _server) = server("opts");
    // -timeout waits for the database (the host reports "not loaded" once).
    let (code, out, _) = fes(&inst, &["-timeout", "2000", "-get-result-count", "iso"]);
    assert_eq!((code, out.as_str()), (EXIT_OK, "7\r\n"));
    assert_eq!(fes(&inst, &["-get-total-size", "*.iso"]).1, "55632\r\n");
    assert!(fes(&inst, &["-get-everything-version"]).1.starts_with("1.4.1."));
    let (_, out, _) = fes(&inst, &["-sort", "size", "-sort-ascending", "-n", "2", "-size", "-name", "iso"]);
    assert_eq!(out, "             5 disk0.iso\r\n             7 disk5.iso\r\n");
    let (_, out, _) = fes(&inst, &["-o", "6", "*.iso"]);
    assert_eq!(out.lines().count(), 1);
    let (_, out, _) = fes(&inst, &["-json", "-n", "1", "-sort", "name", "-name", "-path-column", "notes"]);
    assert_eq!(out, "[{\"name\":\"notes.txt\",\"path\":\"D:\\\\Images\"}]\r\n");
    let (code, _, _) = fes(&inst, &["-save-db"]);
    assert_eq!(code, EXIT_OK);
    assert_eq!(fes(&inst, &["-inc-run-count", "D:\\Images\\notes.txt"]).0, EXIT_OK);
    assert_eq!(fes(&inst, &["-get-run-count", "D:\\Images\\notes.txt"]).1, "1\r\n");
    let (_, out, _) = fes(&inst, &["-sort", "run-count", "-n", "1", "-run-count", "-name"]);
    assert_eq!(out, "      1 notes.txt\r\n");
    let (_, out, _) = fes(&inst, &["-highlight", "-name", "notes"]);
    assert_eq!(out, "notes.txt\r\n", "es does not highlight redirected output");
    // es 1.1.0.38 style `-n` (`count:` filter) works too.
    let (_, out, _) = fes(&inst, &["-sort", "size", "-name", "<", "count:2", ">", "<", "iso", ">"]);
    assert_eq!(out, "disk4.iso\r\ndisk3.iso\r\n");
}

#[test]
fn timeout_when_no_instance() {
    let inst = format!("fes-none-{}", std::process::id());
    let t = std::time::Instant::now();
    let (code, _, err) = fes(&inst, &["-timeout", "200", "abc"]);
    assert_eq!(code, EXIT_NO_IPC, "{err}");
    assert!(t.elapsed() >= Duration::from_millis(200));
}

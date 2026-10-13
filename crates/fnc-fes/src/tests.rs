//! Unit tests of the fes runner against an in-process backend.

use super::*;

const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
    \"C:\\ISO\",,133000000000000000,,16\n\
    \"C:\\ISO\\a.iso\",100,133000000000000001,,32\n\
    \"C:\\ISO\\b.iso\",700,133000000000000002,,32\n\
    \"C:\\ISO\\c.iso\",300,133000000000000003,,32\n\
    \"C:\\ISO\\d.iso\",900,133000000000000004,,32\n\
    \"C:\\ISO\\e.iso\",500,133000000000000005,,32\n\
    \"C:\\ISO\\f.iso\",200,133000000000000006,,33\n\
    \"C:\\Docs\\Report, \"\"final\"\".txt\",42,133000000000000007,132000000000000000,32\n";

fn local() -> Conn {
    let mut store = IndexStore::default();
    fnc_index::efu::load_efu(&mut store, EFU.as_bytes(), "t.efu").unwrap();
    store.rebuild_name_order();
    Conn::Local(Box::new(IndexBackend::new(SharedIndex::new(store), Arc::new(DefaultHost))))
}

fn env() -> Env {
    Env { stdout_newline: None, ..Env::default() }
}

fn fes(args: &[&str]) -> (u8, String) {
    let a = parse_args(args.iter().map(|s| s.to_string())).unwrap();
    let mut out = Vec::new();
    let code = match run_args(&a, &env(), &mut out, Some(&mut local())) {
        Ok(c) => c,
        Err(f) => f.code,
    };
    (code, String::from_utf8(out).unwrap())
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn int02_csv_top5_by_size() {
    let (code, out) = fes(&["-n", "5", "-sort", "size-descending", "*.iso", "-csv"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "Filename\r\n\"C:\\ISO\\d.iso\"\r\n\"C:\\ISO\\b.iso\"\r\n\"C:\\ISO\\e.iso\"\r\n\"C:\\ISO\\c.iso\"\r\n\"C:\\ISO\\f.iso\"\r\n"
    );
}

#[test]
fn plain_and_columns() {
    let (_, out) = fes(&["iso", "-sort", "name"]);
    assert_eq!(out.lines().next(), Some("C:\\ISO"), "folders first, no trailing separator");
    assert_eq!(out.lines().count(), 7);
    let (_, out) = fes(&["-size", "-attributes", "-n", "2", "-o", "1", "-sort", "size", "*.iso"]);
    assert_eq!(out, "           700 A    C:\\ISO\\b.iso\r\n           500 A    C:\\ISO\\e.iso\r\n");
    let (_, out) = fes(&["-name", "-ext", "-no-digit-grouping", "-size", "report"]);
    assert_eq!(out, "Report, \"final\".txt            txt  42\r\n", "the last column is never padded");
    let (_, out) = fes(&["-double-quote", "report"]);
    assert_eq!(out, "\"C:\\Docs\\Report, \"final\".txt\"\r\n");
    let (_, out) = fes(&["-size-format", "2", "-size", "-sort", "size", "-n", "1", "iso"]);
    assert_eq!(out, "          1 KB C:\\ISO\\d.iso\r\n");
}

#[test]
fn every_format() {
    let (_, efu) = fes(&["-efu", "f.iso"]);
    assert_eq!(
        efu,
        "Filename,Size,Date Modified,Date Created,Attributes\r\n\"C:\\ISO\\f.iso\",200,133000000000000006,,33\r\n"
    );
    let (_, tsv) = fes(&["-tsv", "-name", "-size", "report"]);
    assert_eq!(tsv, "Name\tSize\r\n\"Report, \"\"final\"\".txt\"\t42\r\n");
    let (_, txt) = fes(&["-txt", "-size", "-sort", "name", "iso"]);
    assert!(txt.starts_with("C:\\ISO\\\r\nC:\\ISO\\a.iso\r\n"), "{txt}");
    let (_, m3u) = fes(&["-m3u", "a.iso"]);
    assert_eq!(m3u, "C:\\ISO\\a.iso\r\n");
    let (_, m3u8) = fes(&["-m3u8", "a.iso"]);
    assert_eq!(m3u8, "C:\\ISO\\a.iso\r\n");
    let (_, json) = fes(&["-json", "-size", "-dm", "-sort", "size", "-n", "2", "*.iso"]);
    assert_eq!(
        json,
        "[{\"filename\":\"C:\\\\ISO\\\\d.iso\",\"size\":900,\"date_modified\":133000000000000004},\r\n\
         {\"filename\":\"C:\\\\ISO\\\\b.iso\",\"size\":700,\"date_modified\":133000000000000002}]\r\n"
    );
    let (_, csv) = fes(&["-csv", "-no-header", "-dm", "-date-format", "2", "a.iso"]);
    assert_eq!(csv, "\"C:\\ISO\\a.iso\",133000000000000001\r\n");
    let (_, csv) = fes(&["-csv", "-dc", "-date-format", "3", "report"]);
    assert!(csv.ends_with(",2019-04-17T18:40:00Z\r\n"), "{csv}");
}

#[test]
fn counts_sizes_versions_errors() {
    assert_eq!(fes(&["-get-result-count", "*.iso"]), (0, "6\r\n".into()));
    assert_eq!(fes(&["-get-total-size", "*.iso"]), (0, "2700\r\n".into()));
    assert_eq!(fes(&["-get-folder-size", "C:\\ISO"]), (0, "2700\r\n".into()));
    let (code, v) = fes(&["-get-everything-version"]);
    assert_eq!(code, 0);
    assert!(v.starts_with("1.4.1."), "{v}");
    assert_eq!(fes(&["-no-result-error", "nothing-matches-this"]).0, EXIT_NO_RESULTS);
    assert_eq!(fes(&["-no-result-error", "-get-result-count", "zzz"]), (EXIT_NO_RESULTS, "0\r\n".into()));
    assert_eq!(fes(&["-reindex"]).0, EXIT_IPC_ERROR, "default host does not rebuild");
    assert_eq!(fes(&["-get-run-count", "C:\\ISO\\a.iso"]), (0, "0\r\n".into()));
    assert_eq!(fes(&["-inc-run-count", "C:\\ISO\\a.iso"]).0, 0);
    assert!(fes(&["-list-properties"]).1.contains("Date Modified"));
    assert!(fes(&["-help"]).1.contains("Exit codes"));
}

#[test]
fn exit_codes_without_server() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let env = Env::default();
    let inst = format!("nobody-{}", std::process::id());
    let code = run(strings(&["-instance", &inst, "x"]), &env, &mut out, &mut err);
    assert_eq!(code, EXIT_NO_IPC);
    assert!(String::from_utf8_lossy(&err).contains("no Fennec instance"));
    assert_eq!(run(strings(&["-bogus"]), &env, &mut out, &mut err), EXIT_ARGS);
    assert_eq!(run(strings(&["-n"]), &env, &mut out, &mut err), EXIT_ARGS);
}

#[test]
fn export_file_and_settings() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.efu");
    let (code, out) = fes(&["-export-efu", file.to_str().unwrap(), "*.iso"]);
    assert_eq!((code, out.as_str()), (0, ""));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("Filename,Size,Date Modified,Date Created,Attributes\r\n"));
    assert_eq!(text.lines().count(), 7);
    let bad = dir.path().join("missing").join("x.csv");
    assert_eq!(fes(&["-export-csv", bad.to_str().unwrap(), "x"]).0, EXIT_CREATE_FILE);

    // -save-settings stores the options; later runs pick them up; -clear-settings drops them.
    let lists = dir.path().join("t.efu");
    std::fs::write(&lists, EFU).unwrap();
    let env = Env { settings_file: Some(dir.path().join("fes.args")), ..env() };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let l = lists.to_str().unwrap().to_string();
    assert_eq!(run(strings(&["-filelist", &l, "-size", "-save-settings", "a.iso"]), &env, &mut out, &mut err), 0);
    out.clear();
    assert_eq!(run(strings(&["-filelist", &l, "a.iso"]), &env, &mut out, &mut err), 0);
    assert_eq!(String::from_utf8_lossy(&out), "           100 C:\\ISO\\a.iso\r\n");
    out.clear();
    assert_eq!(run(strings(&["-clear-settings", "-filelist", &l, "a.iso"]), &env, &mut out, &mut err), 0);
    assert_eq!(String::from_utf8_lossy(&out), "C:\\ISO\\a.iso\r\n");
}

#[test]
fn offline_index_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::default();
    fnc_index::efu::load_efu(&mut store, EFU.as_bytes(), "t.efu").unwrap();
    store.rebuild_name_order();
    fnc_index::persist::save(&store, &dir.path().join("index.fnx")).unwrap();
    let env = Env { data_root: Some(dir.path().to_path_buf()), ..env() };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(strings(&["-offline", "-get-result-count", "*.iso"]), &env, &mut out, &mut err);
    assert_eq!((code, String::from_utf8_lossy(&out).as_ref()), (0, "6\r\n"));
    let env = Env { data_root: Some(dir.path().join("none")), ..Env::default() };
    assert_eq!(run(strings(&["-offline", "x"]), &env, &mut out, &mut err), EXIT_NO_IPC);
}

#[test]
fn highlight_output() {
    let a = parse_args(strings(&["-highlight", "-name", "report"])).unwrap();
    let mut out = Vec::new();
    run_args(&a, &env(), &mut out, Some(&mut local())).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "*Report*, \"final\".txt\r\n", "markup when not a terminal");
    let mut out = Vec::new();
    let tty = Env { stdout_is_terminal: true, ..env() };
    run_args(&a, &tty, &mut out, Some(&mut local())).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "\x1b[92mReport\x1b[0m, \"final\".txt\r\n");
}

//! `fes` — command-line search (SPEC ch.4 §2).
//!
//! Querying a running instance over IPC arrives in M4. Until then `fes` works in
//! offline mode on EFU file lists: `fes -filelist backup.efu -n 10 -sort size *.iso`.
//!
//! Exit codes: 0 success, 1 IPC unavailable (no `-filelist` given), 2 bad arguments.

#![forbid(unsafe_code)]

use std::io::Write;
use std::process::ExitCode;

use fnc_core::EntryId;
use fnc_exec::{Caches, CancelToken, ExecCtx, SortColumn, SortSpec, execute, sort_ids};
use fnc_index::IndexStore;
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

#[derive(Debug, Default, PartialEq)]
struct Args {
    file_lists: Vec<String>,
    max: Option<usize>,
    offset: usize,
    sort: Option<SortSpec>,
    csv: bool,
    count_only: bool,
    search: String,
}

fn parse_args(raw: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut words = Vec::new();
    let mut it = raw.into_iter();
    while let Some(arg) = it.next() {
        let mut need = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.to_ascii_lowercase().as_str() {
            "-filelist" | "-efu" => a.file_lists.push(need("-filelist")?),
            "-n" | "-max-results" => a.max = Some(need("-n")?.parse().map_err(|_| "bad -n value")?),
            "-o" | "-offset" => a.offset = need("-o")?.parse().map_err(|_| "bad -o value")?,
            "-sort" => {
                let v = need("-sort")?.to_ascii_lowercase();
                let (col, asc) = match v.strip_suffix("-descending") {
                    Some(c) => (c.to_string(), false),
                    None => (v.strip_suffix("-ascending").unwrap_or(&v).to_string(), true),
                };
                let column = SortColumn::parse(&col).ok_or_else(|| format!("unknown sort column {col}"))?;
                a.sort = Some(SortSpec { column, ascending: asc, ..SortSpec::default() });
            }
            "-s" => a.sort = Some(SortSpec { column: SortColumn::Path, ..SortSpec::default() }),
            "-csv" => a.csv = true,
            "-get-result-count" => a.count_only = true,
            "-instance" | "-timeout" => {
                need(&arg)?;
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {arg}")),
            _ => words.push(arg),
        }
    }
    a.search = words.join(" ");
    Ok(a)
}

fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn run(args: Args, out: &mut dyn Write) -> Result<(), (u8, String)> {
    if args.file_lists.is_empty() {
        return Err((1, "no running instance reachable (IPC arrives in M4); use -filelist <file.efu>".into()));
    }
    let mut store = IndexStore::default();
    for f in &args.file_lists {
        fnc_index::efu::load_efu_file(&mut store, std::path::Path::new(f)).map_err(|e| (2, e.to_string()))?;
    }
    store.rebuild_name_order();
    let p = plan(&parse(&args.search, &ParseEnv::default()).ast, &PlanEnv::default(), None);
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let mut rs = execute(&p, &store, &ExecCtx { cancel: &cancel, prev: None, caches: &caches })
        .map_err(|e| (3, e.to_string()))?;
    let spec = args.sort.unwrap_or_default();
    sort_ids(&mut rs.ids, &spec, &store, &caches, &cancel).map_err(|e| (3, e.to_string()))?;
    let io = |e: std::io::Error| (2, e.to_string());
    if args.count_only {
        writeln!(out, "{}", rs.len()).map_err(io)?;
        return Ok(());
    }
    let take = args.max.unwrap_or(usize::MAX);
    let mut path = String::new();
    if args.csv {
        writeln!(out, "Filename").map_err(io)?;
    }
    for &id in rs.ids.iter().skip(args.offset).take(take) {
        let id: EntryId = id;
        store.full_path(id, &mut path);
        if args.csv { writeln!(out, "{}", csv_field(&path)) } else { writeln!(out, "{path}") }.map_err(io)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("fes: {e}");
            return ExitCode::from(2);
        }
    };
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    match run(args, &mut lock) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("fes: {msg}");
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args() {
        let a = parse_args(["-n", "5", "-sort", "size-descending", "*.iso", "-csv"].map(String::from)).unwrap();
        assert_eq!(a.max, Some(5));
        assert!(a.csv);
        assert_eq!(a.search, "*.iso");
        assert_eq!(a.sort.map(|s| (s.column, s.ascending)), Some((SortColumn::Size, false)));
        assert!(parse_args(["-bogus".to_string()]).is_err());
    }

    #[test]
    fn offline_query_on_file_list() {
        let dir = tempfile::tempdir().unwrap();
        let efu = dir.path().join("t.efu");
        std::fs::write(
            &efu,
            "Filename,Size,Date Modified,Date Created,Attributes\n\"D:\\a.iso\",10,,,32\n\"D:\\b.iso\",30,,,32\n\"D:\\c.txt\",5,,,32\n",
        )
        .unwrap();
        let mut out = Vec::new();
        let a = parse_args(
            ["-filelist", efu.to_str().unwrap(), "-n", "1", "-sort", "size-descending", "*.iso"].map(String::from),
        )
        .unwrap();
        run(a, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "D:\\b.iso\n");
        let a = parse_args(["*.iso".to_string()]).unwrap();
        assert_eq!(run(a, &mut Vec::new()).unwrap_err().0, 1);
    }
}

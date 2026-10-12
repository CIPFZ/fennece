//! `gen-fixtures [--count N] [--seed S] [--out FILE]`
//!
//! Writes a synthetic EFU file list (default: 1,000,000 entries to
//! `fixtures/fixture-1m.efu`).

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};

fn main() -> anyhow::Result<()> {
    let mut count = 1_000_000usize;
    let mut seed = 42u64;
    let mut out = PathBuf::from("fixtures/fixture-1m.efu");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--count" | "-n" => count = args.next().context("--count needs a value")?.replace('_', "").parse()?,
            "--seed" => seed = args.next().context("--seed needs a value")?.parse()?,
            "--out" | "-o" => out = args.next().context("--out needs a value")?.into(),
            "--help" | "-h" => {
                eprintln!("usage: gen-fixtures [--count N] [--seed S] [--out FILE]");
                return Ok(());
            }
            other => bail!("unknown argument {other}"),
        }
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let t0 = std::time::Instant::now();
    let file = std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
    let mut w = gen_fixtures::write_efu(count, seed, std::io::BufWriter::with_capacity(1 << 20, file))?;
    w.flush()?;
    eprintln!("wrote {count} entries to {} in {:.2?}", out.display(), t0.elapsed());
    Ok(())
}

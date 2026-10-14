//! `soak-churn` — REL-05 soak tooling (see `tools/soak/README.md`).
//!
//! ```text
//! soak-churn [run] --root D:\soak [--rate 50] [--duration 24h] [--seed N]
//!            [--files 20000] [--dirs 100] [--big 10000]
//!            [--subtree 15000] [--subtree-every 15m] [--stop-file PATH] [--log FILE]
//! soak-churn prepare --root D:\soak [same sizes]    (prefill only, then exit)
//! soak-churn report <samples.csv> [--out FILE.md] [--svg-dir DIR] [--min-hours 24]
//! ```
//!
//! `run` makes a steady `--rate` changes/s (token bucket; creates, renames and deletes
//! balanced so the tree size stays constant), keeps a folder with `--big` children and
//! deletes + recreates a `--subtree`-entry subtree every `--subtree-every`. It stops
//! at `--duration`, when the stop file (default `<root>\soak-churn.stop`) appears, or
//! on Ctrl+C (all state is re-derived from disk at start, so a kill is harmless).

mod churn;
mod plan;
mod report;

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use churn::{Counters, Tree};
use plan::{Every, Rng, SubtreeShape, TokenBucket, hms, parse_duration};

const USAGE: &str = "usage:
  soak-churn [run] --root DIR [--rate 50] [--duration 24h] [--seed N] [--files 20000]
             [--dirs 100] [--big 10000] [--subtree 15000] [--subtree-every 15m]
             [--stop-file PATH] [--log FILE]
  soak-churn prepare --root DIR [size options]
  soak-churn report SAMPLES.csv [--out FILE.md] [--svg-dir DIR] [--min-hours 24] [--warmup 0m]
  soak-churn seconds DURATION";

#[derive(Debug, Clone)]
struct Config {
    root: PathBuf,
    rate: f64,
    duration: Duration,
    seed: u64,
    files: usize,
    dirs: usize,
    big: usize,
    subtree: usize,
    subtree_every: Duration,
    stop_file: Option<PathBuf>,
    log: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            rate: 50.0,
            duration: Duration::from_secs(86_400),
            seed: 0x50AC,
            files: 20_000,
            dirs: 100,
            big: 10_000,
            subtree: 15_000,
            subtree_every: Duration::from_secs(15 * 60),
            stop_file: None,
            log: None,
        }
    }
}

fn parse_config(args: &[String]) -> anyhow::Result<Config> {
    let mut c = Config::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().with_context(|| format!("{a} needs a value"));
        let num = |s: &String| s.replace('_', "");
        match a.as_str() {
            "--root" => c.root = val()?.into(),
            "--rate" => c.rate = val()?.parse().context("--rate")?,
            "--duration" => c.duration = parse_duration(val()?).map_err(anyhow::Error::msg)?,
            "--seed" => c.seed = val()?.parse().context("--seed")?,
            "--files" => c.files = num(val()?).parse().context("--files")?,
            "--dirs" => c.dirs = val()?.parse().context("--dirs")?,
            "--big" => c.big = num(val()?).parse().context("--big")?,
            "--subtree" => c.subtree = num(val()?).parse().context("--subtree")?,
            "--subtree-every" => c.subtree_every = parse_duration(val()?).map_err(anyhow::Error::msg)?,
            "--stop-file" => c.stop_file = Some(val()?.into()),
            "--log" => c.log = Some(val()?.into()),
            other => bail!("unknown argument {other}\n{USAGE}"),
        }
    }
    if c.root.as_os_str().is_empty() {
        bail!("--root is required\n{USAGE}");
    }
    if !c.rate.is_finite() || c.rate <= 0.0 {
        bail!("--rate must be positive");
    }
    Ok(c)
}

/// Log lines go to stdout and, with `--log`, are appended to a file.
struct Logger {
    file: Option<File>,
}

impl Logger {
    fn line(&mut self, msg: &str) {
        let line = format!("[{}] {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("report") => report::main(&args[1..]),
        Some("seconds") => {
            // `seconds <duration>`: prints the duration in seconds (for soak.ps1).
            let d = parse_duration(args.get(1).map_or("", String::as_str)).map_err(anyhow::Error::msg)?;
            writeln!(std::io::stdout(), "{}", d.as_secs())?;
            Ok(())
        }
        Some("prepare") => churn_main(&parse_config(&args[1..])?, true),
        Some("run") => churn_main(&parse_config(&args[1..])?, false),
        Some("--help" | "-h" | "help") | None => {
            eprintln!("{USAGE}");
            Ok(())
        }
        Some(_) => churn_main(&parse_config(&args)?, false),
    }
}

fn status(c: &Counters, tree: &Tree, elapsed: Duration, last_ops: u64, per: Duration) -> String {
    let ops = c.steady_ops();
    let ld = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    format!(
        "t={} ops={ops} rate={:.1}/s create={} rename={} delete={} errors={} work={} big={} cycles={} subtree_deleted={} subtree_created={}",
        hms(elapsed),
        (ops - last_ops) as f64 / per.as_secs_f64().max(0.001),
        ld(&c.creates),
        ld(&c.renames),
        ld(&c.deletes),
        ld(&c.errors),
        tree.work_len(),
        tree.big_len(),
        ld(&c.cycles),
        ld(&c.subtree_deleted),
        ld(&c.subtree_created),
    )
}

fn churn_main(cfg: &Config, prepare_only: bool) -> anyhow::Result<()> {
    let file = match &cfg.log {
        Some(p) => {
            Some(File::options().create(true).append(true).open(p).with_context(|| format!("open {}", p.display()))?)
        }
        None => None,
    };
    let mut log = Logger { file };
    std::fs::create_dir_all(&cfg.root).with_context(|| format!("create {}", cfg.root.display()))?;
    let stop_file = cfg.stop_file.clone().unwrap_or_else(|| cfg.root.join("soak-churn.stop"));
    let _ = std::fs::remove_file(&stop_file);
    let stop = Arc::new(AtomicBool::new(false));
    let shape = SubtreeShape::for_entries(cfg.subtree);

    let t0 = Instant::now();
    let mut tree = Tree::open(&cfg.root, cfg.dirs).with_context(|| format!("scan {}", cfg.root.display()))?;
    log.line(&format!(
        "soak-churn {} root={} found work={} big={} subtree_generations={}",
        env!("CARGO_PKG_VERSION"),
        cfg.root.display(),
        tree.work_len(),
        tree.big_len(),
        churn::generations(&cfg.root)?.len()
    ));
    let made = tree.prefill(cfg.big, cfg.files, &stop).context("prefill")?;
    let sub = if churn::generations(&cfg.root)?.is_empty() {
        churn::create_subtree(&cfg.root, 0, shape, &stop).context("initial subtree")?
    } else {
        0
    };
    log.line(&format!(
        "prefill done in {:.1?}: {made} files, {sub} subtree entries (work={} big={})",
        t0.elapsed(),
        tree.work_len(),
        tree.big_len()
    ));
    if prepare_only {
        return Ok(());
    }

    log.line(&format!(
        "run: rate={}/s duration={} seed={} subtree={} entries every {} stop_file={}",
        cfg.rate,
        hms(cfg.duration),
        cfg.seed,
        shape.entries(),
        hms(cfg.subtree_every),
        stop_file.display()
    ));
    let counters = Arc::new(Counters::default());
    let subtree = {
        let (root, stop, c, every) = (cfg.root.clone(), stop.clone(), counters.clone(), cfg.subtree_every);
        std::thread::Builder::new()
            .name("subtree".into())
            .spawn(move || subtree_loop(&root, shape, every, &stop, &c))?
    };

    let start = Instant::now();
    let mut rng = Rng::new(cfg.seed);
    // One second of burst: a stall (slow disk, AV scan) is made up instead of lost.
    let mut bucket = TokenBucket::new(cfg.rate, cfg.rate.max(1.0));
    let mut minute = Every::new(Duration::from_secs(60), Duration::from_secs(60));
    let mut poll = Every::new(Duration::from_secs(1), Duration::from_secs(1));
    let mut last_ops = 0;
    let reason = loop {
        let now = start.elapsed();
        if now >= cfg.duration {
            break "duration reached";
        }
        if poll.due(now) && stop_file.exists() {
            break "stop file";
        }
        if minute.due(now) {
            log.line(&status(&counters, &tree, now, last_ops, Duration::from_secs(60)));
            last_ops = counters.steady_ops();
        }
        if bucket.try_take(now) {
            tree.step(cfg.files, &mut rng, &counters);
        } else {
            std::thread::sleep(bucket.wait().clamp(Duration::from_millis(1), Duration::from_millis(50)));
        }
    };
    stop.store(true, Ordering::Relaxed);
    let _ = subtree.join();
    let _ = std::fs::remove_file(&stop_file);
    let elapsed = start.elapsed();
    let avg = counters.steady_ops() as f64 / elapsed.as_secs_f64().max(0.001);
    log.line(&format!("stopped ({reason}); avg {avg:.1}/s; {}", status(&counters, &tree, elapsed, 0, elapsed)));
    Ok(())
}

fn subtree_loop(root: &std::path::Path, shape: SubtreeShape, every: Duration, stop: &AtomicBool, c: &Counters) {
    let start = Instant::now();
    let mut sched = Every::new(every, every);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
        if !sched.due(start.elapsed()) {
            continue;
        }
        match churn::recycle_subtree(root, shape, stop) {
            Ok((deleted, created)) => {
                c.subtree_deleted.fetch_add(deleted, Ordering::Relaxed);
                c.subtree_created.fetch_add(created, Ordering::Relaxed);
                c.cycles.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                c.errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("subtree recycle failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn config_parsing() {
        let c = parse_config(&s(&["--root", "D:\\soak", "--rate", "50", "--duration", "24h", "--seed", "7"])).unwrap();
        assert_eq!(c.root, PathBuf::from("D:\\soak"));
        assert_eq!((c.rate, c.duration, c.seed), (50.0, Duration::from_secs(86_400), 7));
        assert_eq!((c.files, c.big, c.subtree), (20_000, 10_000, 15_000));
        let c = parse_config(&s(&["--root", "x", "--subtree", "20_000", "--subtree-every", "10m"])).unwrap();
        assert_eq!((c.subtree, c.subtree_every), (20_000, Duration::from_secs(600)));
        assert!(parse_config(&s(&["--rate", "5"])).is_err(), "root required");
        assert!(parse_config(&s(&["--root", "x", "--rate", "0"])).is_err());
        assert!(parse_config(&s(&["--root", "x", "--bogus"])).is_err());
        assert!(parse_config(&s(&["--root"])).is_err());
    }

    #[test]
    fn run_stops_at_the_duration_and_on_the_stop_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("soak");
        let cfg = |d: &str| {
            parse_config(&s(&[
                "--root",
                root.to_str().unwrap(),
                "--rate",
                "200",
                "--duration",
                d,
                "--files",
                "50",
                "--big",
                "20",
                "--subtree",
                "30",
                "--dirs",
                "4",
            ]))
            .unwrap()
        };
        let t = Instant::now();
        churn_main(&cfg("1s"), false).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(churn::generations(&root).unwrap(), [0]);
        assert!(!root.join("soak-churn.stop").exists());

        let c = cfg("1h");
        let stopper = {
            let f = root.join("soak-churn.stop");
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                File::create(f).unwrap();
            })
        };
        let t = Instant::now();
        churn_main(&c, false).unwrap();
        stopper.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(10), "stop file honoured");
    }
}

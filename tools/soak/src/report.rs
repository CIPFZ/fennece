//! `soak-churn report <csv>`: turns the samples written by `soak.ps1` into the
//! markdown soak report (REL-05) and, optionally, SVG trend charts.
//!
//! The CSV has one row per sample (header names below, as written by `soak.ps1`;
//! fields may be quoted). Empty `app_pid` means fennec.exe was not running.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Column names written by `soak.ps1`.
const REQUIRED: [&str; 8] =
    ["time", "elapsed_s", "app_pid", "app_ws_mb", "app_private_mb", "app_cpu_s", "app_handles", "app_threads"];

/// One sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    /// Local time stamp as written.
    pub time: String,
    /// Seconds since the sampler started.
    pub elapsed_s: f64,
    /// fennec.exe PID (`None` = not running).
    pub app_pid: Option<u64>,
    /// fennec.exe working set (MiB).
    pub app_ws: f64,
    /// fennec.exe private bytes (MiB).
    pub app_private: f64,
    /// fennec.exe total CPU time (s).
    pub app_cpu_s: f64,
    /// fennec.exe handle count.
    pub app_handles: f64,
    /// fennec.exe thread count.
    pub app_threads: f64,
    /// fennec-service.exe PID.
    pub svc_pid: Option<u64>,
    /// fennec-service.exe working set (MiB).
    pub svc_ws: f64,
    /// fennec-service.exe private bytes (MiB).
    pub svc_private: f64,
    /// fennec-service.exe total CPU time (s).
    pub svc_cpu_s: f64,
    /// fennec-service.exe handle count.
    pub svc_handles: f64,
    /// Index entries (all), `None` if the query failed.
    pub entries: Option<f64>,
    /// Index entries below the churn root.
    pub soak_entries: Option<f64>,
    /// Index rows including tombstones (newest "database saved rows=" log line).
    pub index_rows: Option<f64>,
    /// `crash-*.log` files in the logs folder(s).
    pub crash_files: f64,
    /// "index compacted while running" lines in the fennec logs so far.
    pub compactions: f64,
    /// "live compaction discarded" lines (attempts lost to concurrent changes).
    pub discards: f64,
    /// Free-form events (`restarted fennec`, ...).
    pub note: String,
}

/// Splits one CSV line (RFC 4180 quoting, no embedded newlines).
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', true) => quoted = false,
            ('"', false) if cur.is_empty() => quoted = true,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Parses the CSV text.
pub fn parse_csv(text: &str) -> anyhow::Result<Vec<Sample>> {
    let mut lines = text.lines().map(|l| l.trim_start_matches('\u{feff}')).filter(|l| !l.trim().is_empty());
    let header = split_csv(lines.next().context("empty CSV")?);
    let idx: HashMap<&str, usize> = header.iter().enumerate().map(|(i, h)| (h.trim(), i)).collect();
    for r in REQUIRED {
        if !idx.contains_key(r) {
            bail!("CSV lacks column {r:?}");
        }
    }
    let mut out = Vec::new();
    for (n, line) in lines.enumerate() {
        let f = split_csv(line);
        let get = |k: &str| idx.get(k).and_then(|&i| f.get(i)).map_or("", |s| s.trim());
        let num = |k: &str| get(k).parse::<f64>().ok();
        let elapsed_s = num("elapsed_s").with_context(|| format!("row {}: bad elapsed_s", n + 2))?;
        out.push(Sample {
            time: get("time").to_string(),
            elapsed_s,
            app_pid: get("app_pid").parse().ok(),
            app_ws: num("app_ws_mb").unwrap_or(0.0),
            app_private: num("app_private_mb").unwrap_or(0.0),
            app_cpu_s: num("app_cpu_s").unwrap_or(0.0),
            app_handles: num("app_handles").unwrap_or(0.0),
            app_threads: num("app_threads").unwrap_or(0.0),
            svc_pid: get("svc_pid").parse().ok(),
            svc_ws: num("svc_ws_mb").unwrap_or(0.0),
            svc_private: num("svc_private_mb").unwrap_or(0.0),
            svc_cpu_s: num("svc_cpu_s").unwrap_or(0.0),
            svc_handles: num("svc_handles").unwrap_or(0.0),
            entries: num("entries"),
            soak_entries: num("soak_entries"),
            index_rows: num("index_rows"),
            crash_files: num("crash_files").unwrap_or(0.0),
            compactions: num("compactions").unwrap_or(0.0),
            discards: num("compaction_discards").unwrap_or(0.0),
            note: get("note").to_string(),
        });
    }
    if out.is_empty() {
        bail!("CSV has no samples");
    }
    Ok(out)
}

/// First/last/min/max/mean of a series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    /// First value.
    pub first: f64,
    /// Last value.
    pub last: f64,
    /// Minimum.
    pub min: f64,
    /// Maximum.
    pub max: f64,
    /// Mean.
    pub mean: f64,
}

/// Stats of `v` (`None` if empty).
pub fn stats(v: &[f64]) -> Option<Stats> {
    let (&first, &last) = (v.first()?, v.last()?);
    let min = v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(Stats { first, last, min, max, mean: v.iter().sum::<f64>() / v.len() as f64 })
}

/// Growth of the mean of the last window over the mean of the first window, in %.
/// The window is one hour, or a quarter of the run if shorter than 4 h, and the
/// first window starts after `warmup_s` (initial indexing). `None` with < 2 samples.
pub fn window_growth(t: &[f64], v: &[f64], warmup_s: f64) -> Option<(f64, f64, f64)> {
    let (&t0, &t1) = (t.first()?, t.last()?);
    let start = (t0 + warmup_s).min(t1);
    let span = t1 - start;
    if t.len() < 2 || span <= 0.0 {
        return None;
    }
    let w = (span / 4.0).min(3600.0);
    let mean = |lo: f64, hi: f64| {
        let xs: Vec<f64> = t.iter().zip(v).filter(|(x, _)| **x >= lo && **x <= hi).map(|(_, y)| *y).collect();
        (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
    };
    let a = mean(start, start + w)?;
    let b = mean(t1 - w, t1)?;
    (a > 0.0).then(|| (a, b, (b - a) / a * 100.0))
}

/// Least-squares slope of `v` over `t`, per hour.
pub fn slope_per_hour(t: &[f64], v: &[f64]) -> f64 {
    let n = t.len().min(v.len()) as f64;
    if n < 2.0 {
        return 0.0;
    }
    let (mt, mv) = (t.iter().sum::<f64>() / n, v.iter().sum::<f64>() / n);
    let (mut num, mut den) = (0.0, 0.0);
    for (x, y) in t.iter().zip(v) {
        num += (x - mt) * (y - mv);
        den += (x - mt) * (x - mt);
    }
    if den == 0.0 { 0.0 } else { num / den * 3600.0 }
}

/// Indices of the rows to print: all if ≤ `max_rows`, else the first, one per hour
/// (the first sample at or after each full hour) and the last.
pub fn table_rows(t: &[f64], max_rows: usize) -> Vec<usize> {
    if t.len() <= max_rows {
        return (0..t.len()).collect();
    }
    let t0 = t[0];
    let mut rows = vec![0];
    let mut next_hour = 1.0;
    for (i, &x) in t.iter().enumerate().skip(1) {
        if (x - t0) / 3600.0 >= next_hour {
            rows.push(i);
            next_hour = ((x - t0) / 3600.0).floor() + 1.0;
        }
    }
    if rows.last() != Some(&(t.len() - 1)) {
        rows.push(t.len() - 1);
    }
    rows
}

/// Number of PID changes of a process (`Some(a)` → `Some(b ≠ a)`, or back after a
/// gap) and the number of samples where it was missing.
pub fn restarts(pids: &[Option<u64>]) -> (usize, usize) {
    let mut last = None;
    let mut changes = 0;
    for p in pids.iter().flatten() {
        if last.is_some_and(|l| l != *p) {
            changes += 1;
        }
        last = Some(*p);
    }
    let missing = pids.iter().skip_while(|p| p.is_none()).filter(|p| p.is_none()).count();
    (changes, missing)
}

/// Pass/fail thresholds (AGENT.md §7.1 REL-05).
#[derive(Debug, Clone, Copy)]
pub struct Criteria {
    /// Max RSS growth, %.
    pub max_rss_growth: f64,
    /// Max handle growth, % (and at least `handle_slack` absolute).
    pub max_handle_growth: f64,
    /// Absolute handle growth that is always tolerated.
    pub handle_slack: f64,
    /// Minimum run length for a verdict, hours.
    pub min_hours: f64,
    /// Warm-up excluded from the first window, seconds.
    pub warmup_s: f64,
}

impl Default for Criteria {
    fn default() -> Self {
        Self { max_rss_growth: 10.0, max_handle_growth: 10.0, handle_slack: 50.0, min_hours: 24.0, warmup_s: 0.0 }
    }
}

fn verdict(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

fn pct(x: f64) -> String {
    format!("{x:+.1} %")
}

/// The markdown report. `charts` are relative image paths to embed.
pub fn render(samples: &[Sample], c: &Criteria, title: &str, charts: &[(String, String)]) -> String {
    let t: Vec<f64> = samples.iter().map(|s| s.elapsed_s).collect();
    // Memory/handle series only over samples where fennec was running.
    let up: Vec<&Sample> = samples.iter().filter(|s| s.app_pid.is_some()).collect();
    let tu: Vec<f64> = up.iter().map(|s| s.elapsed_s).collect();
    let col = |f: fn(&Sample) -> f64| up.iter().map(|s| f(s)).collect::<Vec<f64>>();
    let (ws, private, handles, threads) =
        (col(|s| s.app_ws), col(|s| s.app_private), col(|s| s.app_handles), col(|s| s.app_threads));
    let svc: Vec<&Sample> = samples.iter().filter(|s| s.svc_pid.is_some()).collect();
    let ts: Vec<f64> = svc.iter().map(|s| s.elapsed_s).collect();
    let (svc_ws, svc_handles): (Vec<f64>, Vec<f64>) = svc.iter().map(|s| (s.svc_ws, s.svc_handles)).unzip();

    let first = &samples[0];
    let last = &samples[samples.len() - 1];
    let hours = (last.elapsed_s - first.elapsed_s) / 3600.0;
    let cpu_avg = |a: &[&Sample], f: fn(&Sample) -> f64| match (a.first(), a.last()) {
        (Some(x), Some(y)) if y.elapsed_s > x.elapsed_s && f(y) >= f(x) => {
            Some((f(y) - f(x)) / (y.elapsed_s - x.elapsed_s) * 100.0)
        }
        _ => None,
    };
    let app_cpu = cpu_avg(&up, |s| s.app_cpu_s);
    let svc_cpu = cpu_avg(&svc, |s| s.svc_cpu_s);
    let crash_new = last.crash_files - first.crash_files;
    let compactions = last.compactions - first.compactions;
    let discards = last.discards - first.discards;
    let (app_restarts, app_missing) = restarts(&samples.iter().map(|s| s.app_pid).collect::<Vec<_>>());
    let (svc_restarts, svc_missing) = restarts(&samples.iter().map(|s| s.svc_pid).collect::<Vec<_>>());
    let ws_g = window_growth(&tu, &ws, c.warmup_s);
    let pr_g = window_growth(&tu, &private, c.warmup_s);
    let h_g = window_growth(&tu, &handles, c.warmup_s);
    let raw = |v: &[f64]| match (v.first(), v.last()) {
        (Some(a), Some(b)) if *a > 0.0 => Some((b - a) / a * 100.0),
        _ => None,
    };

    let rss_ok = ws_g.is_some_and(|g| g.2 < c.max_rss_growth);
    // The working set can be trimmed by Windows and hide a leak; private bytes cannot.
    let private_ok = pr_g.is_some_and(|g| g.2 < c.max_rss_growth);
    let handles_ok = h_g.is_some_and(|(a, b, g)| g < c.max_handle_growth || b - a <= c.handle_slack);
    let crash_ok = last.crash_files == 0.0;
    let up_ok = app_restarts == 0 && app_missing == 0;
    let compact_ok = compactions > 0.0;
    let long_enough = hours + 1e-9 >= c.min_hours;
    let all = rss_ok && private_ok && handles_ok && crash_ok && up_ok && compact_ok;

    let mut m = String::new();
    let _ = writeln!(m, "# {title}\n");
    let _ = writeln!(
        m,
        "Run: {} → {} ({hours:.2} h, {} samples). Generated by `soak-churn report` from the `soak.ps1` CSV.\n",
        first.time,
        last.time,
        samples.len()
    );
    let overall = match (all, long_enough) {
        (true, true) => "**PASS**".to_string(),
        (false, _) => "**FAIL**".to_string(),
        (true, false) => format!("**PASS (trial only: {hours:.2} h < {} h required)**", c.min_hours),
    };
    let _ = writeln!(m, "Overall: {overall}\n");

    let _ = writeln!(m, "## REL-05 criteria\n");
    let _ = writeln!(m, "| Criterion | Measured | Result |\n|---|---|---|");
    let g = |x: Option<(f64, f64, f64)>, unit: &str| match x {
        Some((a, b, p)) => format!("{a:.1} → {b:.1}{unit} ({})", pct(p)),
        None => "n/a".into(),
    };
    let _ = writeln!(
        m,
        "| fennec RSS (working set) growth < {} %, first vs last window mean | {} | {} |",
        c.max_rss_growth,
        g(ws_g, " MiB"),
        verdict(rss_ok)
    );
    let _ = writeln!(
        m,
        "| fennec private bytes growth < {} % (leak check; the working set can be trimmed) | {} | {} |",
        c.max_rss_growth,
        g(pr_g, " MiB"),
        verdict(private_ok)
    );
    let _ = writeln!(
        m,
        "| fennec handles stable (< {} % or ≤ {} more) | {} | {} |",
        c.max_handle_growth,
        c.handle_slack,
        g(h_g, ""),
        verdict(handles_ok)
    );
    let _ = writeln!(m, "| No crash files | {} | {} |", last.crash_files, verdict(crash_ok));
    let _ = writeln!(
        m,
        "| fennec never exited | {app_restarts} restarts, missing in {app_missing} samples | {} |",
        verdict(up_ok)
    );
    let _ = writeln!(
        m,
        "| Live compaction ran | {compactions} times ({discards} attempts discarded: index changed meanwhile) | {} |",
        verdict(compact_ok)
    );
    let _ = writeln!(
        m,
        "| Duration ≥ {} h | {hours:.2} h | {} |\n",
        c.min_hours,
        if long_enough { "PASS" } else { "TRIAL" }
    );
    let w = if hours >= 4.0 { "1 h".to_string() } else { format!("{:.0} min (a quarter of the run)", hours * 15.0) };
    let _ =
        writeln!(m, "Windows: first = the {w} after a {:.0} min warm-up, last = the final {w}.\n", c.warmup_s / 60.0);

    let _ = writeln!(m, "## Summary\n");
    let _ = writeln!(m, "| Series | First | Last | Min | Max | Mean | Raw first→last | Window growth | Slope/h |");
    let _ = writeln!(m, "|---|---|---|---|---|---|---|---|---|");
    let mut row = |name: &str, tt: &[f64], v: &[f64], gr: Option<(f64, f64, f64)>| {
        if let Some(s) = stats(v) {
            let _ = writeln!(
                m,
                "| {name} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {} | {} | {:+.2} |",
                s.first,
                s.last,
                s.min,
                s.max,
                s.mean,
                raw(v).map_or("n/a".into(), pct),
                gr.map_or("n/a".into(), |x| pct(x.2)),
                slope_per_hour(tt, v)
            );
        }
    };
    row("fennec working set (MiB)", &tu, &ws, ws_g);
    row("fennec private bytes (MiB)", &tu, &private, pr_g);
    row("fennec handles", &tu, &handles, h_g);
    row("fennec threads", &tu, &threads, window_growth(&tu, &threads, c.warmup_s));
    row("service working set (MiB)", &ts, &svc_ws, window_growth(&ts, &svc_ws, c.warmup_s));
    row("service handles", &ts, &svc_handles, window_growth(&ts, &svc_handles, c.warmup_s));
    let ent: Vec<(f64, f64)> = samples.iter().filter_map(|s| Some((s.elapsed_s, s.entries?))).collect();
    let (te, ev): (Vec<f64>, Vec<f64>) = ent.into_iter().unzip();
    row("index entries", &te, &ev, None);
    let ent: Vec<(f64, f64)> = samples.iter().filter_map(|s| Some((s.elapsed_s, s.soak_entries?))).collect();
    let (te, ev): (Vec<f64>, Vec<f64>) = ent.into_iter().unzip();
    row("index entries below the churn root", &te, &ev, None);
    let ent: Vec<(f64, f64)> = samples.iter().filter_map(|s| Some((s.elapsed_s, s.index_rows?))).collect();
    let (te, ev): (Vec<f64>, Vec<f64>) = ent.into_iter().unzip();
    row("index rows incl. tombstones (at DB saves)", &te, &ev, None);
    let cpu = |x: Option<f64>| x.map_or("n/a".into(), |v| format!("{v:.1} %"));
    let _ = writeln!(
        m,
        "\nCPU average (Δ CPU time / Δ wall time, % of one core): fennec {}, service {}. \
         Service: {svc_restarts} restarts, missing in {svc_missing} samples. \
         New crash files during the run: {crash_new}.\n",
        cpu(app_cpu),
        cpu(svc_cpu)
    );

    for (alt, path) in charts {
        let _ = writeln!(m, "![{alt}]({path})\n");
    }

    let rows = table_rows(&t, 48);
    let _ =
        writeln!(m, "## Samples{}\n", if rows.len() < samples.len() { " (hourly; full data in the CSV)" } else { "" });
    let _ = writeln!(
        m,
        "| Time | Elapsed | WS MiB | Private MiB | CPU % | Handles | Threads | Svc WS MiB | Svc handles | Entries | Soak entries | Rows | Compactions | Discarded | Crash | Note |"
    );
    let _ = writeln!(m, "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut prev: Option<&Sample> = None;
    for &i in &rows {
        let s = &samples[i];
        let cpu = match prev {
            Some(p) if s.elapsed_s > p.elapsed_s && s.app_cpu_s >= p.app_cpu_s && s.app_pid == p.app_pid => {
                format!("{:.1}", (s.app_cpu_s - p.app_cpu_s) / (s.elapsed_s - p.elapsed_s) * 100.0)
            }
            _ => String::new(),
        };
        let o = |x: Option<f64>| x.map_or(String::new(), |v| format!("{v:.0}"));
        let up = s.app_pid.is_some();
        let a = |v: f64, d: usize| if up { format!("{v:.d$}") } else { "—".into() };
        let sv = |v: f64, d: usize| if s.svc_pid.is_some() { format!("{v:.d$}") } else { "—".into() };
        let _ = writeln!(
            m,
            "| {} | {} | {} | {} | {cpu} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            s.time,
            crate::plan::hms(std::time::Duration::from_secs_f64((s.elapsed_s - first.elapsed_s).max(0.0))),
            a(s.app_ws, 1),
            a(s.app_private, 1),
            a(s.app_handles, 0),
            a(s.app_threads, 0),
            sv(s.svc_ws, 1),
            sv(s.svc_handles, 0),
            o(s.entries),
            o(s.soak_entries),
            o(s.index_rows),
            s.compactions,
            s.discards,
            s.crash_files,
            s.note.replace('|', "/"),
        );
        prev = Some(s);
    }
    m
}

/// One line series for [`svg_chart`].
pub struct Series<'a> {
    /// Legend label.
    pub label: &'a str,
    /// Stroke colour.
    pub color: &'a str,
    /// (hours, value) points.
    pub points: Vec<(f64, f64)>,
}

fn nice_step(range: f64) -> f64 {
    let raw = (range / 5.0).max(1e-9);
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag)
}

/// A small single-axis SVG line chart (x = hours since start).
pub fn svg_chart(title: &str, unit: &str, series: &[Series<'_>]) -> String {
    let (w, h, l, r, top, b) = (760.0, 300.0, 64.0, 20.0, 48.0, 40.0);
    let pts = series.iter().flat_map(|s| s.points.iter());
    let xmax = pts.clone().map(|p| p.0).fold(0.0, f64::max).max(1e-6);
    let mut ymin = pts.clone().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let mut ymax = pts.map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    if !ymin.is_finite() {
        (ymin, ymax) = (0.0, 1.0);
    }
    // Zero-based axis unless the series sits far above zero (memory, handles).
    let step = nice_step(if ymin > 0.5 * ymax { ymax - ymin } else { ymax }.max(1.0));
    let y0 = if ymin > 0.5 * ymax { (ymin / step).floor() * step } else { 0.0 };
    let y1 = ((ymax / step).ceil() * step).max(y0 + step);
    let xs = |x: f64| l + x / xmax * (w - l - r);
    let ys = |y: f64| h - b - (y - y0) / (y1 - y0) * (h - top - b);
    let mut s = String::new();
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="system-ui, sans-serif" font-size="12"><rect width="100%" height="100%" fill="#fcfcfb"/><text x="{l}" y="20" font-size="14" font-weight="600" fill="#0b0b0b">{title}</text>"##
    );
    let mut y = y0;
    while y <= y1 + step * 0.01 {
        let _ = write!(
            s,
            r##"<line x1="{l}" x2="{}" y1="{py:.1}" y2="{py:.1}" stroke="#e6e5e1"/><text x="{}" y="{:.1}" text-anchor="end" fill="#52514e">{y}</text>"##,
            w - r,
            l - 6.0,
            ys(y) + 4.0,
            py = ys(y)
        );
        y += step;
    }
    let xstep = nice_step(xmax);
    let mut x = 0.0;
    while x <= xmax + xstep * 0.01 {
        let _ = write!(
            s,
            r##"<text x="{:.1}" y="{}" text-anchor="middle" fill="#52514e">{x}h</text>"##,
            xs(x),
            h - b + 18.0
        );
        x += xstep;
    }
    let _ = write!(s, r##"<text x="12" y="{}" fill="#52514e">{unit}</text>"##, top - 8.0);
    let mut lx = l + 240.0;
    for se in series {
        let path: Vec<String> = se.points.iter().map(|p| format!("{:.1},{:.1}", xs(p.0), ys(p.1))).collect();
        let _ = write!(
            s,
            r#"<polyline fill="none" stroke="{}" stroke-width="2" stroke-linejoin="round" points="{}"/>"#,
            se.color,
            path.join(" ")
        );
        let _ = write!(
            s,
            r##"<rect x="{lx}" y="11" width="12" height="3" rx="1" fill="{}"/><text x="{}" y="16" fill="#0b0b0b">{}</text>"##,
            se.color,
            lx + 16.0,
            se.label
        );
        lx += 24.0 + 7.0 * se.label.len() as f64;
    }
    s.push_str("</svg>\n");
    s
}

const BLUE: &str = "#2a78d6";
const ORANGE: &str = "#eb6834";

fn write_charts(samples: &[Sample], dir: &Path, rel: &str) -> anyhow::Result<Vec<(String, String)>> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let t0 = samples[0].elapsed_s;
    let pts = |f: fn(&Sample) -> f64| -> Vec<(f64, f64)> {
        samples.iter().filter(|s| s.app_pid.is_some()).map(|s| ((s.elapsed_s - t0) / 3600.0, f(s))).collect()
    };
    let charts = [
        (
            "soak-memory.svg",
            "fennec memory",
            svg_chart(
                "fennec.exe memory",
                "MiB",
                &[
                    Series { label: "working set", color: BLUE, points: pts(|s| s.app_ws) },
                    Series { label: "private bytes", color: ORANGE, points: pts(|s| s.app_private) },
                ],
            ),
        ),
        (
            "soak-handles.svg",
            "fennec handles",
            svg_chart(
                "fennec.exe handles",
                "count",
                &[Series { label: "handles", color: BLUE, points: pts(|s| s.app_handles) }],
            ),
        ),
    ];
    let mut out = Vec::new();
    for (file, alt, svg) in charts {
        let p = dir.join(file);
        std::fs::write(&p, svg).with_context(|| format!("write {}", p.display()))?;
        out.push((alt.to_string(), format!("{rel}{file}")));
    }
    Ok(out)
}

/// `report <csv> [--out FILE] [--svg-dir DIR] [--min-hours H] [--warmup 0m] [--title T]`.
pub fn main(args: &[String]) -> anyhow::Result<()> {
    let mut csv: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut svg_dir: Option<PathBuf> = None;
    let mut c = Criteria::default();
    let mut title = "Fennec soak report (REL-05)".to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--out" | "-o" => out = Some(val()?.into()),
            "--svg-dir" => svg_dir = Some(val()?.into()),
            "--min-hours" => c.min_hours = val()?.parse().context("--min-hours")?,
            "--warmup" => {
                c.warmup_s = crate::plan::parse_duration(val()?).map_err(anyhow::Error::msg)?.as_secs_f64();
            }
            "--title" => title = val()?.clone(),
            s if !s.starts_with("--") && csv.is_none() => csv = Some(s.into()),
            other => bail!("unknown argument {other}"),
        }
    }
    let csv = csv.context("usage: soak-churn report SAMPLES.csv [--out FILE.md] [--svg-dir DIR]")?;
    let text = std::fs::read_to_string(&csv).with_context(|| format!("read {}", csv.display()))?;
    let samples = parse_csv(&text)?;
    let charts = match &svg_dir {
        Some(d) => {
            // Image links relative to the markdown file when both are given.
            let base = out.as_ref().and_then(|o| o.parent()).filter(|p| !p.as_os_str().is_empty());
            let rel = match base.and_then(|b| d.strip_prefix(b).ok()) {
                Some(r) if r.as_os_str().is_empty() => String::new(),
                Some(r) => format!("{}/", r.to_string_lossy().replace('\\', "/")),
                None => format!("{}/", d.to_string_lossy().replace('\\', "/")),
            };
            write_charts(&samples, d, &rel)?
        }
        None => Vec::new(),
    };
    let md = render(&samples, &c, &title, &charts);
    match out {
        Some(p) => std::fs::write(&p, md).with_context(|| format!("write {}", p.display()))?,
        None => {
            use std::io::Write;
            std::io::stdout().lock().write_all(md.as_bytes())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "time,elapsed_s,app_pid,app_ws_mb,app_private_mb,app_cpu_s,app_cpu_pct,app_handles,app_threads,svc_pid,svc_ws_mb,svc_private_mb,svc_cpu_s,svc_cpu_pct,svc_handles,svc_threads,entries,soak_entries,index_rows,crash_files,compactions,compaction_discards,note";

    /// A synthetic run: `hours` long, 5-min samples, RSS growing by `growth` overall.
    fn csv(hours: f64, growth: f64, f: impl Fn(usize, &mut Vec<String>)) -> String {
        let n = (hours * 12.0) as usize + 1;
        let mut s = format!("{HEADER}\n");
        for i in 0..n {
            let frac = i as f64 / (n - 1) as f64;
            let ws = 200.0 * (1.0 + growth * frac);
            let mut row: Vec<String> = vec![
                format!("2026-10-14 {:02}:{:02}:00", i / 12 % 24, i % 12 * 5),
                (i * 300).to_string(),
                "1234".into(),
                format!("{ws:.1}"),
                format!("{:.1}", ws * 1.2),
                format!("{:.1}", i as f64 * 6.0),
                "2.0".into(),
                "400".into(),
                "30".into(),
                "99".into(),
                "8.0".into(),
                "4.0".into(),
                "1.0".into(),
                "0.0".into(),
                "120".into(),
                "6".into(),
                "1500000".into(),
                "45100".into(),
                "1600000".into(),
                "0".into(),
                (i / 3).to_string(),
                (i / 2).to_string(),
                String::new(),
            ];
            f(i, &mut row);
            s.push_str(&row.join(","));
            s.push('\n');
        }
        s
    }

    #[test]
    fn csv_split_and_parse() {
        assert_eq!(split_csv(r#""a","b,c",,"d""e""#), ["a", "b,c", "", "d\"e"]);
        assert_eq!(split_csv("1,2"), ["1", "2"]);
        let s = parse_csv(&format!("\u{feff}{}", csv(1.0, 0.0, |_, _| {}))).unwrap();
        assert_eq!(s.len(), 13);
        assert_eq!(s[1].app_pid, Some(1234));
        assert_eq!(s[1].entries, Some(1_500_000.0));
        assert!(parse_csv("time,elapsed_s\n1,2\n").is_err(), "missing columns");
        assert!(parse_csv(HEADER).is_err(), "no samples");
        let quoted = format!("{}\n", HEADER.split(',').map(|h| format!("\"{h}\"")).collect::<Vec<_>>().join(","))
            + "\"t\",\"0\",\"\",\"0\",\"0\",\"0\",\"0\",\"0\",\"0\",\"\",\"\",\"\",\"\",\"\",\"\",\"\",\"\",\"\",\"\",\"0\",\"0\",\"0\",\"start\"\n";
        let q = parse_csv(&quoted).unwrap();
        assert_eq!((q[0].app_pid, q[0].svc_pid, q[0].entries, q[0].note.as_str()), (None, None, None, "start"));
    }

    #[test]
    fn growth_windows_and_slope() {
        let t: Vec<f64> = (0..=288).map(|i| i as f64 * 300.0).collect(); // 24 h
        let v: Vec<f64> = t.iter().map(|x| 100.0 + x / 3600.0).collect(); // +1 MiB/h
        let (a, b, g) = window_growth(&t, &v, 0.0).unwrap();
        assert!((a - 100.5).abs() < 0.01 && (b - 123.5).abs() < 0.01, "{a} {b}");
        assert!((g - 22.89).abs() < 0.1, "{g}");
        assert!((slope_per_hour(&t, &v) - 1.0).abs() < 1e-9);
        // Short run: quarter windows; warm-up shifts the first window.
        let t: Vec<f64> = (0..=12).map(|i| i as f64 * 300.0).collect();
        let v: Vec<f64> = (0..=12).map(|i| if i < 2 { 50.0 } else { 100.0 }).collect();
        assert!(window_growth(&t, &v, 0.0).unwrap().2 > 0.0);
        assert_eq!(window_growth(&t, &v, 600.0).unwrap().2, 0.0);
        assert!(window_growth(&[0.0], &[1.0], 0.0).is_none());
    }

    #[test]
    fn hourly_rows_and_restarts() {
        let t: Vec<f64> = (0..=288).map(|i| i as f64 * 300.0).collect();
        let rows = table_rows(&t, 48);
        assert_eq!(rows.len(), 25);
        assert_eq!((rows[0], rows[1], rows[24]), (0, 12, 288));
        assert_eq!(table_rows(&t[..10], 48).len(), 10);
        assert_eq!(restarts(&[None, Some(1), Some(1), None, Some(2), Some(2)]), (1, 1));
        assert_eq!(restarts(&[Some(5), Some(5)]), (0, 0));
    }

    #[test]
    fn report_pass_fail_and_trial() {
        let c = Criteria::default();
        let md = render(&parse_csv(&csv(24.0, 0.05, |_, _| {})).unwrap(), &c, "Soak", &[]);
        assert!(md.contains("Overall: **PASS**"), "{md}");
        assert!(md.contains("## Samples (hourly"));
        assert!(
            md.contains("| Live compaction ran | 96 times (144 attempts discarded: index changed meanwhile) | PASS |"),
            "{md}"
        );

        let leak = render(&parse_csv(&csv(24.0, 0.3, |_, _| {})).unwrap(), &c, "Soak", &[]);
        assert!(leak.contains("Overall: **FAIL**"));
        assert!(leak.contains("growth < 10 %, first vs last window mean |") && leak.contains("| FAIL |"));

        let crash = csv(24.0, 0.0, |i, r| {
            if i > 100 {
                r[19] = "1".into();
                r[2] = "4321".into();
                r[22] = "restarted fennec".into();
            }
        });
        let md = render(&parse_csv(&crash).unwrap(), &c, "Soak", &[]);
        assert!(md.contains("| No crash files | 1 | FAIL |"));
        assert!(md.contains("| 1 restarts, missing in 0 samples | FAIL |"));

        let private = csv(24.0, 0.0, |i, r| r[4] = format!("{:.1}", 240.0 + i as f64));
        let p = render(&parse_csv(&private).unwrap(), &c, "Soak", &[]);
        assert!(
            p.contains("Overall: **FAIL**")
                && p.contains("(leak check; the working set can be trimmed) | 246.0 → 522.0 MiB (+112.2 %) | FAIL"),
            "{p}"
        );

        let handles = csv(24.0, 0.0, |i, r| r[7] = (400 + i * 2).to_string());
        let h = render(&parse_csv(&handles).unwrap(), &c, "Soak", &[]);
        assert!(h.contains("handles stable (< 10 % or ≤ 50 more) | 412.0 → 964.0 (+134.0 %) | FAIL"), "{h}");

        let trial =
            render(&parse_csv(&csv(0.75, 0.0, |_, _| {})).unwrap(), &c, "Trial", &[("m".into(), "x.svg".into())]);
        assert!(trial.contains("PASS (trial only: 0.75 h < 24 h required)"), "{trial}");
        assert!(trial.contains("![m](x.svg)"));
        assert!(!trial.contains("hourly"));
    }

    #[test]
    fn svg_is_well_formed() {
        let s = svg_chart("t", "MiB", &[Series { label: "ws", color: BLUE, points: vec![(0.0, 100.0), (1.0, 120.0)] }]);
        assert!(s.starts_with("<svg") && s.trim_end().ends_with("</svg>"));
        assert_eq!(s.matches("<polyline").count(), 1);
        assert!(svg_chart("e", "", &[]).contains("</svg>"));
        assert_eq!(nice_step(100.0), 20.0);
        assert_eq!(nice_step(12.0), 2.5);
    }

    #[test]
    fn main_writes_markdown_and_charts() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("s.csv");
        std::fs::write(&csv_path, csv(2.0, 0.0, |_, _| {})).unwrap();
        let out = dir.path().join("r.md");
        let svg = dir.path().join("img");
        let args: Vec<String> =
            [csv_path.to_str().unwrap(), "--out", out.to_str().unwrap(), "--svg-dir", svg.to_str().unwrap()]
                .iter()
                .map(|s| s.to_string())
                .collect();
        main(&args).unwrap();
        let md = std::fs::read_to_string(out).unwrap();
        assert!(md.contains("](img/soak-memory.svg)"), "{md}");
        assert!(svg.join("soak-handles.svg").is_file());
        assert!(main(&["--bogus".to_string()]).is_err());
    }
}

//! Synthetic file-system generator.
//!
//! Produces a deterministic tree (seeded PRNG) shaped like a real Windows system drive:
//!
//! * folder depth 2–12 (most entries at depth 4–7), ~11% folders;
//! * weighted extension mix (dll/jpg/png/txt/js/... plus 2% without extension);
//! * 10% CJK names, 2% names with diacritics, camera-style `IMG_####.JPG` names;
//! * 5% duplicate files (same name and size in another folder) for `dupe:` tests;
//! * log-uniform sizes per type and dates over the last ten years.

use fnc_core::FileTime;
use fnc_core::flags::attr;

/// SplitMix64 PRNG (deterministic, no dependencies).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seeded generator.
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    /// `true` with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }

    /// Uniform float in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Element of a non-empty slice.
    pub fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

const WORDS: &[&str] = &[
    "project",
    "report",
    "data",
    "backup",
    "photos",
    "music",
    "video",
    "docs",
    "src",
    "build",
    "assets",
    "images",
    "config",
    "notes",
    "invoice",
    "budget",
    "summary",
    "draft",
    "final",
    "archive",
    "client",
    "server",
    "test",
    "release",
    "cache",
    "temp",
    "logs",
    "resources",
    "plugins",
    "themes",
    "fonts",
    "scripts",
    "tools",
    "lib",
    "bin",
    "include",
    "shared",
    "common",
    "core",
    "utils",
    "models",
    "views",
    "templates",
    "static",
    "public",
    "private",
    "meeting",
    "travel",
    "family",
    "holiday",
    "work",
    "school",
    "games",
    "downloads",
    "desktop",
    "settings",
    "profile",
    "update",
    "setup",
    "install",
    "readme",
    "license",
    "changelog",
    "manual",
    "guide",
    "presentation",
    "spreadsheet",
    "contract",
    "proposal",
    "schedule",
    "agenda",
    "minutes",
    "quarterly",
    "annual",
    "monthly",
    "weekly",
    "daily",
    "alpha",
    "beta",
    "gamma",
    "delta",
    "omega",
    "orion",
    "phoenix",
    "falcon",
    "summit",
    "harbor",
];

const CJK_WORDS: &[&str] = &[
    "季度报告",
    "预算表",
    "会议记录",
    "项目计划",
    "照片",
    "文档",
    "合同",
    "简历",
    "发票",
    "旅行",
    "家庭",
    "工作",
    "学习",
    "资料",
    "备份",
    "设计稿",
    "需求文档",
    "用户手册",
    "財務報告",
    "議事録",
    "写真",
    "資料",
    "企画書",
    "報告書",
    "見積書",
    "사진",
    "문서",
    "보고서",
];

const DIACRITIC_WORDS: &[&str] = &[
    "résumé",
    "café",
    "naïve",
    "Straße",
    "Ærøskøbing",
    "façade",
    "jalapeño",
    "smörgåsbord",
    "Łódź",
    "crème brûlée",
    "Ångström",
    "piñata",
    "über",
    "déjà vu",
    "Zürich",
    "São Paulo",
];

const TOP_DIRS: &[&str] = &[
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "Users",
    "Projects",
    "Games",
    "Data",
    "Backup",
    "Media",
];

/// `(extension, weight, log10 min size, log10 max size)`.
const EXTS: &[(&str, u32, f64, f64)] = &[
    ("dll", 80, 4.0, 7.0),
    ("exe", 25, 4.0, 8.0),
    ("jpg", 90, 5.0, 7.0),
    ("JPG", 10, 5.0, 7.0),
    ("png", 60, 3.0, 6.5),
    ("gif", 10, 3.0, 6.0),
    ("svg", 10, 2.5, 5.0),
    ("ico", 8, 3.0, 5.0),
    ("txt", 50, 1.0, 5.0),
    ("log", 20, 2.0, 7.0),
    ("md", 15, 2.0, 4.5),
    ("js", 55, 2.0, 5.5),
    ("ts", 20, 2.0, 5.0),
    ("json", 40, 1.5, 5.5),
    ("xml", 35, 2.0, 6.0),
    ("html", 25, 2.5, 5.5),
    ("css", 15, 2.0, 5.0),
    ("h", 35, 2.0, 5.0),
    ("c", 15, 2.5, 5.0),
    ("cpp", 25, 2.5, 5.5),
    ("rs", 15, 2.5, 5.0),
    ("py", 25, 2.0, 5.0),
    ("java", 10, 2.5, 5.0),
    ("cs", 10, 2.5, 5.0),
    ("go", 5, 2.5, 5.0),
    ("mp3", 30, 6.0, 7.2),
    ("flac", 5, 7.0, 8.0),
    ("wav", 5, 5.0, 8.0),
    ("mp4", 10, 7.0, 9.5),
    ("mkv", 4, 8.0, 9.8),
    ("pdf", 30, 4.0, 7.5),
    ("docx", 20, 4.0, 6.5),
    ("xlsx", 12, 4.0, 6.5),
    ("pptx", 6, 5.0, 7.5),
    ("zip", 12, 4.0, 9.0),
    ("7z", 3, 4.0, 9.0),
    ("iso", 1, 8.5, 9.8),
    ("dat", 20, 1.0, 8.0),
    ("ini", 10, 1.5, 4.0),
    ("cfg", 6, 1.5, 4.0),
    ("lnk", 10, 2.5, 3.5),
    ("sys", 6, 4.0, 6.5),
    ("mui", 15, 3.0, 5.5),
    ("ttf", 4, 4.5, 7.0),
    ("db", 4, 4.0, 8.0),
    ("tmp", 4, 0.0, 6.0),
    ("", 20, 0.0, 6.0),
];

/// One generated record.
#[derive(Clone, Debug)]
pub struct Record<'a> {
    /// Full path.
    pub path: &'a str,
    /// Size (files only).
    pub size: Option<u64>,
    /// Date modified.
    pub dm: FileTime,
    /// Date created.
    pub dc: FileTime,
    /// Win32 attributes.
    pub attributes: u32,
}

struct Dir {
    path: String,
    depth: u8,
}

fn word_name(rng: &mut Rng, out: &mut String) {
    out.clear();
    let r = rng.unit();
    if r < 0.10 {
        out.push_str(rng.pick(CJK_WORDS));
        if rng.chance(0.4) {
            out.push_str(&format!("{}", 2015 + rng.below(12)));
        }
    } else if r < 0.12 {
        out.push_str(rng.pick(DIACRITIC_WORDS));
    } else {
        let w = rng.pick(WORDS);
        if rng.chance(0.3) {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                out.extend(f.to_uppercase());
                out.push_str(c.as_str());
            }
        } else {
            out.push_str(w);
        }
        match rng.below(6) {
            0 => {
                out.push(*rng.pick(&['_', '-', ' ']));
                out.push_str(rng.pick(WORDS));
            }
            1 => out.push_str(&format!("{}", rng.below(100))),
            2 => out.push_str(&format!(" v{}.{}", rng.below(5), rng.below(10))),
            _ => {}
        }
    }
}

fn file_stem(rng: &mut Rng, ext: &str, out: &mut String) {
    let r = rng.unit();
    if matches!(ext, "jpg" | "JPG") && r < 0.6 {
        out.clear();
        let prefix = *rng.pick(&["IMG_", "DSC", "DSC_", "PXL_", "IMG_2026"]);
        out.push_str(prefix);
        out.push_str(&format!("{:04}", rng.below(10_000)));
        return;
    }
    if r < 0.06 {
        out.clear();
        out.push_str(&format!("{:016x}", rng.next_u64()));
        return;
    }
    if r < 0.12 {
        out.clear();
        out.push_str(&format!("file{}", rng.below(2000)));
        return;
    }
    word_name(rng, out);
}

fn pick_ext(rng: &mut Rng, total: u32) -> &'static (&'static str, u32, f64, f64) {
    let mut x = rng.below(total as usize) as u32;
    for e in EXTS {
        if x < e.1 {
            return e;
        }
        x -= e.1;
    }
    &EXTS[0]
}

/// Reference "now" for generated dates (2026-10-01T00:00:00Z) so output is
/// deterministic.
pub const REFERENCE_NOW: FileTime = FileTime(134_352_864_000_000_000);

fn path_hash(p: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.to_lowercase().hash(&mut h);
    h.finish()
}

/// Generates `count` entries (folders and files) and calls `sink` for each, parents
/// before children. Deterministic for a given `seed`; no two entries share a path.
pub fn generate(
    count: usize,
    seed: u64,
    mut sink: impl FnMut(&Record<'_>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut rng = Rng::new(seed);
    let now = REFERENCE_NOW.0;
    let ten_years = 10 * 365 * fnc_core::time::TICKS_PER_DAY;
    let date = |rng: &mut Rng| -> (FileTime, FileTime) {
        let dc = now - (rng.unit() * ten_years as f64) as i64;
        let dm = dc + (rng.unit() * (now - dc) as f64) as i64;
        (FileTime(dm), FileTime(dc))
    };
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::with_capacity(count);
    let folder_target = count / 9;
    let mut dirs: Vec<Dir> = Vec::with_capacity(folder_target + 16);
    let mut emitted = 0usize;
    let mut name = String::new();

    // Roots and well-known top-level folders.
    for root in ["C:", "D:"] {
        if emitted >= count {
            return Ok(());
        }
        let (dm, dc) = date(&mut rng);
        sink(&Record { path: root, size: None, dm, dc, attributes: attr::DIRECTORY })?;
        emitted += 1;
        for top in TOP_DIRS.iter().take(if root == "C:" { TOP_DIRS.len() } else { 4 }) {
            let path = format!("{root}\\{top}");
            let (dm, dc) = date(&mut rng);
            sink(&Record { path: &path, size: None, dm, dc, attributes: attr::DIRECTORY })?;
            emitted += 1;
            dirs.push(Dir { path, depth: 1 });
        }
    }

    // Folder tree: attach new folders preferring shallow parents so depth spreads 2..12.
    while dirs.len() < folder_target && emitted < count {
        let parent_idx = loop {
            let i = rng.below(dirs.len());
            let d = dirs[i].depth;
            let accept = match d {
                0..=3 => 1.0,
                4..=6 => 0.7,
                7..=9 => 0.3,
                10 => 0.1,
                _ => 0.0,
            };
            if rng.chance(accept) {
                break i;
            }
        };
        word_name(&mut rng, &mut name);
        let depth = dirs[parent_idx].depth + 1;
        let path = format!("{}\\{}", dirs[parent_idx].path, name);
        if !seen.insert(path_hash(&path)) {
            continue;
        }
        let hidden = rng.chance(0.01);
        let attrs = attr::DIRECTORY | if hidden { attr::HIDDEN } else { 0 };
        let (dm, dc) = date(&mut rng);
        sink(&Record { path: &path, size: None, dm, dc, attributes: attrs })?;
        emitted += 1;
        dirs.push(Dir { path, depth });
    }

    // Files: deeper folders get more files; 5% duplicates of earlier files.
    let total_w: u32 = EXTS.iter().map(|e| e.1).sum();
    let mut recent: Vec<(String, u64)> = Vec::with_capacity(4096);
    let mut path = String::with_capacity(256);
    while emitted < count {
        let dir = loop {
            let i = rng.below(dirs.len());
            if dirs[i].depth >= 2 || rng.chance(0.15) {
                break i;
            }
        };
        let (file_name, size) = if !recent.is_empty() && rng.chance(0.05) {
            recent[rng.below(recent.len())].clone()
        } else {
            let e = pick_ext(&mut rng, total_w);
            file_stem(&mut rng, e.0, &mut name);
            let mut n = name.clone();
            if !e.0.is_empty() {
                n.push('.');
                n.push_str(e.0);
            }
            let size = 10f64.powf(e.2 + rng.unit() * (e.3 - e.2)) as u64;
            (n, size)
        };
        path.clear();
        path.push_str(&dirs[dir].path);
        path.push('\\');
        path.push_str(&file_name);
        if !seen.insert(path_hash(&path)) {
            continue;
        }
        let mut attrs = attr::ARCHIVE;
        if rng.chance(0.01) {
            attrs |= attr::HIDDEN;
        }
        if rng.chance(0.005) {
            attrs |= attr::SYSTEM;
        }
        if rng.chance(0.005) {
            attrs |= attr::READONLY;
        }
        let (dm, dc) = date(&mut rng);
        sink(&Record { path: &path, size: Some(size), dm, dc, attributes: attrs })?;
        emitted += 1;
        if recent.len() < 4096 {
            recent.push((file_name, size));
        } else {
            let k = rng.below(recent.len());
            recent[k] = (file_name, size);
        }
    }
    Ok(())
}

/// Writes `count` entries as EFU to `out`.
pub fn write_efu<W: std::io::Write>(count: usize, seed: u64, out: W) -> std::io::Result<W> {
    let mut w = fnc_index::efu::EfuWriter::new(out)?;
    generate(count, seed, |r| w.write(r.path, r.size, r.dm, r.dc, r.attributes))?;
    w.finish()
}

/// Generates directly into an in-memory index (for benchmarks), via EFU parsing.
pub fn build_index(count: usize, seed: u64) -> std::io::Result<fnc_index::IndexStore> {
    let buf = write_efu(count, seed, Vec::with_capacity(count * 100))?;
    let mut store = fnc_index::IndexStore::default();
    fnc_index::efu::load_efu(&mut store, &buf[..], "fixture.efu").map_err(std::io::Error::other)?;
    if let Some(v) = store.volumes().first().map(|v| v.id) {
        store.set_volume_online(v, true);
    }
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_counted() {
        let a = write_efu(5000, 7, Vec::new()).unwrap();
        let b = write_efu(5000, 7, Vec::new()).unwrap();
        assert!(a == b, "output must be deterministic");
        let text = String::from_utf8(a).unwrap();
        assert_eq!(text.lines().count(), 5001);
        let c = write_efu(5000, 8, Vec::new()).unwrap();
        assert!(text.as_bytes() != &c[..], "seed must matter");
    }

    #[test]
    fn distribution_is_realistic() {
        let s = build_index(50_000, 1).unwrap();
        assert_eq!(s.live_count(), 50_000);
        let ids: Vec<_> = (0..s.len() as u32).map(fnc_core::EntryId).filter(|&i| s.is_live(i)).collect();
        let folders = ids.iter().filter(|&&i| s.is_dir(i)).count();
        assert!((4_000..8_000).contains(&folders), "folders {folders}");
        let cjk = ids
            .iter()
            .filter(|&&i| {
                s.name_string(i)
                    .chars()
                    .any(|c| ('\u{3040}'..='\u{9fff}').contains(&c) || ('\u{ac00}'..='\u{d7af}').contains(&c))
            })
            .count();
        assert!(cjk * 100 / ids.len() >= 5, "cjk {cjk}");
        let diac = ids.iter().filter(|&&i| s.name_string(i).chars().any(|c| "éïßÆøçñöåŁóźèûÅüàã".contains(c))).count();
        assert!(diac * 1000 / ids.len() >= 10, "diacritics {diac}");
        let max_depth = ids.iter().map(|&i| s.depth(i)).max().unwrap();
        assert!((8..=13).contains(&max_depth), "depth {max_depth}");
        let jpg = s.ext_dict().lookup(b"jpg").unwrap();
        let jpgs = ids.iter().filter(|&&i| s.ext_id(i) == jpg).count();
        assert!(jpgs > 1000, "jpg {jpgs}");
    }
}

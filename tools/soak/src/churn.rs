//! The file system side of the churn: the on-disk tree layout, crash-safe recovery
//! (all state is re-derived from the directory listing, so Ctrl+C or a kill at any
//! point is harmless), the three steady-state change kinds and the subtree recycle.
//!
//! Layout below `--root`:
//!
//! * `work\dNNN\fID.dat` — the working set (`--files`, default 20,000) spread over
//!   `--dirs` folders; creates, deletes and cross-folder renames.
//! * `big\bID.dat` — one folder that always has `--big` (10,000) children; renames.
//! * `burst\gGEN\sNNN\xNNNN.dat` — the subtree that is deleted and recreated every
//!   `--subtree-every` (15 min) with `--subtree` (15,000) entries, so dead index
//!   entries accumulate past the live-compaction threshold.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::plan::{Kind, Rng, SubtreeShape, choose_kind};

/// Shared counters (steady loop + subtree thread).
#[derive(Debug, Default)]
pub struct Counters {
    /// Files created by the steady loop.
    pub creates: AtomicU64,
    /// Files renamed by the steady loop.
    pub renames: AtomicU64,
    /// Files deleted by the steady loop.
    pub deletes: AtomicU64,
    /// Failed operations (all kinds).
    pub errors: AtomicU64,
    /// Completed subtree recycles.
    pub cycles: AtomicU64,
    /// Entries deleted by subtree recycles.
    pub subtree_deleted: AtomicU64,
    /// Entries created by subtree recycles.
    pub subtree_created: AtomicU64,
}

impl Counters {
    /// Steady-state operations so far.
    pub fn steady_ops(&self) -> u64 {
        self.creates.load(Ordering::Relaxed)
            + self.renames.load(Ordering::Relaxed)
            + self.deletes.load(Ordering::Relaxed)
    }
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

/// The working set and the 10k-children folder, as currently on disk.
#[derive(Debug)]
pub struct Tree {
    root: PathBuf,
    dirs: usize,
    work: Vec<u64>,
    next_work: u64,
    big: Vec<u64>,
    next_big: u64,
}

/// Parses `<prefix><digits>.dat`.
fn file_id(name: &str, prefix: char) -> Option<u64> {
    name.strip_prefix(prefix)?.strip_suffix(".dat")?.parse().ok()
}

fn list_ids(dir: &Path, prefix: char, out: &mut Vec<u64>) -> io::Result<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        if let Some(id) = e.file_name().to_str().and_then(|n| file_id(n, prefix)) {
            out.push(id);
        }
    }
    Ok(())
}

impl Tree {
    /// Creates the folders if needed and recovers the state from disk.
    pub fn open(root: &Path, dirs: usize) -> io::Result<Self> {
        let dirs = dirs.max(1);
        let mut t =
            Self { root: root.to_path_buf(), dirs, work: Vec::new(), next_work: 0, big: Vec::new(), next_big: 0 };
        fs::create_dir_all(t.root.join("big"))?;
        fs::create_dir_all(t.root.join("burst"))?;
        for d in 0..dirs {
            fs::create_dir_all(t.dir(d))?;
        }
        // Files of an earlier run with another `--dirs` are moved to where this run
        // expects them (listed first, so a moved file is not seen twice).
        let mut found = Vec::new();
        for e in fs::read_dir(t.root.join("work"))? {
            let p = e?.path();
            if !p.is_dir() {
                continue;
            }
            let mut ids = Vec::new();
            list_ids(&p, 'f', &mut ids)?;
            found.extend(ids.into_iter().map(|id| (p.clone(), id)));
        }
        for (dir, id) in found {
            let want = t.work_path(id);
            if want.parent() != Some(dir.as_path()) {
                fs::rename(dir.join(format!("f{id:010}.dat")), &want)?;
            }
            t.work.push(id);
        }
        list_ids(&t.root.join("big"), 'b', &mut t.big)?;
        t.next_work = t.work.iter().max().map_or(0, |m| m + 1);
        t.next_big = t.big.iter().max().map_or(0, |m| m + 1);
        Ok(t)
    }

    fn dir(&self, d: usize) -> PathBuf {
        self.root.join("work").join(format!("d{d:03}"))
    }

    /// Path of working-set file `id`.
    pub fn work_path(&self, id: u64) -> PathBuf {
        self.dir((id % self.dirs as u64) as usize).join(format!("f{id:010}.dat"))
    }

    /// Path of big-folder child `id`.
    pub fn big_path(&self, id: u64) -> PathBuf {
        self.root.join("big").join(format!("b{id:010}.dat"))
    }

    /// Live working-set files.
    pub fn work_len(&self) -> usize {
        self.work.len()
    }

    /// Children of the big folder.
    pub fn big_len(&self) -> usize {
        self.big.len()
    }

    fn create_file(path: &Path, id: u64) -> io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        writeln!(f, "{id}")
    }

    /// One steady-state change; the kind is chosen to hold `target` files.
    pub fn step(&mut self, target: usize, rng: &mut Rng, c: &Counters) -> Kind {
        let kind = choose_kind(target, self.work.len(), rng.unit());
        let ok = match kind {
            Kind::Create => self.create(),
            Kind::Delete => self.delete(rng),
            Kind::Rename => self.rename(rng),
        };
        match (ok, kind) {
            (false, _) => bump(&c.errors),
            (true, Kind::Create) => bump(&c.creates),
            (true, Kind::Delete) => bump(&c.deletes),
            (true, Kind::Rename) => bump(&c.renames),
        }
        kind
    }

    fn create(&mut self) -> bool {
        let id = self.next_work;
        self.next_work += 1;
        match Self::create_file(&self.work_path(id), id) {
            Ok(()) => {
                self.work.push(id);
                true
            }
            Err(_) => false,
        }
    }

    fn delete(&mut self, rng: &mut Rng) -> bool {
        if self.work.is_empty() {
            return false;
        }
        let id = self.work.swap_remove(rng.below(self.work.len()));
        fs::remove_file(self.work_path(id)).is_ok()
    }

    /// A quarter of the renames go to the big folder (it keeps its child count), the
    /// rest move a working-set file to a new name, usually in another folder.
    fn rename(&mut self, rng: &mut Rng) -> bool {
        if !self.big.is_empty() && (self.work.is_empty() || rng.below(4) == 0) {
            let i = rng.below(self.big.len());
            let (old, new) = (self.big[i], self.next_big);
            self.next_big += 1;
            return match fs::rename(self.big_path(old), self.big_path(new)) {
                Ok(()) => {
                    self.big[i] = new;
                    true
                }
                Err(_) => {
                    if !self.big_path(old).exists() {
                        self.big.swap_remove(i);
                    }
                    false
                }
            };
        }
        if self.work.is_empty() {
            return false;
        }
        let i = rng.below(self.work.len());
        let (old, new) = (self.work[i], self.next_work);
        self.next_work += 1;
        match fs::rename(self.work_path(old), self.work_path(new)) {
            Ok(()) => {
                self.work[i] = new;
                true
            }
            Err(_) => {
                if !self.work_path(old).exists() {
                    self.work.swap_remove(i);
                }
                false
            }
        }
    }

    /// Brings the big folder to exactly `n` children and the working set to `target`
    /// files (unthrottled). Returns the number of changes made.
    pub fn prefill(&mut self, n_big: usize, target: usize, stop: &AtomicBool) -> io::Result<u64> {
        let mut made = 0;
        while self.big.len() < n_big && !stop.load(Ordering::Relaxed) {
            let id = self.next_big;
            self.next_big += 1;
            Self::create_file(&self.big_path(id), id)?;
            self.big.push(id);
            made += 1;
        }
        while self.big.len() > n_big {
            if let Some(id) = self.big.pop() {
                fs::remove_file(self.big_path(id))?;
                made += 1;
            }
        }
        while self.work.len() < target && !stop.load(Ordering::Relaxed) {
            let id = self.next_work;
            self.next_work += 1;
            Self::create_file(&self.work_path(id), id)?;
            self.work.push(id);
            made += 1;
        }
        Ok(made)
    }
}

/// Deletes `dir` recursively, returning the number of entries removed (including
/// `dir`). Missing entries are skipped (an earlier run may have been interrupted).
pub fn remove_tree(dir: &Path) -> io::Result<u64> {
    let mut n = 0;
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for e in entries {
        let e = e?;
        if e.file_type()?.is_dir() {
            n += remove_tree(&e.path())?;
        } else {
            match fs::remove_file(e.path()) {
                Ok(()) => n += 1,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
    }
    fs::remove_dir(dir)?;
    Ok(n + 1)
}

/// Existing subtree generations (`burst\gN`), sorted by generation.
pub fn generations(root: &Path) -> io::Result<Vec<u64>> {
    let mut g = Vec::new();
    let burst = root.join("burst");
    if !burst.is_dir() {
        return Ok(g);
    }
    for e in fs::read_dir(burst)? {
        let e = e?;
        if e.file_type()?.is_dir()
            && let Some(n) = e.file_name().to_str().and_then(|s| s.strip_prefix('g')).and_then(|s| s.parse().ok())
        {
            g.push(n);
        }
    }
    g.sort_unstable();
    Ok(g)
}

/// Creates generation `generation` of the subtree; stops early (returning what was
/// made) when `stop` is set.
pub fn create_subtree(root: &Path, generation: u64, shape: SubtreeShape, stop: &AtomicBool) -> io::Result<u64> {
    let top = root.join("burst").join(format!("g{generation:06}"));
    fs::create_dir_all(&top)?;
    let mut n = 1;
    for d in 0..shape.dirs {
        let dir = top.join(format!("s{d:03}"));
        fs::create_dir(&dir)?;
        n += 1;
        for f in 0..shape.files_per_dir {
            if stop.load(Ordering::Relaxed) {
                return Ok(n);
            }
            Tree::create_file(&dir.join(format!("x{f:04}.dat")), f as u64)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Deletes every existing generation, then creates the next one. Returns
/// `(deleted, created)` entries.
pub fn recycle_subtree(root: &Path, shape: SubtreeShape, stop: &AtomicBool) -> io::Result<(u64, u64)> {
    let gens = generations(root)?;
    let mut deleted = 0;
    for g in &gens {
        deleted += remove_tree(&root.join("burst").join(format!("g{g:06}")))?;
    }
    let next = gens.last().map_or(0, |g| g + 1);
    let created = create_subtree(root, next, shape, stop)?;
    Ok((deleted, created))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_files(dir: &Path) -> usize {
        walk(dir).0
    }

    /// (files, folders) below `dir`.
    fn walk(dir: &Path) -> (usize, usize) {
        let (mut f, mut d) = (0, 0);
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                let (a, b) = walk(&p);
                f += a;
                d += b + 1;
            } else {
                f += 1;
            }
        }
        (f, d)
    }

    #[test]
    fn prefill_steady_state_and_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let stop = AtomicBool::new(false);
        let mut t = Tree::open(root, 8).unwrap();
        assert_eq!(t.prefill(300, 500, &stop).unwrap(), 800);
        assert_eq!(count_files(&root.join("big")), 300);
        assert_eq!(count_files(&root.join("work")), 500);

        let c = Counters::default();
        let mut rng = Rng::new(3);
        for _ in 0..3_000 {
            t.step(500, &mut rng, &c);
        }
        assert_eq!(c.errors.load(Ordering::Relaxed), 0);
        assert_eq!(c.steady_ops(), 3_000);
        for k in [&c.creates, &c.renames, &c.deletes] {
            assert!(k.load(Ordering::Relaxed) > 700, "{c:?}");
        }
        assert_eq!(count_files(&root.join("big")), 300, "big folder keeps its child count");
        let live = count_files(&root.join("work"));
        assert_eq!(live, t.work_len());
        assert!(live.abs_diff(500) < 50, "{live}");

        // A new process (after Ctrl+C) sees the same state and never reuses names.
        let mut again = Tree::open(root, 8).unwrap();
        assert_eq!((again.work_len(), again.big_len()), (t.work_len(), 300));
        assert_eq!(again.prefill(300, 500, &stop).unwrap() as usize, 500 - live.min(500));
        for _ in 0..500 {
            again.step(500, &mut rng, &c);
        }
        assert_eq!(c.errors.load(Ordering::Relaxed), 0);
        assert_eq!(count_files(&root.join("work")), again.work_len());

        // Another folder count: files are moved to their new folders.
        let mut moved = Tree::open(root, 3).unwrap();
        assert_eq!(moved.work_len(), again.work_len());
        for _ in 0..300 {
            moved.step(500, &mut rng, &c);
        }
        assert_eq!(c.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn steady_ops_heal_after_external_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        let mut t = Tree::open(tmp.path(), 2).unwrap();
        t.prefill(10, 50, &stop).unwrap();
        fs::remove_dir_all(tmp.path().join("big")).unwrap();
        fs::create_dir(tmp.path().join("big")).unwrap();
        let c = Counters::default();
        let mut rng = Rng::new(9);
        for _ in 0..400 {
            t.step(50, &mut rng, &c);
        }
        assert_eq!(t.big_len(), 0, "vanished children are dropped");
        assert!(c.errors.load(Ordering::Relaxed) <= 10);
    }

    #[test]
    fn subtree_recycle_replaces_the_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let stop = AtomicBool::new(false);
        let shape = SubtreeShape { dirs: 3, files_per_dir: 7 };
        assert_eq!(recycle_subtree(root, shape, &stop).unwrap(), (0, 25));
        assert_eq!(generations(root).unwrap(), [0]);
        // An interrupted earlier run left a partial generation too.
        create_subtree(root, 5, SubtreeShape { dirs: 1, files_per_dir: 2 }, &stop).unwrap();
        assert_eq!(recycle_subtree(root, shape, &stop).unwrap(), (25 + 4, 25));
        assert_eq!(generations(root).unwrap(), [6]);
        assert_eq!(walk(&root.join("burst")), (21, 4));
        stop.store(true, Ordering::Relaxed);
        let (_, made) = recycle_subtree(root, shape, &stop).unwrap();
        assert!(made < 25, "stops early");
        assert_eq!(remove_tree(&root.join("missing")).unwrap(), 0);
    }
}

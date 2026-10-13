//! Exclusion rules (SPEC ch.1 §7): hidden/system entries, excluded folders (full paths
//! or name wildcards), excluded files (name or path wildcards) and `.gitignore` files
//! below configured roots. One matcher serves the folder walker, the NTFS bulk load
//! and incremental changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fnc_core::flags::attr;
use fnc_core::settings::Settings;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use parking_lot::RwLock;

/// Compiled exclusion rules.
pub struct Exclusions {
    hidden: bool,
    system: bool,
    folder_names: GlobSet,
    folder_paths: GlobSet,
    file_names: GlobSet,
    file_paths: GlobSet,
    git_roots: Vec<PathBuf>,
    /// `.gitignore` per folder (`None` = folder has none).
    git_cache: RwLock<HashMap<PathBuf, Option<Arc<Gitignore>>>>,
    fingerprint: String,
}

impl std::fmt::Debug for Exclusions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fingerprint)
    }
}

fn glob(pattern: &str) -> Option<globset::Glob> {
    GlobBuilder::new(pattern).case_insensitive(true).literal_separator(false).build().ok()
}

fn is_path_pattern(p: &str) -> bool {
    p.contains(['\\', '/']) || p.as_bytes().get(1) == Some(&b':')
}

/// Normalises separators so `C:\a\*` also matches `C:/a/b` and vice versa.
fn norm(s: &str) -> String {
    s.replace('\\', "/").trim_end_matches('/').to_string()
}

impl Exclusions {
    /// Rules from settings; `None` if nothing is excluded.
    pub fn from_settings(s: &Settings) -> Option<Arc<Self>> {
        let mut folder_names = GlobSetBuilder::new();
        let mut folder_paths = GlobSetBuilder::new();
        let mut file_names = GlobSetBuilder::new();
        let mut file_paths = GlobSetBuilder::new();
        let mut any = s.exclude_hidden || s.exclude_system;
        for f in s.exclude_folders.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
            any = true;
            if is_path_pattern(f) {
                let n = norm(f);
                // A folder path excludes the folder itself (its content is never walked).
                if let Some(g) = glob(&n) {
                    folder_paths.add(g);
                }
            } else if let Some(g) = glob(f) {
                folder_names.add(g);
            }
        }
        for f in s.exclude_files.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
            any = true;
            let (set, pat) =
                if is_path_pattern(f) { (&mut file_paths, norm(f)) } else { (&mut file_names, f.to_string()) };
            if let Some(g) = glob(&pat) {
                set.add(g);
            }
        }
        let git_roots: Vec<PathBuf> =
            s.exclude_gitignore_roots.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).map(PathBuf::from).collect();
        any |= !git_roots.is_empty();
        if !any {
            return None;
        }
        let build = |b: GlobSetBuilder| b.build().unwrap_or_else(|_| GlobSet::empty());
        let fingerprint = format!(
            "h={} s={} folders={:?} files={:?} git={:?}",
            s.exclude_hidden, s.exclude_system, s.exclude_folders, s.exclude_files, s.exclude_gitignore_roots
        );
        Some(Arc::new(Self {
            hidden: s.exclude_hidden,
            system: s.exclude_system,
            folder_names: build(folder_names),
            folder_paths: build(folder_paths),
            file_names: build(file_names),
            file_paths: build(file_paths),
            git_roots,
            git_cache: RwLock::new(HashMap::new()),
            fingerprint,
        }))
    }

    /// Text identifying the rules (persisted next to the database).
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Whether the entry is excluded.
    pub fn excluded(&self, path: &Path, name: &str, is_dir: bool, attributes: u32) -> bool {
        if (self.hidden && attributes & attr::HIDDEN != 0) || (self.system && attributes & attr::SYSTEM != 0) {
            // Volume roots carry hidden/system on some file systems; never exclude them.
            return path.parent().is_some();
        }
        let p = norm(&path.to_string_lossy());
        if is_dir {
            if self.folder_names.is_match(name) || self.folder_paths.is_match(&p) {
                return true;
            }
        } else if self.file_names.is_match(name) || self.file_paths.is_match(&p) {
            return true;
        }
        self.gitignored(path, is_dir)
    }

    fn gitignore_of(&self, dir: &Path) -> Option<Arc<Gitignore>> {
        if let Some(g) = self.git_cache.read().get(dir) {
            return g.clone();
        }
        let file = dir.join(".gitignore");
        let g = file.is_file().then(|| {
            let mut b = GitignoreBuilder::new(dir);
            b.add(&file);
            b.build().ok().map(Arc::new)
        });
        let g = g.flatten();
        self.git_cache.write().insert(dir.to_path_buf(), g.clone());
        g
    }

    /// `.gitignore` rules of every folder from the configured root down to the entry's
    /// parent; the deepest file with an opinion wins (`!pattern` re-includes).
    fn gitignored(&self, path: &Path, is_dir: bool) -> bool {
        let Some(root) = self.git_roots.iter().find(|r| path.starts_with(r)) else { return false };
        if path.file_name().is_some_and(|n| n == ".git") {
            return true;
        }
        let mut dirs: Vec<&Path> = path.ancestors().skip(1).take_while(|a| a.starts_with(root)).collect();
        dirs.reverse();
        let mut verdict = false;
        for d in dirs {
            if let Some(g) = self.gitignore_of(d) {
                match g.matched(path, is_dir) {
                    ignore::Match::Ignore(_) => verdict = true,
                    ignore::Match::Whitelist(_) => verdict = false,
                    ignore::Match::None => {}
                }
            }
        }
        verdict
    }

    /// As a closure for `fnc_platform::walk::FolderOptions::exclude`.
    pub fn as_fn(self: &Arc<Self>) -> fnc_platform::walk::ExcludeFn {
        let me = self.clone();
        Arc::new(move |p, n, d, a| me.excluded(p, n, d, a))
    }
}

/// Deletes excluded subtrees of `vol` from `store` (after a bulk load); returns the
/// number of entries removed. Walks top-down, so excluded folders are tested once.
pub fn apply(store: &mut fnc_index::IndexStore, vol: fnc_core::VolumeId, ex: &Exclusions) -> usize {
    let Some(v) = store.volume_info(vol) else { return 0 };
    let mut stack: Vec<fnc_core::EntryId> = v.roots.clone();
    let mut doomed = Vec::new();
    let mut path = String::new();
    while let Some(dir) = stack.pop() {
        let kids: Vec<fnc_core::EntryId> = store.children(dir).filter(|&c| store.is_live(c)).collect();
        for c in kids {
            store.full_path(c, &mut path);
            let name = store.name_string(c);
            let is_dir = store.is_dir(c);
            if ex.excluded(Path::new(&path), &name, is_dir, store.attributes(c)) {
                doomed.push(c);
            } else if is_dir {
                stack.push(c);
            }
        }
    }
    if doomed.is_empty() {
        return 0;
    }
    let batch = doomed.into_iter().map(|id| fnc_index::Change::Delete(fnc_index::EntryRef::Id(id))).collect();
    store.apply_initial(batch).deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(f: impl FnOnce(&mut Settings)) -> Arc<Exclusions> {
        let mut s = Settings::default();
        f(&mut s);
        Exclusions::from_settings(&s).unwrap()
    }

    #[test]
    fn names_paths_and_attributes() {
        assert!(Exclusions::from_settings(&Settings::default()).is_none());
        let ex = rules(|s| {
            s.exclude_folders = vec!["node_modules".into(), r"C:\Windows\WinSxS".into(), "D:/tmp/*".into()];
            s.exclude_files = vec!["*.tmp".into(), "~$*".into()];
            s.exclude_hidden = true;
        });
        let x = |p: &str, d: bool, a: u32| {
            let name = p.rsplit(['\\', '/']).next().unwrap_or(p).to_string();
            ex.excluded(Path::new(p), &name, d, a)
        };
        assert!(x("/src/app/node_modules", true, 0));
        assert!(!x("/src/app/node_modules", false, 0), "a file named like the folder pattern");
        assert!(x(r"C:\Windows\WinSxS", true, 0));
        assert!(x(r"c:\windows\winsxs", true, 0), "case-insensitive");
        assert!(!x(r"C:\Windows\System32", true, 0));
        assert!(x("D:/tmp/build", true, 0));
        assert!(x("/a/report.TMP", false, 0));
        assert!(x("/a/~$draft.docx", false, 0));
        assert!(!x("/a/report.txt", false, 0));
        assert!(x("/a/.cache", true, attr::HIDDEN));
        assert!(!x("/", true, attr::HIDDEN | attr::SYSTEM), "roots stay");
        assert!(ex.fingerprint().contains("node_modules"));
    }

    #[test]
    fn gitignore_files_are_honoured() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n!keep.log\n").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/.gitignore"), "local.txt\n").unwrap();
        let ex = rules(|s| s.exclude_gitignore_roots = vec![root.display().to_string()]);
        let x = |rel: &str, d: bool| ex.excluded(&root.join(rel), rel.rsplit('/').next().unwrap(), d, 0);
        assert!(x("target", true));
        assert!(x("a.log", false));
        assert!(!x("keep.log", false), "negation");
        assert!(x("sub/local.txt", false));
        assert!(!x("local.txt", false), "nested rule only applies below its folder");
        assert!(x(".git", true));
        assert!(!x("src", true));
    }

    #[test]
    fn apply_removes_excluded_subtrees_from_an_index() {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\p\\node_modules\\x\\y.js\",1,,,32\n\
                   \"C:\\p\\src\\main.rs\",1,,,32\n\
                   \"C:\\p\\src\\old.tmp\",1,,,32\n";
        let mut store = fnc_index::IndexStore::default();
        let (vol, _) = fnc_index::efu::load_efu(&mut store, efu.as_bytes(), "t.efu").unwrap();
        let ex = rules(|s| {
            s.exclude_folders = vec!["node_modules".into()];
            s.exclude_files = vec!["*.tmp".into()];
        });
        let removed = apply(&mut store, vol, &ex);
        assert_eq!(removed, 4, "node_modules, x, y.js, old.tmp");
        let names: Vec<String> = (0..store.len() as u32)
            .map(fnc_core::EntryId)
            .filter(|&i| store.is_live(i))
            .map(|i| store.name_string(i))
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.iter().any(|n| n == "y.js" || n == "old.tmp"));
    }
}

//! The FTP view of the index: FTP paths ↔ index paths, and directory listings built from
//! the index only (never from the disk), as Everything's ETP server does.
//!
//! | index path | FTP path |
//! |---|---|
//! | `C:\Data\a.txt` | `/C:/Data/a.txt` |
//! | `\\server\share\a.txt` | `/UNC/server/share/a.txt` |
//! | `/home/u/a.txt` | `/home/u/a.txt` |
//!
//! `/` lists the first component of every indexed root (`C:`, `D:`, `UNC`, `home`);
//! folders between `/` and a root that are not themselves indexed are virtual (listed,
//! enterable, no metadata). Paths are normalised (`.`/`..`, repeated separators) before
//! they are resolved, and resolution only follows index entries, so nothing outside the
//! index can be reached.

use fnc_core::{EntryFlags, EntryId, FileTime};
use fnc_index::{IndexStore, VolumeKind};
use fnc_ipc::rows::resolve_path;

/// Joins `arg` to the working directory `cwd` and normalises the result: always absolute,
/// `/` and `\` both separate, `.` and empty parts are dropped, `..` goes up (never above
/// `/`), no trailing separator except for `/` itself.
pub fn normalize(cwd: &str, arg: &str) -> String {
    let arg = arg.trim_end_matches(['\r', '\n']);
    let joined = if arg.starts_with(['/', '\\']) { arg.to_string() } else { format!("{cwd}/{arg}") };
    let mut parts: Vec<&str> = Vec::new();
    for p in joined.split(['/', '\\']) {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// FTP path of an index path.
pub fn ftp_path(index_path: &str) -> String {
    let p = index_path.trim_end_matches(['\\', '/']);
    if let Some(unc) = p.strip_prefix("\\\\") {
        format!("/UNC/{}", unc.replace('\\', "/"))
    } else if p.starts_with('/') {
        p.to_string()
    } else if p.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", p.replace('\\', "/"))
    }
}

/// Index path of a normalised FTP path (`None` for `/`).
pub fn index_path(ftp: &str) -> Option<String> {
    let rel = ftp.strip_prefix('/').unwrap_or(ftp);
    if rel.is_empty() {
        return None;
    }
    let first = rel.split('/').next().unwrap_or("");
    Some(if first.ends_with(':') {
        rel.replace('/', "\\")
    } else if first == "UNC" {
        format!("\\\\{}", rel.strip_prefix("UNC/").unwrap_or("").replace('/', "\\"))
    } else {
        format!("/{rel}")
    })
}

/// What an FTP path names.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// `/` or a folder above the indexed roots (no metadata).
    Virtual,
    /// An index entry.
    Entry(EntryId),
}

/// One listed child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Name.
    pub name: String,
    /// Folder.
    pub is_dir: bool,
    /// Size of a file.
    pub size: Option<u64>,
    /// Date modified.
    pub modified: FileTime,
    /// The entry (`None` for virtual folders).
    pub id: Option<EntryId>,
}

fn components(ftp: &str) -> Vec<&str> {
    ftp.split('/').filter(|p| !p.is_empty()).collect()
}

/// FTP paths of every live root of the index.
fn root_paths(s: &IndexStore) -> Vec<(String, EntryId)> {
    let mut buf = String::new();
    let mut out = Vec::new();
    for v in s.volumes().iter().filter(|v| !v.is_retired()) {
        for &r in &v.roots {
            if s.is_live(r) {
                s.full_path(r, &mut buf);
                out.push((ftp_path(&buf), r));
            }
        }
    }
    out
}

fn same(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// Resolves a normalised FTP path.
pub fn resolve(s: &IndexStore, ftp: &str) -> Option<Node> {
    let Some(ip) = index_path(ftp) else { return Some(Node::Virtual) };
    if let Some(id) = resolve_path(s, &ip) {
        return Some(Node::Entry(id));
    }
    let want = components(ftp);
    let prefix_of_root = root_paths(s).iter().any(|(rp, _)| {
        let rc = components(rp);
        rc.len() > want.len() && rc.iter().zip(&want).all(|(a, b)| same(a, b))
    });
    prefix_of_root.then_some(Node::Virtual)
}

fn item(s: &IndexStore, id: EntryId, name: String) -> Item {
    let is_dir = s.is_dir(id);
    Item { name, is_dir, size: if is_dir { None } else { s.size(id) }, modified: s.date_modified(id), id: Some(id) }
}

/// Children of a folder (`ftp` must be normalised). `None` if it is not a folder.
pub fn list(s: &IndexStore, ftp: &str) -> Option<Vec<Item>> {
    let mut out = Vec::new();
    match resolve(s, ftp)? {
        Node::Entry(id) if !s.is_dir(id) => return None,
        Node::Entry(id) => {
            for c in s.children(id).filter(|&c| s.is_live(c)) {
                out.push(item(s, c, s.name_string(c)));
            }
        }
        Node::Virtual => {}
    }
    // Roots (or folders leading to them) directly below this path.
    let want = components(ftp);
    for (rp, root) in root_paths(s) {
        let rc = components(&rp);
        if rc.len() <= want.len() || !rc.iter().zip(&want).all(|(a, b)| same(a, b)) {
            continue;
        }
        let name = rc[want.len()].to_string();
        if out.iter().any(|i: &Item| same(&i.name, &name)) {
            continue;
        }
        out.push(if rc.len() == want.len() + 1 {
            item(s, root, name)
        } else {
            Item { name, is_dir: true, size: None, modified: FileTime::UNKNOWN, id: None }
        });
    }
    Some(out)
}

/// The index path of a file that may be downloaded: a live file (not a folder) on an
/// online volume that is not a file list. The returned path comes from the index, never
/// from the client's text.
pub fn downloadable(s: &IndexStore, ftp: &str) -> Option<String> {
    let Node::Entry(id) = resolve(s, ftp)? else { return None };
    if s.is_dir(id) || s.flags(id).contains(EntryFlags::ROOT) {
        return None;
    }
    let vol = s.volume_info(s.volume_of(id))?;
    if vol.info.kind == VolumeKind::FileList || !vol.online {
        return None;
    }
    let mut full = String::new();
    s.full_path(id, &mut full);
    Some(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> IndexStore {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\Data\",,,,16\n\
                   \"C:\\Data\\a.txt\",5,133000000000000000,,32\n\
                   \"C:\\Data\\sub\\b.txt\",7,,,32\n\
                   \"D:\\Projects\\x\\c.rs\",1,,,32\n\
                   \"\\\\srv\\share\\d.doc\",2,,,32\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s.rebuild_name_order();
        s
    }

    #[test]
    fn normalises_paths() {
        assert_eq!(normalize("/", "C:"), "/C:");
        assert_eq!(normalize("/C:/Data", "sub/../a.txt"), "/C:/Data/a.txt");
        assert_eq!(normalize("/C:/Data", "/../../.."), "/");
        assert_eq!(normalize("/C:", "Data\\sub//"), "/C:/Data/sub");
        assert_eq!(normalize("/x", ""), "/x");
    }

    #[test]
    fn maps_paths_both_ways() {
        for (i, f) in [
            ("C:\\Data\\a.txt", "/C:/Data/a.txt"),
            ("C:", "/C:"),
            ("\\\\srv\\share\\d.doc", "/UNC/srv/share/d.doc"),
            ("/home/u/x", "/home/u/x"),
        ] {
            assert_eq!(ftp_path(i), f);
            assert_eq!(index_path(f).as_deref(), Some(i));
        }
        assert_eq!(ftp_path("C:\\"), "/C:");
        assert_eq!(index_path("/"), None);
    }

    #[test]
    fn lists_from_the_index() {
        let s = store();
        let names = |p: &str| {
            let mut v: Vec<String> = list(&s, p).unwrap().into_iter().map(|i| i.name).collect();
            v.sort();
            v
        };
        assert_eq!(names("/"), ["C:", "D:", "UNC"]);
        assert_eq!(names("/C:"), ["Data"]);
        assert_eq!(names("/c:/data"), ["a.txt", "sub"]);
        assert_eq!(names("/UNC"), ["srv"]);
        assert_eq!(names("/UNC/srv/share"), ["d.doc"]);
        let a = list(&s, "/C:/Data").unwrap().into_iter().find(|i| i.name == "a.txt").unwrap();
        assert_eq!((a.is_dir, a.size, a.modified), (false, Some(5), FileTime(133000000000000000)));
        assert!(list(&s, "/C:/Data/a.txt").is_none(), "a file is not a folder");
        assert!(list(&s, "/E:").is_none());
        assert!(list(&s, "/C:/Windows").is_none(), "not in the index");
        assert_eq!(resolve(&s, "/"), Some(Node::Virtual));
        assert!(matches!(resolve(&s, "/C:/Data/sub/b.txt"), Some(Node::Entry(_))));
        // File lists are never downloaded.
        assert_eq!(downloadable(&s, "/C:/Data/a.txt"), None);
    }
}

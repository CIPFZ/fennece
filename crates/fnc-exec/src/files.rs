//! Functions that read small parts of files and have no index (SPEC ch.2 §5.1, `fs`):
//! `target:` (link targets), `fsi:` (file signatures) and `recycled:<text>` (original
//! paths of recycle bin items), plus the index-only recycle bin membership of
//! `recycled:`.
//!
//! Each runs through the slow-path state of the query ([`SlowState`]: on-demand source,
//! file budget, cancellation, cross-query cache), and is decided from the index first:
//! entries that cannot match (no link, a folder for `fsi:`, outside the recycle bin) cost
//! a flag or bit test and never touch the disk.

use std::path::Path;
use std::sync::Arc;

use fnc_core::{EntryFlags, EntryId};
use fnc_index::IndexStore;
use fnc_index::ext::{OVERFLOW_EXT, ext_of};
use fnc_props::link::LinkKind;
use fnc_props::ondemand::OnDemand;
use fnc_props::{recycle, signature};

use crate::eval::{Bits, Compiled, Scratch, TextLeaf};
use crate::ext::{FileText, SlowState};

/// Compiled `parent+N:` (N ≥ 1): the folder `up` levels above the entry's parent is in
/// `set` (folders whose full path matches).
pub struct ParentUpLeaf {
    pub(crate) set: Bits,
    pub(crate) up: u8,
}

impl ParentUpLeaf {
    /// Evaluates entry `id` (at most `up + 1` parent steps).
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId) -> bool {
        let mut p = s.parent(id);
        for _ in 0..self.up {
            if !p.is_some() {
                return false;
            }
            p = s.parent(p);
        }
        p.is_some() && self.set.get(p.idx())
    }
}

/// Compiled `target:` leaf.
pub struct TargetLeaf {
    /// Pattern (`None`: any readable target).
    pub(crate) text: Option<Box<TextLeaf>>,
    /// Match the target's last component only (wildcards without a separator).
    pub(crate) name_only: bool,
    /// Extension ids of `lnk` / `url` (absent from the dictionary = no such files).
    pub(crate) lnk: Option<u16>,
    pub(crate) url: Option<u16>,
    pub(crate) slow: Arc<SlowState>,
}

/// Last path (or URL) component of a target (`C:\a\b.exe` → `b.exe`).
fn last_component(t: &str) -> &str {
    let t = t.trim_end_matches(['\\', '/']);
    t.rsplit(['\\', '/']).next().unwrap_or(t)
}

/// Link kind of `id` from the index alone (`None`: not a link, never read): `.lnk` and
/// `.url` files and reparse points that are not cloud or dedup placeholders.
pub fn link_kind(s: &IndexStore, id: EntryId) -> Option<LinkKind> {
    if !s.is_dir(id)
        && let Some(k) = std::str::from_utf8(ext_of(s.fold(id))).ok().and_then(LinkKind::of_ext)
    {
        return Some(k);
    }
    (s.flags(id).contains(EntryFlags::REPARSE) && fnc_core::flags::attr::is_link_reparse(s.attributes(id)))
        .then_some(LinkKind::Reparse)
}

/// The recycle bin item `id` belongs to: the top-level `$R…` entry in
/// `<root>\$Recycle.Bin\<SID>\` that is `id` or one of its ancestors (index only).
pub fn recycle_item(s: &IndexStore, id: EntryId) -> Option<EntryId> {
    let mut x = id;
    for _ in 0..fnc_index::store::MAX_DEPTH {
        if !x.is_some() {
            return None;
        }
        let sid = s.parent(x);
        if recycle::is_data_name(s.fold(x)) && sid.is_some() {
            let bin = s.parent(sid);
            if bin.is_some()
                && s.fold(bin) == recycle::BIN_FOLDER_FOLD.as_bytes()
                && s.flags(s.parent(bin)).contains(EntryFlags::ROOT)
            {
                return Some(x);
            }
        }
        x = sid;
    }
    None
}

/// Original path of the recycle bin item whose `$R…` data is at `item` (from its `$I`
/// record).
pub fn recycle_origin(src: &dyn OnDemand, item: &Path) -> Option<String> {
    let data = src.head(Path::new(&info_path(item.to_str()?)?), recycle::MAX_INFO_BYTES)?;
    recycle::parse_info(&data).map(|i| i.original_path)
}

/// File signature of `path` (`fsi:` names; `Some("")` = unknown, `None` = unreadable).
pub fn file_signature(src: &dyn OnDemand, path: &Path) -> Option<String> {
    read_signature(src, path)
}

impl TargetLeaf {
    /// Link kind of `id` from the index alone (`None`: not a link, never read).
    fn kind(&self, s: &IndexStore, id: EntryId) -> Option<LinkKind> {
        if !s.is_dir(id) {
            let e = s.ext_id(id);
            if self.lnk == Some(e) {
                return Some(LinkKind::Shortcut);
            }
            if self.url == Some(e) {
                return Some(LinkKind::Internet);
            }
            if e == OVERFLOW_EXT
                && let Some(k) = std::str::from_utf8(ext_of(s.fold(id))).ok().and_then(LinkKind::of_ext)
            {
                return Some(k);
            }
        }
        (s.flags(id).contains(EntryFlags::REPARSE) && fnc_core::flags::attr::is_link_reparse(s.attributes(id)))
            .then_some(LinkKind::Reparse)
    }

    /// Evaluates entry `id`.
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        let Some(kind) = self.kind(s, id) else { return false };
        let Some(target) = self.slow.file_text(s, id, FileText::LinkTarget, sc, |src, p| src.link_target(p, kind))
        else {
            return false;
        };
        match &self.text {
            None => true,
            Some(l) => Compiled::match_str(l, if self.name_only { last_component(&target) } else { &target }, sc),
        }
    }
}

/// Compiled `fsi:` leaf.
pub struct SignatureLeaf {
    /// Accepted canonical names (empty: the signature must be unknown).
    pub(crate) names: Vec<&'static str>,
    pub(crate) slow: Arc<SlowState>,
}

/// Signature of the file at `path`: the first [`signature::HEAD_BYTES`] bytes, plus the
/// 5-byte ISO descriptor probe when those are all zeros. `Some("")` = unknown, `None` =
/// unreadable.
fn read_signature(src: &dyn OnDemand, path: &Path) -> Option<String> {
    let head = src.head(path, signature::HEAD_BYTES)?;
    let found = signature::detect(&head).or_else(|| {
        (signature::wants_iso_probe(&head)
            && src.read_at(path, signature::ISO_PROBE_OFFSET, 5).is_some_and(|p| signature::detect_iso(&p)))
        .then_some("iso")
    });
    Some(found.unwrap_or("").to_string())
}

impl SignatureLeaf {
    /// Evaluates entry `id`.
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        if s.is_dir(id) {
            return false;
        }
        let Some(sig) = self.slow.file_text(s, id, FileText::Signature, sc, read_signature) else { return false };
        if self.names.is_empty() { sig.is_empty() } else { self.names.iter().any(|n| **n == *sig) }
    }
}

/// Deleted items of the index's recycle bins.
pub(crate) struct RecycleSets {
    /// The `$R…` entries directly in `<root>\$Recycle.Bin\<SID>\`.
    pub(crate) top: Bits,
    /// Those and everything below them.
    pub(crate) all: Bits,
}

/// Finds the recycle bin items of every volume root (index only).
pub(crate) fn recycle_sets(s: &IndexStore) -> RecycleSets {
    let n = s.len();
    let (mut top, mut all) = (Bits::new(n), Bits::new(n));
    for v in s.volumes() {
        for &root in &v.roots {
            let Some(bin) = s.find_child(root, recycle::BIN_FOLDER_FOLD.as_bytes()).filter(|&b| s.is_dir(b)) else {
                continue;
            };
            for sid in s.children(bin).filter(|&d| s.is_dir(d)) {
                for item in s.children(sid).filter(|&i| recycle::is_data_name(s.fold(i))) {
                    top.set(item.idx());
                    s.for_each_descendant(item, |d| all.set(d.idx()));
                }
            }
        }
    }
    RecycleSets { top, all }
}

/// Path of the `$I` record next to the `$R` item at `path` (either separator).
fn info_path(path: &str) -> Option<String> {
    let cut = path.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let name = recycle::info_name(&path[cut..])?;
    Some(format!("{}{name}", &path[..cut]))
}

/// Compiled `recycled:<text>` leaf.
pub struct RecycledLeaf {
    pub(crate) sets: Arc<RecycleSets>,
    pub(crate) text: Box<TextLeaf>,
    pub(crate) slow: Arc<SlowState>,
}

impl RecycledLeaf {
    /// Original path of the top-level item `top` (per query and across queries).
    fn origin(&self, s: &IndexStore, top: EntryId, sc: &mut Scratch) -> Option<Arc<str>> {
        if let Some(v) = self.slow.recycled.lock().get(&top.0) {
            return v.clone();
        }
        let v = self.slow.file_text(s, top, FileText::RecycleOrigin, sc, recycle_origin);
        self.slow.recycled.lock().insert(top.0, v.clone());
        v
    }

    /// Evaluates entry `id`.
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        if !self.sets.all.get(id.idx()) {
            return false;
        }
        let mut top = id;
        let mut steps = 0;
        while top.is_some() && !self.sets.top.get(top.idx()) && steps < fnc_index::store::MAX_DEPTH {
            top = s.parent(top);
            steps += 1;
        }
        if !top.is_some() || !self.sets.top.get(top.idx()) {
            return false;
        }
        let Some(origin) = self.origin(s, top, sc) else { return false };
        if top == id {
            return Compiled::match_str(&self.text, &origin, sc);
        }
        // Below a deleted folder: its original path plus the path inside it.
        let mut below = String::new();
        s.full_path(top, &mut sc.t);
        s.full_path(id, &mut below);
        let suffix = below.get(sc.t.len()..).unwrap_or("");
        let mut path = String::with_capacity(origin.len() + suffix.len());
        path.push_str(origin.trim_end_matches(['\\', '/']));
        path.push_str(suffix);
        Compiled::match_str(&self.text, &path, sc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components() {
        assert_eq!(last_component("C:\\a\\b.exe"), "b.exe");
        assert_eq!(last_component("C:\\a\\dir\\"), "dir");
        assert_eq!(last_component("https://x.com/a.html"), "a.html");
        assert_eq!(last_component("x"), "x");
        assert_eq!(
            info_path("C:\\$Recycle.Bin\\S-1\\$RAB12.txt").as_deref(),
            Some("C:\\$Recycle.Bin\\S-1\\$IAB12.txt")
        );
        assert_eq!(info_path("/r/$Recycle.Bin/S-1/$RAB12").as_deref(), Some("/r/$Recycle.Bin/S-1/$IAB12"));
        assert_eq!(info_path("/r/x.txt"), None);
    }
}

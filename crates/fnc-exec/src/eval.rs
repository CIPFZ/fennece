//! Plan compilation and per-entry evaluation.

use std::sync::Arc;

use fnc_core::{EntryFlags, EntryId, FileTime};
use fnc_index::ext::{OVERFLOW_EXT, ext_of, stem_of};
use fnc_index::{IndexStore, SIZE_UNKNOWN};
use fnc_query::plan::{
    ATTRIB_UNIVERSE, DateCol, KindClass, Node, NumPred, NumProp, PatternKind, Pred, ReparseKind, Target, TextPred,
};
use rayon::prelude::*;

use crate::cache::{Caches, CancelToken, Cancelled, ChildCounts};
use crate::matcher::{Pattern, SubjectMode, compile_pattern, name_subject, strip_chars, subject_mode};
use crate::paths::DirPaths;

/// Fixed-size bit set over rows or ids.
#[derive(Clone, Debug)]
pub struct Bits(Vec<u64>);

impl Bits {
    /// All clear, `n` bits.
    pub fn new(n: usize) -> Self {
        Bits(vec![0; n.div_ceil(64)])
    }

    /// Sets bit `i`.
    #[inline]
    pub fn set(&mut self, i: usize) {
        if let Some(w) = self.0.get_mut(i / 64) {
            *w |= 1 << (i % 64);
        }
    }

    /// Tests bit `i`.
    #[inline]
    pub fn get(&self, i: usize) -> bool {
        self.0.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
    }
}

/// Compiled text leaf.
pub struct TextLeaf {
    /// Matcher.
    pub pattern: Pattern,
    /// Source predicate.
    pub pred: TextPred,
    mode: SubjectMode,
    /// For substring path matches: folders whose own path already contains the needle.
    dir_hit: Option<Bits>,
    needle_len: usize,
}

/// Compiled leaf.
pub enum Leaf {
    /// Text.
    Text(Box<TextLeaf>),
    /// Extension ids (+ overflow names) and "no extension".
    Ext {
        /// Matching extension ids.
        ids: Bits,
        /// Names for entries whose id overflowed the dictionary.
        overflow: Vec<Vec<u8>>,
        /// Match files without extension.
        none: bool,
    },
    /// Folder / file.
    IsDir(bool),
    /// Numeric property.
    Num {
        /// Property.
        prop: NumProp,
        /// Ranges.
        pred: NumPred,
        /// Child counts (for child-count properties).
        counts: Option<Arc<ChildCounts>>,
    },
    /// Attributes.
    Attrib {
        /// Required bits.
        mask: u32,
        /// Exact comparison.
        exact: bool,
    },
    /// Roots.
    Root,
    /// Empty folders / zero-byte files.
    Empty(Arc<ChildCounts>),
    /// Parent folder in set.
    ParentIn(Bits),
    /// Some ancestor in set.
    AncestorIn(Bits),
    /// The entry itself or an ancestor in set.
    SelfOrAncestorIn(Bits),
    /// Row is in set (child:).
    In(Bits),
    /// Volume in set.
    VolumeIn(Vec<bool>),
    /// Reparse points.
    Reparse(ReparseKind),
    /// Never matches.
    Nothing,
}

/// Compiled node.
pub enum CNode {
    /// Everything.
    All,
    /// Nothing.
    Nothing,
    /// Conjunction.
    And(Vec<CNode>),
    /// Disjunction.
    Or(Vec<CNode>),
    /// Negation.
    Not(Box<CNode>),
    /// Leaf.
    Leaf(Leaf),
}

/// Per-thread scratch buffers.
#[derive(Default)]
pub struct Scratch {
    a: Vec<u8>,
    b: Vec<u8>,
    s: String,
    t: String,
}

/// A compiled query ready for evaluation against one snapshot.
pub struct Compiled {
    /// Root node.
    pub root: CNode,
    dir_paths: Option<Arc<DirPaths>>,
    /// Invalid patterns (rendered as diagnostics).
    pub errors: Vec<String>,
}

fn needs_paths(n: &Node) -> bool {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().any(needs_paths),
        Node::Not(c) => needs_paths(c),
        Node::Leaf(Pred::Text(t)) => matches!(t.target, Target::FullPath | Target::PathPart),
        Node::Leaf(Pred::Parent(_) | Pred::Ancestor(_)) => true,
        Node::Leaf(Pred::Num(p, _)) => matches!(p, NumProp::PathLength | NumProp::FullPathLength),
        _ => false,
    }
}

impl Compiled {
    /// Compiles `root` for snapshot `s`, precomputing folder sets and caches.
    pub fn new(root: &Node, s: &IndexStore, caches: &Caches, cancel: &CancelToken) -> Result<Self, Cancelled> {
        let dir_paths = needs_paths(root).then(|| caches.dir_paths(s));
        let mut c = Compiled { root: CNode::All, dir_paths, errors: Vec::new() };
        c.root = c.compile(root, s, caches, cancel)?;
        Ok(c)
    }

    fn text_leaf(&mut self, t: &TextPred, s: &IndexStore) -> Option<TextLeaf> {
        let pattern = match compile_pattern(t) {
            Ok(p) => p,
            Err(e) => {
                self.errors.push(e.to_string());
                return None;
            }
        };
        let mode = subject_mode(t);
        let mut leaf = TextLeaf { pattern, pred: t.clone(), mode, dir_hit: None, needle_len: 0 };
        if t.target == Target::FullPath
            && mode == SubjectMode::StoredFold
            && t.kind == PatternKind::Substring
            && !t.ignore_punct
            && !t.ignore_ws
            && let (Pattern::Contains(f), Some(dp)) = (&leaf.pattern, &self.dir_paths)
        {
            let mut hit = Bits::new(s.len());
            for &d in dp.folders() {
                if dp.get(d).is_some_and(|p| f.find(p).is_some()) {
                    hit.set(d.idx());
                }
            }
            leaf.needle_len = f.needle().len();
            leaf.dir_hit = Some(hit);
        }
        Some(leaf)
    }

    /// Rows (folders) whose full path matches `t`.
    fn folder_set(&mut self, t: &TextPred, s: &IndexStore) -> Bits {
        let mut set = Bits::new(s.len());
        let Some(leaf) = self.text_leaf(t, s) else { return set };
        let Some(dp) = self.dir_paths.clone() else { return set };
        let mut sc = Scratch::default();
        for &d in dp.folders() {
            if self.eval_text(&leaf, s, d, &mut sc) {
                set.set(d.idx());
            }
        }
        set
    }

    fn compile(&mut self, n: &Node, s: &IndexStore, caches: &Caches, cancel: &CancelToken) -> Result<CNode, Cancelled> {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        Ok(match n {
            Node::All => CNode::All,
            Node::Nothing => CNode::Nothing,
            Node::And(v) => CNode::And(v.iter().map(|c| self.compile(c, s, caches, cancel)).collect::<Result<_, _>>()?),
            Node::Or(v) => CNode::Or(v.iter().map(|c| self.compile(c, s, caches, cancel)).collect::<Result<_, _>>()?),
            Node::Not(c) => CNode::Not(Box::new(self.compile(c, s, caches, cancel)?)),
            Node::Leaf(p) => CNode::Leaf(self.leaf(p, s, caches, cancel)?),
        })
    }

    fn ext_set(s: &IndexStore, exts: &[String]) -> (Bits, Vec<Vec<u8>>) {
        let mut ids = Bits::new(65_536);
        for e in exts {
            if let Some(id) = s.ext_dict().lookup(e.as_bytes()) {
                ids.set(id as usize);
            }
        }
        (ids, exts.iter().map(|e| e.as_bytes().to_vec()).collect())
    }

    fn leaf(&mut self, p: &Pred, s: &IndexStore, caches: &Caches, cancel: &CancelToken) -> Result<Leaf, Cancelled> {
        Ok(match p {
            Pred::Text(t) => match self.text_leaf(t, s) {
                Some(l) => Leaf::Text(Box::new(l)),
                None => Leaf::Nothing,
            },
            Pred::Ext { exts, none } => {
                let (ids, overflow) = Self::ext_set(s, exts);
                Leaf::Ext { ids, overflow, none: *none }
            }
            Pred::Kind(KindClass::Folder) => Leaf::IsDir(true),
            Pred::Kind(KindClass::Exts(exts)) => {
                let (ids, overflow) = Self::ext_set(s, exts);
                Leaf::Ext { ids, overflow, none: false }
            }
            Pred::IsDir(d) => Leaf::IsDir(*d),
            Pred::Num(prop, pred) => {
                let counts = matches!(prop, NumProp::ChildCount | NumProp::ChildFileCount | NumProp::ChildFolderCount)
                    .then(|| caches.child_counts(s));
                Leaf::Num { prop: *prop, pred: pred.clone(), counts }
            }
            Pred::Attrib { mask, exact } => Leaf::Attrib { mask: *mask, exact: *exact },
            Pred::Root => Leaf::Root,
            Pred::Empty => Leaf::Empty(caches.child_counts(s)),
            Pred::Parent(t) => Leaf::ParentIn(self.folder_set(t, s)),
            Pred::Ancestor(t) => Leaf::AncestorIn(self.folder_set(t, s)),
            Pred::Child(t) => {
                let Some(leaf) = self.text_leaf(t, s) else { return Ok(Leaf::Nothing) };
                let n = s.len();
                let parents: Vec<Vec<u32>> = (0..n.div_ceil(CHUNK))
                    .into_par_iter()
                    .map_init(Scratch::default, |sc, k| {
                        let mut out = Vec::new();
                        if cancel.is_cancelled() {
                            return out;
                        }
                        for i in k * CHUNK..((k + 1) * CHUNK).min(n) {
                            let id = EntryId(i as u32);
                            if s.is_live(id) && s.parent(id).is_some() && self.eval_text(&leaf, s, id, sc) {
                                out.push(s.parent(id).0);
                            }
                        }
                        out
                    })
                    .collect();
                if cancel.is_cancelled() {
                    return Err(Cancelled);
                }
                let mut set = Bits::new(n);
                parents.iter().flatten().for_each(|&p| set.set(p as usize));
                Leaf::In(set)
            }
            Pred::Volume(prefix) => {
                // Match by drive/root name (`C:`), or by the volume's root path.
                let mut roots = Bits::new(s.len());
                for v in s.volumes() {
                    let vol_hit = fnc_core::fold::fold_str(&v.info.root_path).starts_with(prefix.as_str());
                    for &r in &v.roots {
                        let name = String::from_utf8_lossy(s.fold(r)).replace('/', "\\");
                        if vol_hit || name.trim_end_matches([':', '\\']).starts_with(prefix.as_str()) {
                            roots.set(r.idx());
                        }
                    }
                }
                Leaf::SelfOrAncestorIn(roots)
            }
            Pred::VolumeKind(k) => Leaf::VolumeIn(s.volumes().iter().map(|v| v.info.kind.as_str() == k).collect()),
            Pred::Online(on) => Leaf::VolumeIn(s.volumes().iter().map(|v| v.online == *on).collect()),
            Pred::Reparse(k) => Leaf::Reparse(*k),
            Pred::Unsupported(_) | Pred::Invalid(_) => Leaf::Nothing,
        })
    }

    /// Evaluates a text leaf for entry `id`.
    #[inline]
    pub fn eval_text(&self, l: &TextLeaf, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        let t = &l.pred;
        let strip = t.ignore_punct || t.ignore_ws;
        let subject: &[u8] = match t.target {
            Target::Name | Target::Stem | Target::Ext => {
                let base: &[u8] = if l.mode == SubjectMode::StoredFold {
                    s.fold(id)
                } else {
                    name_subject(s.name(id), l.mode, &mut sc.a);
                    &sc.a
                };
                match t.target {
                    Target::Stem => stem_of(base),
                    Target::Ext => {
                        if s.is_dir(id) {
                            return false;
                        }
                        ext_of(base)
                    }
                    _ => base,
                }
            }
            Target::FullPath | Target::PathPart => {
                let Some(dp) = self.dir_paths.as_deref() else { return false };
                if t.target == Target::FullPath
                    && let Some(hit) = &l.dir_hit
                {
                    // Substring fast path: the folder part alone may already match.
                    let own_dir = s.is_dir(id);
                    let probe = if own_dir { id } else { s.parent(id) };
                    if probe.is_some() && hit.get(probe.idx()) {
                        return true;
                    }
                    if own_dir {
                        return false;
                    }
                    // Otherwise only the tail (straddling `\`) and the name remain.
                    sc.b.clear();
                    if let Some(p) = dp.get(s.parent(id)) {
                        let keep = l.needle_len.saturating_sub(1).min(p.len());
                        sc.b.extend_from_slice(&p[p.len() - keep..]);
                        if p.last() != Some(&b'\\') {
                            sc.b.push(b'\\');
                        }
                    }
                    sc.b.extend_from_slice(s.fold(id));
                    return l.pattern.is_match(&sc.b);
                }
                if l.mode == SubjectMode::StoredFold {
                    if t.target == Target::PathPart {
                        sc.b.clear();
                        if let Some(p) = dp.get(s.parent(id)) {
                            sc.b.extend_from_slice(p);
                        }
                    } else {
                        dp.full_path_into(s, id, &mut sc.b);
                    }
                } else {
                    // Slow path: re-fold every path component with the term's options.
                    sc.t.clear();
                    if t.target == Target::PathPart {
                        s.parent_path(id, &mut sc.t);
                    } else {
                        s.full_path(id, &mut sc.t);
                    }
                    let units: Vec<u16> = sc.t.replace('/', "\\").encode_utf16().collect();
                    name_subject(&units, l.mode, &mut sc.b);
                }
                &sc.b
            }
        };
        if strip {
            let Ok(text) = std::str::from_utf8(subject) else { return false };
            strip_chars(text, t.ignore_punct, t.ignore_ws, &mut sc.s);
            return l.pattern.is_match(sc.s.as_bytes());
        }
        l.pattern.is_match(subject)
    }

    fn num_value(
        &self,
        prop: NumProp,
        s: &IndexStore,
        id: EntryId,
        counts: Option<&ChildCounts>,
        sc: &mut Scratch,
    ) -> Option<i64> {
        let known = |t: FileTime| t.is_known().then_some(t.0);
        match prop {
            NumProp::Size => {
                if s.is_dir(id) {
                    return None;
                }
                let v = s.size_column()[id.idx()];
                (v != SIZE_UNKNOWN).then(|| i64::try_from(v).unwrap_or(i64::MAX))
            }
            NumProp::Date(DateCol::Modified) => known(s.dm_column()[id.idx()]),
            NumProp::Date(DateCol::Created) => known(s.dc_column()[id.idx()]),
            NumProp::Date(DateCol::Accessed) => known(s.date_accessed(id)),
            NumProp::Date(DateCol::Run) => {
                let r = s.run_info(id);
                (r.count > 0).then_some(r.last.0)
            }
            NumProp::Date(DateCol::RecentlyChanged) => known(s.date_recently_changed(id)),
            NumProp::Depth => Some(s.depth(id) as i64),
            NumProp::NameLength => Some(s.name(id).len() as i64),
            NumProp::FullPathLength => {
                s.full_path(id, &mut sc.t);
                Some(sc.t.encode_utf16().count() as i64)
            }
            NumProp::PathLength => {
                s.parent_path(id, &mut sc.t);
                Some(sc.t.encode_utf16().count() as i64)
            }
            NumProp::RunCount => Some(i64::from(s.run_info(id).count)),
            NumProp::Frn => Some(s.frn(id) as i64),
            NumProp::ChildCount | NumProp::ChildFileCount | NumProp::ChildFolderCount => {
                if !s.is_dir(id) {
                    return None;
                }
                let c = counts?;
                let (f, d) = (c.files[id.idx()], c.folders[id.idx()]);
                Some(i64::from(match prop {
                    NumProp::ChildFileCount => f,
                    NumProp::ChildFolderCount => d,
                    _ => f + d,
                }))
            }
        }
    }

    /// `true` if `start` or one of its ancestors is in `set`.
    fn ancestor_in(set: &Bits, s: &IndexStore, start: EntryId) -> bool {
        let mut p = start;
        let mut steps = 0;
        while p.is_some() && steps < fnc_index::store::MAX_DEPTH {
            if set.get(p.idx()) {
                return true;
            }
            p = s.parent(p);
            steps += 1;
        }
        false
    }

    fn eval_leaf(&self, l: &Leaf, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        match l {
            Leaf::Text(t) => self.eval_text(t, s, id, sc),
            Leaf::Ext { ids, overflow, none } => {
                let e = s.ext_id(id);
                if e == fnc_index::ext::NO_EXT {
                    return *none && !s.is_dir(id);
                }
                if e == OVERFLOW_EXT {
                    let ext = ext_of(s.fold(id));
                    return overflow.iter().any(|o| o.as_slice() == ext);
                }
                ids.get(e as usize)
            }
            Leaf::IsDir(d) => s.is_dir(id) == *d,
            Leaf::Num { prop, pred, counts } => match self.num_value(*prop, s, id, counts.as_deref(), sc) {
                Some(v) => pred.matches(v),
                None => pred.unknown,
            },
            Leaf::Attrib { mask, exact } => {
                let mut a = s.attributes(id);
                if s.is_dir(id) {
                    a |= fnc_core::flags::attr::DIRECTORY;
                }
                if *exact { a & ATTRIB_UNIVERSE == *mask } else { a & mask == *mask }
            }
            Leaf::Root => s.flags(id).contains(EntryFlags::ROOT),
            Leaf::Empty(c) => {
                if s.is_dir(id) {
                    c.files[id.idx()] + c.folders[id.idx()] == 0
                } else {
                    s.size(id) == Some(0)
                }
            }
            Leaf::ParentIn(set) => {
                let p = s.parent(id);
                p.is_some() && set.get(p.idx())
            }
            Leaf::AncestorIn(set) => Self::ancestor_in(set, s, s.parent(id)),
            Leaf::SelfOrAncestorIn(set) => Self::ancestor_in(set, s, id),
            Leaf::In(set) => set.get(id.idx()),
            Leaf::VolumeIn(v) => v.get(s.volume_of(id).idx()).copied().unwrap_or(false),
            Leaf::Reparse(k) => {
                let f = s.flags(id);
                f.contains(EntryFlags::REPARSE)
                    && match k {
                        ReparseKind::Any => true,
                        ReparseKind::Junction => f.contains(EntryFlags::IS_DIR),
                        ReparseKind::Symlink => !f.contains(EntryFlags::IS_DIR),
                    }
            }
            Leaf::Nothing => false,
        }
    }

    /// Evaluates `n` for entry `id`.
    pub fn eval_node(&self, n: &CNode, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        match n {
            CNode::All => true,
            CNode::Nothing => false,
            CNode::And(v) => v.iter().all(|c| self.eval_node(c, s, id, sc)),
            CNode::Or(v) => v.iter().any(|c| self.eval_node(c, s, id, sc)),
            CNode::Not(c) => !self.eval_node(c, s, id, sc),
            CNode::Leaf(l) => self.eval_leaf(l, s, id, sc),
        }
    }

    /// Evaluates the whole query for entry `id`.
    #[inline]
    pub fn matches(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        self.eval_node(&self.root, s, id, sc)
    }
}

/// Rows per parallel work unit (SPEC ch.1 §6.1: 64K entries per block).
pub const CHUNK: usize = 64 * 1024;

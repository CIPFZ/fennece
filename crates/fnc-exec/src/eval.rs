//! Plan compilation and per-entry evaluation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fnc_core::fold::{FoldOptions, fold_str_into};
use fnc_core::{EntryFlags, EntryId, FileTime};
use fnc_index::ext::{NO_EXT, OVERFLOW_EXT, ext_of, stem_of};
use fnc_index::{IndexStore, SIZE_UNKNOWN};
use fnc_props::PropertyId;
use fnc_props::hash::HashAlgo;
use fnc_query::date::DateWild;
use fnc_query::plan::{
    ATTRIB_UNIVERSE, DateCol, EntryVar, KindClass, Node, NumPred, NumProp, PathPiece, PatternKind, Pred, ReparseKind,
    SiblingScope, Target, TextPred, uses_stores,
};
use rayon::prelude::*;

use crate::cache::{Caches, CancelToken, Cancelled, ChildCounts};
use crate::ext::{
    ExecWarning as ExtWarning, ExtCtx, ExtLeaf, SlowState, compile_content, compile_content_hits, compile_hash,
    compile_prop,
};
use crate::matcher::{Pattern, SubjectMode, compile_pattern, name_subject, strip_chars, subject_mode};
use crate::paths::DirPaths;
use crate::tree::{LinkCounts, NameLookup, TreeAggregates, resolve_path, root_paths};
use crate::types::{FOLDER_TYPE, NO_EXT_TYPE};

/// Properties referenced by property predicates in `n`.
fn collect_props(n: &Node, out: &mut Vec<PropertyId>) {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().for_each(|c| collect_props(c, out)),
        Node::Not(c) => collect_props(c, out),
        Node::Leaf(Pred::Prop(p)) if !out.contains(&p.id) => out.push(p.id),
        _ => {}
    }
}

/// Hash algorithms referenced in `n` (computed together in one pass per file).
fn collect_hashes(n: &Node, out: &mut Vec<HashAlgo>) {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().for_each(|c| collect_hashes(c, out)),
        Node::Not(c) => collect_hashes(c, out),
        Node::Leaf(Pred::Hash(h)) if !out.contains(&h.algo) => out.push(h.algo),
        _ => {}
    }
}

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
        /// Precomputed column the property is read from.
        aux: NumAux,
    },
    /// Date column matches a wildcard date.
    DateWild(DateCol, DateWild),
    /// `type:`: matching extension ids, plus folders / extensionless files.
    TypeIn {
        /// Extension ids whose description matches.
        ids: Bits,
        /// "File folder" matches.
        folder: bool,
        /// "File" (no extension) matches.
        no_ext: bool,
        /// The pattern, for extensions that overflowed the dictionary (rare; matched
        /// against the fallback description).
        overflow: Box<TextLeaf>,
    },
    /// `filelist:`: whole names or resolved paths (boxed: keeps `Leaf` small).
    NameSet(Box<NameSetLeaf>),
    /// `fileexists:` / `folderexists:`.
    Exists(Box<ExistsLeaf>),
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
    /// Property, content or hash function (one boxed arm keeps the evaluator of the
    /// literal leaves unchanged).
    Extension(Box<ExtLeaf>),
    /// Never matches.
    Nothing,
}

/// Column behind a numeric leaf.
pub enum NumAux {
    /// Read directly from the index.
    None,
    /// Direct child counts.
    Children(Arc<ChildCounts>),
    /// Descendant counts / folder sizes.
    Tree(Arc<TreeAggregates>),
    /// Hard link counts.
    Links(Arc<LinkCounts>),
}

/// Compiled `filelist:`.
pub struct NameSetLeaf {
    /// Folded whole names.
    names: HashSet<Box<[u8]>>,
    /// Entries resolved from full paths.
    ids: Bits,
}

/// Compiled `fileexists:` / `folderexists:`.
pub struct ExistsLeaf {
    template: Vec<PathPiece>,
    folder: bool,
    roots: Vec<(Vec<u8>, EntryId)>,
    lookup: Arc<NameLookup>,
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
    pub(crate) a: Vec<u8>,
    pub(crate) b: Vec<u8>,
    pub(crate) s: String,
    pub(crate) t: String,
    pub(crate) u: String,
}

/// A compiled query ready for evaluation against one snapshot.
pub struct Compiled {
    /// Root node.
    pub root: CNode,
    dir_paths: Option<Arc<DirPaths>>,
    /// Invalid patterns (rendered as diagnostics).
    pub errors: Vec<String>,
    /// Slow-path state of property/content/hash leaves (`None` when the plan has none).
    pub(crate) slow: Option<Arc<SlowState>>,
    /// Hits of nested semantic terms, computed before compiling.
    pub(crate) semantic: HashMap<fnc_query::semantic::SemPred, Arc<crate::semantic::SemSet>>,
}

fn needs_paths(n: &Node) -> bool {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().any(needs_paths),
        Node::Not(c) => needs_paths(c),
        Node::Leaf(Pred::Text(t)) => matches!(t.target, Target::FullPath | Target::PathPart),
        Node::Leaf(Pred::Parent(_) | Pred::Ancestor(_)) => true,
        Node::Leaf(Pred::Child(t) | Pred::Descendant(t) | Pred::Sibling(_, t)) => {
            matches!(t.target, Target::FullPath | Target::PathPart)
        }
        Node::Leaf(Pred::Num(p, _)) => matches!(p, NumProp::PathLength | NumProp::FullPathLength),
        _ => false,
    }
}

impl Compiled {
    /// Compiles `root` for snapshot `s`, precomputing folder sets and caches. Property,
    /// content and hash predicates see no stores (everything "not indexed").
    pub fn new(root: &Node, s: &IndexStore, caches: &Caches, cancel: &CancelToken) -> Result<Self, Cancelled> {
        Self::with_ext(root, s, caches, cancel, &ExtCtx::default())
    }

    /// Like [`Compiled::new`] with property/content stores, hash cache and slow-path
    /// source.
    pub fn with_ext(
        root: &Node,
        s: &IndexStore,
        caches: &Caches,
        cancel: &CancelToken,
        ext: &ExtCtx<'_>,
    ) -> Result<Self, Cancelled> {
        Self::with_ext_semantic(root, s, caches, cancel, ext, HashMap::new())
    }

    /// [`Compiled::with_ext`] with precomputed hits of nested semantic terms.
    pub fn with_ext_semantic(
        root: &Node,
        s: &IndexStore,
        caches: &Caches,
        cancel: &CancelToken,
        ext: &ExtCtx<'_>,
        semantic: HashMap<fnc_query::semantic::SemPred, Arc<crate::semantic::SemSet>>,
    ) -> Result<Self, Cancelled> {
        let dir_paths = needs_paths(root).then(|| caches.dir_paths(s));
        let slow = uses_stores(root).then(|| {
            let mut st = SlowState::new(ext, cancel, caches);
            let mut props = Vec::new();
            collect_props(root, &mut props);
            for p in props {
                st.want(p);
            }
            Arc::new(st)
        });
        let mut algos = Vec::new();
        collect_hashes(root, &mut algos);
        let mut c = Compiled { root: CNode::All, dir_paths, errors: Vec::new(), slow, semantic };
        c.root = c.compile(root, s, caches, cancel, ext, &algos)?;
        Ok(c)
    }

    /// Warnings collected so far (slow-path limits, unindexed functions).
    pub fn warnings(&self) -> Vec<ExtWarning> {
        self.slow.as_ref().map(|s| s.finish()).unwrap_or_default()
    }

    /// Content first-hit `(offset, hits)` per entry recorded by content leaves.
    pub fn content_hits(&self) -> HashMap<u32, (u32, u32)> {
        self.slow.as_ref().map(|s| s.content_hits.lock().clone()).unwrap_or_default()
    }

    pub(crate) fn text_leaf(&mut self, t: &TextPred, s: &IndexStore) -> Option<TextLeaf> {
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

    /// Live non-root entries whose name matches `t` (parallel scan); `None` if the
    /// pattern is invalid.
    fn hits(&mut self, t: &TextPred, s: &IndexStore, cancel: &CancelToken) -> Result<Option<Vec<u32>>, Cancelled> {
        let Some(leaf) = self.text_leaf(t, s) else { return Ok(None) };
        let n = s.len();
        let this = &*self;
        let parts: Vec<Vec<u32>> = (0..n.div_ceil(CHUNK))
            .into_par_iter()
            .map_init(Scratch::default, |sc, k| {
                let mut out = Vec::new();
                if cancel.is_cancelled() {
                    return out;
                }
                for i in k * CHUNK..((k + 1) * CHUNK).min(n) {
                    let id = EntryId(i as u32);
                    if s.is_live(id) && s.parent(id).is_some() && this.eval_text(&leaf, s, id, sc) {
                        out.push(id.0);
                    }
                }
                out
            })
            .collect();
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        Ok(Some(parts.concat()))
    }

    /// Entries with at least one sibling (another live entry with the same parent)
    /// among `hits`.
    fn sibling_set(s: &IndexStore, hits: &[u32], cancel: &CancelToken) -> Result<Bits, Cancelled> {
        let n = s.len();
        let mut is_hit = Bits::new(n);
        // Matching children per folder, saturated at 2 (all we need to know).
        let mut per_dir = vec![0u8; n];
        for &h in hits {
            is_hit.set(h as usize);
            let p = s.parent(EntryId(h));
            if let Some(c) = per_dir.get_mut(p.idx()) {
                *c = (*c + 1).min(2);
            }
        }
        let mut out = Bits::new(n);
        for i in 0..n {
            if i % CHUNK == 0 && cancel.is_cancelled() {
                return Err(Cancelled);
            }
            let id = EntryId(i as u32);
            let p = s.parent(id);
            if !s.is_live(id) || !p.is_some() {
                continue;
            }
            let c = per_dir.get(p.idx()).copied().unwrap_or(0);
            if c >= 2 || (c == 1 && !is_hit.get(i)) {
                out.set(i);
            }
        }
        Ok(out)
    }

    /// `type:` — descriptions are per extension, so the pattern is evaluated once per
    /// dictionary entry here and per entry it is a bit lookup.
    fn type_leaf(&mut self, t: &TextPred, s: &IndexStore, caches: &Caches) -> Leaf {
        let Some(leaf) = self.text_leaf(t, s) else { return Leaf::Nothing };
        let mut sc = Scratch::default();
        let mut ids = Bits::new(65_536);
        for (id, ext) in s.ext_dict().iter() {
            if id != NO_EXT && id != OVERFLOW_EXT && Self::match_str(&leaf, &caches.type_description(ext), &mut sc) {
                ids.set(id as usize);
            }
        }
        let folder = Self::match_str(&leaf, FOLDER_TYPE, &mut sc);
        let no_ext = Self::match_str(&leaf, NO_EXT_TYPE, &mut sc);
        Leaf::TypeIn { ids, folder, no_ext, overflow: Box::new(leaf) }
    }

    /// Matches `l` against arbitrary text (folded like an entry name).
    pub(crate) fn match_str(l: &TextLeaf, text: &str, sc: &mut Scratch) -> bool {
        sc.u.clear();
        match l.mode {
            SubjectMode::StoredFold => fold_str_into(text, FoldOptions::DEFAULT, &mut sc.u),
            SubjectMode::Refold(o) => fold_str_into(text, o, &mut sc.u),
            SubjectMode::Original => sc.u.push_str(text),
            SubjectMode::Ascii => {
                sc.u.push_str(text);
                sc.u.make_ascii_lowercase();
            }
        }
        let t = &l.pred;
        if t.ignore_punct || t.ignore_ws {
            strip_chars(&sc.u, t.ignore_punct, t.ignore_ws, &mut sc.s);
            return l.pattern.is_match(sc.s.as_bytes());
        }
        l.pattern.is_match(sc.u.as_bytes())
    }

    fn compile(
        &mut self,
        n: &Node,
        s: &IndexStore,
        caches: &Caches,
        cancel: &CancelToken,
        ext: &ExtCtx<'_>,
        algos: &[HashAlgo],
    ) -> Result<CNode, Cancelled> {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        Ok(match n {
            Node::All => CNode::All,
            Node::Nothing => CNode::Nothing,
            Node::And(v) => {
                CNode::And(v.iter().map(|c| self.compile(c, s, caches, cancel, ext, algos)).collect::<Result<_, _>>()?)
            }
            Node::Or(v) => {
                CNode::Or(v.iter().map(|c| self.compile(c, s, caches, cancel, ext, algos)).collect::<Result<_, _>>()?)
            }
            Node::Not(c) => CNode::Not(Box::new(self.compile(c, s, caches, cancel, ext, algos)?)),
            Node::Leaf(p) => CNode::Leaf(self.leaf(p, s, caches, cancel, ext, algos)?),
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

    fn leaf(
        &mut self,
        p: &Pred,
        s: &IndexStore,
        caches: &Caches,
        cancel: &CancelToken,
        ext: &ExtCtx<'_>,
        algos: &[HashAlgo],
    ) -> Result<Leaf, Cancelled> {
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
                let aux = match prop {
                    NumProp::ChildCount
                    | NumProp::ChildFileCount
                    | NumProp::ChildFolderCount
                    | NumProp::SiblingCount => NumAux::Children(caches.child_counts(s)),
                    NumProp::DescendantCount
                    | NumProp::DescendantFileCount
                    | NumProp::DescendantFolderCount
                    | NumProp::FolderSize => NumAux::Tree(caches.tree_aggregates(s)),
                    NumProp::HardlinkCount => NumAux::Links(caches.link_counts(s)),
                    _ => NumAux::None,
                };
                Leaf::Num { prop: *prop, pred: pred.clone(), aux }
            }
            Pred::Attrib { mask, exact } => Leaf::Attrib { mask: *mask, exact: *exact },
            Pred::Root => Leaf::Root,
            Pred::Empty => Leaf::Empty(caches.child_counts(s)),
            Pred::Parent(t) => Leaf::ParentIn(self.folder_set(t, s)),
            Pred::Ancestor(t) => Leaf::AncestorIn(self.folder_set(t, s)),
            Pred::Child(t) => {
                let Some(hits) = self.hits(t, s, cancel)? else { return Ok(Leaf::Nothing) };
                let mut set = Bits::new(s.len());
                for &h in &hits {
                    let p = s.parent(EntryId(h));
                    if p.is_some() {
                        set.set(p.idx());
                    }
                }
                Leaf::In(set)
            }
            Pred::Descendant(t) => {
                let Some(hits) = self.hits(t, s, cancel)? else { return Ok(Leaf::Nothing) };
                let mut set = Bits::new(s.len());
                for &h in &hits {
                    // Mark the ancestor chain; stop at the first folder already marked
                    // (its own ancestors are marked too), so the total work is O(n).
                    let mut p = s.parent(EntryId(h));
                    let mut steps = 0;
                    while p.is_some() && !set.get(p.idx()) && steps < fnc_index::store::MAX_DEPTH {
                        set.set(p.idx());
                        p = s.parent(p);
                        steps += 1;
                    }
                }
                Leaf::In(set)
            }
            Pred::Sibling(scope, t) => {
                let Some(hits) = self.hits(t, s, cancel)? else { return Ok(Leaf::Nothing) };
                let set = Self::sibling_set(s, &hits, cancel)?;
                match scope {
                    SiblingScope::Own => Leaf::In(set),
                    SiblingScope::Parent => Leaf::ParentIn(set),
                    SiblingScope::Ancestor => Leaf::AncestorIn(set),
                }
            }
            Pred::Type(t) => self.type_leaf(t, s, caches),
            Pred::FileList { names, paths } => {
                let roots = root_paths(s);
                let mut ids = Bits::new(s.len());
                for p in paths {
                    if let Some(id) = resolve_path(s, p.as_bytes(), None, &roots, |d, name| s.find_child(d, name)) {
                        ids.set(id.idx());
                    }
                }
                Leaf::NameSet(Box::new(NameSetLeaf { names: names.iter().map(|n| n.as_bytes().into()).collect(), ids }))
            }
            Pred::Exists { template, folder } => Leaf::Exists(Box::new(ExistsLeaf {
                template: template.clone(),
                folder: *folder,
                roots: root_paths(s),
                lookup: caches.name_lookup(s),
            })),
            Pred::Omitted(set) => {
                let mut bits = Bits::new(s.len());
                if set.instance == s.instance() {
                    set.ids.iter().for_each(|id| bits.set(id.idx()));
                }
                Leaf::In(bits)
            }
            Pred::DateWild(col, w) => Leaf::DateWild(*col, *w),
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
            Pred::Prop(pp) => {
                let Some(slow) = self.slow.clone() else { return Ok(Leaf::Nothing) };
                match compile_prop(self, pp, s, ext, &slow) {
                    Some(l) => Leaf::Extension(Box::new(ExtLeaf::Prop(l))),
                    None => Leaf::Nothing,
                }
            }
            Pred::Content(cp) => {
                let Some(slow) = self.slow.clone() else { return Ok(Leaf::Nothing) };
                match compile_content(self, cp, s, ext, &slow, cancel) {
                    Some(l) => Leaf::Extension(Box::new(ExtLeaf::Content(l))),
                    None if cancel.is_cancelled() => return Err(Cancelled),
                    None => Leaf::Nothing,
                }
            }
            Pred::ContentHits(n, parts) => {
                let Some(slow) = self.slow.clone() else { return Ok(Leaf::Nothing) };
                match compile_content_hits(self, n, parts, s, ext, &slow, cancel) {
                    Some(l) => Leaf::Extension(Box::new(ExtLeaf::ContentHits(l))),
                    None => Leaf::Nothing,
                }
            }
            Pred::Hash(h) => {
                let Some(slow) = self.slow.clone() else { return Ok(Leaf::Nothing) };
                Leaf::Extension(Box::new(ExtLeaf::Hash(compile_hash(h, algos, ext, &slow))))
            }
            Pred::Eval(f) => {
                Leaf::Extension(Box::new(ExtLeaf::Eval(crate::formula::EvalLeaf::new(&f.0, s, ext.props))))
            }
            Pred::Semantic(p) => match self.semantic.get(p) {
                Some(set) => Leaf::In(set.bits.clone()),
                None => Leaf::Nothing,
            },
            Pred::Set(_) => Leaf::Nothing,
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

    fn num_value(&self, prop: NumProp, s: &IndexStore, id: EntryId, aux: &NumAux, sc: &mut Scratch) -> Option<i64> {
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
                let NumAux::Children(c) = aux else { return None };
                if !s.is_dir(id) {
                    return None;
                }
                let (f, d) = (c.files[id.idx()], c.folders[id.idx()]);
                Some(i64::from(match prop {
                    NumProp::ChildFileCount => f,
                    NumProp::ChildFolderCount => d,
                    _ => f + d,
                }))
            }
            NumProp::SiblingCount => {
                let NumAux::Children(c) = aux else { return None };
                let p = s.parent(id);
                if !p.is_some() {
                    return None;
                }
                let total = i64::from(c.files[p.idx()]) + i64::from(c.folders[p.idx()]);
                Some((total - 1).max(0))
            }
            NumProp::DescendantCount | NumProp::DescendantFileCount | NumProp::DescendantFolderCount => {
                let NumAux::Tree(t) = aux else { return None };
                if !s.is_dir(id) {
                    return None;
                }
                let (f, d) = (i64::from(t.files[id.idx()]), i64::from(t.folders[id.idx()]));
                Some(match prop {
                    NumProp::DescendantFileCount => f,
                    NumProp::DescendantFolderCount => d,
                    _ => f + d,
                })
            }
            NumProp::FolderSize => {
                if !s.is_dir(id) {
                    return self.num_value(NumProp::Size, s, id, aux, sc);
                }
                let NumAux::Tree(t) = aux else { return None };
                Some(i64::try_from(t.bytes[id.idx()]).unwrap_or(i64::MAX))
            }
            NumProp::HardlinkCount => {
                let NumAux::Links(l) = aux else { return None };
                Some(i64::from(l.counts[id.idx()]))
            }
            NumProp::NameParts => Some(name_parts(s.fold(id), s.is_dir(id))),
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

    /// `fileexists:` / `folderexists:` for entry `id`: expands the template with the
    /// entry's properties and resolves it in the index (relative to the entry's folder).
    fn exists(x: &ExistsLeaf, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        let name = s.name(id);
        let dot = if s.is_dir(id) { None } else { ext_dot(name) };
        let parent = s.parent(id);
        sc.t.clear();
        for piece in &x.template {
            match piece {
                PathPiece::Lit(l) => sc.t.push_str(l),
                PathPiece::Var(EntryVar::Name) => push_utf16(name, &mut sc.t),
                PathPiece::Var(EntryVar::Stem) => push_utf16(&name[..dot.unwrap_or(name.len())], &mut sc.t),
                PathPiece::Var(EntryVar::Ext) => {
                    if let Some(d) = dot {
                        push_utf16(&name[d + 1..], &mut sc.t);
                    }
                }
                PathPiece::Var(EntryVar::Path) => {
                    if parent.is_some() {
                        s.write_path(parent, &mut sc.t);
                    }
                }
                PathPiece::Var(EntryVar::Parent) => {
                    if parent.is_some() {
                        push_utf16(s.name(parent), &mut sc.t);
                    }
                }
                PathPiece::Var(EntryVar::FullPath) => s.write_path(id, &mut sc.t),
            }
        }
        sc.u.clear();
        fold_str_into(&sc.t, FoldOptions::DEFAULT, &mut sc.u);
        let base = parent.is_some().then_some(parent);
        resolve_path(s, sc.u.as_bytes(), base, &x.roots, |d, n| x.lookup.child(s, d, n))
            .is_some_and(|found| s.is_dir(found) == x.folder)
    }

    #[inline]
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
            Leaf::Num { prop, pred, aux } => match self.num_value(*prop, s, id, aux, sc) {
                Some(v) => pred.matches(v),
                None => pred.unknown,
            },
            Leaf::DateWild(col, w) => {
                self.num_value(NumProp::Date(*col), s, id, &NumAux::None, sc).is_some_and(|v| w.matches(v))
            }
            Leaf::TypeIn { ids, folder, no_ext, overflow } => {
                if s.is_dir(id) {
                    return *folder;
                }
                match s.ext_id(id) {
                    NO_EXT => *no_ext,
                    OVERFLOW_EXT => {
                        // Outside the dictionary: fallback description only.
                        let ext = ext_of(s.fold(id));
                        sc.t.clear();
                        sc.t.extend(String::from_utf8_lossy(ext).chars().flat_map(char::to_uppercase));
                        sc.t.push_str(" File");
                        let mut desc = std::mem::take(&mut sc.t);
                        let hit = Self::match_str(overflow, &desc, sc);
                        desc.clear();
                        sc.t = desc;
                        hit
                    }
                    e => ids.get(e as usize),
                }
            }
            Leaf::NameSet(n) => n.ids.get(id.idx()) || (!n.names.is_empty() && n.names.contains(s.fold(id))),
            Leaf::Exists(x) => Self::exists(x, s, id, sc),
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
            Leaf::Extension(x) => x.eval(s, id, sc),
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

    /// The query as plain substrings of the folded name, if it is just that (one term or
    /// an AND of terms without options): the scan then runs a monomorphic loop instead of
    /// the tree walk (most searches typed into the box have this shape).
    pub fn name_substrings(&self) -> Option<Vec<&memchr::memmem::Finder<'static>>> {
        fn plain(n: &CNode) -> Option<&memchr::memmem::Finder<'static>> {
            match n {
                CNode::Leaf(Leaf::Text(l))
                    if l.pred.target == Target::Name
                        && l.mode == SubjectMode::StoredFold
                        && !l.pred.ignore_punct
                        && !l.pred.ignore_ws =>
                {
                    match &l.pattern {
                        Pattern::Contains(f) => Some(f),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        match &self.root {
            CNode::And(v) if !v.is_empty() => v.iter().map(plain).collect(),
            n => plain(n).map(|f| vec![f]),
        }
    }

    /// Evaluates the whole query for entry `id`.
    #[inline]
    pub fn matches(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        self.eval_node(&self.root, s, id, sc)
    }
}

/// Number of words (runs of letters/digits) in a folded name; the extension of files
/// is not counted.
fn name_parts(fold: &[u8], is_dir: bool) -> i64 {
    let base = if is_dir { fold } else { stem_of(fold) };
    let mut words = 0;
    let mut in_word = false;
    match std::str::from_utf8(base) {
        Ok(text) => {
            for c in text.chars() {
                let w = c.is_alphanumeric();
                if w && !in_word {
                    words += 1;
                }
                in_word = w;
            }
        }
        Err(_) => {
            for &b in base {
                let w = b.is_ascii_alphanumeric();
                if w && !in_word {
                    words += 1;
                }
                in_word = w;
            }
        }
    }
    words
}

/// Appends a UTF-16 name to `out`.
fn push_utf16(units: &[u16], out: &mut String) {
    out.extend(char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)));
}

/// Position of the extension dot in a UTF-16 name (not a leading dot).
fn ext_dot(units: &[u16]) -> Option<usize> {
    units.iter().rposition(|&u| u == u16::from(b'.')).filter(|&i| i > 0)
}

/// Rows per parallel work unit (SPEC ch.1 §6.1: 64K entries per block).
pub const CHUNK: usize = 64 * 1024;

//! `IndexStore`: the columnar (SoA) entry table.

use std::collections::HashMap;

use fnc_core::arena::utf16_to_string_into;
use fnc_core::fold::fold_utf16_into;
use fnc_core::natsort::name_cmp;
use fnc_core::{EntryFlags, EntryId, FileTime, FoldArena, NameArena, VolumeId};

use crate::change::{ApplyStats, Change, ChangeBatch, EntryRef, Meta};
use crate::children::Children;
use crate::error::{IndexError, Result};
use crate::ext::{ExtDict, NO_EXT, ext_of};
use crate::frnmap::FrnMap;
use crate::order::NameOrder;
use crate::volume::{Volume, VolumeInfo, VolumeKind};

/// Size value meaning "unknown / not indexed".
pub const SIZE_UNKNOWN: u64 = u64::MAX;

/// Maximum supported path depth (guards against parent cycles in corrupt sources).
pub const MAX_DEPTH: usize = 1024;

/// Name of the hidden folder that collects entries whose parent is unknown.
pub const ORPHANS_NAME: &str = "$Orphans";

/// Which optional columns are maintained.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct IndexOptions {
    /// Keep date accessed (off by default, like Everything).
    pub date_accessed: bool,
}

/// Run history of one entry.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RunInfo {
    /// Times opened through Fennec.
    pub count: u32,
    /// Last run.
    pub last: FileTime,
}

/// Cheap statistics for the query planner.
#[derive(Copy, Clone, Debug, Default)]
pub struct IndexStats {
    /// Rows (including tombstones).
    pub rows: usize,
    /// Live entries.
    pub live: usize,
    /// Live folders.
    pub folders: usize,
}

/// Columnar index of all entries of all volumes.
///
/// Row `i` of every column describes `EntryId(i)`. Deleted entries keep their row
/// as a tombstone ([`EntryFlags::DELETED`]) so result sets never dangle; rows are
/// reclaimed by [`IndexStore::compact`].
#[derive(Clone)]
pub struct IndexStore {
    pub(crate) volumes: Vec<Volume>,
    pub(crate) names: NameArena,
    pub(crate) folds: FoldArena,
    pub(crate) parent: Vec<EntryId>,
    pub(crate) volume: Vec<VolumeId>,
    pub(crate) flags: Vec<EntryFlags>,
    pub(crate) frn: Vec<u64>,
    pub(crate) ext: Vec<u16>,
    pub(crate) size: Vec<u64>,
    pub(crate) dm: Vec<FileTime>,
    pub(crate) dc: Vec<FileTime>,
    pub(crate) da: Option<Vec<FileTime>>,
    pub(crate) attr: Vec<u32>,
    pub(crate) run: HashMap<EntryId, RunInfo>,
    pub(crate) recent: HashMap<EntryId, FileTime>,
    pub(crate) ext_dict: ExtDict,
    pub(crate) children: Children,
    pub(crate) frn_maps: Vec<FrnMap>,
    pub(crate) orphans: Vec<Option<EntryId>>,
    pub(crate) name_order: NameOrder,
    generation: u64,
    tree_generation: u64,
    instance: u64,
    live: usize,
    folders: usize,
    scratch: Vec<u8>,
}

static NEXT_INSTANCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_instance() -> u64 {
    NEXT_INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for IndexStore {
    fn default() -> Self {
        Self::new(IndexOptions::default())
    }
}

impl IndexStore {
    /// Empty index.
    pub fn new(options: IndexOptions) -> Self {
        Self {
            volumes: Vec::new(),
            names: NameArena::new(),
            folds: FoldArena::new(),
            parent: Vec::new(),
            volume: Vec::new(),
            flags: Vec::new(),
            frn: Vec::new(),
            ext: Vec::new(),
            size: Vec::new(),
            dm: Vec::new(),
            dc: Vec::new(),
            da: options.date_accessed.then(Vec::new),
            attr: Vec::new(),
            run: HashMap::new(),
            recent: HashMap::new(),
            ext_dict: ExtDict::new(),
            children: Children::default(),
            frn_maps: Vec::new(),
            orphans: Vec::new(),
            name_order: NameOrder::default(),
            generation: 0,
            tree_generation: 0,
            instance: next_instance(),
            live: 0,
            folders: 0,
            scratch: Vec::new(),
        }
    }

    /// Reserves room for `entries` more rows with `name_units` UTF-16 units in total.
    pub fn reserve(&mut self, entries: usize, name_units: usize) {
        let mut names = NameArena::with_capacity(self.len() + entries, self.names.units() + name_units);
        let mut folds = FoldArena::with_capacity(self.len() + entries, self.folds.units() + name_units);
        if self.is_empty() {
            std::mem::swap(&mut names, &mut self.names);
            std::mem::swap(&mut folds, &mut self.folds);
        }
        self.parent.reserve(entries);
        self.volume.reserve(entries);
        self.flags.reserve(entries);
        self.frn.reserve(entries);
        self.ext.reserve(entries);
        self.size.reserve(entries);
        self.dm.reserve(entries);
        self.dc.reserve(entries);
        if let Some(da) = &mut self.da {
            da.reserve(entries);
        }
        self.attr.reserve(entries);
    }

    // ------------------------------------------------------------------ volumes

    /// Registers a volume. With `root_frn = Some(frn)` its root entry (named after
    /// `root_path`, e.g. `C:`) is created with that FRN (`5` on NTFS); file lists pass
    /// `None` and add one root per drive with [`IndexStore::add_root`].
    pub fn add_volume(&mut self, info: VolumeInfo, root_frn: Option<u64>) -> Result<VolumeId> {
        let id = VolumeId(u16::try_from(self.volumes.len()).map_err(|_| IndexError::TooManyVolumes)?);
        let root_name = root_display_name(&info.root_path, info.separator);
        self.volumes.push(Volume {
            id,
            info,
            roots: Vec::new(),
            usn_journal_id: 0,
            next_usn: 0,
            online: true,
            include_in_index: true,
        });
        self.frn_maps.push(FrnMap::default());
        self.orphans.push(None);
        if let Some(frn) = root_frn {
            self.add_root(id, frn, &root_name)?;
        }
        self.bump(true);
        Ok(id)
    }

    /// Adds an extra root to a volume (file lists spanning several drives).
    pub fn add_root(&mut self, vol: VolumeId, frn: u64, name: &str) -> Result<EntryId> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let root = self.push_row(
            vol,
            frn,
            EntryId::NONE,
            &units,
            &Meta::attrs(fnc_core::flags::attr::DIRECTORY),
            EntryFlags::ROOT,
        )?;
        self.volumes[vol.idx()].roots.push(root);
        self.frn_maps[vol.idx()].insert(frn, root);
        self.children.add(EntryId::NONE, root);
        self.bump(true);
        Ok(root)
    }

    /// All volumes.
    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    /// Volume by id.
    pub fn volume_info(&self, id: VolumeId) -> Option<&Volume> {
        self.volumes.get(id.idx())
    }

    /// Mutable volume record (USN bookkeeping, online state).
    pub fn volume_mut(&mut self, id: VolumeId) -> Option<&mut Volume> {
        self.generation += 1;
        self.volumes.get_mut(id.idx())
    }

    // ------------------------------------------------------------------ rows

    /// Appends one row; does not touch the FRN map or children index.
    pub(crate) fn push_row(
        &mut self,
        vol: VolumeId,
        frn: u64,
        parent: EntryId,
        name: &[u16],
        meta: &Meta,
        extra: EntryFlags,
    ) -> Result<EntryId> {
        let id = u32::try_from(self.parent.len()).ok().filter(|&i| i != u32::MAX);
        let id = EntryId(id.ok_or(IndexError::Full)?);
        self.names.push(name).map_err(IndexError::Core)?;
        self.scratch.clear();
        fold_utf16_into(name, &mut self.scratch);
        self.folds.push(&self.scratch).map_err(IndexError::Core)?;
        let attributes = meta.attributes.unwrap_or(0);
        let flags = EntryFlags::from_attributes(attributes).union(extra);
        let ext = if flags.contains(EntryFlags::IS_DIR) { NO_EXT } else { self.ext_dict.intern(ext_of(&self.scratch)) };
        self.parent.push(parent);
        self.volume.push(vol);
        self.flags.push(flags);
        self.frn.push(frn);
        self.ext.push(ext);
        self.size.push(meta.size.unwrap_or(SIZE_UNKNOWN));
        self.dm.push(meta.date_modified.unwrap_or(FileTime::UNKNOWN));
        self.dc.push(meta.date_created.unwrap_or(FileTime::UNKNOWN));
        if let Some(da) = &mut self.da {
            da.push(meta.date_accessed.unwrap_or(FileTime::UNKNOWN));
        }
        self.attr.push(attributes);
        self.live += 1;
        if flags.contains(EntryFlags::IS_DIR) {
            self.folders += 1;
        }
        Ok(id)
    }

    fn bump(&mut self, tree: bool) {
        self.generation += 1;
        if tree {
            self.tree_generation += 1;
        }
    }

    /// Number of rows (including tombstones). Valid ids are `0..len()`.
    #[inline]
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    /// `true` if there are no rows.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// Number of live entries.
    pub fn live_count(&self) -> usize {
        self.live
    }

    /// Planner statistics.
    pub fn stats(&self) -> IndexStats {
        IndexStats { rows: self.len(), live: self.live, folders: self.folders }
    }

    /// Incremented by every mutation; result sets record it to detect staleness.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Incremented when folder names or the tree shape change (path caches key on it).
    #[inline]
    pub fn tree_generation(&self) -> u64 {
        self.tree_generation
    }

    /// Identity of this id space: changes when ids are renumbered ([`IndexStore::compact`])
    /// or the store is cloned/replaced, so caches keyed by `(instance, generation)` are safe.
    #[inline]
    pub fn instance(&self) -> u64 {
        self.instance
    }

    // ------------------------------------------------------------------ accessors

    /// UTF-16 name.
    #[inline]
    pub fn name(&self, id: EntryId) -> &[u16] {
        self.names.get(id.idx())
    }

    /// Appends the name to `out`.
    pub fn name_into(&self, id: EntryId, out: &mut String) {
        utf16_to_string_into(self.name(id), out);
    }

    /// Name as a new `String` (UI/tests; not for hot paths).
    pub fn name_string(&self, id: EntryId) -> String {
        let mut s = String::new();
        self.name_into(id, &mut s);
        s
    }

    /// Fold key (UTF-8).
    #[inline]
    pub fn fold(&self, id: EntryId) -> &[u8] {
        self.folds.get(id.idx())
    }

    /// Parent entry ([`EntryId::NONE`] for roots).
    #[inline]
    pub fn parent(&self, id: EntryId) -> EntryId {
        self.parent[id.idx()]
    }

    /// Volume.
    #[inline]
    pub fn volume_of(&self, id: EntryId) -> VolumeId {
        self.volume[id.idx()]
    }

    /// Flags.
    #[inline]
    pub fn flags(&self, id: EntryId) -> EntryFlags {
        self.flags[id.idx()]
    }

    /// Whole flag column (scan kernels).
    #[inline]
    pub fn flags_column(&self) -> &[EntryFlags] {
        &self.flags
    }

    /// `true` for folders.
    #[inline]
    pub fn is_dir(&self, id: EntryId) -> bool {
        self.flags[id.idx()].contains(EntryFlags::IS_DIR)
    }

    /// `true` unless tombstoned.
    #[inline]
    pub fn is_live(&self, id: EntryId) -> bool {
        id.idx() < self.flags.len() && !self.flags[id.idx()].contains(EntryFlags::DELETED)
    }

    /// FRN.
    #[inline]
    pub fn frn(&self, id: EntryId) -> u64 {
        self.frn[id.idx()]
    }

    /// Extension id.
    #[inline]
    pub fn ext_id(&self, id: EntryId) -> u16 {
        self.ext[id.idx()]
    }

    /// Whole extension column.
    #[inline]
    pub fn ext_column(&self) -> &[u16] {
        &self.ext
    }

    /// Extension dictionary.
    pub fn ext_dict(&self) -> &ExtDict {
        &self.ext_dict
    }

    /// Size in bytes (`None` if unknown).
    #[inline]
    pub fn size(&self, id: EntryId) -> Option<u64> {
        let s = self.size[id.idx()];
        (s != SIZE_UNKNOWN).then_some(s)
    }

    /// Whole size column ([`SIZE_UNKNOWN`] = unknown).
    #[inline]
    pub fn size_column(&self) -> &[u64] {
        &self.size
    }

    /// Date modified.
    #[inline]
    pub fn date_modified(&self, id: EntryId) -> FileTime {
        self.dm[id.idx()]
    }

    /// Whole date-modified column.
    #[inline]
    pub fn dm_column(&self) -> &[FileTime] {
        &self.dm
    }

    /// Date created.
    #[inline]
    pub fn date_created(&self, id: EntryId) -> FileTime {
        self.dc[id.idx()]
    }

    /// Whole date-created column.
    #[inline]
    pub fn dc_column(&self) -> &[FileTime] {
        &self.dc
    }

    /// Date accessed (unknown when the column is disabled).
    #[inline]
    pub fn date_accessed(&self, id: EntryId) -> FileTime {
        self.da.as_ref().map_or(FileTime::UNKNOWN, |c| c[id.idx()])
    }

    /// Raw Win32 attributes.
    #[inline]
    pub fn attributes(&self, id: EntryId) -> u32 {
        self.attr[id.idx()]
    }

    /// Run history.
    pub fn run_info(&self, id: EntryId) -> RunInfo {
        self.run.get(&id).copied().unwrap_or_default()
    }

    /// Date recently changed (index journal), unknown if never changed while indexed.
    pub fn date_recently_changed(&self, id: EntryId) -> FileTime {
        self.recent.get(&id).copied().unwrap_or(FileTime::UNKNOWN)
    }

    /// Fast-sort index for names.
    pub fn name_order(&self) -> &NameOrder {
        &self.name_order
    }

    /// Depth (volume root = 0).
    pub fn depth(&self, id: EntryId) -> usize {
        let mut d = 0;
        let mut cur = self.parent(id);
        while cur.is_some() && d < MAX_DEPTH {
            d += 1;
            cur = self.parent(cur);
        }
        d
    }

    /// Path separator of the entry's volume.
    #[inline]
    pub fn separator(&self, id: EntryId) -> char {
        self.volumes.get(self.volume_of(id).idx()).map_or('\\', |v| v.info.separator)
    }

    /// Live children of `dir`.
    pub fn children(&self, dir: EntryId) -> impl Iterator<Item = EntryId> + '_ {
        self.children.candidates(dir).filter(move |&c| self.parent(c) == dir && self.is_live(c))
    }

    /// Visits `root` and all live descendants (depth-first, iterative).
    pub fn for_each_descendant(&self, root: EntryId, mut f: impl FnMut(EntryId)) {
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            f(d);
            if self.is_dir(d) {
                stack.extend(self.children(d));
            }
        }
    }

    /// Walks the ancestor chain of `id` root-first, calling `f` for each node
    /// (including `id`). Uses a fixed stack buffer, so it never allocates; chains
    /// deeper than the buffer recurse (bounded by [`MAX_DEPTH`]).
    fn visit_chain(&self, id: EntryId, budget: usize, f: &mut impl FnMut(EntryId)) {
        const CHUNK: usize = 64;
        let mut stack = [EntryId::NONE; CHUNK];
        let mut n = 0;
        let mut cur = id;
        while cur.is_some() && n < CHUNK {
            stack[n] = cur;
            n += 1;
            cur = self.parent(cur);
        }
        if cur.is_some() && budget > CHUNK {
            self.visit_chain(cur, budget - CHUNK, f);
        }
        for &node in stack[..n].iter().rev() {
            f(node);
        }
    }

    /// Writes the full path of `id` into `buf` (cleared first). Allocation-free once
    /// `buf` has grown.
    pub fn full_path(&self, id: EntryId, buf: &mut String) {
        buf.clear();
        self.write_path(id, buf);
    }

    /// Appends the full path of `id` to `buf`.
    pub fn write_path(&self, id: EntryId, buf: &mut String) {
        let sep = self.separator(id);
        let start = buf.len();
        self.visit_chain(id, MAX_DEPTH, &mut |node| {
            if buf.len() > start && !buf.ends_with(sep) {
                buf.push(sep);
            }
            utf16_to_string_into(self.name(node), buf);
        });
    }

    /// Writes the folded full path of `id` into `buf` (cleared first).
    pub fn full_path_fold(&self, id: EntryId, buf: &mut Vec<u8>) {
        buf.clear();
        let sep = self.separator(id) as u8;
        self.visit_chain(id, MAX_DEPTH, &mut |node| {
            if !buf.is_empty() && buf.last() != Some(&sep) {
                buf.push(sep);
            }
            buf.extend_from_slice(self.fold(node));
        });
    }

    /// Path of the parent folder of `id` (empty for roots).
    pub fn parent_path(&self, id: EntryId, buf: &mut String) {
        buf.clear();
        let p = self.parent(id);
        if p.is_some() {
            self.write_path(p, buf);
        }
    }

    /// Total natural order of two entries by name.
    #[inline]
    pub fn cmp_names(&self, a: EntryId, b: EntryId) -> std::cmp::Ordering {
        name_cmp(self.fold(a), self.name(a), self.fold(b), self.name(b)).then(a.cmp(&b))
    }

    /// Resolves an [`EntryRef`] to a live entry.
    pub fn resolve(&self, r: EntryRef) -> Option<EntryId> {
        match r {
            EntryRef::Id(id) => self.is_live(id).then_some(id),
            EntryRef::Frn(vol, frn) => {
                let id = self.frn_maps.get(vol.idx())?.get(frn)?;
                (self.is_live(id) && self.frn[id.idx()] == frn && self.volume[id.idx()] == vol).then_some(id)
            }
        }
    }

    /// Looks up a direct child of `dir` by folded name.
    pub fn find_child(&self, dir: EntryId, fold: &[u8]) -> Option<EntryId> {
        self.children(dir).find(|&c| self.fold(c) == fold)
    }

    /// Heap memory used by the index (approximate, bytes).
    pub fn heap_bytes(&self) -> usize {
        self.names.heap_bytes()
            + self.folds.heap_bytes()
            + self.parent.capacity() * 4
            + self.volume.capacity() * 2
            + self.flags.capacity() * 2
            + self.frn.capacity() * 8
            + self.ext.capacity() * 2
            + self.size.capacity() * 8
            + self.dm.capacity() * 8
            + self.dc.capacity() * 8
            + self.da.as_ref().map_or(0, |c| c.capacity() * 8)
            + self.attr.capacity() * 4
            + self.children.heap_bytes()
            + self.frn_maps.iter().map(FrnMap::heap_bytes).sum::<usize>()
            + self.name_order.heap_bytes()
            + (self.run.capacity() + self.recent.capacity()) * 24
    }

    // ------------------------------------------------------------------ bulk build

    /// Starts a bulk load into `vol` (initial MFT enumeration, folder walk, EFU).
    pub fn bulk_load(&mut self, vol: VolumeId) -> BulkLoader<'_> {
        let first = self.len();
        BulkLoader { store: self, vol, first, pending: Vec::new() }
    }

    /// Rebuilds the children index from the parent column.
    pub fn rebuild_children(&mut self) {
        let flags = &self.flags;
        self.children = Children::build(&self.parent, |i| !flags[i].contains(EntryFlags::DELETED));
    }

    /// Rebuilds the name fast-sort index (parallel sort; ~O(n log n)).
    pub fn rebuild_name_order(&mut self) {
        self.name_order = NameOrder::build(self);
    }

    // ------------------------------------------------------------------ changes

    /// Applies a batch of changes. Callers keep batches small enough that the write
    /// lock is held < 5 ms (SPEC ch.0 §4.3).
    pub fn apply(&mut self, batch: ChangeBatch) -> ApplyStats {
        let mut stats = ApplyStats::default();
        let now = FileTime::now();
        let mut tree_changed = false;
        for change in batch {
            match change {
                Change::Create { volume, frn, parent, name, meta } => {
                    tree_changed |= self.apply_create(volume, frn, parent, &name, &meta, now, &mut stats);
                }
                Change::Delete(target) => match self.resolve(target) {
                    Some(id) => {
                        tree_changed |= self.is_dir(id);
                        stats.deleted += self.delete_subtree(id, now);
                    }
                    None => stats.unresolved += 1,
                },
                Change::Rename { target, new_name, new_parent } => {
                    match (self.resolve(target), self.resolve(new_parent)) {
                        (Some(id), Some(p)) if !self.flags(id).contains(EntryFlags::ROOT) => {
                            if self.is_ancestor_or_self(id, p) {
                                stats.unresolved += 1;
                                continue;
                            }
                            tree_changed |= self.is_dir(id);
                            self.rename(id, &new_name, p, now);
                            stats.renamed += 1;
                        }
                        _ => stats.unresolved += 1,
                    }
                }
                Change::Meta { target, meta } => match self.resolve(target) {
                    Some(id) => {
                        self.update_meta(id, &meta);
                        self.recent.insert(id, now);
                        stats.updated += 1;
                    }
                    None => stats.unresolved += 1,
                },
            }
        }
        if stats.changed() {
            self.bump(tree_changed);
        }
        stats
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_create(
        &mut self,
        volume: VolumeId,
        frn: u64,
        parent: EntryRef,
        name: &[u16],
        meta: &Meta,
        now: FileTime,
        stats: &mut ApplyStats,
    ) -> bool {
        if volume.idx() >= self.volumes.len() {
            stats.unresolved += 1;
            return false;
        }
        let Some(p) = self.resolve(parent) else {
            stats.unresolved += 1;
            return false;
        };
        // A create for an FRN we already know (re-reported record): treat as move+meta.
        if let Some(existing) = self.resolve(EntryRef::Frn(volume, frn)) {
            if !self.is_ancestor_or_self(existing, p) {
                self.rename(existing, name, p, now);
            }
            self.update_meta(existing, meta);
            stats.renamed += 1;
            return self.is_dir(existing);
        }
        match self.push_row(volume, frn, p, name, meta, EntryFlags::empty()) {
            Ok(id) => {
                self.frn_maps[volume.idx()].insert(frn, id);
                self.children.add(p, id);
                self.name_order.mark_dirty(id);
                self.recent.insert(id, now);
                stats.created += 1;
                self.is_dir(id)
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot insert entry");
                stats.unresolved += 1;
                false
            }
        }
    }

    fn is_ancestor_or_self(&self, ancestor: EntryId, mut node: EntryId) -> bool {
        let mut steps = 0;
        while node.is_some() && steps < MAX_DEPTH {
            if node == ancestor {
                return true;
            }
            node = self.parent(node);
            steps += 1;
        }
        false
    }

    fn rename(&mut self, id: EntryId, new_name: &[u16], new_parent: EntryId, now: FileTime) {
        if self.names.get(id.idx()) != new_name {
            if let Err(e) = self.names.replace(id.idx(), new_name) {
                tracing::warn!(error = %e, "rename: name too long");
                return;
            }
            self.scratch.clear();
            fold_utf16_into(new_name, &mut self.scratch);
            if let Err(e) = self.folds.replace(id.idx(), &self.scratch) {
                tracing::warn!(error = %e, "rename: fold key too long");
            }
            if !self.is_dir(id) {
                self.ext[id.idx()] = self.ext_dict.intern(ext_of(&self.scratch));
            }
            self.name_order.mark_dirty(id);
        }
        if self.parent[id.idx()] != new_parent {
            self.parent[id.idx()] = new_parent;
            self.children.add(new_parent, id);
        }
        self.recent.insert(id, now);
    }

    fn update_meta(&mut self, id: EntryId, meta: &Meta) {
        let i = id.idx();
        if let Some(a) = meta.attributes {
            let was_dir = self.flags[i].contains(EntryFlags::IS_DIR);
            self.attr[i] = a;
            let keep = EntryFlags(self.flags[i].0 & !EntryFlags::ATTRIBUTE_MASK.0);
            let mut f = EntryFlags::from_attributes(a).union(keep);
            // Directory-ness is fixed at creation (USN records never flip it).
            f.set(EntryFlags::IS_DIR, was_dir);
            self.flags[i] = f;
        }
        if let Some(s) = meta.size {
            self.size[i] = s;
        }
        if let Some(t) = meta.date_modified {
            self.dm[i] = t;
        }
        if let Some(t) = meta.date_created {
            self.dc[i] = t;
        }
        if let (Some(t), Some(da)) = (meta.date_accessed, self.da.as_mut()) {
            da[i] = t;
        }
    }

    /// Tombstones `id` and its subtree; returns the number of entries deleted.
    fn delete_subtree(&mut self, id: EntryId, now: FileTime) -> usize {
        let mut doomed = Vec::new();
        self.for_each_descendant(id, |d| doomed.push(d));
        for &d in &doomed {
            let i = d.idx();
            let vol = self.volume[i];
            self.frn_maps[vol.idx()].remove(self.frn[i], d);
            if self.flags[i].contains(EntryFlags::IS_DIR) {
                self.folders -= 1;
            }
            self.flags[i].set(EntryFlags::DELETED, true);
            self.names.clear_item(i);
            self.folds.clear_item(i);
            self.run.remove(&d);
            self.recent.insert(d, now);
            self.live -= 1;
        }
        doomed.len()
    }

    /// Records that `id` was opened.
    pub fn record_run(&mut self, id: EntryId) {
        if self.is_live(id) {
            let e = self.run.entry(id).or_default();
            e.count = e.count.saturating_add(1);
            e.last = FileTime::now();
            self.generation += 1;
        }
    }

    /// Sets the run count of `id`.
    pub fn set_run_count(&mut self, id: EntryId, count: u32) {
        if self.is_live(id) {
            self.run.entry(id).or_default().count = count;
            self.generation += 1;
        }
    }

    /// Marks every entry of `vol` online/offline.
    pub fn set_volume_online(&mut self, vol: VolumeId, online: bool) {
        if let Some(v) = self.volumes.get_mut(vol.idx()) {
            v.online = online;
        }
        for (i, f) in self.flags.iter_mut().enumerate() {
            if self.volume[i] == vol {
                f.set(EntryFlags::OFFLINE, !online || self.attr[i] & fnc_core::flags::attr::OFFLINE != 0);
            }
        }
        self.bump(false);
    }

    /// Removes tombstones and garbage, renumbering entries. Returns the old→new id map
    /// (`u32::MAX` for removed rows); result sets must be re-run afterwards.
    pub fn compact(&mut self) -> Vec<u32> {
        let mut remap = vec![u32::MAX; self.len()];
        let mut order = Vec::with_capacity(self.live);
        for (i, slot) in remap.iter_mut().enumerate() {
            if !self.flags[i].contains(EntryFlags::DELETED) {
                *slot = order.len() as u32;
                order.push(i as u32);
            }
        }
        fn pick<T: Copy>(col: &[T], order: &[u32]) -> Vec<T> {
            order.iter().map(|&i| col[i as usize]).collect()
        }
        let map_id = |id: EntryId| {
            if id.is_some() { EntryId(remap[id.idx()]) } else { EntryId::NONE }
        };
        self.names = self.names.permuted(&order);
        self.folds = self.folds.permuted(&order);
        self.parent = order.iter().map(|&i| map_id(self.parent[i as usize])).collect();
        self.volume = pick(&self.volume, &order);
        self.flags = pick(&self.flags, &order);
        self.frn = pick(&self.frn, &order);
        self.ext = pick(&self.ext, &order);
        self.size = pick(&self.size, &order);
        self.dm = pick(&self.dm, &order);
        self.dc = pick(&self.dc, &order);
        if let Some(da) = &self.da {
            self.da = Some(pick(da, &order));
        }
        self.attr = pick(&self.attr, &order);
        self.run = self.run.iter().filter(|(k, _)| remap[k.idx()] != u32::MAX).map(|(k, v)| (map_id(*k), *v)).collect();
        self.recent =
            self.recent.iter().filter(|(k, _)| remap[k.idx()] != u32::MAX).map(|(k, v)| (map_id(*k), *v)).collect();
        for v in &mut self.volumes {
            v.roots = v.roots.iter().map(|&r| map_id(r)).filter(|r| r.is_some()).collect();
        }
        self.orphans = self.orphans.iter().map(|o| o.map(map_id).filter(|e| e.is_some())).collect();
        for (vi, m) in self.frn_maps.iter_mut().enumerate() {
            let pairs: Vec<(u64, EntryId)> = (0..order.len())
                .filter(|&n| self.volume[n].idx() == vi)
                .map(|n| (self.frn[n], EntryId(n as u32)))
                .collect();
            let ntfs_like = matches!(self.volumes[vi].info.kind, VolumeKind::Ntfs);
            *m = FrnMap::build(&pairs, ntfs_like);
        }
        self.rebuild_children();
        self.name_order = self.name_order.remapped(&remap, order.len());
        self.instance = next_instance();
        self.bump(true);
        remap
    }

    fn orphans_dir(&mut self, vol: VolumeId) -> Result<EntryId> {
        if let Some(id) = self.orphans[vol.idx()].filter(|&id| self.is_live(id)) {
            return Ok(id);
        }
        let root = *self.volumes[vol.idx()].roots.first().ok_or(IndexError::NoRoot)?;
        let units: Vec<u16> = ORPHANS_NAME.encode_utf16().collect();
        let attrs = fnc_core::flags::attr::DIRECTORY | fnc_core::flags::attr::HIDDEN | fnc_core::flags::attr::SYSTEM;
        // Synthetic FRN outside the NTFS record range so it never collides.
        let id = self.push_row(vol, u64::MAX - 1, root, &units, &Meta::attrs(attrs), EntryFlags::empty())?;
        self.children.add(root, id);
        self.orphans[vol.idx()] = Some(id);
        Ok(id)
    }
}

/// Root entry name from a root path: `C:\` → `C:`, `/home/u/` → `/home/u`, `/` → `/`.
fn root_display_name(root_path: &str, sep: char) -> String {
    let trimmed = root_path.trim_end_matches(sep);
    if trimmed.is_empty() { root_path.to_string() } else { trimmed.to_string() }
}

/// Bulk loader returned by [`IndexStore::bulk_load`].
///
/// Records may arrive in any order (MFT order is not parent-first); parents are
/// resolved in [`BulkLoader::finish`].
pub struct BulkLoader<'a> {
    store: &'a mut IndexStore,
    vol: VolumeId,
    first: usize,
    pending: Vec<(EntryId, u64)>,
}

/// Result of a bulk load.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BulkStats {
    /// Entries inserted.
    pub entries: usize,
    /// Entries whose parent was missing (moved under `$Orphans`).
    pub orphans: usize,
}

impl BulkLoader<'_> {
    /// Volume being loaded.
    pub fn volume(&self) -> VolumeId {
        self.vol
    }

    /// Read access to the store being loaded.
    pub fn store(&self) -> &IndexStore {
        self.store
    }

    /// Adds one record. A record whose FRN equals a root's FRN updates that root.
    pub fn push(&mut self, frn: u64, parent_frn: u64, name: &[u16], meta: &Meta) -> Result<EntryId> {
        let vol = self.vol;
        if let Some(&root) = self.store.volumes[vol.idx()].roots.iter().find(|&&r| self.store.frn[r.idx()] == frn) {
            let mut m = *meta;
            m.attributes = m.attributes.map(|a| a | fnc_core::flags::attr::DIRECTORY);
            self.store.update_meta(root, &m);
            return Ok(root);
        }
        let id = self.store.push_row(vol, frn, EntryId::NONE, name, meta, EntryFlags::empty())?;
        self.pending.push((id, parent_frn));
        Ok(id)
    }

    /// Adds another root (file lists spanning several drives).
    pub fn add_root(&mut self, frn: u64, name: &str) -> Result<EntryId> {
        self.store.add_root(self.vol, frn, name)
    }

    /// Updates metadata of an entry pushed earlier.
    pub fn update(&mut self, id: EntryId, meta: &Meta) {
        self.store.update_meta(id, meta);
    }

    /// Number of records pushed so far.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// `true` if nothing was pushed.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Resolves parents, builds the FRN map and the children index.
    pub fn finish(self) -> Result<BulkStats> {
        let BulkLoader { store, vol, first, pending } = self;
        let ntfs_like = matches!(store.volumes[vol.idx()].info.kind, VolumeKind::Ntfs);
        let mut pairs: Vec<(u64, EntryId)> = (first..store.len())
            .filter(|&i| store.volume[i] == vol)
            .map(|i| (store.frn[i], EntryId(i as u32)))
            .collect();
        pairs.extend(
            store.volumes[vol.idx()].roots.iter().filter(|r| r.idx() < first).map(|&r| (store.frn[r.idx()], r)),
        );
        let mut map = FrnMap::build(&pairs, ntfs_like);
        let mut orphans = Vec::new();
        for &(id, pfrn) in &pending {
            let parent =
                map.get(pfrn).filter(|p| store.frn[p.idx()] == pfrn && store.volume[p.idx()] == vol && *p != id);
            match parent {
                Some(p) => store.parent[id.idx()] = p,
                None => orphans.push(id),
            }
        }
        if !orphans.is_empty() {
            let dir = store.orphans_dir(vol)?;
            map.insert(store.frn[dir.idx()], dir);
            for &id in &orphans {
                store.parent[id.idx()] = dir;
            }
        }
        // Break parent cycles (corrupt sources) by re-parenting to $Orphans.
        let cyclic: Vec<EntryId> =
            pending.iter().map(|&(id, _)| id).filter(|&id| store.depth(id) >= MAX_DEPTH).collect();
        if !cyclic.is_empty() {
            let dir = store.orphans_dir(vol)?;
            for &id in &cyclic {
                store.parent[id.idx()] = dir;
            }
        }
        store.frn_maps[vol.idx()] = map;
        store.rebuild_children();
        store.bump(true);
        Ok(BulkStats { entries: pending.len(), orphans: orphans.len() + cyclic.len() })
    }
}

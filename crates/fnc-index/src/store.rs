//! `IndexStore`: the columnar (SoA) entry table.

use std::collections::HashMap;

use fnc_core::arena::utf16_to_string_into;
use fnc_core::fold::fold_utf16_into;
use fnc_core::natsort::name_cmp;
use fnc_core::{EntryFlags, EntryId, FileTime, FoldArena, NameArena, VolumeId};

use crate::change::{ApplyStats, Change, ChangeBatch, EntryRef, Meta, PathChange};
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

/// Synthetic FRN of the `$Orphans` folder (outside any NTFS record range).
pub const ORPHANS_FRN: u64 = u64::MAX - 1;

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
    /// Additional (hard-link) names per file: `(volume, frn)` → alias rows. The FRN map
    /// points at the primary row.
    pub(crate) links: HashMap<(VolumeId, u64), Vec<EntryId>>,
    /// Next synthetic FRN per volume (0 = not computed yet).
    next_frn: Vec<u64>,
    /// `false` while applying initial metadata (not changes: no index-journal entry).
    record_recent: bool,
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

impl Clone for IndexStore {
    /// A clone is an independent id space: it gets a fresh [`IndexStore::instance`], so
    /// caches keyed by `(instance, generation)` never mix up diverging copies.
    fn clone(&self) -> Self {
        Self {
            volumes: self.volumes.clone(),
            names: self.names.clone(),
            folds: self.folds.clone(),
            parent: self.parent.clone(),
            volume: self.volume.clone(),
            flags: self.flags.clone(),
            frn: self.frn.clone(),
            ext: self.ext.clone(),
            size: self.size.clone(),
            dm: self.dm.clone(),
            dc: self.dc.clone(),
            da: self.da.clone(),
            attr: self.attr.clone(),
            run: self.run.clone(),
            recent: self.recent.clone(),
            ext_dict: self.ext_dict.clone(),
            children: self.children.clone(),
            frn_maps: self.frn_maps.clone(),
            orphans: self.orphans.clone(),
            name_order: self.name_order.clone(),
            links: self.links.clone(),
            next_frn: self.next_frn.clone(),
            record_recent: true,
            generation: self.generation,
            tree_generation: self.tree_generation,
            instance: next_instance(),
            live: self.live,
            folders: self.folders,
            scratch: Vec::new(),
        }
    }
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
            links: HashMap::new(),
            next_frn: Vec::new(),
            record_recent: true,
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

    /// Releases excess capacity of all columns (call after bulk loading).
    pub fn shrink_to_fit(&mut self) {
        self.names.shrink_to_fit();
        self.folds.shrink_to_fit();
        self.parent.shrink_to_fit();
        self.volume.shrink_to_fit();
        self.flags.shrink_to_fit();
        self.frn.shrink_to_fit();
        self.ext.shrink_to_fit();
        self.size.shrink_to_fit();
        self.dm.shrink_to_fit();
        self.dc.shrink_to_fit();
        if let Some(da) = &mut self.da {
            da.shrink_to_fit();
        }
        self.attr.shrink_to_fit();
        self.recent.shrink_to_fit();
        self.links.shrink_to_fit();
        self.scratch = Vec::new();
    }

    // ------------------------------------------------------------------ volumes

    /// Registers a volume. With `root_frn = Some(frn)` its root entry (named after
    /// `root_path`, e.g. `C:`) is created with that FRN (`5` on NTFS); file lists pass
    /// `None` and add one root per drive with [`IndexStore::add_root`].
    pub fn add_volume(&mut self, info: VolumeInfo, root_frn: Option<u64>) -> Result<VolumeId> {
        let root_name = root_display_name(&info.root_path, info.separator);
        let reuse = self.volumes.iter().position(Volume::is_retired);
        let id = VolumeId(u16::try_from(reuse.unwrap_or(self.volumes.len())).map_err(|_| IndexError::TooManyVolumes)?);
        let v = Volume {
            id,
            info,
            roots: Vec::new(),
            usn_journal_id: 0,
            next_usn: 0,
            online: true,
            include_in_index: true,
            metadata_complete: false,
        };
        match reuse {
            Some(i) => {
                self.volumes[i] = v;
                self.frn_maps[i] = FrnMap::default();
                self.orphans[i] = None;
                if let Some(n) = self.next_frn.get_mut(i) {
                    *n = 0;
                }
            }
            None => {
                self.volumes.push(v);
                self.frn_maps.push(FrnMap::default());
                self.orphans.push(None);
            }
        }
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

    /// Records the journal position reached for `id` without invalidating results.
    pub fn set_journal_position(&mut self, id: VolumeId, journal_id: u64, next_usn: i64) {
        if let Some(v) = self.volumes.get_mut(id.idx()) {
            v.usn_journal_id = journal_id;
            v.next_usn = next_usn;
        }
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

    /// Visits `root` and all live descendants (depth-first, iterative). Visits at most
    /// `len()` entries, so even a corrupt (cyclic) tree cannot loop forever.
    pub fn for_each_descendant(&self, root: EntryId, mut f: impl FnMut(EntryId)) {
        let mut stack = vec![root];
        let mut budget = self.len();
        while let Some(d) = stack.pop() {
            if budget == 0 {
                tracing::error!(?root, "descendant walk exceeded the entry count (cycle?)");
                return;
            }
            budget -= 1;
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

    /// The live entry at the full path `path` (`\\` or `/` separators, case-insensitive),
    /// if it is indexed.
    pub fn find_path(&self, path: &str) -> Option<EntryId> {
        let path = path.trim_end_matches(['\\', '/']);
        let mut root_path = String::new();
        let mut fold = Vec::new();
        let mut units = Vec::new();
        for v in &self.volumes {
            for &root in &v.roots {
                if !self.is_live(root) {
                    continue;
                }
                self.full_path(root, &mut root_path);
                let r = root_path.trim_end_matches(['\\', '/']);
                let Some(head) = path.get(..r.len()) else { continue };
                if head.to_lowercase() != r.to_lowercase() {
                    continue;
                }
                let rest = &path[r.len()..];
                if !rest.is_empty() && !rest.starts_with(['\\', '/']) {
                    continue;
                }
                let mut cur = Some(root);
                for comp in rest.split(['\\', '/']).filter(|c| !c.is_empty()) {
                    units.clear();
                    units.extend(comp.encode_utf16());
                    fold.clear();
                    fnc_core::fold::fold_utf16_into(&units, &mut fold);
                    cur = cur
                        .and_then(|d| self.children(d).find(|&c| self.is_live(c) && self.fold(c) == fold.as_slice()));
                }
                if cur.is_some() {
                    return cur;
                }
            }
        }
        None
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

    /// Like [`IndexStore::apply`] for metadata that completes the initial index (size and
    /// date back-fill, hard-link names found while listing): the entries did not change,
    /// so nothing is recorded in the index journal (`date-recently-changed:`).
    pub fn apply_initial(&mut self, batch: ChangeBatch) -> ApplyStats {
        self.record_recent = false;
        let stats = self.apply(batch);
        self.record_recent = true;
        stats
    }

    #[inline]
    fn mark_recent(&mut self, id: EntryId, now: FileTime) {
        if self.record_recent {
            self.recent.insert(id, now);
        }
    }

    /// Applies a batch of changes. Callers keep batches small enough that the write
    /// lock is held < 5 ms (SPEC ch.0 §4.3).
    ///
    /// Changes whose target or parent is not (yet) known are retried after the rest of
    /// the batch, repeatedly while progress is made, so a batch does not need to be
    /// ordered parents-first (e.g. a file moved into a folder created later in the
    /// same USN batch).
    pub fn apply(&mut self, batch: ChangeBatch) -> ApplyStats {
        let mut stats = ApplyStats::default();
        let now = FileTime::now();
        let mut tree_changed = false;
        let mut pending = batch;
        loop {
            let before = pending.len();
            let mut deferred = Vec::new();
            for change in pending {
                if self.is_resolvable(&change) {
                    tree_changed |= self.apply_one(change, now, &mut stats);
                } else {
                    deferred.push(change);
                }
            }
            if deferred.is_empty() || deferred.len() == before {
                stats.unresolved += deferred.len();
                break;
            }
            pending = deferred;
        }
        if stats.changed() {
            self.bump(tree_changed);
        }
        stats
    }

    /// `true` if every entry the change refers to exists.
    fn is_resolvable(&self, change: &Change) -> bool {
        match change {
            Change::Create { parent, .. } | Change::Touch { parent, .. } => self.resolve(*parent).is_some(),
            Change::Delete(t) | Change::Meta { target: t, .. } => self.resolve(*t).is_some(),
            Change::Rename { target, new_parent, .. } => {
                self.resolve(*target).is_some() && self.resolve(*new_parent).is_some()
            }
            Change::Path { volume, change } => match change {
                PathChange::Upsert { path, .. } | PathChange::Remove { path } => {
                    path.split_last().is_some_and(|(_, dir)| self.resolve_path(*volume, dir).is_some())
                }
                PathChange::Rename { from, to } => {
                    self.resolve_path(*volume, from).is_some()
                        && to.split_last().is_some_and(|(_, dir)| self.resolve_path(*volume, dir).is_some())
                }
                PathChange::SyncDir { dir, .. } => self.resolve_path(*volume, dir).is_some(),
            },
        }
    }

    /// The entry at `path` below the root of `vol` (empty path = the first root).
    pub fn resolve_path(&self, vol: VolumeId, path: &[Box<[u16]>]) -> Option<EntryId> {
        let mut cur = *self.volumes.get(vol.idx())?.roots.first()?;
        let mut fold = Vec::new();
        for c in path {
            fold.clear();
            fold_utf16_into(c, &mut fold);
            cur = self.find_child(cur, &fold)?;
        }
        Some(cur)
    }

    /// A fresh synthetic FRN for `vol` (folder indexes number entries sequentially).
    fn next_synthetic_frn(&mut self, vol: VolumeId) -> u64 {
        let v = vol.idx();
        if self.next_frn.len() <= v {
            self.next_frn.resize(v + 1, 0);
        }
        if self.next_frn[v] == 0 {
            let max = (0..self.len())
                .filter(|&i| self.volume[i] == vol && self.frn[i] != ORPHANS_FRN)
                .map(|i| self.frn[i])
                .max()
                .unwrap_or(0);
            self.next_frn[v] = max + 1;
        }
        let f = self.next_frn[v];
        self.next_frn[v] += 1;
        f
    }

    fn apply_path(&mut self, vol: VolumeId, change: PathChange, now: FileTime, stats: &mut ApplyStats) -> bool {
        match change {
            PathChange::Upsert { path, meta } => {
                let Some((name, dir)) = path.split_last() else { return false };
                let Some(parent) = self.resolve_path(vol, dir) else {
                    stats.unresolved += 1;
                    return false;
                };
                self.upsert_child(vol, parent, name, &meta, now, stats)
            }
            PathChange::Remove { path } => match self.resolve_path(vol, &path) {
                Some(id) if !self.flags(id).contains(EntryFlags::ROOT) => {
                    let dir = self.is_dir(id);
                    stats.deleted += self.delete_subtree(id, now);
                    dir
                }
                _ => {
                    stats.unresolved += 1;
                    false
                }
            },
            PathChange::Rename { from, to } => {
                let (Some(id), Some((name, dir))) = (self.resolve_path(vol, &from), to.split_last()) else {
                    stats.unresolved += 1;
                    return false;
                };
                let Some(p) = self.resolve_path(vol, dir) else {
                    stats.unresolved += 1;
                    return false;
                };
                if self.flags(id).contains(EntryFlags::ROOT) || self.is_ancestor_or_self(id, p) {
                    stats.unresolved += 1;
                    return false;
                }
                // Renaming onto an existing name replaces it (as the file system did).
                let mut fold = Vec::new();
                fold_utf16_into(name, &mut fold);
                if let Some(old) = self.find_child(p, &fold).filter(|&o| o != id) {
                    stats.deleted += self.delete_subtree(old, now);
                }
                self.rename(id, name, p, now);
                stats.renamed += 1;
                self.is_dir(id)
            }
            PathChange::SyncDir { dir, children } => {
                let Some(d) = self.resolve_path(vol, &dir) else {
                    stats.unresolved += 1;
                    return false;
                };
                let mut tree = false;
                let mut fold = Vec::new();
                let mut seen: std::collections::HashSet<Vec<u8>> =
                    std::collections::HashSet::with_capacity(children.len());
                for (name, meta) in &children {
                    fold.clear();
                    fold_utf16_into(name, &mut fold);
                    seen.insert(fold.clone());
                    tree |= self.upsert_child(vol, d, name, meta, now, stats);
                }
                let gone: Vec<EntryId> =
                    self.children(d).filter(|&c| self.is_live(c) && !seen.contains(self.fold(c))).collect();
                for g in gone {
                    tree |= self.is_dir(g);
                    stats.deleted += self.delete_subtree(g, now);
                }
                tree
            }
        }
    }

    /// Creates or updates child `name` of `parent`. A file that became a folder (or the
    /// reverse) is replaced.
    fn upsert_child(
        &mut self,
        vol: VolumeId,
        parent: EntryId,
        name: &[u16],
        meta: &Meta,
        now: FileTime,
        stats: &mut ApplyStats,
    ) -> bool {
        let mut fold = Vec::new();
        fold_utf16_into(name, &mut fold);
        let want_dir = meta.attributes.is_some_and(|a| a & fnc_core::flags::attr::DIRECTORY != 0);
        if let Some(id) = self.find_child(parent, &fold) {
            if self.is_dir(id) == want_dir || meta.attributes.is_none() {
                let changed = self.size(id) != meta.size.or(self.size(id))
                    || meta.date_modified.is_some_and(|t| t != self.dm[id.idx()])
                    || meta.attributes.is_some_and(|a| a != self.attr[id.idx()])
                    || self.names.get(id.idx()) != name;
                if self.names.get(id.idx()) != name {
                    // Case-only rename.
                    self.rename(id, name, parent, now);
                }
                self.update_meta(id, meta);
                if changed {
                    self.mark_recent(id, now);
                    stats.updated += 1;
                }
                return false;
            }
            stats.deleted += self.delete_subtree(id, now);
        }
        let frn = self.next_synthetic_frn(vol);
        match self.push_row(vol, frn, parent, name, meta, EntryFlags::empty()) {
            Ok(id) => {
                self.frn_maps[vol.idx()].insert(frn, id);
                self.children.add(parent, id);
                self.name_order.mark_dirty(id);
                self.mark_recent(id, now);
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

    /// Applies one resolvable change; returns `true` if the folder tree changed.
    fn apply_one(&mut self, change: Change, now: FileTime, stats: &mut ApplyStats) -> bool {
        let mut tree_changed = false;
        match change {
            Change::Create { volume, frn, parent, name, meta } => {
                tree_changed |= self.apply_create(volume, frn, parent, &name, &meta, now, stats);
            }
            Change::Delete(target) => match self.resolve(target) {
                Some(id) => {
                    tree_changed |= self.is_dir(id);
                    // The file is gone: all of its names go with it.
                    for n in self.names_of(id) {
                        if self.is_live(n) {
                            stats.deleted += self.delete_subtree(n, now);
                        }
                    }
                }
                None => stats.unresolved += 1,
            },
            Change::Rename { target, new_name, new_parent } => match (self.resolve(target), self.resolve(new_parent)) {
                (Some(id), Some(p)) if !self.flags(id).contains(EntryFlags::ROOT) => {
                    if self.is_ancestor_or_self(id, p) {
                        stats.unresolved += 1;
                    } else {
                        tree_changed |= self.is_dir(id);
                        self.rename(id, &new_name, p, now);
                        stats.renamed += 1;
                    }
                }
                _ => stats.unresolved += 1,
            },
            Change::Meta { target, meta } => match self.resolve(target) {
                Some(id) => {
                    // Every name of a hard-linked file shares its metadata.
                    for n in self.names_of(id) {
                        self.update_meta(n, &meta);
                        self.mark_recent(n, now);
                    }
                    stats.updated += 1;
                }
                None => stats.unresolved += 1,
            },
            Change::Touch { volume, frn, parent, name, meta } => {
                tree_changed |= self.apply_touch(volume, frn, parent, &name, &meta, now, stats);
            }
            Change::Path { volume, change } => tree_changed |= self.apply_path(volume, change, now, stats),
        }
        tree_changed
    }

    /// The primary entry of `id`'s file followed by its live hard-link aliases.
    fn names_of(&self, id: EntryId) -> Vec<EntryId> {
        let key = (self.volume[id.idx()], self.frn[id.idx()]);
        let mut v = vec![id];
        if let Some(links) = self.links.get(&key) {
            v.extend(links.iter().copied().filter(|&l| l != id && self.is_live(l)));
        }
        v
    }

    /// Entry of file `frn` with name `fold` in folder `p` (primary or alias).
    fn find_name(&self, primary: EntryId, p: EntryId, name: &[u16]) -> Option<EntryId> {
        let mut fold = Vec::with_capacity(name.len());
        fold_utf16_into(name, &mut fold);
        self.names_of(primary).into_iter().find(|&n| self.parent(n) == p && self.fold(n) == fold.as_slice())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_touch(
        &mut self,
        volume: VolumeId,
        frn: u64,
        parent: EntryRef,
        name: &[u16],
        meta: &Meta,
        now: FileTime,
        stats: &mut ApplyStats,
    ) -> bool {
        let (Some(primary), Some(p)) = (self.resolve(EntryRef::Frn(volume, frn)), self.resolve(parent)) else {
            // Unknown file: insert it (or report the missing parent).
            return self.apply_create(volume, frn, parent, name, meta, now, stats);
        };
        if self.find_name(primary, p, name).is_some() {
            for n in self.names_of(primary) {
                self.update_meta(n, meta);
            }
            stats.updated += 1;
            return false;
        }
        if self.is_dir(primary) {
            // Folders cannot have hard links: this is a rename we have not seen.
            return self.apply_create(volume, frn, parent, name, meta, now, stats);
        }
        // A new name of a known file: add a hard-link alias carrying the file's metadata.
        let i = primary.idx();
        let shared = Meta {
            attributes: meta.attributes.or(Some(self.attr[i])),
            size: meta.size.or((self.size[i] != SIZE_UNKNOWN).then_some(self.size[i])),
            date_modified: meta.date_modified.or(self.dm[i].is_known().then_some(self.dm[i])),
            date_created: meta.date_created.or(self.dc[i].is_known().then_some(self.dc[i])),
            date_accessed: meta.date_accessed,
        };
        match self.push_row(volume, frn, p, name, &shared, EntryFlags::HARDLINK_ALIAS) {
            Ok(id) => {
                self.links.entry((volume, frn)).or_default().push(id);
                self.children.add(p, id);
                self.name_order.mark_dirty(id);
                self.mark_recent(id, now);
                stats.created += 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot insert hard-link alias");
                stats.unresolved += 1;
            }
        }
        false
    }

    /// Number of names (hard links) of the file of `id`.
    pub fn hardlink_count(&self, id: EntryId) -> usize {
        self.names_of(id).len()
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
        // A create for an FRN we already know (re-reported record): treat as move+meta,
        // unless the name is one the file already has (e.g. one of its hard links).
        if let Some(existing) = self.resolve(EntryRef::Frn(volume, frn)) {
            if self.find_name(existing, p, name).is_none() && !self.is_ancestor_or_self(existing, p) {
                self.rename(existing, name, p, now);
            }
            for n in self.names_of(existing) {
                self.update_meta(n, meta);
            }
            stats.renamed += 1;
            return self.is_dir(existing);
        }
        match self.push_row(volume, frn, p, name, meta, EntryFlags::empty()) {
            Ok(id) => {
                self.frn_maps[volume.idx()].insert(frn, id);
                self.children.add(p, id);
                self.name_order.mark_dirty(id);
                self.mark_recent(id, now);
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

    /// `true` if `ancestor` is `node` or one of its ancestors. Chains deeper than
    /// [`MAX_DEPTH`] are treated as "yes" so a move can never create a cycle.
    fn is_ancestor_or_self(&self, ancestor: EntryId, mut node: EntryId) -> bool {
        let mut steps = 0;
        while node.is_some() {
            if node == ancestor || steps >= MAX_DEPTH {
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
        self.mark_recent(id, now);
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
            let key = (vol, self.frn[i]);
            if let Some(links) = self.links.get_mut(&key) {
                links.retain(|&l| l != d && !self.flags[l.idx()].contains(EntryFlags::DELETED));
                // A deleted primary hands the FRN over to a surviving alias.
                if !self.flags[i].contains(EntryFlags::HARDLINK_ALIAS)
                    && let Some(&heir) = links.first()
                {
                    links.remove(0);
                    self.flags[heir.idx()].set(EntryFlags::HARDLINK_ALIAS, false);
                    self.frn_maps[vol.idx()].insert(key.1, heir);
                }
                if links.is_empty() {
                    self.links.remove(&key);
                }
            }
            self.frn_maps[vol.idx()].remove(self.frn[i], d);
            if self.flags[i].contains(EntryFlags::IS_DIR) {
                self.folders -= 1;
            }
            self.flags[i].set(EntryFlags::DELETED, true);
            self.names.clear_item(i);
            self.folds.clear_item(i);
            self.run.remove(&d);
            self.mark_recent(d, now);
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
        self.links = std::mem::take(&mut self.links)
            .into_iter()
            .filter_map(|(k, v)| {
                let v: Vec<EntryId> = v.into_iter().map(map_id).filter(|e| e.is_some()).collect();
                (!v.is_empty()).then_some((k, v))
            })
            .collect();
        for v in &mut self.volumes {
            v.roots = v.roots.iter().map(|&r| map_id(r)).filter(|r| r.is_some()).collect();
        }
        self.orphans = self.orphans.iter().map(|o| o.map(map_id).filter(|e| e.is_some())).collect();
        self.rebuild_frn_maps();
        self.rebuild_children();
        self.name_order = self.name_order.remapped(&remap, order.len());
        self.instance = next_instance();
        self.bump(true);
        remap
    }

    /// Rebuilds every volume's FRN map from the columns (live primary rows only).
    fn rebuild_frn_maps(&mut self) {
        let mut per_vol: Vec<Vec<(u64, EntryId)>> = vec![Vec::new(); self.volumes.len()];
        let mut orphans: Vec<(usize, EntryId)> = Vec::new();
        for n in 0..self.len() {
            let f = self.flags[n];
            if f.contains(EntryFlags::DELETED) || f.contains(EntryFlags::HARDLINK_ALIAS) {
                continue;
            }
            let v = self.volume[n].idx();
            // The `$Orphans` folder's synthetic FRN would force the hash-map
            // representation; it is inserted afterwards (into the overflow map).
            if self.frn[n] == ORPHANS_FRN {
                orphans.push((v, EntryId(n as u32)));
            } else {
                per_vol[v].push((self.frn[n], EntryId(n as u32)));
            }
        }
        self.frn_maps = per_vol
            .iter()
            .enumerate()
            .map(|(vi, pairs)| FrnMap::build(pairs, matches!(self.volumes[vi].info.kind, VolumeKind::Ntfs)))
            .collect();
        for (v, id) in orphans {
            self.frn_maps[v].insert(ORPHANS_FRN, id);
        }
    }

    /// Completes a store whose columns were filled directly (database load): derived
    /// maps, counters and the name order (`ranks` if persisted, else rebuilt).
    pub(crate) fn finish_load(&mut self, ranks: Option<NameOrder>) {
        self.live = self.flags.iter().filter(|f| !f.contains(EntryFlags::DELETED)).count();
        self.folders =
            self.flags.iter().filter(|f| !f.contains(EntryFlags::DELETED) && f.contains(EntryFlags::IS_DIR)).count();
        self.rebuild_frn_maps();
        self.rebuild_children();
        match ranks {
            Some(r) => self.name_order = r,
            None => self.rebuild_name_order(),
        }
        self.bump(true);
    }

    /// Deletes every entry of `vol` except its roots (before re-enumerating it); the
    /// FRN map keeps only the roots. Returns the number of entries removed.
    pub fn clear_volume(&mut self, vol: VolumeId) -> usize {
        let Some(v) = self.volumes.get(vol.idx()) else { return 0 };
        let roots = v.roots.clone();
        let now = FileTime::now();
        let mut removed = 0;
        for root in &roots {
            let kids: Vec<EntryId> = self.children(*root).collect();
            for k in kids {
                if self.is_live(k) {
                    removed += self.delete_subtree(k, now);
                }
            }
        }
        // Rows the tree walk could not reach (corrupt parents) go as well.
        for n in 0..self.len() {
            if self.volume[n] == vol
                && !self.flags[n].contains(EntryFlags::DELETED)
                && !roots.contains(&EntryId(n as u32))
            {
                removed += self.delete_subtree(EntryId(n as u32), now);
            }
        }
        self.recent.retain(|k, _| self.volume[k.idx()] != vol);
        self.orphans[vol.idx()] = None;
        if removed > 0 {
            self.bump(true);
        }
        removed
    }

    /// Removes a volume's content and marks it unusable (no longer present). Ids stay
    /// valid; the volume record remains (ids are positions).
    pub fn drop_volume(&mut self, vol: VolumeId) {
        self.clear_volume(vol);
        let roots = self.volumes.get(vol.idx()).map(|v| v.roots.clone()).unwrap_or_default();
        let now = FileTime::now();
        for r in roots {
            if self.is_live(r) {
                self.delete_subtree(r, now);
            }
        }
        if let Some(v) = self.volumes.get_mut(vol.idx()) {
            v.roots.clear();
            v.online = false;
            v.include_in_index = false;
        }
        self.bump(true);
    }

    /// Moves every volume of `other` (built privately, e.g. a volume that appeared
    /// while running) into this index; returns their new ids in `other`'s order.
    /// Existing ids stay valid; cost O(rows of `other`) plus a children rebuild.
    pub fn absorb(&mut self, mut other: IndexStore) -> Result<Vec<VolumeId>> {
        if other.live != other.len() {
            other.compact();
        }
        let offset = u32::try_from(self.len()).map_err(|_| IndexError::Full)?;
        if (offset as u64) + other.len() as u64 >= u32::MAX as u64 {
            return Err(IndexError::Full);
        }
        // Volumes (reusing retired records like `add_volume`).
        let mut vmap = Vec::with_capacity(other.volumes.len());
        for v in &other.volumes {
            let reuse =
                self.volumes.iter().position(Volume::is_retired).filter(|i| !vmap.contains(&VolumeId(*i as u16)));
            let id =
                VolumeId(u16::try_from(reuse.unwrap_or(self.volumes.len())).map_err(|_| IndexError::TooManyVolumes)?);
            let mut nv = v.clone();
            nv.id = id;
            nv.roots = v.roots.iter().map(|r| EntryId(r.0 + offset)).collect();
            match reuse {
                Some(i) => {
                    self.volumes[i] = nv;
                    self.frn_maps[i] = FrnMap::default();
                    self.orphans[i] = None;
                }
                None => {
                    self.volumes.push(nv);
                    self.frn_maps.push(FrnMap::default());
                    self.orphans.push(None);
                }
            }
            vmap.push(id);
        }
        for (i, o) in other.orphans.iter().enumerate() {
            self.orphans[vmap[i].idx()] = o.map(|e| EntryId(e.0 + offset));
        }
        let shift = |e: EntryId| if e.is_some() { EntryId(e.0 + offset) } else { EntryId::NONE };
        let n = other.len();
        self.reserve(n, other.names.units());
        for i in 0..n {
            self.names.push(other.names.get(i)).map_err(IndexError::Core)?;
            self.folds.push(other.folds.get(i)).map_err(IndexError::Core)?;
            self.parent.push(shift(other.parent[i]));
            self.volume.push(vmap[other.volume[i].idx()]);
            self.flags.push(other.flags[i]);
            self.frn.push(other.frn[i]);
            let e = other.ext[i];
            self.ext.push(match e {
                NO_EXT | crate::ext::OVERFLOW_EXT => e,
                _ => self.ext_dict.intern(other.ext_dict.name(e)),
            });
            self.size.push(other.size[i]);
            self.dm.push(other.dm[i]);
            self.dc.push(other.dc[i]);
            if let Some(da) = &mut self.da {
                da.push(other.da.as_ref().map_or(FileTime::UNKNOWN, |d| d[i]));
            }
            self.attr.push(other.attr[i]);
        }
        self.live += other.live;
        self.folders += other.folders;
        self.run.extend(other.run.into_iter().map(|(k, v)| (shift(k), v)));
        self.recent.extend(other.recent.into_iter().map(|(k, v)| (shift(k), v)));
        self.links.extend(
            other.links.into_iter().map(|((v, f), ids)| ((vmap[v.idx()], f), ids.into_iter().map(shift).collect())),
        );
        // FRN maps of the new volumes, from their rows.
        let mut per_vol: HashMap<VolumeId, Vec<(u64, EntryId)>> = HashMap::new();
        for r in offset as usize..self.len() {
            let f = self.flags[r];
            if !f.contains(EntryFlags::HARDLINK_ALIAS) && self.frn[r] != ORPHANS_FRN {
                per_vol.entry(self.volume[r]).or_default().push((self.frn[r], EntryId(r as u32)));
            }
        }
        for (v, pairs) in per_vol {
            self.frn_maps[v.idx()] = FrnMap::build(&pairs, matches!(self.volumes[v.idx()].info.kind, VolumeKind::Ntfs));
        }
        for &v in &vmap {
            if let Some(n) = self.next_frn.get_mut(v.idx()) {
                *n = 0;
            }
            if let Some(o) = self.orphans[v.idx()] {
                self.frn_maps[v.idx()].insert(ORPHANS_FRN, o);
            }
        }
        self.rebuild_children();
        self.bump(true);
        Ok(vmap)
    }

    fn orphans_dir(&mut self, vol: VolumeId) -> Result<EntryId> {
        if let Some(id) = self.orphans[vol.idx()].filter(|&id| self.is_live(id)) {
            return Ok(id);
        }
        let root = *self.volumes[vol.idx()].roots.first().ok_or(IndexError::NoRoot)?;
        let units: Vec<u16> = ORPHANS_NAME.encode_utf16().collect();
        let attrs = fnc_core::flags::attr::DIRECTORY | fnc_core::flags::attr::HIDDEN | fnc_core::flags::attr::SYSTEM;
        let id = self.push_row(vol, ORPHANS_FRN, root, &units, &Meta::attrs(attrs), EntryFlags::empty())?;
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

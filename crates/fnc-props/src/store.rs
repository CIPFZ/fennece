//! Sparse property store (SPEC ch.1 §8.3).
//!
//! Layout: a [`RowMap`] gives every extracted entry a dense row; each property has its own
//! column over rows, created on first use:
//!
//! * `Int` / `Bool` / `Date` / `Duration` → `Vec<i64>` (`i64::MIN` = no value),
//! * `Float` → `Vec<f64>` (NaN = no value),
//! * `Text` → `Vec<u32>` indices into a per-column dictionary (repeated values such as
//!   artists or camera models are stored once; text predicates are evaluated once per
//!   dictionary entry),
//! * `MultiText` → `Vec<u32>` indices into interned lists of dictionary indices.
//!
//! Lookup is O(1) (`row_of[id]`, then the column). An entry with a row has been extracted
//! for every property in [`PropertyStore::indexed`]: a missing value there is *known*
//! to be absent. Entries without a row are "not indexed" (`unknown` to the evaluator).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fnc_core::{EntryId, FileTime};
use fnc_index::IndexStore;
use parking_lot::lock_api::ArcRwLockReadGuard;
use parking_lot::{RawRwLock, RwLock, RwLockWriteGuard};

use crate::fnx::{Dec, Enc, FnxError, FnxResult, FnxWriter, segments};
use crate::rows::{NONE, RowMap};
use crate::schema::{PropType, PropertyId};
use crate::value::Value;

/// `props.fnx` magic.
pub const PROPS_MAGIC: [u8; 4] = *b"FNXP";
/// `props.fnx` layout version.
pub const PROPS_VERSION: u32 = 1;
const NO_INT: i64 = i64::MIN;

/// Interned strings of one text column.
#[derive(Clone, Debug, Default)]
pub struct Dict {
    strings: Vec<Arc<str>>,
    map: HashMap<Arc<str>, u32>,
}

impl Dict {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.map.get(s) {
            return i;
        }
        let a: Arc<str> = s.into();
        let i = self.strings.len() as u32;
        self.strings.push(a.clone());
        self.map.insert(a, i);
        i
    }

    /// All strings (index = dictionary id).
    pub fn strings(&self) -> &[Arc<str>] {
        &self.strings
    }
}

/// Storage of one property.
#[derive(Clone, Debug, Default)]
enum Column {
    #[default]
    Empty,
    Int(Vec<i64>),
    Float(Vec<f64>),
    Text(Vec<u32>, Dict),
    Multi(Vec<u32>, Vec<Box<[u32]>>, HashMap<Box<[u32]>, u32>, Dict),
}

/// Borrowed view of one column for fast scans (slices may be shorter than the row count;
/// missing tails mean "no value").
#[derive(Clone, Copy, Debug)]
pub enum ColumnView<'a> {
    /// Never set.
    Empty,
    /// Integers by row (`i64::MIN` = none).
    Int(&'a [i64]),
    /// Floats by row (NaN = none).
    Float(&'a [f64]),
    /// Dictionary ids by row (`u32::MAX` = none) and the dictionary.
    Text(&'a [u32], &'a [Arc<str>]),
    /// List ids by row (`u32::MAX` = none), the lists (of dictionary ids) and the dictionary.
    Multi(&'a [u32], &'a [Box<[u32]>], &'a [Arc<str>]),
}

/// Borrowed value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueRef<'a> {
    /// Integer / bool / date ticks / duration ms (see the property type).
    Int(i64),
    /// Real.
    Float(f64),
    /// Text.
    Text(&'a str),
    /// List of dictionary ids into `dict`.
    Multi(&'a [u32], &'a [Arc<str>]),
}

impl ValueRef<'_> {
    /// Owned value for property `p`.
    pub fn to_value(self, p: PropertyId) -> Value {
        match self {
            ValueRef::Int(v) => match p.ty() {
                PropType::Date => Value::Date(FileTime(v)),
                PropType::Duration => Value::Duration(v),
                _ => Value::Int(v),
            },
            ValueRef::Float(f) => Value::Float(f),
            ValueRef::Text(t) => Value::Text(t.into()),
            ValueRef::Multi(ids, dict) => Value::MultiText(
                ids.iter().filter_map(|&i| dict.get(i as usize)).map(|s| Box::<str>::from(&**s)).collect(),
            ),
        }
    }
}

impl<'a> ColumnView<'a> {
    /// Value at `row`.
    #[inline]
    pub fn get(self, row: u32) -> Option<ValueRef<'a>> {
        let r = row as usize;
        match self {
            ColumnView::Empty => None,
            ColumnView::Int(v) => v.get(r).copied().filter(|&x| x != NO_INT).map(ValueRef::Int),
            ColumnView::Float(v) => v.get(r).copied().filter(|x| !x.is_nan()).map(ValueRef::Float),
            ColumnView::Text(v, d) => {
                v.get(r).copied().filter(|&x| x != NONE).and_then(|i| d.get(i as usize)).map(|s| ValueRef::Text(s))
            }
            ColumnView::Multi(v, lists, d) => v
                .get(r)
                .copied()
                .filter(|&x| x != NONE)
                .and_then(|i| lists.get(i as usize))
                .map(|l| ValueRef::Multi(l, d)),
        }
    }
}

/// The property store of one index instance.
#[derive(Clone, Debug)]
pub struct PropertyStore {
    rows: RowMap,
    generation: u64,
    indexed: Vec<PropertyId>,
    cols: Vec<Column>,
}

impl PropertyStore {
    /// Empty store for `IndexStore::instance()` `instance`.
    pub fn new(instance: u64) -> Self {
        Self {
            rows: RowMap::new(instance),
            generation: 1,
            indexed: Vec::new(),
            cols: vec![Column::Empty; PropertyId::count()],
        }
    }

    /// Index instance the entry ids belong to.
    pub fn instance(&self) -> u64 {
        self.rows.instance()
    }

    /// Incremented by every mutation (result sets record it to detect staleness).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn bump(&mut self) {
        self.generation += 1;
    }

    /// Number of extracted entries.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` if nothing was extracted.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row map (entry ↔ row).
    pub fn rows(&self) -> &RowMap {
        &self.rows
    }

    /// Properties that extraction covers (stored, not derived).
    pub fn indexed(&self) -> &[PropertyId] {
        &self.indexed
    }

    /// `true` if extraction covers `p`.
    pub fn is_indexed(&self, p: PropertyId) -> bool {
        self.indexed.contains(&p)
    }

    /// Sets the covered properties. Adding a property invalidates every row (the entries
    /// must be re-extracted, so they become "not indexed"); removing one drops its column.
    /// Returns `true` if the rows were cleared.
    pub fn set_indexed(&mut self, props: &[PropertyId]) -> bool {
        let mut props: Vec<PropertyId> = props.iter().copied().filter(|p| !p.is_derived()).collect();
        props.sort();
        props.dedup();
        if props == self.indexed {
            return false;
        }
        let added = props.iter().any(|p| !self.indexed.contains(p));
        for (i, c) in self.cols.iter_mut().enumerate() {
            if !props.contains(&PropertyId(i as u16)) {
                *c = Column::Empty;
            }
        }
        self.indexed = props;
        if added {
            self.clear_rows();
        }
        self.bump();
        added
    }

    fn clear_rows(&mut self) {
        self.rows.clear();
        self.cols.fill(Column::Empty);
    }

    /// `true` if `id` has been extracted.
    #[inline]
    pub fn has(&self, id: EntryId) -> bool {
        self.rows.row(id).is_some()
    }

    /// `true` if the value of `p` for `id` is known (present, or absent after extraction).
    #[inline]
    pub fn covers(&self, id: EntryId, p: PropertyId) -> bool {
        self.has(id) && (self.is_indexed(p) || self.get_ref(id, p).is_some())
    }

    /// Stores the extraction result of `id`, replacing earlier values. Values of other
    /// types than the property's are coerced or dropped; derived properties are ignored.
    pub fn insert(&mut self, id: EntryId, values: Vec<(PropertyId, Value)>) {
        let (row, new) = self.rows.ensure(id);
        if !new {
            self.clear_row(row);
        }
        for (p, v) in values {
            if p.is_derived() {
                continue;
            }
            if let Some(v) = v.coerce(p.ty()) {
                self.set(row, p, v);
            }
        }
        self.bump();
    }

    /// Forgets `id` (file deleted or changed: it becomes "not indexed").
    pub fn remove(&mut self, id: EntryId) {
        if let Some(row) = self.rows.remove(id) {
            self.clear_row(row);
            self.bump();
        }
    }

    /// Follows [`IndexStore::compact`] (`remap` as returned by it) into `new_instance`.
    pub fn remap(&mut self, remap: &[u32], new_instance: u64) {
        for row in self.rows.remap(remap, new_instance) {
            self.clear_row(row);
        }
        self.bump();
    }

    fn clear_row(&mut self, row: u32) {
        let r = row as usize;
        for c in &mut self.cols {
            match c {
                Column::Empty => {}
                Column::Int(v) => {
                    if let Some(x) = v.get_mut(r) {
                        *x = NO_INT;
                    }
                }
                Column::Float(v) => {
                    if let Some(x) = v.get_mut(r) {
                        *x = f64::NAN;
                    }
                }
                Column::Text(v, _) | Column::Multi(v, ..) => {
                    if let Some(x) = v.get_mut(r) {
                        *x = NONE;
                    }
                }
            }
        }
    }

    fn set(&mut self, row: u32, p: PropertyId, v: Value) {
        let Some(col) = self.cols.get_mut(p.0 as usize) else { return };
        let r = row as usize;
        fn slot<T: Clone>(v: &mut Vec<T>, r: usize, none: T) -> &mut T {
            if v.len() <= r {
                v.resize(r + 1, none);
            }
            &mut v[r]
        }
        match (p.ty(), v) {
            (PropType::Float, Value::Float(f)) => {
                if !matches!(col, Column::Float(_)) {
                    *col = Column::Float(Vec::new());
                }
                if let Column::Float(c) = col {
                    *slot(c, r, f64::NAN) = f;
                }
            }
            (PropType::Text, Value::Text(t)) => {
                if !matches!(col, Column::Text(..)) {
                    *col = Column::Text(Vec::new(), Dict::default());
                }
                if let Column::Text(c, d) = col {
                    let i = d.intern(&t);
                    *slot(c, r, NONE) = i;
                }
            }
            (PropType::MultiText, Value::MultiText(items)) => {
                if !matches!(col, Column::Multi(..)) {
                    *col = Column::Multi(Vec::new(), Vec::new(), HashMap::new(), Dict::default());
                }
                if let Column::Multi(c, lists, lmap, d) = col {
                    let ids: Box<[u32]> = items.iter().map(|s| d.intern(s)).collect();
                    let li = match lmap.get(&ids) {
                        Some(&i) => i,
                        None => {
                            let i = lists.len() as u32;
                            lists.push(ids.clone());
                            lmap.insert(ids, i);
                            i
                        }
                    };
                    *slot(c, r, NONE) = li;
                }
            }
            (_, v) => {
                let Some(x) = v.as_i64() else { return };
                if x == NO_INT {
                    return;
                }
                if !matches!(col, Column::Int(_)) {
                    *col = Column::Int(Vec::new());
                }
                if let Column::Int(c) = col {
                    *slot(c, r, NO_INT) = x;
                }
            }
        }
    }

    /// Column view of `p` for scans.
    pub fn column(&self, p: PropertyId) -> ColumnView<'_> {
        match self.cols.get(p.0 as usize) {
            None | Some(Column::Empty) => ColumnView::Empty,
            Some(Column::Int(v)) => ColumnView::Int(v),
            Some(Column::Float(v)) => ColumnView::Float(v),
            Some(Column::Text(v, d)) => ColumnView::Text(v, d.strings()),
            Some(Column::Multi(v, l, _, d)) => ColumnView::Multi(v, l, d.strings()),
        }
    }

    /// Borrowed value of `p` for `id`.
    #[inline]
    pub fn get_ref(&self, id: EntryId, p: PropertyId) -> Option<ValueRef<'_>> {
        let row = self.rows.row(id)?;
        self.column(p).get(row)
    }

    /// Owned value of `p` for `id` (derived properties are computed).
    pub fn get(&self, id: EntryId, p: PropertyId) -> Option<Value> {
        match p {
            PropertyId::DIMENSIONS => {
                let (w, h) = self.dimensions(id)?;
                Some(Value::Text(format!("{w} x {h}").into()))
            }
            PropertyId::ASPECT_RATIO => {
                let (w, h) = self.dimensions(id)?;
                (h > 0).then(|| Value::Float(w as f64 / h as f64))
            }
            _ => self.get_ref(id, p).map(|v| v.to_value(p)),
        }
    }

    /// `(width, height)` of an image, else the frame size of a video.
    pub fn dimensions(&self, id: EntryId) -> Option<(i64, i64)> {
        let int = |p| match self.get_ref(id, p) {
            Some(ValueRef::Int(v)) => Some(v),
            _ => None,
        };
        match (int(PropertyId::WIDTH), int(PropertyId::HEIGHT)) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => Some((int(PropertyId::FRAME_WIDTH)?, int(PropertyId::FRAME_HEIGHT)?)),
        }
    }

    /// All stored values of `id`.
    pub fn values(&self, id: EntryId) -> Vec<(PropertyId, Value)> {
        let Some(row) = self.rows.row(id) else { return Vec::new() };
        PropertyId::all().filter_map(|p| self.column(p).get(row).map(|v| (p, v.to_value(p)))).collect()
    }

    /// Approximate heap use in bytes.
    pub fn heap_bytes(&self) -> usize {
        let dict = |d: &Dict| d.strings.iter().map(|s| s.len() + 40).sum::<usize>();
        self.rows.heap_bytes()
            + self
                .cols
                .iter()
                .map(|c| match c {
                    Column::Empty => 0,
                    Column::Int(v) => v.capacity() * 8,
                    Column::Float(v) => v.capacity() * 8,
                    Column::Text(v, d) => v.capacity() * 4 + dict(d),
                    Column::Multi(v, l, _, d) => {
                        v.capacity() * 4 + l.iter().map(|x| x.len() * 8 + 16).sum::<usize>() + dict(d)
                    }
                })
                .sum::<usize>()
    }

    // ------------------------------------------------------------------ persistence

    /// Serialises the store (`props.fnx`). `s` must be the index the ids belong to.
    pub fn to_bytes(&self, s: &IndexStore) -> FnxResult<Vec<u8>> {
        if s.instance() != self.instance() {
            return Err(FnxError::Instance);
        }
        let mut w = FnxWriter::new(PROPS_MAGIC, PROPS_VERSION);
        let mut meta = Enc::default();
        meta.u32(self.indexed.len() as u32);
        for p in &self.indexed {
            meta.str(p.name());
        }
        w.segment(*b"META", &meta.0);
        w.segment(*b"ROWS", &self.rows.encode(s));
        for p in PropertyId::all() {
            let mut e = Enc::default();
            e.str(p.name());
            match self.column(p) {
                ColumnView::Empty => continue,
                ColumnView::Int(v) => {
                    e.u8(0);
                    e.u32(v.len() as u32);
                    v.iter().for_each(|&x| e.i64(x));
                }
                ColumnView::Float(v) => {
                    e.u8(1);
                    e.u32(v.len() as u32);
                    v.iter().for_each(|&x| e.f64(x));
                }
                ColumnView::Text(v, d) => {
                    e.u8(2);
                    e.u32(d.len() as u32);
                    d.iter().for_each(|s| e.str(s));
                    e.u32(v.len() as u32);
                    v.iter().for_each(|&x| e.u32(x));
                }
                ColumnView::Multi(v, lists, d) => {
                    e.u8(3);
                    e.u32(d.len() as u32);
                    d.iter().for_each(|s| e.str(s));
                    e.u32(v.len() as u32);
                    for &li in v {
                        match lists.get(li as usize) {
                            Some(l) => {
                                e.u32(l.len() as u32);
                                l.iter().for_each(|&x| e.u32(x));
                            }
                            None => e.u32(NONE),
                        }
                    }
                }
            }
            w.segment(*b"PCOL", &e.0);
        }
        Ok(w.finish())
    }

    /// Saves atomically to `path`.
    pub fn save(&self, s: &IndexStore, path: &Path) -> FnxResult<()> {
        crate::fnx::write_atomic(path, &self.to_bytes(s)?)
    }

    /// Loads `path` and attaches it to `s` (rows of changed or vanished entries dropped).
    pub fn load(path: &Path, s: &IndexStore) -> FnxResult<Self> {
        Self::from_bytes(&crate::fnx::read_file(path)?, s)
    }

    /// Decodes `props.fnx` bytes against `s`.
    pub fn from_bytes(buf: &[u8], s: &IndexStore) -> FnxResult<Self> {
        let segs = segments(buf, PROPS_MAGIC, PROPS_VERSION, "property store")?;
        let mut store = PropertyStore::new(s.instance());
        let mut assign: Option<Vec<u32>> = None;
        for (tag, seg) in &segs {
            match tag {
                b"META" => {
                    let mut d = Dec::new(seg);
                    let n = d.count(4)?;
                    for _ in 0..n {
                        if let Some(p) = PropertyId::lookup(d.str()?) {
                            store.indexed.push(p);
                        }
                    }
                    store.indexed.sort();
                    store.indexed.dedup();
                }
                b"ROWS" => {
                    let (map, a) = RowMap::decode(seg, s)?;
                    store.rows = map;
                    assign = Some(a);
                }
                b"PCOL" => {
                    let a = assign.as_deref().ok_or(FnxError::Corrupt("column before rows"))?;
                    store.decode_column(seg, a)?;
                }
                _ => {}
            }
        }
        if assign.is_none() {
            return Err(FnxError::Corrupt("missing rows"));
        }
        Ok(store)
    }

    fn decode_column(&mut self, seg: &[u8], assign: &[u32]) -> FnxResult<()> {
        let mut d = Dec::new(seg);
        let Some(p) = PropertyId::lookup(d.str()?) else { return Ok(()) };
        let kind = d.u8()?;
        let rows_of = |i: usize| assign.get(i).copied().unwrap_or(NONE);
        match kind {
            0 | 1 => {
                let n = d.count(8)?;
                for i in 0..n {
                    let v = if kind == 0 {
                        let x = d.i64()?;
                        if x == NO_INT {
                            continue;
                        }
                        match p.ty() {
                            PropType::Date => Value::Date(FileTime(x)),
                            PropType::Duration => Value::Duration(x),
                            _ => Value::Int(x),
                        }
                    } else {
                        let f = d.f64()?;
                        if f.is_nan() {
                            continue;
                        }
                        Value::Float(f)
                    };
                    let r = rows_of(i);
                    if r != NONE
                        && let Some(v) = v.coerce(p.ty())
                    {
                        self.set(r, p, v);
                    }
                }
            }
            2 | 3 => {
                let nd = d.count(4)?;
                let mut dict = Vec::with_capacity(nd);
                for _ in 0..nd {
                    dict.push(d.str()?);
                }
                let n = d.count(4)?;
                for i in 0..n {
                    let value = if kind == 2 {
                        let x = d.u32()?;
                        dict.get(x as usize).map(|s| Value::Text((*s).into()))
                    } else {
                        let len = d.u32()?;
                        if len == NONE {
                            None
                        } else {
                            let len = len as usize;
                            if len > nd.max(1) * 64 {
                                return Err(FnxError::Corrupt("list length"));
                            }
                            let mut items = Vec::with_capacity(len);
                            for _ in 0..len {
                                let x = d.u32()?;
                                items.push(*dict.get(x as usize).ok_or(FnxError::Corrupt("dictionary id"))?);
                            }
                            Value::multi(items)
                        }
                    };
                    let r = rows_of(i);
                    if r != NONE
                        && let Some(v) = value.and_then(|v| v.coerce(p.ty()))
                    {
                        self.set(r, p, v);
                    }
                }
            }
            _ => return Err(FnxError::Corrupt("column kind")),
        }
        Ok(())
    }
}

/// Shared, thread-safe handle to a [`PropertyStore`] (one writer — the app's extraction
/// thread — many readers: the search engine and the UI).
#[derive(Clone, Debug)]
pub struct PropertyIndex(Arc<RwLock<PropertyStore>>);

/// Owned read guard (`Send`): a consistent view for the duration of a query.
pub type PropertySnapshot = ArcRwLockReadGuard<RawRwLock, PropertyStore>;

impl PropertyIndex {
    /// Wraps a store.
    pub fn new(store: PropertyStore) -> Self {
        Self(Arc::new(RwLock::new(store)))
    }

    /// Read snapshot (blocks while a writer holds the lock; writers keep batches short).
    pub fn snapshot(&self) -> PropertySnapshot {
        self.0.read_arc()
    }

    /// Read snapshot within `timeout` (UI thread: never block > 8 ms).
    pub fn try_snapshot_for(&self, timeout: std::time::Duration) -> Option<PropertySnapshot> {
        self.0.try_read_arc_for(timeout)
    }

    /// Exclusive access (insert/remove/remap a batch, then drop the guard).
    pub fn write(&self) -> RwLockWriteGuard<'_, PropertyStore> {
        self.0.write()
    }

    /// `true` if both handles share one store.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl PartialEq for PropertyIndex {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> IndexStore {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\p\\a.jpg\",10,3,,32\n\
                   \"C:\\p\\b.jpg\",20,3,,32\n\
                   \"C:\\p\\c.mp3\",30,3,,32\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s
    }

    fn id_of(s: &IndexStore, name: &str) -> EntryId {
        (0..s.len() as u32).map(EntryId).find(|&i| s.name_string(i) == name).unwrap()
    }

    fn sample(s: &IndexStore) -> PropertyStore {
        let mut p = PropertyStore::new(s.instance());
        p.set_indexed(&[
            PropertyId::WIDTH,
            PropertyId::HEIGHT,
            PropertyId::ARTIST,
            PropertyId::TAGS,
            PropertyId::F_STOP,
            PropertyId::LENGTH,
        ]);
        p.insert(
            id_of(s, "a.jpg"),
            vec![
                (PropertyId::WIDTH, Value::Int(4000)),
                (PropertyId::HEIGHT, Value::Int(3000)),
                (PropertyId::F_STOP, Value::Float(2.8)),
                (PropertyId::TAGS, Value::multi(["sea", "sun"]).unwrap()),
                (PropertyId::DIMENSIONS, Value::Text("ignored".into())),
            ],
        );
        p.insert(id_of(s, "b.jpg"), vec![(PropertyId::WIDTH, Value::Int(800)), (PropertyId::HEIGHT, Value::Int(600))]);
        p.insert(
            id_of(s, "c.mp3"),
            vec![(PropertyId::ARTIST, Value::Text("John Doe".into())), (PropertyId::LENGTH, Value::Duration(185_000))],
        );
        p
    }

    #[test]
    fn insert_get_derived_and_cover() {
        let s = index();
        let p = sample(&s);
        let a = id_of(&s, "a.jpg");
        assert_eq!(p.get(a, PropertyId::WIDTH), Some(Value::Int(4000)));
        assert_eq!(p.get(a, PropertyId::DIMENSIONS), Some(Value::Text("4000 x 3000".into())));
        assert!(matches!(p.get(a, PropertyId::ASPECT_RATIO), Some(Value::Float(r)) if (r - 4.0 / 3.0).abs() < 1e-9));
        assert_eq!(p.get(a, PropertyId::TAGS).unwrap().as_str(), Some("sea"));
        assert_eq!(p.get(a, PropertyId::ARTIST), None);
        assert!(p.covers(a, PropertyId::ARTIST), "extracted: known absent");
        assert!(!p.covers(a, PropertyId::CAMERA_MODEL), "not configured");
        let c = id_of(&s, "c.mp3");
        assert_eq!(p.get(c, PropertyId::LENGTH), Some(Value::Duration(185_000)));
        assert_eq!(p.values(c).len(), 2);
        let folder = id_of(&s, "p");
        assert!(!p.has(folder));
        assert!(p.heap_bytes() > 0);
    }

    #[test]
    fn reinsert_remove_and_remap() {
        let mut s = index();
        let mut p = sample(&s);
        let a = id_of(&s, "a.jpg");
        let g = p.generation();
        p.insert(a, vec![(PropertyId::WIDTH, Value::Int(10))]);
        assert!(p.generation() > g);
        assert_eq!(p.get(a, PropertyId::HEIGHT), None, "re-extraction replaces all values");
        p.remove(a);
        assert!(!p.has(a));
        // Delete b.jpg from the index, compact, follow the remap.
        let b = id_of(&s, "b.jpg");
        s.apply(vec![fnc_index::Change::Delete(fnc_index::EntryRef::Id(b))]);
        let remap = s.compact();
        p.remap(&remap, s.instance());
        let c = id_of(&s, "c.mp3");
        assert_eq!(p.get(c, PropertyId::ARTIST), Some(Value::Text("John Doe".into())));
        assert_eq!(p.len(), 1);
        assert_eq!(p.instance(), s.instance());
    }

    #[test]
    fn indexed_set_changes() {
        let s = index();
        let mut p = sample(&s);
        assert!(!p.set_indexed(&[PropertyId::WIDTH, PropertyId::HEIGHT]), "removal keeps rows");
        let a = id_of(&s, "a.jpg");
        assert_eq!(p.get(a, PropertyId::F_STOP), None);
        assert_eq!(p.get(a, PropertyId::WIDTH), Some(Value::Int(4000)));
        assert!(p.set_indexed(&[PropertyId::WIDTH, PropertyId::CAMERA_MODEL]), "addition clears rows");
        assert!(p.is_empty());
    }

    #[test]
    fn persistence_roundtrip_and_reattach() {
        let s = index();
        let p = sample(&s);
        let bytes = p.to_bytes(&s).unwrap();
        // Reload into a clone of the index (new instance id).
        let s2 = s.clone();
        let q = PropertyStore::from_bytes(&bytes, &s2).unwrap();
        assert_eq!(q.instance(), s2.instance());
        for name in ["a.jpg", "b.jpg", "c.mp3"] {
            let (i1, i2) = (id_of(&s, name), id_of(&s2, name));
            assert_eq!(p.values(i1), q.values(i2), "{name}");
        }
        assert_eq!(q.indexed(), p.indexed());
        // Wrong instance on save.
        assert!(matches!(p.to_bytes(&s2), Err(FnxError::Instance)));
        // A changed file (different size) is dropped on load.
        let mut s3 = s.clone();
        s3.apply(vec![fnc_index::Change::Meta {
            target: fnc_index::EntryRef::Id(id_of(&s3, "a.jpg")),
            meta: fnc_index::Meta { size: Some(99), ..fnc_index::Meta::default() },
        }]);
        let q3 = PropertyStore::from_bytes(&p.to_bytes(&s).unwrap(), &s3).unwrap();
        assert!(!q3.has(id_of(&s3, "a.jpg")));
        assert!(q3.has(id_of(&s3, "b.jpg")));
        // Corruption is detected.
        let mut bad = bytes.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xff;
        assert!(PropertyStore::from_bytes(&bad, &s2).is_err());
    }

    #[test]
    fn shared_handle() {
        let s = index();
        let h = PropertyIndex::new(sample(&s));
        let snap = h.snapshot();
        assert_eq!(snap.len(), 3);
        drop(snap);
        h.write().remove(id_of(&s, "a.jpg"));
        assert_eq!(h.try_snapshot_for(std::time::Duration::from_millis(5)).unwrap().len(), 2);
        assert_eq!(h, h.clone());
    }
}

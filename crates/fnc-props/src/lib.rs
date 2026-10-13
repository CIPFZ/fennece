//! Fennec property, content, hash and duplicate support (SPEC ch.1 §8, §9, §11).
//!
//! * [`schema`] — the property table: [`PropertyId`], names/aliases, column titles, value
//!   types, units, extractor hints, Windows `System.*` names.
//! * [`value`] — [`Value`] and its display forms ([`display`], [`PropertyId::format`]).
//! * [`config`] — [`PropertyIndexConfig`] / [`ContentIndexConfig`] (what to index, where).
//! * [`extract`] — pure-Rust extractors ([`extract::PropertyExtractor`]) and the
//!   [`extract::PropertyProvider`] fallback trait (Windows Property System, injected by the app).
//! * [`store`] — the sparse [`PropertyStore`] and its shared handle [`PropertyIndex`].
//! * [`content`] — [`content::ContentExtractor`], [`content::ContentStore`] (LZ4 text +
//!   token index) and [`content::ContentIndex`].
//! * [`hash`] — CRC-32/CRC-64/MD5/SHA-1/SHA-256/SHA-512 and the [`hash::HashCache`].
//! * [`ondemand`] — [`ondemand::OnDemand`]: file reads for searches on unindexed entries.
//! * [`fnx`] / [`rows`] — the `props.fnx` / `content.fnx` / `hashes.fnx` container and the
//!   entry ↔ row mapping that survives index compaction and restarts.
//!
//! Without the default `extract` feature only the model, the stores and the hash cache are
//! built (what `fnc-query` needs).
//!
//! # Integration contract (application side)
//!
//! **Setup** (once, and whenever the settings change):
//!
//! ```text
//! let (cfg, unknown) = PropertyIndexConfig::from_names(&settings.property_names,
//!                          Scope::parse(&settings.property_folders, &settings.property_exts), fast_sort);
//! let props = PropertyIndex::new(PropertyStore::load(dir.join("props.fnx"), &index)
//!                 .unwrap_or_else(|_| PropertyStore::new(index.instance())));
//! if props.write().set_indexed(&cfg.stored()) { /* every entry must be re-extracted */ }
//! let content = ContentIndex::new(ContentStore::load(dir.join("content.fnx"), &index)
//!                 .unwrap_or_else(|_| ContentStore::new(index.instance())));
//! let hashes = Arc::new(HashCache::load(dir.join("hashes.fnx")).unwrap_or_default());
//! let on_demand: Arc<dyn OnDemand> = Arc::new(DefaultOnDemand::with_fallback(windows_provider));
//! search_options.props = Some(props.clone());          // fnc_exec::SearchOptions
//! search_options.content = Some(content.clone());
//! search_options.hashes = Some(fnc_exec::Shared::new(hashes.clone()));
//! search_options.on_demand = Some(fnc_exec::Shared::new(on_demand));
//! search_options.slow = fnc_exec::SlowPolicy { max_files: 10_000, ..Default::default() };
//! ```
//!
//! Handles compare by identity: create them once and keep reusing the same `Arc`s, or every
//! search is treated as new.
//!
//! **Background extraction** (low-priority thread, never the UI or search thread):
//!
//! 1. With a short index snapshot, collect candidates: live files with
//!    `cfg.wants(&path, ext)` and `!props.snapshot().has(id)` (content: `content_cfg.wants(..)`
//!    and `!content.has(id)`), remembering `(id, path, ext)`. Drop the snapshot.
//! 2. Without holding any lock: `PropertyExtractor::extract_config(&path, ext, &cfg)` and
//!    `ContentExtractor::extract(&path, ext)` (both bounded and panic-safe).
//! 3. Insert in short batches (≈ 100 files): `props.write().insert(id, values)`,
//!    `content.write().insert(id, &text)`; skip results whose store `instance()` no longer
//!    matches the index. Then `EngineHandle::refresh()` so the visible search picks them up
//!    (results carry the store generation; stale ones are recomputed).
//! 4. On USN/watcher changes: `remove(id)` on both stores for modified, renamed-away or
//!    deleted files (debounce 2 s, then re-queue). Right after `IndexStore::compact()`,
//!    call `remap(&remap, index.instance())` on both stores; until then they are ignored
//!    (instance mismatch = not indexed), never wrong.
//! 5. Persist with the index: `store.save(&index_snapshot, path)` (atomic, `.bak` kept), and
//!    `hashes.save(path)` when `hashes.generation()` changed.
//!
//! **Locking**: lock order is index → props → content (the search engine takes read
//! guards in that order). Never acquire the index lock while holding a store write guard;
//! keep write batches short. The UI uses `try_snapshot_for(Duration::from_millis(2))` and
//! shows empty cells when it cannot get a guard.
//!
//! **UI**: property columns `PropertyStore::display(id, p)` (unit-aware text) or `get(id, p)`
//! with [`display`]; sorting `fnc_exec::SortSpec::by_property(p, asc, folders_first)` /
//! `push_property_key`; duplicate group column `ResultSet::dupe_group(id)`; preview text
//! `ContentStore::text(id)` with `fnc_exec::content_highlights(&plan, &text, max)` and the
//! first hit `ResultSet::content_hit(id)`; warnings (`not indexed`, slow-search limit) in
//! `ExecStats::warnings` (also rendered into `ExecStats::errors`).

#![forbid(unsafe_code)]

pub mod config;
pub mod content;
#[cfg(feature = "extract")]
pub mod extract;
pub mod fnx;
pub mod hash;
#[cfg(feature = "extract")]
pub mod ondemand;
pub mod rows;
pub mod schema;
pub mod store;
pub mod value;

pub use config::{ContentIndexConfig, ContentLimits, PropertyIndexConfig, Scope};
pub use schema::{Extractor, PROPERTIES, PropType, PropertyDef, PropertyId, Unit};
pub use store::{ColumnView, PropertyIndex, PropertySnapshot, PropertyStore, ValueRef};
pub use value::{Value, display};

//! Shared, thread-safe handle to the index ("single writer, many readers").

use std::sync::Arc;

use parking_lot::lock_api::ArcRwLockReadGuard;
use parking_lot::{RawRwLock, RwLock, RwLockWriteGuard};

use crate::store::IndexStore;

/// Owned read guard: a consistent view of the index for the duration of a query.
/// `Send`, so a query can run on a worker thread while holding it.
pub type IndexSnapshot = ArcRwLockReadGuard<RawRwLock, IndexStore>;

/// Shared index handle (`Arc<RwLock<IndexStore>>`).
#[derive(Clone, Default)]
pub struct SharedIndex {
    inner: Arc<RwLock<IndexStore>>,
}

impl SharedIndex {
    /// Wraps a store.
    pub fn new(store: IndexStore) -> Self {
        Self { inner: Arc::new(RwLock::new(store)) }
    }

    /// Takes a read snapshot (blocks while a writer holds the lock, which is kept
    /// below 5 ms per batch).
    pub fn snapshot(&self) -> IndexSnapshot {
        self.inner.read_arc()
    }

    /// Tries to take a snapshot within `timeout` (UI thread: never block > 8 ms).
    pub fn try_snapshot_for(&self, timeout: std::time::Duration) -> Option<IndexSnapshot> {
        self.inner.try_read_arc_for(timeout)
    }

    /// Exclusive access for applying a batch.
    pub fn write(&self) -> RwLockWriteGuard<'_, IndexStore> {
        self.inner.write()
    }

    /// Replaces the whole store (publishing a privately built index).
    pub fn replace(&self, store: IndexStore) {
        *self.inner.write() = store;
    }

    /// Current generation without holding a guard.
    pub fn generation(&self) -> u64 {
        self.inner.read().generation()
    }
}

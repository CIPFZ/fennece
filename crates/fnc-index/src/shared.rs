//! Shared, thread-safe handle to the index ("single writer, many readers").

use std::sync::Arc;

use parking_lot::lock_api::ArcRwLockReadGuard;
use parking_lot::{RawRwLock, RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};

use crate::store::IndexStore;

/// Owned read guard: a consistent view of the index for the duration of a query.
/// `Send`, so a query can run on a worker thread while holding it.
pub type IndexSnapshot = ArcRwLockReadGuard<RawRwLock, IndexStore>;

/// Shared index handle (`Arc<RwLock<IndexStore>>`).
#[derive(Clone, Default)]
pub struct SharedIndex {
    inner: Arc<RwLock<IndexStore>>,
    /// Writer epoch: a writer that obtained epoch `e` may only write while it is current
    /// ([`SharedIndex::write_in`]); [`SharedIndex::new_epoch`] retires all older writers.
    epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl SharedIndex {
    /// Wraps a store.
    pub fn new(store: IndexStore) -> Self {
        Self { inner: Arc::new(RwLock::new(store)), epoch: Arc::default() }
    }

    /// Current writer epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Retires every writer of older epochs: once this returns, no write through
    /// [`SharedIndex::write_in`] with an older epoch can happen (the switch is made under
    /// the write lock). Returns the new epoch.
    pub fn new_epoch(&self) -> u64 {
        let _w = self.inner.write();
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
    }

    /// Exclusive access for a writer of `epoch`; `None` if that epoch was retired.
    pub fn write_in(&self, epoch: u64) -> Option<RwLockWriteGuard<'_, IndexStore>> {
        let w = self.inner.write();
        (self.epoch.load(std::sync::atomic::Ordering::Acquire) == epoch).then_some(w)
    }

    /// Read access that excludes every other writer of `epoch` while readers continue
    /// (upgrade it to write with [`RwLockUpgradableReadGuard::upgrade`]); `None` if that
    /// epoch was retired. For long work on a copy that must not miss concurrent changes.
    pub fn upgradable_in(&self, epoch: u64) -> Option<RwLockUpgradableReadGuard<'_, IndexStore>> {
        let g = self.inner.upgradable_read();
        (self.epoch.load(std::sync::atomic::Ordering::Acquire) == epoch).then_some(g)
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

/// A writer's view of a [`SharedIndex`], bound to the epoch current when it was made:
/// after [`SharedIndex::new_epoch`] its writes are refused (`None`). Reads are unaffected.
#[derive(Clone)]
pub struct EpochIndex {
    shared: SharedIndex,
    epoch: u64,
}

impl EpochIndex {
    /// Binds to the current epoch of `shared`.
    pub fn new(shared: &SharedIndex) -> Self {
        Self { shared: shared.clone(), epoch: shared.epoch() }
    }

    /// The underlying handle (reads, snapshots).
    pub fn shared(&self) -> &SharedIndex {
        &self.shared
    }

    /// Takes a read snapshot.
    pub fn snapshot(&self) -> IndexSnapshot {
        self.shared.snapshot()
    }

    /// Exclusive access, or `None` if this writer was retired.
    pub fn write(&self) -> Option<RwLockWriteGuard<'_, IndexStore>> {
        self.shared.write_in(self.epoch)
    }

    /// Read access that holds off other writers, or `None` if this writer was retired
    /// ([`SharedIndex::upgradable_in`]).
    pub fn upgradable(&self) -> Option<RwLockUpgradableReadGuard<'_, IndexStore>> {
        self.shared.upgradable_in(self.epoch)
    }

    /// `true` while this writer may write.
    pub fn is_current(&self) -> bool {
        self.shared.epoch() == self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_writers_cannot_write() {
        let s = SharedIndex::default();
        let e = s.epoch();
        assert!(s.write_in(e).is_some());
        let e2 = s.new_epoch();
        assert!(s.write_in(e).is_none());
        assert!(s.write_in(e2).is_some());
    }

    #[test]
    fn upgradable_holds_off_writers_but_not_readers() {
        let s = SharedIndex::default();
        let g = s.upgradable_in(s.epoch()).unwrap();
        let wrote = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (s, wrote) = (s.clone(), wrote.clone());
            std::thread::spawn(move || {
                let _w = s.write();
                wrote.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!wrote.load(std::sync::atomic::Ordering::SeqCst), "writer waits");
        // A parked writer must not block readers (searches and the UI keep running).
        let reader = {
            let s = s.clone();
            std::thread::spawn(move || s.try_snapshot_for(std::time::Duration::from_millis(500)).is_some())
        };
        assert!(reader.join().unwrap(), "reader got a snapshot");
        drop(RwLockUpgradableReadGuard::upgrade(g));
        writer.join().unwrap();
        assert!(wrote.load(std::sync::atomic::Ordering::SeqCst));
        assert!(s.upgradable_in(s.epoch() + 1).is_none(), "retired epoch");
    }
}

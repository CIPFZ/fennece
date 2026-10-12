//! Portable folder-index source: recursive directory walk (SPEC ch.1 §3).
//!
//! Used for non-NTFS locations and as the backend on non-Windows platforms. Entries
//! get synthetic, sequential FRNs (root = 1). Change notification is a later milestone
//! (M3); until then folder indexes are refreshed by rescanning.

use std::path::{Path, PathBuf};

use fnc_core::flags::attr;
use fnc_core::{EntryId, FileTime, VolumeId};
use fnc_index::{Meta, VolumeInfo, VolumeKind};

use crate::source::{ChangeSink, EntrySink, MetaSink, RawEntry, SourceError, StopFlag, VolumeSource};

/// Root FRN used by folder sources.
pub const FOLDER_ROOT_FRN: u64 = 1;

/// Recursive directory walker for one root.
pub struct FolderSource {
    root: PathBuf,
    info: VolumeInfo,
}

impl FolderSource {
    /// Source for `root` (must be an existing directory).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let display = root.to_string_lossy().into_owned();
        let mut info = VolumeInfo::new(VolumeKind::Folder, display);
        info.separator = std::path::MAIN_SEPARATOR;
        info.fs_name = "folder".into();
        Self { root, info }
    }

    /// Root path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Converts an OS file name to UTF-16 (lossless on Windows; lossy for non-UTF-8 names
/// elsewhere).
// SPEC-GAP: non-UTF-8 names on Unix are stored lossily (U+FFFD).
pub fn os_name_to_utf16(name: &std::ffi::OsStr, out: &mut Vec<u16>) {
    out.clear();
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        out.extend(name.encode_wide());
    }
    #[cfg(not(windows))]
    {
        out.extend(name.to_string_lossy().encode_utf16());
    }
}

/// Maps `std::fs::Metadata` to index metadata.
pub fn meta_from_fs(name: &std::ffi::OsStr, md: &std::fs::Metadata) -> Meta {
    #[cfg(windows)]
    let mut attributes = {
        use std::os::windows::fs::MetadataExt;
        let _ = name;
        md.file_attributes()
    };
    #[cfg(not(windows))]
    let mut attributes = {
        let mut a = if md.is_dir() { attr::DIRECTORY } else { attr::ARCHIVE };
        if name.to_string_lossy().starts_with('.') {
            a |= attr::HIDDEN;
        }
        if md.permissions().readonly() {
            a |= attr::READONLY;
        }
        if md.file_type().is_symlink() {
            a |= attr::REPARSE_POINT;
        }
        a
    };
    if md.is_dir() {
        attributes |= attr::DIRECTORY;
    }
    let ft = |t: std::io::Result<std::time::SystemTime>| t.ok().map(FileTime::from_system_time);
    Meta {
        attributes: Some(attributes),
        size: (!md.is_dir()).then_some(md.len()),
        date_modified: ft(md.modified()),
        date_created: ft(md.created()),
        date_accessed: ft(md.accessed()),
    }
}

impl VolumeSource for FolderSource {
    fn info(&self) -> VolumeInfo {
        self.info.clone()
    }

    fn root_frn(&self) -> u64 {
        FOLDER_ROOT_FRN
    }

    fn enumerate(&mut self, sink: &mut dyn EntrySink, stop: &StopFlag) -> Result<(), SourceError> {
        let mut next = FOLDER_ROOT_FRN + 1;
        let mut stack: Vec<(PathBuf, u64)> = vec![(self.root.clone(), FOLDER_ROOT_FRN)];
        let mut name = Vec::with_capacity(256);
        let mut done = 0u64;
        while let Some((dir, frn)) = stack.pop() {
            if stop.is_stopped() {
                return Err(SourceError::Stopped);
            }
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) if dir == self.root => return Err(SourceError::io(format!("read {}", dir.display()), e)),
                Err(e) => {
                    tracing::debug!(dir = %dir.display(), error = %e, "skipping unreadable folder");
                    continue;
                }
            };
            for entry in rd.flatten() {
                let Ok(md) = entry.metadata() else { continue };
                let fname = entry.file_name();
                os_name_to_utf16(&fname, &mut name);
                let meta = meta_from_fs(&fname, &md);
                let id = next;
                next += 1;
                sink.entry(&RawEntry { frn: id, parent_frn: frn, name: &name, meta })?;
                done += 1;
                if done.is_multiple_of(65_536) {
                    sink.progress(done, 0);
                }
                // Do not follow directory symlinks/junctions (avoids cycles).
                if md.is_dir() && !md.file_type().is_symlink() {
                    stack.push((entry.path(), id));
                }
            }
        }
        sink.progress(done, done);
        Ok(())
    }

    fn watch(&mut self, _volume: VolumeId, _sink: &mut dyn ChangeSink, _stop: &StopFlag) -> Result<(), SourceError> {
        Err(SourceError::Unsupported("folder change notification (M3)"))
    }

    fn fetch_metadata(
        &self,
        _dirs: &[(EntryId, String)],
        _out: &mut dyn MetaSink,
        _stop: &StopFlag,
    ) -> Result<(), SourceError> {
        Ok(())
    }

    fn enumerates_metadata(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Collect(Vec<(u64, u64, String, Meta)>);

    impl EntrySink for Collect {
        fn entry(&mut self, e: &RawEntry<'_>) -> Result<(), SourceError> {
            self.0.push((e.frn, e.parent_frn, String::from_utf16_lossy(e.name), e.meta));
            Ok(())
        }
    }

    #[test]
    fn walks_tree_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/file.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("top.md"), b"x").unwrap();
        let mut src = FolderSource::new(dir.path());
        let mut c = Collect(Vec::new());
        src.enumerate(&mut c, &StopFlag::new()).unwrap();
        assert_eq!(c.0.len(), 4);
        let file = c.0.iter().find(|r| r.2 == "file.txt").unwrap();
        assert_eq!(file.3.size, Some(5));
        assert!(file.3.date_modified.is_some());
        let b = c.0.iter().find(|r| r.2 == "b").unwrap();
        assert_eq!(file.1, b.0, "file's parent is b");
        assert!(b.3.attributes.unwrap() & attr::DIRECTORY != 0);
        assert_eq!(c.0.iter().find(|r| r.2 == "a").unwrap().1, FOLDER_ROOT_FRN);
    }

    #[test]
    fn into_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("x/y.txt"), b"").unwrap();
        let mut src = FolderSource::new(dir.path());
        let mut store = fnc_index::IndexStore::default();
        let vol = store.add_volume(src.info(), Some(src.root_frn())).unwrap();
        struct Sink<'a>(fnc_index::BulkLoader<'a>);
        impl EntrySink for Sink<'_> {
            fn entry(&mut self, e: &RawEntry<'_>) -> Result<(), SourceError> {
                self.0
                    .push(e.frn, e.parent_frn, e.name, &e.meta)
                    .map(|_| ())
                    .map_err(|e| SourceError::io("push", std::io::Error::other(e)))
            }
        }
        let mut sink = Sink(store.bulk_load(vol));
        src.enumerate(&mut sink, &StopFlag::new()).unwrap();
        sink.0.finish().unwrap();
        let y = (0..store.len() as u32).map(EntryId).find(|&i| store.name_string(i) == "y.txt").unwrap();
        let mut p = String::new();
        store.full_path(y, &mut p);
        let expected = dir.path().join("x").join("y.txt");
        assert_eq!(p, expected.to_string_lossy());
    }
}

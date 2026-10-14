//! Data-format versioning shared by every persisted store (ADR-038).
//!
//! Every file Fennec writes carries a format version: binary stores in their header
//! (`magic [u8;4] | version u32`), TOML files as a top-level `schema_version` key
//! (missing = 1). On load:
//!
//! * a version in `oldest_readable..=current` is decoded (per-version decoders);
//! * an older unreadable version, a future version or a damaged file is never used and
//!   never silently deleted: it is moved (or, for files still partly used, copied) to
//!   `<name>.v<N>.bak` / `<name>.corrupt.bak` and the store is rebuilt; the caller tells
//!   the user ([`Recovery`]).
//!
//! Before a store replaces a file on disk, [`protect_future`] moves a file of a *newer*
//! format out of the way first, so a future-version file is never overwritten even when
//! the loader did not look at it (`-reindex`, a rebuild while running).

use std::io::Read;
use std::path::{Path, PathBuf};

/// Why a stored file is not loaded as it is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// Written in an older format that this build cannot decode any more.
    Old(u32),
    /// Written by a newer Fennec (format version above this build's).
    Future(u32),
    /// Damaged: bad magic, checksum mismatch, truncated or unparseable.
    Corrupt,
}

impl Unreadable {
    /// Middle part of the backup name: `v<N>` or `corrupt`.
    pub fn tag(self) -> String {
        match self {
            Unreadable::Old(v) | Unreadable::Future(v) => format!("v{v}"),
            Unreadable::Corrupt => "corrupt".to_string(),
        }
    }
}

/// Checks a stored format version: `Ok(found)` when a decoder for it exists
/// (`oldest..=current`), otherwise why not.
pub fn check_version(found: u32, oldest: u32, current: u32) -> Result<u32, Unreadable> {
    if found > current {
        Err(Unreadable::Future(found))
    } else if found < oldest {
        Err(Unreadable::Old(found))
    } else {
        Ok(found)
    }
}

/// A store that was not loaded from its file (or only partly): what to tell the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// The file that was found.
    pub file: PathBuf,
    /// Why it was not (fully) used.
    pub why: Unreadable,
    /// Where the original was kept; `Err` = the backup failed (message).
    pub backup: Result<PathBuf, String>,
}

impl Recovery {
    /// Moves `file` aside ([`quarantine`]) and describes it.
    pub fn quarantined(file: &Path, why: Unreadable) -> Self {
        Self { file: file.to_path_buf(), why, backup: quarantine(file, why).map_err(|e| e.to_string()) }
    }

    /// Copies `file` ([`backup_copy`]) and describes it (the file stays in use).
    pub fn copied(file: &Path, why: Unreadable) -> Self {
        Self { file: file.to_path_buf(), why, backup: backup_copy(file, why).map_err(|e| e.to_string()) }
    }
}

fn candidate(path: &Path, tag: &str, n: u32) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    if n <= 1 {
        s.push(format!(".{tag}.bak"));
    } else {
        s.push(format!(".{tag}.{n}.bak"));
    }
    PathBuf::from(s)
}

/// Most numbered backups of one kind tried before the last one is reused.
const MAX_BACKUPS: u32 = 1000;

/// First free backup name for `path`: `<path>.<tag>.bak`, then `<path>.<tag>.2.bak`, …
pub fn backup_path(path: &Path, why: Unreadable) -> PathBuf {
    let tag = why.tag();
    (1..MAX_BACKUPS)
        .map(|n| candidate(path, &tag, n))
        .find(|p| !p.exists())
        .unwrap_or_else(|| candidate(path, &tag, MAX_BACKUPS))
}

/// Moves `path` to [`backup_path`] (rename; copy + remove when the rename fails, e.g.
/// across volumes). Returns the backup.
pub fn quarantine(path: &Path, why: Unreadable) -> std::io::Result<PathBuf> {
    let bak = backup_path(path, why);
    if std::fs::rename(path, &bak).is_err() {
        std::fs::copy(path, &bak)?;
        std::fs::remove_file(path)?;
    }
    tracing::warn!(file = %path.display(), backup = %bak.display(), reason = ?why, "data file set aside");
    Ok(bak)
}

/// Copies `path` to a backup name and keeps it in place (files that are still loaded, as
/// far as they can be). An existing backup with the same content is reused, so starting
/// several times on the same file makes one copy.
pub fn backup_copy(path: &Path, why: Unreadable) -> std::io::Result<PathBuf> {
    let content = std::fs::read(path)?;
    let tag = why.tag();
    for n in 1..MAX_BACKUPS {
        let c = candidate(path, &tag, n);
        match std::fs::read(&c) {
            Ok(old) if old == content => return Ok(c),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::write(&c, &content)?;
                tracing::warn!(file = %path.display(), backup = %c.display(), reason = ?why, "data file copied");
                return Ok(c);
            }
            Err(e) => return Err(e),
        }
    }
    let last = candidate(path, &tag, MAX_BACKUPS);
    std::fs::write(&last, &content)?;
    Ok(last)
}

/// Format version in the 8-byte header (`magic | version u32 LE`) of an existing file
/// with that `magic`; `None` if missing, shorter or another kind of file.
pub fn header_version(path: &Path, magic: [u8; 4]) -> Option<u32> {
    let mut head = [0u8; 8];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    (head[..4] == magic).then(|| u32::from_le_bytes([head[4], head[5], head[6], head[7]]))
}

/// Call before replacing `path` with a file of format `current`: if `path` holds a newer
/// format (same `magic`), it is moved to `<path>.v<N>.bak` first. Returns that backup.
/// An error means the newer file could not be moved and must not be replaced.
pub fn protect_future(path: &Path, magic: [u8; 4], current: u32) -> std::io::Result<Option<PathBuf>> {
    match header_version(path, magic) {
        Some(v) if v > current => quarantine(path, Unreadable::Future(v)).map(Some),
        _ => Ok(None),
    }
}

/// `schema_version` of a TOML document (missing or not an integer = 1).
pub fn toml_schema(table: &toml::Table) -> u32 {
    table.get(SCHEMA_KEY).and_then(toml::Value::as_integer).and_then(|v| u32::try_from(v).ok()).unwrap_or(1)
}

/// Name of the version key of TOML data files.
pub const SCHEMA_KEY: &str = "schema_version";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_classes() {
        assert_eq!(check_version(1, 1, 1), Ok(1));
        assert_eq!(check_version(2, 1, 3), Ok(2));
        assert_eq!(check_version(0, 1, 1), Err(Unreadable::Old(0)));
        assert_eq!(check_version(7, 1, 1), Err(Unreadable::Future(7)));
        assert_eq!(Unreadable::Future(7).tag(), "v7");
        assert_eq!(Unreadable::Corrupt.tag(), "corrupt");
    }

    #[test]
    fn quarantine_never_overwrites_a_backup() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.fnx");
        std::fs::write(&p, b"first").unwrap();
        let a = quarantine(&p, Unreadable::Corrupt).unwrap();
        assert_eq!(a, d.path().join("index.fnx.corrupt.bak"));
        assert!(!p.exists());
        std::fs::write(&p, b"second").unwrap();
        let b = quarantine(&p, Unreadable::Corrupt).unwrap();
        assert_eq!(b, d.path().join("index.fnx.corrupt.2.bak"));
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        assert_eq!(std::fs::read(&b).unwrap(), b"second");
        std::fs::write(&p, b"third").unwrap();
        assert_eq!(quarantine(&p, Unreadable::Future(3)).unwrap(), d.path().join("index.fnx.v3.bak"));
        let r = Recovery::quarantined(&d.path().join("missing"), Unreadable::Corrupt);
        assert!(r.backup.is_err());
    }

    #[test]
    fn backup_copy_keeps_the_file_and_dedups() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        std::fs::write(&p, b"schema_version = 2").unwrap();
        let a = backup_copy(&p, Unreadable::Future(2)).unwrap();
        assert_eq!(a, d.path().join("settings.toml.v2.bak"));
        assert!(p.exists());
        assert_eq!(backup_copy(&p, Unreadable::Future(2)).unwrap(), a, "same content: one copy");
        std::fs::write(&p, b"schema_version = 2\nx = 1").unwrap();
        assert_eq!(backup_copy(&p, Unreadable::Future(2)).unwrap(), d.path().join("settings.toml.v2.2.bak"));
    }

    #[test]
    fn future_files_are_moved_before_a_save() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("props.fnx");
        assert_eq!(protect_future(&p, *b"FNXP", 1).unwrap(), None);
        let mut b = b"FNXP".to_vec();
        b.extend_from_slice(&1u32.to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        assert_eq!(header_version(&p, *b"FNXP"), Some(1));
        assert_eq!(header_version(&p, *b"FNX1"), None);
        assert_eq!(protect_future(&p, *b"FNXP", 1).unwrap(), None, "current version stays");
        assert!(p.exists());
        b[4] = 2;
        std::fs::write(&p, &b).unwrap();
        assert_eq!(protect_future(&p, *b"FNXP", 1).unwrap(), Some(d.path().join("props.fnx.v2.bak")));
        assert!(!p.exists());
    }

    #[test]
    fn toml_schema_defaults_to_one() {
        let t: toml::Table = "a = 1".parse().unwrap();
        assert_eq!(toml_schema(&t), 1);
        let t: toml::Table = "schema_version = 4".parse().unwrap();
        assert_eq!(toml_schema(&t), 4);
        let t: toml::Table = "schema_version = \"x\"".parse().unwrap();
        assert_eq!(toml_schema(&t), 1);
    }
}

//! Versioned TOML data files and recovery notices (ADR-038).
//!
//! `filters.toml`, `bookmarks.toml`, `search_history.toml` and `session.toml` start with
//! `schema_version` ([`DATA_SCHEMA`]; files without it, as written by 0.1.0, are schema 1).
//! A file of another schema is read as far as possible and copied to `<name>.v<N>.bak`
//! first; a file that cannot be read is copied to `<name>.corrupt.bak` and the data
//! starts empty. Either way nothing the user saved is lost when the file is written
//! again, and the user is told ([`notice`]).

use std::path::Path;

use fnc_core::upgrade::{Recovery, SCHEMA_KEY, Unreadable, backup_copy, toml_schema};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::i18n::t_args;

/// Schema of the TOML data files written by this version.
pub const DATA_SCHEMA: u32 = 1;

/// Reads a TOML data file: missing = default; see the module docs for the rest.
pub fn load_toml<T: DeserializeOwned + Default>(path: &Path) -> (T, Option<Recovery>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (T::default(), None),
        Err(e) => {
            tracing::warn!(file = %path.display(), error = %e, "cannot read data file");
            return (T::default(), Some(Recovery::copied(path, Unreadable::Corrupt)));
        }
    };
    let mut table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let e: toml::de::Error = e;
            tracing::warn!(file = %path.display(), error = %e, "damaged data file");
            return (T::default(), Some(Recovery::copied(path, Unreadable::Corrupt)));
        }
    };
    let schema = toml_schema(&table);
    table.remove(SCHEMA_KEY);
    let mut recovery = match schema {
        DATA_SCHEMA => None,
        v if v > DATA_SCHEMA => Some(Recovery::copied(path, Unreadable::Future(v))),
        v => Some(Recovery::copied(path, Unreadable::Old(v))),
    };
    match T::deserialize(table) {
        Ok(v) => (v, recovery),
        Err(e) => {
            tracing::warn!(file = %path.display(), error = %e, "unreadable data file");
            if recovery.is_none() {
                recovery = Some(Recovery::copied(path, Unreadable::Corrupt));
            }
            (T::default(), recovery)
        }
    }
}

/// `value` as TOML with `schema_version` first.
pub fn to_toml<T: Serialize>(value: &T) -> std::io::Result<String> {
    let body = toml::to_string(value).map_err(std::io::Error::other)?;
    Ok(format!("{SCHEMA_KEY} = {DATA_SCHEMA}\n{body}"))
}

/// Writes `value` to `path` (atomic); a file of a newer schema is copied first.
pub fn save_toml<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let text = to_toml(value)?;
    if let Ok(old) = std::fs::read_to_string(path)
        && let Ok(table) = old.parse::<toml::Table>()
        && toml_schema(&table) > DATA_SCHEMA
    {
        backup_copy(path, Unreadable::Future(toml_schema(&table)))?;
    }
    crate::history::write_atomic(path, &text)
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Localised status-bar text for a recovered file. `rebuilt`: the data is derived and
/// is being rebuilt (index, property/content stores) rather than starting empty.
pub fn notice(r: &Recovery, rebuilt: bool) -> String {
    let file = file_name(&r.file);
    let backup = match &r.backup {
        Ok(b) => file_name(b),
        Err(e) => return t_args("upgrade-backup-failed", &[("file", file.into()), ("message", e.clone().into())]),
    };
    let (key, version) = match (r.why, rebuilt) {
        (Unreadable::Future(v), true) => ("upgrade-rebuild-future", v),
        (Unreadable::Old(v), true) => ("upgrade-rebuild-old", v),
        (Unreadable::Corrupt, true) => ("upgrade-rebuild-corrupt", 0),
        (Unreadable::Future(v), false) => ("upgrade-data-future", v),
        (Unreadable::Old(v), false) => ("upgrade-data-old", v),
        (Unreadable::Corrupt, false) => ("upgrade-data-corrupt", 0),
    };
    t_args(key, &[("file", file.into()), ("backup", backup.into()), ("version", version.into())])
}

/// Status-bar text for what [`fnc_core::settings::Settings::load_checked`] found.
pub fn settings_notice(load: &fnc_core::settings::SettingsLoad) -> Option<String> {
    let r = load.recovery.as_ref()?;
    match (&r.backup, load.reset_keys.is_empty()) {
        (Ok(b), false) => Some(t_args(
            "upgrade-settings-reset",
            &[("keys", load.reset_keys.join(", ").into()), ("backup", file_name(b).into())],
        )),
        _ => Some(notice(r, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, serde::Deserialize, Serialize)]
    struct Doc {
        #[serde(default)]
        item: Vec<String>,
    }

    #[test]
    fn data_file_header_cases() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("filters.toml");
        assert_eq!(load_toml::<Doc>(&p), (Doc::default(), None));
        // 0.1.0 file (no schema_version) and the current round trip.
        std::fs::write(&p, "item = [\"a\"]\n").unwrap();
        assert_eq!(load_toml::<Doc>(&p), (Doc { item: vec!["a".into()] }, None));
        let doc = Doc { item: vec!["a".into(), "b".into()] };
        save_toml(&p, &doc).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().starts_with("schema_version = 1\n"));
        assert_eq!(load_toml::<Doc>(&p), (Doc { item: vec!["a".into(), "b".into()] }, None));
        // Future schema: read what is understood, original copied, kept across a save.
        let future = "schema_version = 3\nitem = [\"x\"]\nnew_thing = 1\n";
        std::fs::write(&p, future).unwrap();
        let (v, r) = load_toml::<Doc>(&p);
        assert_eq!(v.item, vec!["x".to_string()]);
        let r = r.unwrap();
        assert_eq!(r.why, Unreadable::Future(3));
        let bak = d.path().join("filters.toml.v3.bak");
        assert_eq!(r.backup.as_ref().unwrap(), &bak);
        assert!(notice(&r, false).contains("filters.toml.v3.bak"), "{}", notice(&r, false));
        std::fs::remove_file(&bak).unwrap();
        save_toml(&p, &v).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), future, "a save copies the newer file first");
        // Old schema (below the first one): loaded as far as possible, copied.
        std::fs::write(&p, "schema_version = 0\nitem = [\"o\"]\n").unwrap();
        let (v, r) = load_toml::<Doc>(&p);
        assert_eq!((v.item, r.unwrap().why), (vec!["o".to_string()], Unreadable::Old(0)));
        // Damaged (syntax or types): empty data, original copied.
        for bad in ["item = [", "item = 5"] {
            std::fs::write(&p, bad).unwrap();
            let (v, r) = load_toml::<Doc>(&p);
            let r = r.unwrap();
            assert_eq!((v, r.why), (Doc::default(), Unreadable::Corrupt));
            assert_eq!(std::fs::read_to_string(r.backup.unwrap()).unwrap(), bad);
        }
    }

    #[test]
    fn notices_name_the_backup() {
        crate::i18n::init("en");
        let r = Recovery {
            file: "C:\\x\\index.fnx".into(),
            why: Unreadable::Future(4),
            backup: Ok("C:\\x\\index.fnx.v4.bak".into()),
        };
        let n = notice(&r, true);
        assert!(n.contains("index.fnx.v4.bak") && n.contains('4'), "{n}");
        let r = Recovery { backup: Err("denied".into()), ..r };
        assert!(notice(&r, true).contains("denied"));
    }

    #[test]
    fn settings_notices() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        std::fs::write(&p, "search_delay = 5\n").unwrap();
        let (_, load) = fnc_core::settings::Settings::load_checked(&p);
        assert_eq!(settings_notice(&load), None, "a 0.1.0 file loads silently");
        std::fs::write(&p, "search_delay = \"x\"\n").unwrap();
        let (_, load) = fnc_core::settings::Settings::load_checked(&p);
        let n = settings_notice(&load).unwrap();
        assert!(n.contains("search_delay") && n.contains("settings.toml.corrupt.bak"), "{n}");
        std::fs::write(&p, "schema_version = 9\n").unwrap();
        let (_, load) = fnc_core::settings::Settings::load_checked(&p);
        assert!(settings_notice(&load).unwrap().contains("settings.toml.v9.bak"));
    }
}

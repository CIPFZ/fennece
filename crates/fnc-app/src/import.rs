//! Settings import/export (SPEC ch.4 §7): `settings.toml` files and the mappable subset
//! of Everything's `Everything.ini` (keys Fennec shares with Everything 1.5).

use fnc_core::settings::{Settings, SettingsMeta};

/// Everything.ini keys whose names differ from Fennec's.
const RENAMED: &[(&str, &str)] = &[
    ("match_whole_word", "match_whole_word"),
    ("search_as_you_type_delay", "search_delay"),
    ("run_in_background", "close_to_tray"),
    ("show_tray_icon", "show_tray_icon"),
    ("folders_first", "folders_first"),
    ("hide_on_close", "close_to_tray"),
    ("index_date_accessed", "index_date_accessed"),
    ("index_size", "index_size"),
    ("index_folder_size", "index_size"),
    ("exclude_hidden_files_and_folders", "exclude_hidden"),
    ("exclude_system_files_and_folders", "exclude_system"),
    ("exclude_folders", "exclude_folders"),
    ("exclude_files", "exclude_files"),
    ("filelists", "file_lists"),
    ("folders", "folders"),
    ("language", "language"),
];

/// Applies the keys of an `Everything.ini` that Fennec understands; returns the keys
/// applied. Everything stores lists comma separated with quotes; Fennec uses `;`.
pub fn import_everything_ini(settings: &mut Settings, ini: &str) -> Vec<String> {
    let mut applied = Vec::new();
    for line in ini.lines() {
        let line = line.trim();
        if line.starts_with('[') || line.starts_with(';') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let key = RENAMED.iter().find(|(e, _)| *e == k).map_or(k.as_str(), |(_, f)| f);
        if !Settings::descriptors().iter().any(|d| d.key == key) {
            continue;
        }
        // `"C:\a","D:\b"` → `C:\a;D:\b`
        let v = v.trim();
        let value = if v.contains("\",\"") || (v.starts_with('"') && v.ends_with('"')) {
            v.split("\",\"").map(|p| p.trim_matches('"')).collect::<Vec<_>>().join(";")
        } else {
            v.to_string()
        };
        // Everything writes booleans as 0/1, which `set` understands.
        if settings.set(key, &value).is_ok() {
            applied.push(key.to_string());
        }
    }
    applied
}

/// Reads a `settings.toml` (unknown keys are ignored with a warning).
pub fn import_settings(text: &str) -> Result<Settings, String> {
    Settings::from_toml(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_ini_subset() {
        let ini = "[Everything]\nmatch_case=1\nsearch_as_you_type_delay=150\nexclude_folders=\"C:\\tmp\",\"D:\\cache\"\nunknown_key=5\nhide_on_close=0\n";
        let mut s = Settings::default();
        let applied = import_everything_ini(&mut s, ini);
        assert!(s.match_case);
        assert_eq!(s.search_delay, 150);
        assert_eq!(s.exclude_folders, vec!["C:\\tmp".to_string(), "D:\\cache".to_string()]);
        assert!(!s.close_to_tray);
        assert_eq!(applied.len(), 4);
        let t = s.to_toml().unwrap();
        assert_eq!(import_settings(&t).unwrap(), s);
    }
}

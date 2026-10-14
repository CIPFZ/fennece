//! Data directory resolution (SPEC ch.0 §6.4).
//!
//! * Installed: `%APPDATA%\Fennec` (Windows) or `$XDG_DATA_HOME/fennec`
//!   (`~/.local/share/fennec`) elsewhere.
//! * Portable: if a file named `portable` exists next to the executable, everything
//!   is stored in `<exe dir>/data`.
//! * Instances: `-instance <name>` uses `<base>/instances/<name>`.
//! * Read-only portable folder (e.g. unpacked below `C:\Program Files`): the data goes to
//!   `%LOCALAPPDATA%\Fennec\portable` (`<installed base>/portable` elsewhere) and
//!   [`DataPaths::unwritable_portable`] names the refused folder so the UI can warn.

use std::path::{Path, PathBuf};

/// Resolved locations of all persistent files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPaths {
    /// Root data directory.
    pub root: PathBuf,
    /// Whether portable mode is active.
    pub portable: bool,
    /// Portable mode only: the `<exe dir>/data` folder that could not be written, when
    /// [`DataPaths::root`] is the per-user fallback instead (`None`: no fallback).
    pub unwritable_portable: Option<PathBuf>,
}

impl DataPaths {
    /// Resolves the data directory for `instance` (empty = default instance).
    pub fn resolve(instance: &str) -> Self {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        Self::resolve_with(instance, exe_dir.as_deref(), &env).with_writable_fallback(instance, &env, &is_writable)
    }

    /// Portable data folder next to the executable (`-portable`, `-no-app-data`).
    pub fn portable(instance: &str) -> Self {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
        let root = instance_root(exe_dir.join("data"), instance);
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        Self { root, portable: true, unwritable_portable: None }.with_writable_fallback(instance, &env, &is_writable)
    }

    /// Testable core of [`DataPaths::resolve`].
    pub fn resolve_with(instance: &str, exe_dir: Option<&Path>, env: &dyn Fn(&str) -> Option<PathBuf>) -> Self {
        let (base, portable) = match exe_dir {
            Some(dir) if dir.join("portable").is_file() => (dir.join("data"), true),
            _ => (Self::installed_base(env), false),
        };
        Self { root: instance_root(base, instance), portable, unwritable_portable: None }
    }

    /// Portable mode: if `writable(root)` fails, moves the data to the per-user fallback
    /// (`%LOCALAPPDATA%\Fennec\portable`) and records the refused folder. Installed mode
    /// and writable portable folders are returned unchanged.
    pub fn with_writable_fallback(
        self,
        instance: &str,
        env: &dyn Fn(&str) -> Option<PathBuf>,
        writable: &dyn Fn(&Path) -> bool,
    ) -> Self {
        if !self.portable || writable(&self.root) {
            return self;
        }
        let base = match env("LOCALAPPDATA").filter(|_| cfg!(windows)) {
            Some(local) => local.join("Fennec").join("portable"),
            None => Self::installed_base(env).join("portable"),
        };
        Self { root: instance_root(base, instance), portable: true, unwritable_portable: Some(self.root) }
    }

    fn installed_base(env: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        if cfg!(windows)
            && let Some(appdata) = env("APPDATA")
        {
            return appdata.join("Fennec");
        }
        if let Some(xdg) = env("XDG_DATA_HOME").filter(|p| p.is_absolute()) {
            return xdg.join("fennec");
        }
        if let Some(home) = env("HOME").or_else(|| env("USERPROFILE")) {
            return home.join(".local").join("share").join("fennec");
        }
        PathBuf::from("fennec-data")
    }

    /// `settings.toml`.
    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    /// `index.fnx`.
    pub fn index_file(&self) -> PathBuf {
        self.root.join("index.fnx")
    }

    /// `content.fnx`.
    pub fn content_file(&self) -> PathBuf {
        self.root.join("content.fnx")
    }

    /// `vectors/`.
    pub fn vectors_dir(&self) -> PathBuf {
        self.root.join("vectors")
    }

    /// `logs/`.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
}

/// Whether files can be created in `dir` (created if missing): a probe file is written
/// and removed again.
pub fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".fennec-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// `base` or `base/instances/<name>`.
fn instance_root(base: PathBuf, instance: &str) -> PathBuf {
    if instance.is_empty() { base } else { base.join("instances").join(sanitize_instance(instance)) }
}

/// Keeps instance names safe as a single path component.
fn sanitize_instance(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_mode_uses_exe_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("portable"), b"").unwrap();
        let p = DataPaths::resolve_with("", Some(dir.path()), &|_| None);
        assert!(p.portable);
        assert_eq!(p.root, dir.path().join("data"));
        assert_eq!(p.settings_file(), dir.path().join("data").join("settings.toml"));
    }

    #[test]
    fn installed_mode_and_instances() {
        let dir = tempfile::tempdir().unwrap();
        let home = PathBuf::from("/home/u");
        let env = |k: &str| match k {
            "HOME" => Some(home.clone()),
            "APPDATA" => Some(PathBuf::from("C:\\Users\\u\\AppData\\Roaming")),
            _ => None,
        };
        let p = DataPaths::resolve_with("", Some(dir.path()), &env);
        assert!(!p.portable);
        let base = if cfg!(windows) {
            PathBuf::from("C:\\Users\\u\\AppData\\Roaming").join("Fennec")
        } else {
            home.join(".local/share/fennec")
        };
        assert_eq!(p.root, base);
        let q = DataPaths::resolve_with("work/../x", Some(dir.path()), &env);
        assert_eq!(q.root, base.join("instances").join("work____x"));
    }

    #[test]
    fn unwritable_portable_folder_falls_back_per_user() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("portable"), b"").unwrap();
        let env = |k: &str| match k {
            "HOME" => Some(PathBuf::from("/home/u")),
            "APPDATA" => Some(PathBuf::from("C:\\Users\\u\\AppData\\Roaming")),
            "LOCALAPPDATA" => Some(PathBuf::from("C:\\Users\\u\\AppData\\Local")),
            _ => None,
        };
        let fallback = if cfg!(windows) {
            PathBuf::from("C:\\Users\\u\\AppData\\Local").join("Fennec").join("portable")
        } else {
            PathBuf::from("/home/u/.local/share/fennec/portable")
        };
        // Writable: unchanged.
        let p = DataPaths::resolve_with("", Some(dir.path()), &env).with_writable_fallback("", &env, &|_| true);
        assert_eq!((p.root.clone(), p.unwritable_portable.clone()), (dir.path().join("data"), None));
        // Read-only: per-user fallback, the refused folder is reported; instances stay apart.
        let p = DataPaths::resolve_with("", Some(dir.path()), &env).with_writable_fallback("", &env, &|_| false);
        assert!(p.portable);
        assert_eq!(p.root, fallback);
        assert_eq!(p.unwritable_portable, Some(dir.path().join("data")));
        let q = DataPaths::resolve_with("w", Some(dir.path()), &env).with_writable_fallback("w", &env, &|_| false);
        assert_eq!(q.root, fallback.join("instances").join("w"));
        assert_eq!(q.unwritable_portable, Some(dir.path().join("data").join("instances").join("w")));
        // Installed mode never falls back.
        let other = tempfile::tempdir().unwrap();
        let i = DataPaths::resolve_with("", Some(other.path()), &env).with_writable_fallback("", &env, &|_| false);
        assert!(!i.portable && i.unwritable_portable.is_none());
    }

    #[test]
    fn writable_probe() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        assert!(is_writable(&data));
        assert!(data.is_dir());
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0, "probe file left behind");
        // A path below a regular file can never be created (holds for root too).
        let file = dir.path().join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(!is_writable(&file.join("data")));
    }
}

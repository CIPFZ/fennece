//! Data directory resolution (SPEC ch.0 §6.4).
//!
//! * Installed: `%APPDATA%\Fennec` (Windows) or `$XDG_DATA_HOME/fennec`
//!   (`~/.local/share/fennec`) elsewhere.
//! * Portable: if a file named `portable` exists next to the executable, everything
//!   is stored in `<exe dir>/data`.
//! * Instances: `-instance <name>` uses `<base>/instances/<name>`.

use std::path::{Path, PathBuf};

/// Resolved locations of all persistent files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPaths {
    /// Root data directory.
    pub root: PathBuf,
    /// Whether portable mode is active.
    pub portable: bool,
}

impl DataPaths {
    /// Resolves the data directory for `instance` (empty = default instance).
    pub fn resolve(instance: &str) -> Self {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        Self::resolve_with(instance, exe_dir.as_deref(), &|k| std::env::var_os(k).map(PathBuf::from))
    }

    /// Testable core of [`DataPaths::resolve`].
    pub fn resolve_with(instance: &str, exe_dir: Option<&Path>, env: &dyn Fn(&str) -> Option<PathBuf>) -> Self {
        let (base, portable) = match exe_dir {
            Some(dir) if dir.join("portable").is_file() => (dir.join("data"), true),
            _ => (Self::installed_base(env), false),
        };
        let root = if instance.is_empty() { base } else { base.join("instances").join(sanitize_instance(instance)) };
        Self { root, portable }
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
}

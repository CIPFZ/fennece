//! File type descriptions for `type:` (SPEC ch.2 §5.1).
//!
//! Folders are "File folder", files without an extension "File". For other files the
//! front end may inject a [`TypeNames`] provider (on Windows: registry `HKCR\.ext` →
//! ProgID → default value); without one, or when it has no answer, the description is
//! Explorer's fallback "`EXT` File".

use std::fmt;
use std::sync::Arc;

/// Description of folders.
pub const FOLDER_TYPE: &str = "File folder";
/// Description of files without an extension.
pub const NO_EXT_TYPE: &str = "File";

/// Type-name lookup: folded extension without the dot (`"txt"`) → description
/// (`"Text Document"`). Called once per extension per index generation.
pub type TypeNameFn = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Shared type-name provider (compared by identity).
#[derive(Clone)]
pub struct TypeNames(pub Arc<TypeNameFn>);

impl TypeNames {
    /// Wraps a lookup function.
    pub fn new(f: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Looks up `ext`.
    pub fn lookup(&self, ext: &str) -> Option<String> {
        (self.0)(ext)
    }
}

impl fmt::Debug for TypeNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TypeNames(..)")
    }
}

impl PartialEq for TypeNames {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Explorer's fallback description: `"TXT File"`.
pub fn fallback_type_name(ext: &str) -> String {
    format!("{} File", ext.to_uppercase())
}

/// Description of a file with extension `ext` (no dot).
// SPEC-GAP: registry type names arrive via the injected provider (fnc-platform on
// Windows); fnc-exec itself only knows the fallback.
pub fn type_name(ext: &str, provider: Option<&TypeNames>) -> String {
    if ext.is_empty() {
        return NO_EXT_TYPE.to_string();
    }
    provider.and_then(|p| p.lookup(ext)).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| fallback_type_name(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_and_provider() {
        assert_eq!(type_name("txt", None), "TXT File");
        assert_eq!(type_name("", None), "File");
        let p = TypeNames::new(|e| (e == "txt").then(|| "Text Document".to_string()));
        assert_eq!(type_name("txt", Some(&p)), "Text Document");
        assert_eq!(type_name("jpg", Some(&p)), "JPG File");
        assert_eq!(p, p.clone());
        assert_ne!(p, TypeNames::new(|_| None));
        assert_eq!(format!("{p:?}"), "TypeNames(..)");
    }
}

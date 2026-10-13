//! Search history (SPEC ch.3 §2): the last searches ranked by frequency and recency,
//! persisted in `search_history.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One remembered search.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Search text.
    pub text: String,
    /// Times used.
    pub count: u32,
    /// Last use (Unix seconds).
    pub last: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    search: Vec<HistoryEntry>,
}

/// Search history with a capacity.
#[derive(Debug, Default)]
pub struct SearchHistory {
    entries: Vec<HistoryEntry>,
    path: Option<PathBuf>,
    max: usize,
    dirty: bool,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl SearchHistory {
    /// Loads `path` (missing or unreadable files start empty).
    pub fn load(path: PathBuf, max: usize) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<File>(&t).ok())
            .map(|f| f.search)
            .unwrap_or_default();
        let mut h = Self { entries, path: Some(path), max, dirty: false };
        h.trim();
        h
    }

    /// In-memory history (tests).
    pub fn in_memory(max: usize) -> Self {
        Self { max, ..Self::default() }
    }

    fn score(e: &HistoryEntry, now: i64) -> f64 {
        // Frequency, decayed by age (half-life one week).
        let age_weeks = (now - e.last).max(0) as f64 / (7.0 * 86_400.0);
        f64::from(e.count) * 0.5f64.powf(age_weeks)
    }

    fn trim(&mut self) {
        let t = now();
        self.entries.sort_by(|a, b| Self::score(b, t).total_cmp(&Self::score(a, t)).then(b.last.cmp(&a.last)));
        self.entries.truncate(self.max);
    }

    /// Records a use of `text` (blank searches are ignored).
    pub fn record(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() || self.max == 0 {
            return;
        }
        let t = now();
        match self.entries.iter_mut().find(|e| e.text == text) {
            Some(e) => {
                e.count = e.count.saturating_add(1);
                e.last = t;
            }
            None => self.entries.push(HistoryEntry { text: text.to_string(), count: 1, last: t }),
        }
        self.trim();
        self.dirty = true;
    }

    /// Removes one entry (`Del` in the history list).
    pub fn remove(&mut self, text: &str) {
        self.entries.retain(|e| e.text != text);
        self.dirty = true;
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.dirty = true;
    }

    /// Entries, best first.
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Entries containing `prefix` (case-insensitive), best first, prefix matches first.
    pub fn matching(&self, text: &str, limit: usize) -> Vec<&HistoryEntry> {
        let q = text.to_lowercase();
        let mut v: Vec<&HistoryEntry> =
            self.entries.iter().filter(|e| e.text != text && e.text.to_lowercase().contains(&q)).collect();
        v.sort_by_key(|e| !e.text.to_lowercase().starts_with(&q));
        v.truncate(limit);
        v
    }

    /// The history as CSV (`Search,Count,Last Used`).
    pub fn export_csv(&self) -> String {
        let mut out = String::from("Search,Count,Last Used\r\n");
        for e in &self.entries {
            let when = fnc_core::FileTime::from_unix(e.last, 0)
                .to_local()
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            out.push_str(&format!("\"{}\",{},{when}\r\n", e.text.replace('"', "\"\""), e.count));
        }
        out
    }

    /// Writes the file if changed.
    pub fn save(&mut self) {
        let Some(path) = &self.path else { return };
        if !self.dirty {
            return;
        }
        match toml::to_string(&File { search: self.entries.clone() }) {
            Ok(text) => {
                if let Err(e) = write_atomic(path, &text) {
                    tracing::warn!(error = %e, "cannot save search history");
                } else {
                    self.dirty = false;
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot serialise search history"),
        }
    }
}

/// `path.tmp` + rename.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_by_frequency_and_persists() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("h.toml");
        let mut h = SearchHistory::load(p.clone(), 3);
        for t in ["a", "b", "b", "c", "  ", "d"] {
            h.record(t);
        }
        let texts: Vec<_> = h.entries().iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[0], "b");
        h.save();
        let h2 = SearchHistory::load(p, 3);
        assert_eq!(h2.entries(), h.entries());
        assert_eq!(h2.matching("B", 5)[0].text, "b");
        let mut h3 = h2;
        h3.remove("b");
        assert!(h3.entries().iter().all(|e| e.text != "b"));
    }
}

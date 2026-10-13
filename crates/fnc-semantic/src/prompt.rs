//! Task prefixes (SPEC ch.5 §1, §6.2). They must match the model card exactly, missing or
//! altered prefixes silently lower recall. Media inputs take no prefix.

use serde::{Deserialize, Serialize};

/// What the query is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Task {
    /// Search result retrieval (file names, documents).
    Search,
    /// Code retrieval.
    Code,
}

/// Prompt templates of one model; `{q}`, `{title}` and `{text}` are substituted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prompts {
    /// Search query.
    pub query: String,
    /// Code query.
    pub code_query: String,
    /// Document.
    pub document: String,
}

impl Default for Prompts {
    /// EmbeddingGemma (v1 and v2).
    fn default() -> Self {
        Self {
            query: "task: search result | query: {q}".into(),
            code_query: "task: code retrieval | query: {q}".into(),
            document: "title: {title} | text: {text}".into(),
        }
    }
}

impl Prompts {
    /// Templates that pass the text through unchanged (models without prefixes).
    pub fn plain() -> Self {
        Self { query: "{q}".into(), code_query: "{q}".into(), document: "{text}".into() }
    }

    /// The query text to embed.
    pub fn query(&self, task: Task, q: &str) -> String {
        let t = match task {
            Task::Search => &self.query,
            Task::Code => &self.code_query,
        };
        t.replace("{q}", q)
    }

    /// The document text to embed; no title becomes `none`.
    pub fn document(&self, title: Option<&str>, text: &str) -> String {
        // `{text}` last so a title containing "{text}" is not expanded.
        self.document.replace("{title}", title.unwrap_or("none")).replace("{text}", text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeddinggemma_prefixes_are_exact() {
        let p = Prompts::default();
        assert_eq!(p.query(Task::Search, "季度预算"), "task: search result | query: 季度预算");
        assert_eq!(p.query(Task::Code, "parse usn"), "task: code retrieval | query: parse usn");
        assert_eq!(p.document(None, "hello"), "title: none | text: hello");
        assert_eq!(p.document(Some("usn.rs"), "fn read()"), "title: usn.rs | text: fn read()");
        assert_eq!(Prompts::plain().document(Some("x"), "y"), "y");
    }
}

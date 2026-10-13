//! Model registry (SPEC ch.5 §5): built-in entries plus `models.toml` (`[[model]]` tables,
//! same fields; an entry with a built-in id replaces it).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::prompt::Prompts;

/// One embedding model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSpec {
    /// Stable id (stored with the vectors).
    pub id: String,
    /// Display name.
    pub name: String,
    /// `gguf` (llama.cpp) — the only family this build runs.
    pub family: String,
    /// File name in the models folder, or an absolute path.
    pub file: String,
    /// Download URL (`None`: the user downloads it).
    pub url: Option<String>,
    /// Expected SHA-256 of the file (hex).
    pub sha256: Option<String>,
    /// Expected size in bytes.
    pub size: Option<u64>,
    /// Native output dimension.
    pub dim: usize,
    /// Longest input in tokens.
    pub max_tokens: usize,
    /// Vectors may be truncated (Matryoshka); otherwise only `dim` is valid.
    pub matryoshka: bool,
    /// Prompt templates.
    pub prompts: Prompts,
    /// Licence of the weights.
    pub license: String,
}

impl Default for ModelSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            family: "gguf".into(),
            file: String::new(),
            url: None,
            sha256: None,
            size: None,
            dim: 768,
            max_tokens: 2048,
            matryoshka: false,
            prompts: Prompts::default(),
            license: String::new(),
        }
    }
}

impl ModelSpec {
    /// Dimension actually stored for a requested `dim` (non-Matryoshka models: native).
    pub fn store_dim(&self, dim: usize) -> usize {
        if self.matryoshka && dim > 0 { dim.min(self.dim) } else { self.dim }
    }
}

/// Id of the default model.
pub const DEFAULT_MODEL: &str = "embeddinggemma-2";

/// The built-in entries.
pub fn builtin() -> Vec<ModelSpec> {
    let gemma = Prompts::default();
    vec![
        ModelSpec {
            id: DEFAULT_MODEL.into(),
            name: "EmbeddingGemma-2 (Q8_0, text)".into(),
            file: "embeddinggemma-2-Q8_0.gguf".into(),
            url: Some(
                "https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/main/embeddinggemma-2-Q8_0.gguf".into(),
            ),
            sha256: Some("2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135".into()),
            size: Some(309_855_456),
            dim: 768,
            max_tokens: 8192,
            matryoshka: true,
            prompts: gemma.clone(),
            license: "Apache-2.0".into(),
            ..ModelSpec::default()
        },
        ModelSpec {
            id: "embeddinggemma-300m".into(),
            name: "EmbeddingGemma 300M (v1, Q8_0)".into(),
            file: "embeddinggemma-300M-Q8_0.gguf".into(),
            url: Some(
                "https://huggingface.co/ggml-org/embeddinggemma-300M-GGUF/resolve/main/embeddinggemma-300M-Q8_0.gguf"
                    .into(),
            ),
            size: Some(333_590_944),
            dim: 768,
            max_tokens: 2048,
            matryoshka: true,
            prompts: gemma,
            license: "Gemma".into(),
            ..ModelSpec::default()
        },
        ModelSpec {
            id: "bge-m3".into(),
            name: "BGE-M3".into(),
            file: "bge-m3-Q8_0.gguf".into(),
            dim: 1024,
            max_tokens: 8192,
            prompts: Prompts::plain(),
            license: "MIT".into(),
            ..ModelSpec::default()
        },
        ModelSpec {
            id: "qwen3-embedding-0.6b".into(),
            name: "Qwen3-Embedding 0.6B".into(),
            file: "Qwen3-Embedding-0.6B-Q8_0.gguf".into(),
            dim: 1024,
            max_tokens: 8192,
            matryoshka: true,
            prompts: Prompts {
                query:
                    "Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:{q}"
                        .into(),
                code_query: "Instruct: Given a question about code, retrieve the code that answers it\nQuery:{q}"
                    .into(),
                document: "{text}".into(),
            },
            license: "Apache-2.0".into(),
            ..ModelSpec::default()
        },
        ModelSpec {
            id: "nomic-embed-text-v2".into(),
            name: "Nomic Embed Text v2 MoE".into(),
            file: "nomic-embed-text-v2-moe.Q8_0.gguf".into(),
            dim: 768,
            max_tokens: 512,
            matryoshka: true,
            prompts: Prompts {
                query: "search_query: {q}".into(),
                code_query: "search_query: {q}".into(),
                document: "search_document: {text}".into(),
            },
            license: "Apache-2.0".into(),
            ..ModelSpec::default()
        },
    ]
}

#[derive(Deserialize)]
struct Registry {
    #[serde(default)]
    model: Vec<ModelSpec>,
}

/// Built-in models merged with `models.toml` at `path` (if present and valid).
pub fn registry(path: Option<&Path>) -> Vec<ModelSpec> {
    let mut list = builtin();
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else { return list };
    match toml::from_str::<Registry>(&text) {
        Ok(r) => {
            for m in r.model.into_iter().filter(|m| !m.id.is_empty()) {
                match list.iter_mut().find(|x| x.id == m.id) {
                    Some(x) => *x = m,
                    None => list.push(m),
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "ignoring invalid models.toml"),
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_merges_user_entries() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("models.toml");
        std::fs::write(
            &p,
            "[[model]]\nid = \"bge-m3\"\nname = \"Mine\"\nfile = \"C:/m/bge.gguf\"\ndim = 1024\n\n[[model]]\nid = \"custom\"\nfile = \"c.gguf\"\ndim = 384\nmatryoshka = false\n",
        )
        .unwrap();
        let r = registry(Some(&p));
        assert_eq!(r.iter().find(|m| m.id == "bge-m3").unwrap().name, "Mine");
        let c = r.iter().find(|m| m.id == "custom").unwrap();
        assert_eq!((c.dim, c.store_dim(256)), (384, 384));
        let g = r.iter().find(|m| m.id == DEFAULT_MODEL).unwrap();
        assert_eq!(g.store_dim(256), 256);
        assert!(g.url.is_some() && g.sha256.is_some());
        std::fs::write(&p, "not toml [").unwrap();
        assert_eq!(registry(Some(&p)).len(), builtin().len());
    }
}

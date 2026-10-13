# Vendored dependencies

| Crate | Upstream | Changes |
|---|---|---|
| `llama-cpp-sys-2` 0.1.159 | crates.io | bundled `llama.cpp` replaced by upstream master `LLAMA_CPP_COMMIT` (adds the `gemma-embedding2` architecture of EmbeddingGemma-2); `common` removed from default features (its wrapper does not build against that commit); trimmed: models, docs, tests, examples, tools except `mtmd` |
| `llama-cpp-2` 0.1.159 | crates.io | `timing.rs`: initialise the two fields new in `llama_perf_context_data` |

Both are wired in through `[patch.crates-io]` in the workspace `Cargo.toml` (ADR-020).
Update: replace `llama-cpp-sys-2/llama.cpp` with a newer checkout, rerun `cargo build -p fnc-semantic`.

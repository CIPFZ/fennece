# fnc-semantic

Semantic search for Fennec (SPEC ch.5): the `Embedder` trait and its llama.cpp implementation
(feature `llama`, EmbeddingGemma-2 GGUF), filename/text/code chunking, the vector store (usearch
HNSW, int8, cosine) with its chunk table, the background semantic indexer and score fusion.
The llama.cpp sources are vendored under `third_party/` (ADR-020).

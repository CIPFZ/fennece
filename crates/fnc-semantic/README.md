# fnc-semantic (M5)

Local semantic search with EmbeddingGemma-2 (SPEC ch.5): `Embedder` trait, llama.cpp backend, chunker,
vector index, RRF fusion, background indexer.

**Invariants (planned)**: zero overhead when disabled (SEM-06); the model is loaded on demand and
unloaded when idle; literal results are never delayed by the semantic channel.

# ADR-020: Vendored llama.cpp for EmbeddingGemma-2

- Status: accepted (M5)
- Related: SPEC ch.5 §3.1 (Embedder), §6.1, ch.6 §6 (`llama-cpp-2`)

## Background
SPEC picks llama.cpp (through `llama-cpp-2`) to run EmbeddingGemma-2 GGUF models. The newest
published `llama-cpp-sys-2` (0.1.159, 2026-10-07) bundles a llama.cpp from before the
`gemma-embedding2` architecture was added, so loading `embeddinggemma-2-Q8_0.gguf` fails with
"unknown model architecture". Upstream llama.cpp master (2026-10-13) supports it.

## Decision
Vendor `llama-cpp-sys-2` and `llama-cpp-2` 0.1.159 under `third_party/` and patch them in with
`[patch.crates-io]`:
- the bundled llama.cpp is replaced by upstream master (commit in `LLAMA_CPP_COMMIT`), trimmed to
  the sources the CMake build needs (42 MB instead of 178 MB);
- the `common` feature (speculative-decoding helpers Fennec does not use) is off by default, its
  C++ wrapper does not compile against the newer commit;
- one struct initialiser in `llama-cpp-2` gets the two fields new in the C API.
The semantic crate is behind the `semantic` cargo feature of `fnc-app` (SPEC ch.6 §6), so builds
without it never compile llama.cpp.

Measured (Linux, 8 cores, Q8_0 text model): load 0.48–0.58 s; warm query embedding 32–85 ms;
512-token document 0.45 s; "季度预算表" ranks `2026Q2_budget.xlsx` first (cos 0.724 vs 0.58).

## Alternatives
Wait for a crates.io release (blocks M5); ONNX Runtime (`ort`) — no official ONNX export of
EmbeddingGemma-2 yet and a much larger runtime; candle — no gemma-embedding2 implementation.

## Consequences
Updating llama.cpp is a manual step (documented in `third_party/README.md`). Drop the vendored
copies once crates.io catches up.

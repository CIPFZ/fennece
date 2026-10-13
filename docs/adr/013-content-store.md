# ADR-013: Content store — LZ4 blocks and a token inverted index

- Status: accepted (M3, IDX-09)
- Related: SPEC ch.1 §9.3, ch.6 §8 Q6

## Background
`content.fnx` keeps one text per entry for `content:`, `contenthits:`, the line/word/char
counts and the preview panel. SPEC suggests zstd with a trained dictionary and an optional
inverted index; IDX-09 asks for `content:"quarterly plan"` < 200 ms on an indexed folder.
`content:` has substring semantics (`content:plan` finds "planning"), case/diacritic folding,
`ww:`, `regex:`, and must report the first hit offset.

## Decision
- **Compression: LZ4 blocks (`lz4_flex`, safe encode/decode, pure Rust)**, one block per
  document with its length prefixed. zstd (C build, ADR needed) compresses text ~2× better,
  but the store is read on every content search: LZ4 decompresses at several GB/s per core,
  zstd at ~1 GB/s, and a trained dictionary would have to be retrained as the corpus changes.
  Synthetic 10k-document corpus: 29.5 MiB text → 21.4 MiB LZ4 (natural prose compresses
  better than the random-syllable benchmark text). `ruzstd` cannot compress.
- **Inverted index over tokens of the default fold**: runs of letters/digits (≤ 48 chars),
  overlapping bigrams for Han/kana/Hangul (no word separators). Postings are row lists;
  the token dictionary is one `\n`-joined buffer so "tokens containing X" is a single
  SIMD `memmem` pass. A pattern's letter/digit tokens must be *substrings* of a document token
  (keeps substring semantics), CJK bigrams must exist, a lone CJK character must occur in a
  bigram. Candidates are intersected as bit sets and **verified** on the decompressed text
  (exact semantics, first offset, hit count) in parallel. Regexes scan all documents.
- Folding for verification copies ASCII/CJK runs in bulk and maps only the first hit back to
  an original offset (the full byte map is built only for preview highlighting).
- Persistence: rows identified like `props.fnx` (volume root + FRN + name/size/date check),
  dictionary compacted on save, postings rebuilt on load, every block validated on load.

## Alternatives
- tantivy: full-text engine with its own segment files, ~100k lines of dependencies, word
  (not substring) semantics; overkill for "grep the indexed notes".
- Trigram index: exact substring pruning but 3–5× the postings memory.
- No index (scan every document): the benchmark corpus contains both words in almost every
  document, so the verification path is what the 200 ms budget is measured against
  (`regex:` content searches take that path: 11 ms for 10k documents).

## Consequences
Measured (Linux, 8 cores, `bench-report`): `ContentStore::search("quarterly plan")` over
10k synthetic documents (29.5 MiB, 44 % of them matching) p50 9.3 ms; the full
`content:"quarterly plan"` query over a 1M-entry index with 10k indexed documents p50 14 ms,
`content:季度` 14 ms (budget 200 ms). Store heap 60 MiB for 30 MiB of text (postings
dominate); a compressed posting format is a later optimisation.

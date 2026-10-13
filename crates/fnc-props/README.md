# fnc-props (M3)

Property index, content index, file hashes and the data model behind the property, content,
hash and duplicate search functions (SPEC ch.1 §8–9, §11; ch.2 §5.3–5.9).

**Responsibilities**
- Property table (`schema`): every `width:`/`artist:`/`file-version:`… name with aliases, column
  title, type, unit, extractor hint and Windows `System.*` name.
- Extractors (`extract`, feature `extract`): image header + EXIF, audio tags (lofty), MP4/MOV,
  Matroska/WebM, PE version info, Office Open XML metadata, PDF `/Info`; `PropertyProvider`
  fallback trait for the Windows Property System (implemented by the app).
- `PropertyStore`: rows + typed columns, `props.fnx`. `ContentStore`: LZ4 text + token
  inverted index, `content.fnx` (ADR-013). `HashCache`: `hashes.fnx`.
- Content extraction: encoding detection, HTML stripping, OOXML and PDF text.
- `OnDemand`: file access for searches over unindexed entries (slow path), incl. bounded
  `head`/`read_at` and `link_target`.
- Parsers without an index: `link` (`.lnk` MS-SHLLINK, `.url`, reparse targets for
  `target:`), `signature` (magic numbers for `fsi:`), `recycle` (`$I` records for
  `recycled:`) — pure, bounds-checked, fuzz-tested.

**Invariants**
- Extraction never runs on the UI or search thread; every parser is byte/time bounded and
  panic-contained — malformed files yield fewer properties, never a crash.
- Stores are bound to one `IndexStore::instance()`; `remap` follows compaction; persisted rows
  re-attach by volume + FRN + identity check and drop changed files.
- An entry with a property row was extracted for every configured property; no row = unknown.
- Nothing here is touched by searches that do not use property/content/hash functions.

See the crate docs (`src/lib.rs`) for the integration contract.

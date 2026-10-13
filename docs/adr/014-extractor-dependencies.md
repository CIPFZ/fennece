# ADR-014: Property and content extractor dependencies

- Status: accepted (M3)
- Related: SPEC ch.1 §8.2, §9.2, ch.6 §6 (dependency list), AGENT.md iron rule 8

## Background
SPEC ch.6 §6 pre-approves `image`, `kamadak-exif`, `lofty`, `mp4parse`, `pdf-extract`, `zip`,
`quick-xml`, `encoding_rs`, `chardetng`, `sha2`, `md-5`, `sha1`, `crc32fast`. Extraction runs on
untrusted files, so every parser is bounded (bytes and wall clock, `extract::Bounded`) and
runs under `catch_unwind`. No crate with a C build may be added without an ADR.

## Decision
| Need | Choice | Notes |
|---|---|---|
| Image size / bit depth | `image` 0.25, formats bmp gif ico jpeg png tga tiff webp, no default features | `ImageReader::into_decoder()` reads headers only; no pixel decoding |
| EXIF | `kamadak-exif` | JPEG/TIFF/HEIF/PNG/WebP containers; Windows XP* tags decoded by hand |
| Audio | `lofty` (default feature = flate2 for compressed ID3v2 frames; the crate does not build without it) | tags + duration/bit rate/sample rate/channels; VBR by Xing/VBRI header |
| MP4/MOV | `mp4parse` on the `moov` box only | our own top-level scanner seeks over `mdat` (`mp4parse` would read it); `mvhd`/`stsd` read directly for date, duration and FourCC |
| Matroska/WebM | own EBML reader (~200 lines) | `matroska` crate pulls more than it saves; only Info and Tracks are read |
| PE | own parser | COFF/optional header, security directory, `VS_VERSIONINFO` |
| OOXML | `zip` 9 with only `deflate-flate2-zlib-rs` (pure Rust) + `quick-xml` 0.41 | default `zip` features include bzip2/zstd/lzma C builds and AES |
| PDF | **own minimal reader** instead of `pdf-extract` | see below |
| Text decoding | `encoding_rs` + `chardetng` | BOM → UTF-16 heuristic → UTF-8 → detector |
| Hashes | `crc32fast`, `md-5`, `sha1`, `sha2`, own CRC-64/XZ table | one pass for all requested algorithms |
| Compression | `lz4_flex` (ADR-013) | pure Rust |

PDF: `pdf-extract` depends on `lopdf` plus font/encoding/crypto crates (≈ 30 crates) and
panics on many malformed files; we need `/Info`, the page count and plain text only. The
own reader (`extract::pdf`, ~700 lines) indexes `N G obj` and object streams, inflates
FlateDecode with `miniz_oxide` (already in the tree), decodes text strings (PDFDocEncoding,
UTF-16BE) and interprets `Tj`/`TJ`/`'`/`"`/`Td`/`T*` with `/ToUnicode` CMaps (bfchar,
bfrange, codespace widths) and Type0 two-byte fonts. Not supported: encryption, LZW/ASCII85/
DCT filters on content streams, fonts without ToUnicode that use custom encodings (their text
is emitted as Latin-1), OCR.

## Alternatives
- Windows IFilter / Property System for everything: Windows-only, COM in a separate process,
  slower; kept as the injected fallback (`extract::PropertyProvider`).
- `lopdf` alone: still ~15 dependencies and no text extraction.

## Consequences
No new C build. Fuzz-style tests (every truncation / bit flip of the generated fixtures) run
for PE, MP4, Matroska and PDF; corrupt images, archives and audio files return fewer
properties. `fnc-query` depends on `fnc-props` with `default-features = false` (the property
table and value model only, no parsers).

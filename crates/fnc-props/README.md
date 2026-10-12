# fnc-props (M3)

Property index (image/audio/video/document/PE metadata, Windows Property System fallback), content
index (text/OOXML/PDF extraction, compressed store), hashes and duplicate detection (SPEC ch.1 §8–11).

**Invariants (planned)**: extraction runs on low-priority threads, never on the UI or search path;
everything is off by default and costs nothing when disabled.

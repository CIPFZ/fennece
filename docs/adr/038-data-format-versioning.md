# ADR-038: Versions of persisted data and the upgrade policy

- Status: accepted (0.2.0, REL-03; amends ADR-003/012/013/015, SPEC ch.1 §5)

## Background
0.2.0 must start on a 0.1.0 data folder without losing anything. Every binary store already
had `magic | version u32` (`index.fnx` "FNX1", `props.fnx` "FNXP", `content.fnx` "FNXC",
`hashes.fnx` "FNXH", semantic `store.bin` "FSEM"; 0.2.0 adds `files.fnx` "FNXF"), but any other version was treated like
damage, and a file that failed to load was renamed `index.fnx.bad` (index) or simply left to
be overwritten by the next save (property/content/hash/vector stores, `filters.toml`,
`bookmarks.toml`, `search_history.toml`, `session.toml`, a bad `settings.toml`). A newer
Fennec's file would have been destroyed by a downgrade. The TOML files had no version.
Audit of 0.1.0 (`f47d7dd`) → 0.2.0: no layout changed. `index.fnx` gained header flag
`FLAG_PINYIN_RANKS` (additive: a 0.1.0 file has it clear = code-point ranks, rebuilt under
pinyin collation); folder indexes now use path-derived file ids, but folder volumes are
re-walked on every start and property/content rows are matched by `(root, id, check hash)`,
so 0.1.0 rows of folder-indexed files are dropped and re-extracted once (never mis-attached).
No version bump was needed; every format stays at 1, `settings.toml` schema 1.

## Decision
- **One rule for every file** (`fnc_core::upgrade`): a version in `oldest..=current` is
  decoded by its own decoder (`check_version`; per store `*_OLDEST_READABLE`, today = 1).
  Older-unreadable, future or damaged → never used, never deleted: binary stores are
  *moved* to `<name>.v<N>.bak` / `<name>.corrupt.bak` (`.2.bak`, `.3.bak`… if taken) and
  rebuilt (index re-enumerated, properties/content re-extracted, vectors re-embedded);
  TOML files are *copied* and loaded as far as possible (unknown keys and fields are kept
  in the copy). Missing file = start empty, no notice.
- **Writers protect newer files**: `persist::save`, `fnx::write_atomic`, `Settings::save`
  and the TOML data writers first move/copy a file whose version is newer than the one
  being written — `-reindex`, a rebuild while running or a store that was never loaded can
  therefore never overwrite a future file either. The rolling `.bak` stays for same-version
  saves.
- **User notice**: localised status-bar text (`upgrade-*` keys; several start-up notices
  are joined), plus a `warn` log line with full paths. Index: indexer `IndexEvent::Notice`;
  stores: `Stores::take_notices`; settings: `main` → `IndexEvent::Notice`; filters,
  bookmarks, history, session: at window creation. The semantic store logs only
  (`VectorStore::open_checked` returns the `Recovery` for a future UI).
- **`settings.toml` `schema_version`** is written first by `to_toml`; missing = 1. It is a
  property of the file, *not* a `#[setting]` field: no descriptor, not on the Advanced
  page, not settable by `/key=value`/IPC, stripped before deserialising. Upgrades are a
  table of `Migration { from, apply: fn(&mut toml::Table) }` (`MIGRATIONS`, empty for
  schema 1) run by `migrate(table, from, to)` before the values are read. A newer schema,
  a schema without a migration path, values of the wrong type (only those keys reset) or a
  syntax error keep a copy and load what they can (`Settings::load_checked`).
- TOML data files (filters, bookmarks, history, session) write `schema_version = 1`
  (`fnc-app::upgrade`); 0.1.0 ignores the extra key, so a downgrade still reads them.

## Not versioned (on purpose)
`journal.tsv` (retention-limited log; extra columns are ignored), `index.excl` (rule
fingerprint; a mismatch only triggers a rebuild), the thumbnail cache, logs, `models.toml`
(user-written, read only).

## Consequences
Unit tests cover current round trip / old / corrupt / future headers for every store and
TOML file; `tests/fixtures/0.1.0/` holds files written by the 0.1.0 sources that must keep
loading. Bumping a format = raise `VERSION`, add a decoder (or raise `*_OLDEST_READABLE`
to force a rebuild), add a fixture of the old layout. Known limit: run counts live in
`index.fnx`, so a rebuilt index starts without them (they stay in the backup), and the
property/content stores re-extract after a full rebuild (rows re-attach by size/date, which
NTFS fills in after the index is published).

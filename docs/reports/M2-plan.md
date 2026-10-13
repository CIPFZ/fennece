# M2 — understanding and plan

**Scope (SPEC ch.6 §3 M2).** Persistence and restart (`.fnx`, USN replay, per-volume rebuild), the
service for non-admin indexing, the complete core query language, and the full desktop UI
(columns, multi-level sort, context menu, drag & drop, clipboard, file operations, options dialog,
dark mode, key bindings, filters, bookmarks, history).

**Units, in order.**
1. `fnc-index::persist` (ADR-003 format), `replace_volume` for per-volume rebuild; indexer: load →
   publish → validate journals → replay or rebuild; periodic + exit save (IDX-04/05).
2. Journal access abstraction in `fnc-ntfs` (direct handle or service pipe); `fennec-service`
   (Windows service, DACL-protected pipe, QueryJournal / EnumMft / ReadUsn only); install/uninstall
   switches (IDX-10).
3. Volume arrival/removal (device notifications) and offline marking.
4. Query: remaining core functions (sibling, descendant, duration, `$var:` …), filters/macros from
   settings, ≥ 300 parser cases.
5. UI: all basic columns + column chooser, multi-level sort (Ctrl+click), menu bar, options window
   with Advanced key/value page (UI-08), theme switch (UI-07), Shell context menu (UI-04), drag out
   (UI-05), CF_HDROP copy, IFileOperation delete/rename, keymap, filters bar, bookmarks, search/run
   history, search builder, autocomplete.

**Acceptance targeted.** IDX-04/05/10, UI-03/04/05/07/08, parser ≥ 300 cases — verified on the
Windows test machine.

**Risks.** Shell COM integration from egui (context menu needs an HWND and a modal menu loop);
service testing needs a second, non-admin account (created on the test VM); long write-lock holds
during per-volume replacement (measured, documented).

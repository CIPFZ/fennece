# fnc-app — `fennec`

The desktop application (SPEC ch.3).

**Responsibilities**
- eframe/egui window: search box (search-as-you-type, `search_delay`), virtualised result table
  (Name/Path/Size/Date Modified, click-to-sort), highlighting, status bar with index progress and diagnostics.
- Keyboard: ↑/↓/PgUp/PgDn/Ctrl+Home/End, Enter (open), Ctrl+Enter (open path), Ctrl+Shift+C (copy path),
  Ctrl+Alt+C (copy name), Ctrl+F/F3/Alt+D (focus search), Esc (clear / hide), F5 (refresh); middle click opens the path.
- Background indexer: private build → publish → NTFS size/date back-fill → USN monitoring (debounced refresh).
- Tray icon and global hotkey (Windows), single instance with command forwarding, Fluent localisation (en, zh-CN),
  CJK font fallback, embedded icons.

**Invariants**
- The UI thread never blocks: searches run on the engine thread; index snapshots are taken with a 2 ms timeout.
- Only visible rows are formatted (row cache invalidated on index generation change).
- Every UI string goes through `i18n::t` (no hard-coded UI text).

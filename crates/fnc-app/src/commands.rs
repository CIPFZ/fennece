//! Commands and key bindings (SPEC ch.3 §13): every menu item, shortcut and tray
//! action is a [`Command`]; [`Keymap`] maps key chords to commands per context
//! (search box, result list, global). Defaults follow Everything; the
//! `key_bindings` setting overrides them (`results:delete=Shift+Delete`).

use egui::{Key, Modifiers};

use crate::i18n::t;

macro_rules! commands {
    ($($(#[$m:meta])* $v:ident = $id:literal),+ $(,)?) => {
        /// A user-invokable action.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum Command { $($(#[$m])* $v),+ }

        impl Command {
            /// Every command.
            pub const ALL: &'static [Command] = &[$(Command::$v),+];

            /// Stable id (`copy-full-path`), used in settings and `/commands`.
            pub fn id(self) -> &'static str {
                match self { $(Command::$v => $id),+ }
            }
        }
    };
}

commands! {
    /// Focus the search box and select its text.
    FocusSearch = "focus-search",
    /// Re-run the search.
    Refresh = "refresh",
    /// Open the selected items.
    Open = "open",
    /// Open the folder of the selection and select it.
    OpenPath = "open-path",
    /// "Open with" dialog.
    OpenWith = "open-with",
    /// Open with administrator rights.
    RunAsAdmin = "run-as-admin",
    /// Properties dialog.
    Properties = "properties",
    /// Explorer context menu for the selection.
    ContextMenu = "context-menu",
    /// Copy the selected files (Shell clipboard).
    CopyFiles = "copy",
    /// Cut the selected files.
    CutFiles = "cut",
    /// Paste files into the selected folder.
    Paste = "paste",
    /// Copy full paths.
    CopyFullPath = "copy-full-path",
    /// Copy full paths in quotes.
    CopyFullPathQuoted = "copy-full-path-quoted",
    /// Copy names.
    CopyName = "copy-name",
    /// Copy folder paths.
    CopyPath = "copy-path",
    /// Copy the selection as tab-separated values.
    CopyAsTsv = "copy-as-tsv",
    /// Copy the selection as CSV.
    CopyAsCsv = "copy-as-csv",
    /// Copy the selection as an EFU file list.
    CopyAsEfu = "copy-as-efu",
    /// Delete to the recycle bin.
    Delete = "delete",
    /// Delete permanently.
    DeletePermanently = "delete-permanently",
    /// Rename in place.
    Rename = "rename",
    /// Rename many items with a template, find/replace, case (SPEC ch.3 §9).
    AdvancedRename = "advanced-rename",
    /// Undo the last file operation.
    Undo = "undo",
    /// New tab.
    NewTab = "new-tab",
    /// Close the current tab.
    CloseTab = "close-tab",
    /// Next tab.
    NextTab = "next-tab",
    /// Previous tab.
    PreviousTab = "previous-tab",
    /// Redo the last undone file operation.
    Redo = "redo",
    /// New folder in the selected folder (or the selection's folder).
    NewFolder = "new-folder",
    /// Search the selected folder (`Explore in Fennec`).
    ExploreInFennec = "explore-in-fennec",
    /// Hide the selected results until the search changes.
    Omit = "omit",
    /// Set the run count of the selection.
    SetRunCount = "set-run-count",
    /// Select all results.
    SelectAll = "select-all",
    /// Invert the selection.
    InvertSelection = "invert-selection",
    /// Toggle the selection of the focused row.
    ToggleSelection = "toggle-selection",
    /// Move the selection down.
    MoveDown = "move-down",
    /// Move the selection up.
    MoveUp = "move-up",
    /// Extend the selection down.
    ExtendDown = "extend-down",
    /// Extend the selection up.
    ExtendUp = "extend-up",
    /// Next item (thumbnail views: right).
    FocusNext = "next-item",
    /// Previous item (thumbnail views: left).
    FocusPrev = "previous-item",
    /// Move the focus down without selecting.
    FocusDown = "focus-down",
    /// Move the focus up without selecting.
    FocusUp = "focus-up",
    /// Page down.
    PageDown = "page-down",
    /// Page up.
    PageUp = "page-up",
    /// First result.
    First = "first",
    /// Last result.
    Last = "last",
    /// Toggle "match case".
    ToggleMatchCase = "match-case",
    /// Toggle "match whole word".
    ToggleWholeWord = "match-whole-word",
    /// Toggle "match path".
    ToggleMatchPath = "match-path",
    /// Toggle "match diacritics".
    ToggleDiacritics = "match-diacritics",
    /// Toggle "enable regex".
    ToggleRegex = "enable-regex",
    /// Advanced search (search builder).
    AdvancedSearch = "advanced-search",
    /// Add the current search as a bookmark.
    AddBookmark = "add-bookmark",
    /// Bookmark manager.
    OrganizeBookmarks = "organize-bookmarks",
    /// Filter manager.
    OrganizeFilters = "organize-filters",
    /// Add the current search as a filter.
    AddFilter = "add-filter",
    /// Everything (no filter).
    FilterNone = "filter-everything",
    /// Filter 1..9.
    Filter1 = "filter-1",
    /// Filter 2.
    Filter2 = "filter-2",
    /// Filter 3.
    Filter3 = "filter-3",
    /// Filter 4.
    Filter4 = "filter-4",
    /// Filter 5.
    Filter5 = "filter-5",
    /// Filter 6.
    Filter6 = "filter-6",
    /// Filter 7.
    Filter7 = "filter-7",
    /// Filter 8.
    Filter8 = "filter-8",
    /// Filter 9.
    Filter9 = "filter-9",
    /// Options window.
    Options = "options",
    /// Export the results to a file.
    Export = "export",
    /// Import settings from a `settings.toml`.
    ImportSettings = "import-settings",
    /// Export settings to a `settings.toml`.
    ExportSettings = "export-settings",
    /// Import the mappable part of an `Everything.ini`.
    ImportEverythingIni = "import-everything-ini",
    /// Export filters as Everything's Filters.csv.
    ExportFilters = "export-filters",
    /// Export bookmarks as Everything's Bookmarks.csv.
    ExportBookmarks = "export-bookmarks",
    /// Export the search history as CSV.
    ExportHistory = "export-search-history",
    /// Show/hide the menu bar.
    ToggleMenuBar = "toggle-menu-bar",
    /// Show/hide the status bar.
    ToggleStatusBar = "toggle-status-bar",
    /// Show/hide the filter bar.
    ToggleFilterBar = "toggle-filter-bar",
    /// Show/hide the preview panel.
    TogglePreview = "toggle-preview",
    /// Details view.
    ViewDetails = "view-details",
    /// Medium thumbnails.
    ViewMedium = "view-medium-thumbnails",
    /// Large thumbnails.
    ViewLarge = "view-large-thumbnails",
    /// Extra large thumbnails.
    ViewExtraLarge = "view-extra-large-thumbnails",
    /// Light theme.
    ThemeLight = "theme-light",
    /// Dark theme.
    ThemeDark = "theme-dark",
    /// Follow the OS theme.
    ThemeSystem = "theme-system",
    /// Zoom in.
    ZoomIn = "zoom-in",
    /// Zoom out.
    ZoomOut = "zoom-out",
    /// Reset zoom.
    ZoomReset = "zoom-reset",
    /// Show the search history.
    SearchHistory = "search-history",
    /// Forget the search history.
    ClearHistory = "clear-history",
    /// Install fennec-service.
    InstallService = "install-service",
    /// Uninstall fennec-service.
    UninstallService = "uninstall-service",
    /// Save the database now.
    SaveDatabase = "save-database",
    /// Pause/resume applying index updates.
    PauseUpdates = "pause-updates",
    /// Show the index journal (recent changes).
    IndexJournal = "index-journal",
    /// Semantic search setup (scope, model download).
    SemanticSetup = "semantic-setup",
    /// Search syntax help.
    Help = "help",
    /// About.
    About = "about",
    /// Hide the window (tray) or minimise.
    HideWindow = "hide-window",
    /// Open a new search window.
    NewWindow = "new-window",
    /// Close this window (main window: hide).
    CloseWindow = "close-window",
    /// Exit Fennec.
    Exit = "exit",
}

impl Command {
    /// Parses an id.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.id() == id.trim())
    }

    /// Localised label (menus, Options → Keyboard).
    pub fn label(self) -> String {
        t(&format!("cmd-{}", self.id()))
    }

    /// Filter slot `1..=9` of `Filter1..Filter9`.
    pub fn filter_slot(self) -> Option<usize> {
        let slots = [
            Command::Filter1,
            Command::Filter2,
            Command::Filter3,
            Command::Filter4,
            Command::Filter5,
            Command::Filter6,
            Command::Filter7,
            Command::Filter8,
            Command::Filter9,
        ];
        slots.iter().position(|&c| c == self).map(|i| i + 1)
    }
}

/// Where a binding applies.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyCtx {
    /// The search box has focus.
    Search,
    /// The result list has focus.
    Results,
    /// Anywhere in the main window.
    Global,
}

impl KeyCtx {
    fn id(self) -> &'static str {
        match self {
            KeyCtx::Search => "search",
            KeyCtx::Results => "results",
            KeyCtx::Global => "global",
        }
    }

    fn from_id(s: &str) -> Option<Self> {
        [KeyCtx::Search, KeyCtx::Results, KeyCtx::Global].into_iter().find(|c| c.id() == s.trim())
    }

    /// Localised name.
    pub fn label(self) -> String {
        t(&format!("keyctx-{}", self.id()))
    }
}

/// A key with exact Ctrl/Shift/Alt state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Ctrl (Cmd on macOS).
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt.
    pub alt: bool,
    /// Key.
    pub key: Key,
}

impl Chord {
    const fn new(ctrl: bool, shift: bool, alt: bool, key: Key) -> Self {
        Self { ctrl, shift, alt, key }
    }

    /// From an egui key event.
    pub fn from_event(key: Key, m: Modifiers) -> Self {
        Self { ctrl: m.command || m.ctrl, shift: m.shift, alt: m.alt, key }
    }

    /// Parses `Ctrl+Shift+C`, `Alt+Enter`, `F5`, `Delete`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut c = Self::new(false, false, false, Key::A);
        let mut key = None;
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" => c.alt = true,
                _ => key = Some(Key::from_name(part).or_else(|| Key::from_name(&capitalise(part)))?),
            }
        }
        c.key = key?;
        Some(c)
    }

    /// Display / setting form.
    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        s.push_str(self.key.name());
        s
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
}

/// Bindings for all contexts.
#[derive(Clone, Debug)]
pub struct Keymap {
    bindings: Vec<(KeyCtx, Chord, Command)>,
}

const C: bool = true;
const N: bool = false;

/// Everything-compatible defaults.
fn defaults() -> Vec<(KeyCtx, Chord, Command)> {
    use Command as K;
    use KeyCtx::{Global as G, Results as R, Search as S};
    let mut v = vec![
        (G, Chord::new(C, N, N, Key::F), K::FocusSearch),
        (G, Chord::new(N, N, N, Key::F3), K::FocusSearch),
        (G, Chord::new(N, N, C, Key::D), K::FocusSearch),
        (G, Chord::new(N, N, N, Key::F5), K::Refresh),
        (G, Chord::new(C, N, N, Key::I), K::ToggleMatchCase),
        (G, Chord::new(C, N, N, Key::B), K::ToggleWholeWord),
        (G, Chord::new(C, N, N, Key::U), K::ToggleMatchPath),
        (G, Chord::new(C, N, N, Key::M), K::ToggleDiacritics),
        (G, Chord::new(C, N, N, Key::R), K::ToggleRegex),
        (G, Chord::new(C, N, N, Key::D), K::AddBookmark),
        (G, Chord::new(C, C, N, Key::B), K::OrganizeBookmarks),
        (G, Chord::new(C, C, N, Key::F), K::OrganizeFilters),
        (G, Chord::new(C, N, N, Key::S), K::Export),
        (G, Chord::new(C, N, N, Key::Num0), K::FilterNone),
        (G, Chord::new(C, N, N, Key::Equals), K::ZoomIn),
        (G, Chord::new(C, N, N, Key::Plus), K::ZoomIn),
        (G, Chord::new(C, N, N, Key::Minus), K::ZoomOut),
        (G, Chord::new(C, N, C, Key::Num0), K::ZoomReset),
        (G, Chord::new(C, N, N, Key::Q), K::Exit),
        (G, Chord::new(C, N, N, Key::N), K::NewWindow),
        (G, Chord::new(N, N, N, Key::F1), K::Help),
        (G, Chord::new(C, N, N, Key::P), K::TogglePreview),
        (G, Chord::new(C, C, N, Key::Num1), K::ViewDetails),
        (G, Chord::new(C, C, N, Key::Num2), K::ViewMedium),
        (G, Chord::new(C, C, N, Key::Num3), K::ViewLarge),
        (G, Chord::new(C, C, N, Key::Num4), K::ViewExtraLarge),
    ];
    let nums = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    let filters =
        [K::Filter1, K::Filter2, K::Filter3, K::Filter4, K::Filter5, K::Filter6, K::Filter7, K::Filter8, K::Filter9];
    for (k, f) in nums.iter().zip(filters) {
        v.push((G, Chord::new(C, N, N, *k), f));
    }
    // Navigation and opening work from both the search box and the list.
    for ctx in [S, R] {
        v.extend([
            (ctx, Chord::new(N, N, N, Key::ArrowDown), K::MoveDown),
            (ctx, Chord::new(N, N, N, Key::ArrowUp), K::MoveUp),
            (ctx, Chord::new(N, N, N, Key::PageDown), K::PageDown),
            (ctx, Chord::new(N, N, N, Key::PageUp), K::PageUp),
            (ctx, Chord::new(N, N, N, Key::Enter), K::Open),
            (ctx, Chord::new(C, N, N, Key::Enter), K::OpenPath),
            (ctx, Chord::new(N, C, N, Key::Enter), K::OpenWith),
            (ctx, Chord::new(C, C, N, Key::Enter), K::RunAsAdmin),
            (ctx, Chord::new(N, N, C, Key::Enter), K::Properties),
            (ctx, Chord::new(C, C, N, Key::C), K::CopyFullPath),
            (ctx, Chord::new(C, N, C, Key::C), K::CopyName),
        ]);
    }
    v.extend([
        (S, Chord::new(C, N, N, Key::Home), K::First),
        (S, Chord::new(C, N, N, Key::End), K::Last),
        (R, Chord::new(N, N, N, Key::ArrowRight), K::FocusNext),
        (R, Chord::new(N, N, N, Key::ArrowLeft), K::FocusPrev),
        (R, Chord::new(N, N, N, Key::Home), K::First),
        (R, Chord::new(N, N, N, Key::End), K::Last),
        (R, Chord::new(N, C, N, Key::ArrowDown), K::ExtendDown),
        (R, Chord::new(N, C, N, Key::ArrowUp), K::ExtendUp),
        (R, Chord::new(C, N, N, Key::ArrowDown), K::FocusDown),
        (R, Chord::new(C, N, N, Key::ArrowUp), K::FocusUp),
        (R, Chord::new(N, N, N, Key::Space), K::ToggleSelection),
        (R, Chord::new(C, N, N, Key::A), K::SelectAll),
        // In the list Ctrl+I inverts the selection (match case stays global elsewhere).
        (R, Chord::new(C, N, N, Key::I), K::InvertSelection),
        (R, Chord::new(C, N, N, Key::C), K::CopyFiles),
        (R, Chord::new(C, N, N, Key::X), K::CutFiles),
        (R, Chord::new(C, N, N, Key::V), K::Paste),
        (R, Chord::new(N, N, N, Key::Delete), K::Delete),
        (R, Chord::new(N, C, N, Key::Delete), K::DeletePermanently),
        (R, Chord::new(N, N, N, Key::F2), K::Rename),
        (R, Chord::new(C, N, N, Key::Z), K::Undo),
        (G, Chord::new(C, N, N, Key::T), K::NewTab),
        (G, Chord::new(C, N, N, Key::W), K::CloseTab),
        (G, Chord::new(C, N, N, Key::Tab), K::NextTab),
        (G, Chord::new(C, C, N, Key::Tab), K::PreviousTab),
        (R, Chord::new(C, N, N, Key::Y), K::Redo),
        (R, Chord::new(C, C, N, Key::Z), K::Redo),
        (R, Chord::new(N, C, N, Key::F10), K::ContextMenu),
        (R, Chord::new(C, C, N, Key::N), K::NewFolder),
    ]);
    v
}

impl Default for Keymap {
    fn default() -> Self {
        Self { bindings: defaults() }
    }
}

impl Keymap {
    /// Defaults with `key_bindings` overrides applied (`ctx:command=Chord`; an empty chord
    /// removes the command's default bindings in that context). Invalid lines are
    /// reported in the returned list.
    pub fn with_overrides(overrides: &[String]) -> (Self, Vec<String>) {
        let mut km = Self::default();
        let mut bad = Vec::new();
        for line in overrides {
            let parsed = (|| {
                let (lhs, chord) = line.split_once('=')?;
                let (ctx, cmd) = lhs.split_once(':')?;
                let ctx = KeyCtx::from_id(ctx)?;
                let cmd = Command::from_id(cmd)?;
                let chord = chord.trim();
                let chord = if chord.is_empty() { None } else { Some(Chord::parse(chord)?) };
                Some((ctx, cmd, chord))
            })();
            match parsed {
                Some((ctx, cmd, chord)) => km.bind(ctx, cmd, chord),
                None => bad.push(line.clone()),
            }
        }
        (km, bad)
    }

    /// Replaces `cmd`'s bindings in `ctx` with `chord` (`None` = unbound). A chord taken
    /// by another command in the same context moves to `cmd`.
    pub fn bind(&mut self, ctx: KeyCtx, cmd: Command, chord: Option<Chord>) {
        self.bindings.retain(|(c, ch, k)| !(*c == ctx && (*k == cmd || Some(*ch) == chord)));
        if let Some(ch) = chord {
            self.bindings.push((ctx, ch, cmd));
        }
    }

    /// The command for `chord` in `ctx` (context bindings win over global ones).
    pub fn lookup(&self, ctx: KeyCtx, chord: &Chord) -> Option<Command> {
        let find = |c: KeyCtx| self.bindings.iter().find(|(bc, ch, _)| *bc == c && ch == chord).map(|b| b.2);
        find(ctx).or_else(|| find(KeyCtx::Global))
    }

    /// Chords bound to `cmd` in `ctx`.
    pub fn chords(&self, ctx: KeyCtx, cmd: Command) -> Vec<Chord> {
        self.bindings.iter().filter(|(c, _, k)| *c == ctx && *k == cmd).map(|b| b.1).collect()
    }

    /// Shortcut text shown in menus (first binding: results, search, then global).
    pub fn shortcut_text(&self, cmd: Command) -> String {
        [KeyCtx::Results, KeyCtx::Search, KeyCtx::Global]
            .iter()
            .find_map(|&c| self.chords(c, cmd).first().map(Chord::display))
            .unwrap_or_default()
    }

    /// Overrides that turn the defaults into this keymap (for the setting).
    pub fn to_overrides(&self) -> Vec<String> {
        let d = Keymap::default();
        let mut out = Vec::new();
        for ctx in [KeyCtx::Search, KeyCtx::Results, KeyCtx::Global] {
            for &cmd in Command::ALL {
                let mine = self.chords(ctx, cmd);
                if mine != d.chords(ctx, cmd) {
                    if mine.is_empty() {
                        out.push(format!("{}:{}=", ctx.id(), cmd.id()));
                    }
                    for ch in mine {
                        out.push(format!("{}:{}={}", ctx.id(), cmd.id(), ch.display()));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse_and_display() {
        let c = Chord::parse("Ctrl+Shift+C").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && c.key == Key::C);
        assert_eq!(c.display(), "Ctrl+Shift+C");
        assert_eq!(Chord::parse("alt+enter").unwrap().display(), "Alt+Enter");
        assert_eq!(Chord::parse("F5").unwrap().key, Key::F5);
        assert_eq!(Chord::parse("delete").unwrap().key, Key::Delete);
        assert!(Chord::parse("Ctrl+").is_none());
        assert!(Chord::parse("Ctrl+Nope").is_none());
    }

    #[test]
    fn everything_defaults() {
        let km = Keymap::default();
        let ch = |s| Chord::parse(s).unwrap();
        assert_eq!(km.lookup(KeyCtx::Results, &ch("Delete")), Some(Command::Delete));
        assert_eq!(km.lookup(KeyCtx::Search, &ch("Delete")), None, "Del edits text in the search box");
        assert_eq!(km.lookup(KeyCtx::Search, &ch("Ctrl+Enter")), Some(Command::OpenPath));
        assert_eq!(km.lookup(KeyCtx::Results, &ch("Ctrl+I")), Some(Command::InvertSelection));
        assert_eq!(km.lookup(KeyCtx::Search, &ch("Ctrl+I")), Some(Command::ToggleMatchCase));
        assert_eq!(km.lookup(KeyCtx::Results, &ch("Ctrl+3")), Some(Command::Filter3));
        assert_eq!(km.lookup(KeyCtx::Search, &ch("F5")), Some(Command::Refresh));
        // Every command id is unique and labelled.
        let mut ids: Vec<_> = Command::ALL.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Command::ALL.len());
    }

    #[test]
    fn overrides_roundtrip() {
        let lines = vec![
            "results:delete=Ctrl+D".to_string(),
            "global:refresh=".to_string(),
            "nonsense".to_string(),
            "results:bogus=F1".to_string(),
        ];
        let (km, bad) = Keymap::with_overrides(&lines);
        assert_eq!(bad.len(), 2);
        let ch = |s| Chord::parse(s).unwrap();
        assert_eq!(km.lookup(KeyCtx::Results, &ch("Ctrl+D")), Some(Command::Delete));
        assert_eq!(km.lookup(KeyCtx::Results, &ch("Delete")), None);
        assert_eq!(km.lookup(KeyCtx::Search, &ch("F5")), None);
        let (again, bad) = Keymap::with_overrides(&km.to_overrides());
        assert!(bad.is_empty());
        assert_eq!(again.to_overrides(), km.to_overrides());
        assert_eq!(km.shortcut_text(Command::Delete), "Ctrl+D");
    }

    #[test]
    fn every_label_is_translated() {
        crate::i18n::init("en");
        for &c in Command::ALL {
            assert_ne!(c.label(), format!("cmd-{}", c.id()), "missing cmd-{}", c.id());
        }
        for c in crate::columns::ColumnId::ALL {
            assert_ne!(c.title(), format!("col-{}", c.key()), "missing col-{}", c.key());
        }
        for ctx in [KeyCtx::Search, KeyCtx::Results, KeyCtx::Global] {
            assert!(!ctx.label().starts_with("keyctx-"));
        }
    }
}

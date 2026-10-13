//! Settings: the single `Settings` struct, its metadata and `settings.toml` persistence.
//!
//! Every configurable value lives in [`Settings`] as a flat field annotated with
//! `#[setting(key, default, page)]` (SPEC ch.6 §5). The derive generates `Default`
//! and a [`SettingsMeta`] implementation so the Options → Advanced page can list,
//! filter, edit and reset every key generically (UI-08).
//!
//! Defaults follow Everything 1.5 (e.g. `search_delay = 0`, OR binds tighter than AND).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use fnc_macros::Settings;

use crate::error::{CoreError, Result};

/// Value kind of a setting (drives the editor widget).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SettingKind {
    /// `true` / `false`.
    Bool,
    /// Integer.
    Int,
    /// Floating point.
    Float,
    /// Free text.
    Text,
    /// `;`-separated list of text values.
    List,
    /// One of a fixed set of values.
    Choice(&'static [&'static str]),
}

/// Static description of one setting.
#[derive(Debug, Clone, Copy)]
pub struct SettingDescriptor {
    /// Stable key (`search_delay`), used in `settings.toml`, `/key=value` commands and IPC.
    pub key: &'static str,
    /// Options page id (`general`, `search`, `results`, `view`, `index`, ...).
    pub page: &'static str,
    /// Rust field name.
    pub field: &'static str,
    /// Description (from the field's doc comment).
    pub doc: &'static str,
    /// Source text of the default expression.
    pub default_src: &'static str,
    /// Value kind.
    pub kind: SettingKind,
}

/// Errors from key-based access.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingError {
    /// No setting with this key.
    #[error("unknown setting `{0}`")]
    UnknownKey(String),
    /// The value could not be parsed for this setting.
    #[error("invalid value for `{key}`: {message}")]
    Invalid {
        /// Setting key.
        key: &'static str,
        /// Parse message.
        message: String,
    },
}

/// Implemented by `#[derive(Settings)]`.
pub trait SettingsMeta {
    /// All settings, in declaration order.
    fn descriptors() -> &'static [SettingDescriptor];
    /// Current value of `key` as text.
    fn get(&self, key: &str) -> Option<String>;
    /// Parses and assigns `value` to `key`.
    fn set(&mut self, key: &str, value: &str) -> std::result::Result<(), SettingError>;
    /// Restores the default of `key`; `false` if unknown.
    fn reset(&mut self, key: &str) -> bool;

    /// Descriptors whose key or description contains `filter` (case-insensitive),
    /// as used by the "show settings containing" box.
    fn search(filter: &str) -> Vec<&'static SettingDescriptor> {
        let f = filter.to_lowercase();
        Self::descriptors()
            .iter()
            .filter(|d| f.is_empty() || d.key.contains(&f) || d.doc.to_lowercase().contains(&f))
            .collect()
    }
}

/// A type usable as a setting field.
pub trait SettingType: Sized {
    /// Editor kind.
    const KIND: SettingKind;
    /// Text form shown in the Advanced page.
    fn to_setting_string(&self) -> String;
    /// Parses the text form.
    fn parse_setting(s: &str) -> std::result::Result<Self, String>;
}

impl SettingType for bool {
    const KIND: SettingKind = SettingKind::Bool;
    fn to_setting_string(&self) -> String {
        self.to_string()
    }
    fn parse_setting(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(format!("expected true/false, got `{other}`")),
        }
    }
}

macro_rules! int_setting {
    ($($t:ty),*) => {$(
        impl SettingType for $t {
            const KIND: SettingKind = SettingKind::Int;
            fn to_setting_string(&self) -> String { self.to_string() }
            fn parse_setting(s: &str) -> std::result::Result<Self, String> {
                s.trim().parse::<$t>().map_err(|e| e.to_string())
            }
        }
    )*};
}
int_setting!(u8, u16, u32, u64, i32, i64, usize);

impl SettingType for f32 {
    const KIND: SettingKind = SettingKind::Float;
    fn to_setting_string(&self) -> String {
        self.to_string()
    }
    fn parse_setting(s: &str) -> std::result::Result<Self, String> {
        s.trim().parse::<f32>().map_err(|e| e.to_string())
    }
}

impl SettingType for String {
    const KIND: SettingKind = SettingKind::Text;
    fn to_setting_string(&self) -> String {
        self.clone()
    }
    fn parse_setting(s: &str) -> std::result::Result<Self, String> {
        Ok(s.to_string())
    }
}

impl SettingType for Vec<String> {
    const KIND: SettingKind = SettingKind::List;
    fn to_setting_string(&self) -> String {
        self.join(";")
    }
    fn parse_setting(s: &str) -> std::result::Result<Self, String> {
        Ok(s.split(';').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect())
    }
}

/// Defines a lowercase-serialised choice enum usable as a setting.
macro_rules! choice_setting {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vm])* #[serde(rename = $text)] $variant),+
        }
        impl $name {
            /// All textual values.
            pub const CHOICES: &'static [&'static str] = &[$($text),+];
            /// Text form.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
        impl SettingType for $name {
            const KIND: SettingKind = SettingKind::Choice(Self::CHOICES);
            fn to_setting_string(&self) -> String { self.as_str().to_string() }
            fn parse_setting(s: &str) -> std::result::Result<Self, String> {
                match s.trim().to_ascii_lowercase().as_str() {
                    $($text => Ok(Self::$variant),)+
                    other => Err(format!("expected one of {:?}, got `{other}`", Self::CHOICES)),
                }
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
    };
}

choice_setting!(
    /// Colour theme.
    Theme {
        /// Follow the OS.
        System => "system",
        /// Light.
        Light => "light",
        /// Dark.
        Dark => "dark",
        /// High contrast (accessibility, SPEC ch.3 §14).
        HighContrast => "high-contrast",
    }
);

choice_setting!(
    /// Operator precedence between `|` and whitespace (AND).
    Precedence {
        /// `a b | c` = `a (b | c)` (Everything default).
        Or => "or",
        /// `a b | c` = `(a b) | c`.
        And => "and",
    }
);

choice_setting!(
    /// Date column format.
    DateFormat {
        /// `2026-07-02 10:52`.
        Iso => "iso",
        /// `07/02/2026 10:52`.
        Us => "us",
        /// `02/07/2026 10:52`.
        Eu => "eu",
        /// `3 minutes ago` (falls back to ISO after a week).
        Relative => "relative",
    }
);

choice_setting!(
    /// Size column format.
    SizeFormat {
        /// Automatic unit (B, KB, MB, ...).
        Auto => "auto",
        /// Bytes with digit grouping.
        Bytes => "bytes",
        /// Kilobytes.
        Kb => "kb",
        /// Megabytes.
        Mb => "mb",
    }
);

choice_setting!(
    /// Result view (Ctrl+Shift+1..4).
    ViewMode {
        /// Details (columns).
        Details => "details",
        /// Medium thumbnails (96 px).
        Medium => "medium",
        /// Large thumbnails (160 px).
        Large => "large",
        /// Extra large thumbnails (256 px).
        ExtraLarge => "extra-large",
    }
);

choice_setting!(
    /// Whether folders are listed before files when sorting.
    FoldersFirst {
        /// Folders first when sorting ascending only.
        Ascending => "ascending",
        /// Folders always first.
        Always => "always",
        /// Files and folders mixed.
        Never => "never",
    }
);

/// All Fennec settings (flat; `settings.toml` mirrors this struct one key per line).
#[derive(Settings, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    // ---- general ----
    /// UI language (BCP-47 tag, e.g. `en`, `zh-CN`). Empty = follow the OS.
    #[setting(key = "language", default = "", page = "general")]
    pub language: String,
    /// Keep a single running instance and forward command lines to it.
    #[setting(key = "single_instance", default = true, page = "general")]
    pub single_instance: bool,
    /// Show the notification area (tray) icon.
    #[setting(key = "show_tray_icon", default = true, page = "general")]
    pub show_tray_icon: bool,
    /// Closing the main window hides it to the tray instead of exiting.
    #[setting(key = "close_to_tray", default = true, page = "general")]
    pub close_to_tray: bool,
    /// Global hotkey that shows the search window (e.g. `Ctrl+Backquote`). Empty = none.
    #[setting(key = "hotkey_show_window", default = "Ctrl+Backquote", page = "general")]
    pub hotkey_show_window: String,

    // ---- ui ----
    /// Colour theme: system, light or dark.
    #[setting(key = "theme", default = Theme::System, page = "ui")]
    pub theme: Theme,
    /// UI zoom factor.
    #[setting(key = "ui_scale", default = 1.0, page = "ui")]
    pub ui_scale: f32,
    /// Result list row height in points (0 = derived from the font).
    #[setting(key = "row_height", default = 0, page = "ui")]
    pub row_height: u32,
    /// Show the menu bar (Alt shows it temporarily when hidden).
    #[setting(key = "show_menu_bar", default = true, page = "ui")]
    pub show_menu_bar: bool,
    /// Show the status bar.
    #[setting(key = "show_status_bar", default = true, page = "ui")]
    pub show_status_bar: bool,
    /// Show the filter bar (one button per filter).
    #[setting(key = "show_filter_bar", default = false, page = "ui")]
    pub show_filter_bar: bool,
    /// Window geometry `x,y,width,height` in points (empty = default).
    #[setting(key = "window_rect", default = "", page = "ui")]
    pub window_rect: String,
    /// Start maximised.
    #[setting(key = "window_maximized", default = false, page = "ui")]
    pub window_maximized: bool,

    // ---- search ----
    /// Delay in milliseconds between the last keystroke and running the search.
    #[setting(key = "search_delay", default = 0, page = "search")]
    pub search_delay: u32,
    /// Match case by default.
    #[setting(key = "match_case", default = false, page = "search")]
    pub match_case: bool,
    /// Match whole words by default.
    #[setting(key = "match_whole_word", default = false, page = "search")]
    pub match_whole_word: bool,
    /// Match against the full path by default.
    #[setting(key = "match_path", default = false, page = "search")]
    pub match_path: bool,
    /// Match diacritics by default.
    #[setting(key = "match_diacritics", default = false, page = "search")]
    pub match_diacritics: bool,
    /// Treat the whole search as a regular expression.
    #[setting(key = "match_regex", default = false, page = "search")]
    pub match_regex: bool,
    /// Operator precedence: `or` (Everything default) or `and`.
    #[setting(key = "operator_precedence", default = Precedence::Or, page = "search")]
    pub operator_precedence: Precedence,
    /// `*` also matches the path separator.
    #[setting(key = "wildcards_star_all", default = false, page = "search")]
    pub wildcards_star_all: bool,
    /// Reuse the previous result set when the new search only narrows it.
    #[setting(key = "reuse_results", default = true, page = "search")]
    pub reuse_results: bool,
    /// Also match Chinese names by pinyin (full or initials) for plain search terms.
    #[setting(key = "match_pinyin", default = false, page = "search")]
    pub match_pinyin: bool,
    /// Show completions (history, functions, paths) while typing.
    #[setting(key = "auto_complete", default = true, page = "search")]
    pub auto_complete: bool,
    /// Typing while the result list has focus continues in the search box.
    #[setting(key = "type_to_search", default = true, page = "search")]
    pub type_to_search: bool,
    /// Active filter (name; empty = Everything).
    #[setting(key = "filter", default = "", page = "search")]
    pub filter: String,

    // ---- results ----
    /// Highlight matched text in the name and path columns.
    #[setting(key = "highlight", default = true, page = "results")]
    pub highlight: bool,
    /// Folder ordering relative to files: ascending, always or never.
    // SPEC-GAP: Everything 1.5's default for "folders first" is not pinned by SPEC.
    #[setting(key = "folders_first", default = FoldersFirst::Ascending, page = "results")]
    pub folders_first: FoldersFirst,
    /// Show entries of offline volumes (greyed out).
    #[setting(key = "show_offline", default = true, page = "results")]
    pub show_offline: bool,
    /// Single click opens (instead of double click).
    #[setting(key = "single_click_open", default = false, page = "results")]
    pub single_click_open: bool,

    // ---- view ----
    /// Visible result columns with widths: `id:width;...` (`name path size ext type dm dc
    /// da attributes runcount dr rc fullpath filelist indexsource volume depth namelength
    /// frn`).
    #[setting(key = "columns", default = vec!["name:300".to_string(), "path:420".to_string(), "size:90".to_string(), "dm:140".to_string()], page = "view")]
    pub columns: Vec<String>,
    /// Sort keys: `column-ascending|descending;...` (up to three).
    #[setting(key = "sort", default = "name-ascending", page = "view")]
    pub sort: String,
    /// Alternate row background colours.
    #[setting(key = "alternate_row_color", default = true, page = "view")]
    pub alternate_row_color: bool,
    /// Date format of date columns.
    #[setting(key = "date_format", default = DateFormat::Iso, page = "view")]
    pub date_format: DateFormat,
    /// Size format of the size column.
    #[setting(key = "size_format", default = SizeFormat::Auto, page = "view")]
    pub size_format: SizeFormat,
    /// Result view: details or thumbnails.
    #[setting(key = "view_mode", default = ViewMode::Details, page = "view")]
    pub view_mode: ViewMode,
    /// Show file icons in the result list (Windows shell icons).
    #[setting(key = "show_icons", default = true, page = "view")]
    pub show_icons: bool,
    /// Show the preview panel (Ctrl+P).
    #[setting(key = "show_preview", default = false, page = "view")]
    pub show_preview: bool,
    /// Preview panel width in points.
    #[setting(key = "preview_width", default = 360.0, page = "view")]
    pub preview_width: f32,
    /// Thumbnail disk cache limit in MiB (0 = memory only).
    #[setting(key = "thumbnail_cache_mb", default = 256, page = "view")]
    pub thumbnail_cache_mb: u32,

    // ---- keyboard ----
    /// Key binding overrides: `context:command=Ctrl+Shift+X` (context `search`, `results`
    /// or `global`; empty chord removes the default).
    #[setting(key = "key_bindings", default = Vec::new(), page = "keyboard")]
    pub key_bindings: Vec<String>,

    // ---- open commands ----
    /// Custom open commands: `Label|program arguments` with `%1` for the full path.
    #[setting(key = "open_commands", default = Vec::new(), page = "open_commands")]
    pub open_commands: Vec<String>,

    // ---- history ----
    /// Remember search history.
    #[setting(key = "search_history_enabled", default = true, page = "history")]
    pub search_history_enabled: bool,
    /// Maximum number of search history entries.
    #[setting(key = "search_history_max", default = 100, page = "history")]
    pub search_history_max: u32,
    /// Record run counts and dates when opening results.
    #[setting(key = "run_history_enabled", default = true, page = "history")]
    pub run_history_enabled: bool,

    // ---- index ----
    /// Index file sizes.
    #[setting(key = "index_size", default = true, page = "index")]
    pub index_size: bool,
    /// Index date modified.
    #[setting(key = "index_date_modified", default = true, page = "index")]
    pub index_date_modified: bool,
    /// Index date created.
    #[setting(key = "index_date_created", default = true, page = "index")]
    pub index_date_created: bool,
    /// Index date accessed.
    #[setting(key = "index_date_accessed", default = false, page = "index")]
    pub index_date_accessed: bool,
    /// Index attributes.
    #[setting(key = "index_attributes", default = true, page = "index")]
    pub index_attributes: bool,
    /// Pause applying index updates.
    #[setting(key = "pause_updates", default = false, page = "index")]
    pub pause_updates: bool,
    /// Database save interval in minutes (0 = only on exit).
    #[setting(key = "db_save_interval", default = 5, page = "index")]
    pub db_save_interval: u32,

    // ---- ntfs ----
    /// Automatically index all local NTFS/ReFS volumes.
    #[setting(key = "ntfs_auto_include", default = true, page = "ntfs")]
    pub ntfs_auto_include: bool,
    /// Create the USN journal when a volume has none.
    #[setting(key = "usn_create_journal", default = true, page = "ntfs")]
    pub usn_create_journal: bool,
    /// USN journal maximum size in MiB when Fennec creates it.
    #[setting(key = "usn_max_size_mb", default = 32, page = "ntfs")]
    pub usn_max_size_mb: u32,
    /// USN journal allocation delta in MiB when Fennec creates it.
    #[setting(key = "usn_allocation_delta_mb", default = 4, page = "ntfs")]
    pub usn_allocation_delta_mb: u32,
    /// Poll interval (ms) for USN journal reads when no change is pending.
    #[setting(key = "usn_poll_interval", default = 1000, page = "ntfs")]
    pub usn_poll_interval: u32,
    /// Volumes to exclude (drive letters or GUID paths, `;`-separated).
    #[setting(key = "ntfs_exclude_volumes", default = Vec::new(), page = "ntfs")]
    pub ntfs_exclude_volumes: Vec<String>,

    // ---- folders / file lists ----
    /// Folder index roots (`;`-separated). On non-Windows platforms an empty list means
    /// the user's home directory.
    #[setting(key = "folders", default = Vec::new(), page = "folders")]
    pub folders: Vec<String>,
    /// Folder rescan interval in minutes (0 = only after notification overflow).
    #[setting(key = "folder_rescan_interval", default = 60, page = "folders")]
    pub folder_rescan_interval: u32,
    /// Watch folder indexes for changes (ReadDirectoryChangesW / inotify).
    #[setting(key = "folder_watch", default = true, page = "folders")]
    pub folder_watch: bool,
    /// EFU file lists mounted into the index (`;`-separated paths).
    #[setting(key = "file_lists", default = Vec::new(), page = "filelists")]
    pub file_lists: Vec<String>,

    // ---- exclude ----
    /// Exclude hidden files and folders.
    #[setting(key = "exclude_hidden", default = false, page = "exclude")]
    pub exclude_hidden: bool,
    /// Exclude system files and folders.
    #[setting(key = "exclude_system", default = false, page = "exclude")]
    pub exclude_system: bool,
    /// Excluded folders: full paths or folder-name wildcards (`node_modules`, `C:\\Temp\\*`).
    #[setting(key = "exclude_folders", default = Vec::new(), page = "exclude")]
    pub exclude_folders: Vec<String>,
    /// Excluded files: name wildcards (`*.tmp;~$*`) or full-path wildcards.
    #[setting(key = "exclude_files", default = Vec::new(), page = "exclude")]
    pub exclude_files: Vec<String>,
    /// Folders whose `.gitignore` files are honoured as exclusions (`;`-separated).
    #[setting(key = "exclude_gitignore_roots", default = Vec::new(), page = "exclude")]
    pub exclude_gitignore_roots: Vec<String>,

    // ---- index journal ----
    /// Keep the index journal (created/deleted/renamed/modified entries).
    #[setting(key = "index_journal_enabled", default = true, page = "index_journal")]
    pub index_journal_enabled: bool,
    /// Days of index journal history to keep.
    #[setting(key = "index_journal_days", default = 7, page = "index_journal")]
    pub index_journal_days: u32,
    /// Maximum index journal records.
    #[setting(key = "index_journal_max", default = 1_000_000, page = "index_journal")]
    pub index_journal_max: u32,

    // ---- properties / content (SPEC ch.1 §8, §9) ----
    /// Indexed properties (`width;height;length;artist`…; empty = property index off).
    #[setting(key = "property_names", default = Vec::new(), page = "properties")]
    pub property_names: Vec<String>,
    /// Folders whose files get their properties indexed (empty = all folders).
    #[setting(key = "property_folders", default = Vec::new(), page = "properties")]
    pub property_folders: Vec<String>,
    /// Extensions whose files get their properties indexed (empty = all files).
    #[setting(key = "property_extensions", default = Vec::new(), page = "properties")]
    pub property_extensions: Vec<String>,
    /// Fast sort for indexed properties.
    #[setting(key = "property_fast_sort", default = false, page = "properties")]
    pub property_fast_sort: bool,
    /// Read properties of files that are not indexed when a search needs them (slow).
    #[setting(key = "property_read_unindexed", default = true, page = "properties")]
    pub property_read_unindexed: bool,
    /// Most files a search may read from disk for unindexed properties, content and hashes.
    #[setting(key = "slow_search_max_files", default = 10_000, page = "properties")]
    pub slow_search_max_files: u32,
    /// Index file content (`content:`).
    #[setting(key = "content_enabled", default = false, page = "content")]
    pub content_enabled: bool,
    /// Folders whose files get their content indexed (empty = all folders).
    #[setting(key = "content_folders", default = Vec::new(), page = "content")]
    pub content_folders: Vec<String>,
    /// Extensions whose files get their content indexed.
    #[setting(key = "content_extensions", default = ["txt", "md", "ini", "log", "json", "xml", "csv", "html", "htm", "js", "ts", "py", "rs", "java", "c", "cpp", "h", "cs", "go", "toml", "yaml", "yml"].map(String::from).to_vec(), page = "content")]
    pub content_extensions: Vec<String>,
    /// Largest file whose content is indexed or read, in MiB.
    #[setting(key = "content_max_file_mb", default = 4, page = "content")]
    pub content_max_file_mb: u32,
    /// Total size limit of the content index in MiB (compressed; 0 = unlimited).
    #[setting(key = "content_max_total_mb", default = 0, page = "content")]
    pub content_max_total_mb: u32,
    /// Extract the text of Office documents (docx, xlsx, pptx).
    #[setting(key = "content_office", default = true, page = "content")]
    pub content_office: bool,
    /// Extract the text of PDF documents.
    #[setting(key = "content_pdf", default = true, page = "content")]
    pub content_pdf: bool,
    /// Read the content of files that are not indexed when a search needs it (slow).
    #[setting(key = "content_read_unindexed", default = true, page = "content")]
    pub content_read_unindexed: bool,

    // ---- server (SPEC ch.4 §3–4: IPC, Everything compatibility, HTTP) ----
    /// Native IPC server for fes.exe and the SDK (pipe `Fennec.<instance>.ipc`).
    #[setting(key = "ipc_server", default = true, page = "server")]
    pub ipc_server: bool,
    /// Everything 1.4 compatible IPC window for es.exe, the Everything SDK and launchers
    /// (not created while Everything itself runs).
    #[setting(key = "everything_ipc", default = true, page = "server")]
    pub everything_ipc: bool,
    /// Enable the HTTP server (search page, Everything JSON, downloads).
    #[setting(key = "http_enabled", default = false, page = "server")]
    pub http_enabled: bool,
    /// HTTP server bind address (`127.0.0.1` = this computer only, `0.0.0.0` = all).
    #[setting(key = "http_bind", default = "127.0.0.1", page = "server")]
    pub http_bind: String,
    /// HTTP server port.
    #[setting(key = "http_port", default = 8080, page = "server")]
    pub http_port: u16,
    /// HTTP user name (Basic authentication).
    #[setting(key = "http_username", default = "", page = "server")]
    pub http_username: String,
    /// HTTP password (required; the server does not start without one).
    #[setting(key = "http_password", default = "", page = "server")]
    pub http_password: String,
    /// Allow file downloads over HTTP.
    #[setting(key = "http_allow_downloads", default = true, page = "server")]
    pub http_allow_downloads: bool,
    /// Results per page of the HTTP search page.
    #[setting(key = "http_page_size", default = 32, page = "server")]
    pub http_page_size: u32,
    /// Most results per HTTP request.
    #[setting(key = "http_max_results", default = 10_000, page = "server")]
    pub http_max_results: u32,
    /// Most HTTP requests processed at once.
    #[setting(key = "http_max_concurrency", default = 16, page = "server")]
    pub http_max_concurrency: u32,
    /// HTTP request time limit in milliseconds.
    #[setting(key = "http_timeout_ms", default = 5000, page = "server")]
    pub http_timeout_ms: u32,
    /// Folder served instead of the built-in HTTP page (empty = built-in).
    #[setting(key = "http_static_root", default = "", page = "server")]
    pub http_static_root: String,
    /// Log every HTTP request.
    #[setting(key = "http_access_log", default = false, page = "server")]
    pub http_access_log: bool,

    // ---- tabs and sessions (SPEC ch.3 §7) ----
    /// Show the tab bar even with a single tab.
    #[setting(key = "show_tab_bar", default = false, page = "ui")]
    pub show_tab_bar: bool,
    /// Closing the last tab clears it instead of closing the window.
    #[setting(key = "close_last_tab_keeps_window", default = true, page = "ui")]
    pub close_last_tab_keeps_window: bool,
    /// Show the folder/bookmark sidebar.
    #[setting(key = "show_sidebar", default = false, page = "view")]
    pub show_sidebar: bool,
    /// Sidebar page: `folders` or `bookmarks`.
    #[setting(key = "sidebar_page", default = "folders", page = "view")]
    pub sidebar_page: String,
    /// Sidebar width in points.
    #[setting(key = "sidebar_width", default = 220.0, page = "view")]
    pub sidebar_width: f32,
    /// Show the toolbar.
    #[setting(key = "show_toolbar", default = false, page = "ui")]
    pub show_toolbar: bool,
    /// Toolbar buttons: command ids (`|` = separator).
    #[setting(key = "toolbar", default = vec!["navigate-back".to_string(), "navigate-forward".to_string(), "|".to_string(), "new-tab".to_string(), "new-window".to_string(), "refresh".to_string(), "|".to_string(), "toggle-preview".to_string(), "toggle-sidebar".to_string(), "options".to_string()], page = "ui")]
    pub toolbar: Vec<String>,
    /// Ctrl+F in the result list opens the find bar instead of focusing the search box.
    #[setting(key = "findbar_ctrl_f", default = false, page = "results")]
    pub findbar_ctrl_f: bool,
    /// Save windows and tabs on exit and restore them at start.
    #[setting(key = "restore_session", default = false, page = "ui")]
    pub restore_session: bool,

    // ---- semantic (SPEC ch.5) ----
    /// Enable semantic search (`sem:`) and the background semantic indexer.
    #[setting(key = "semantic_enabled", default = false, page = "semantic")]
    pub semantic_enabled: bool,
    /// Embedding model id (`embeddinggemma-2`, or one from `models.toml`).
    #[setting(key = "semantic_model", default = "embeddinggemma-2", page = "semantic")]
    pub semantic_model: String,
    /// Model file (empty = the model's file in the `models` folder).
    #[setting(key = "semantic_model_path", default = "", page = "semantic")]
    pub semantic_model_path: String,
    /// Stored vector dimension: 128, 256, 512 or 768 (Matryoshka truncation).
    #[setting(key = "semantic_dim", default = 256, page = "semantic")]
    pub semantic_dim: u32,
    /// Folders whose files are embedded (`;`-separated). Empty = Documents and Desktop.
    #[setting(key = "semantic_folders", default = Vec::new(), page = "semantic")]
    pub semantic_folders: Vec<String>,
    /// Embed file names (with their two parent folders).
    #[setting(key = "semantic_names", default = true, page = "semantic")]
    pub semantic_names: bool,
    /// Embed document text.
    #[setting(key = "semantic_content", default = true, page = "semantic")]
    pub semantic_content: bool,
    /// Embed source code.
    #[setting(key = "semantic_code", default = true, page = "semantic")]
    pub semantic_code: bool,
    /// Largest file whose content is embedded (MiB).
    #[setting(key = "semantic_max_file_mb", default = 16, page = "semantic")]
    pub semantic_max_file_mb: u32,
    /// Most chunks per file.
    #[setting(key = "semantic_max_chunks", default = 32, page = "semantic")]
    pub semantic_max_chunks: u32,
    /// CPU limit for semantic indexing in percent of the cores.
    #[setting(key = "semantic_cpu_limit", default = 25, page = "semantic")]
    pub semantic_cpu_limit: u32,
    /// Unload the model after this many idle minutes.
    #[setting(key = "semantic_idle_unload_min", default = 10, page = "semantic")]
    pub semantic_idle_unload_min: u32,
    /// Similarity threshold in percent (`sem:` results below are dropped).
    #[setting(key = "semantic_min_score", default = 45, page = "semantic")]
    pub semantic_min_score: u32,
    /// Semantic candidates per query.
    #[setting(key = "semantic_top", default = 200, page = "semantic")]
    pub semantic_top: u32,
    /// Seconds a changed file waits before it is embedded again.
    #[setting(key = "semantic_debounce", default = 10, page = "semantic")]
    pub semantic_debounce: u32,
    /// Pause semantic indexing.
    #[setting(key = "semantic_paused", default = false, page = "semantic")]
    pub semantic_paused: bool,
    /// Model layers offloaded to the GPU (builds with the `vulkan` feature; 0 = CPU).
    #[setting(key = "semantic_gpu_layers", default = 0, page = "semantic")]
    pub semantic_gpu_layers: u32,
    /// Smart mode: send plain searches to the semantic channel too and fuse (`?sem:`).
    #[setting(key = "semantic_smart", default = false, page = "semantic")]
    pub semantic_smart: bool,

    // ---- advanced ----
    /// Log level: error, warn, info, debug or trace.
    #[setting(key = "log_level", default = "info", page = "advanced")]
    pub log_level: String,
    /// Number of daily log files to keep.
    #[setting(key = "log_max_files", default = 7, page = "advanced")]
    pub log_max_files: u32,
    /// Number of worker threads for searching (0 = all cores).
    #[setting(key = "search_threads", default = 0, page = "advanced")]
    pub search_threads: u32,
}

impl Settings {
    /// Loads `path`; a missing file yields defaults. Unknown keys are ignored with a
    /// warning so settings written by newer versions still load.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(CoreError::io(path, e)),
        };
        Self::from_toml(&text).map_err(|message| CoreError::SettingsParse { path: path.into(), message })
    }

    /// Parses TOML text.
    pub fn from_toml(text: &str) -> std::result::Result<Self, String> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        for key in table.keys() {
            if !Self::descriptors().iter().any(|d| d.key == key) {
                tracing::warn!(key, "ignoring unknown setting");
            }
        }
        Self::deserialize(table).map_err(|e| e.to_string())
    }

    /// Serialises to TOML text.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).map_err(|e| CoreError::SettingsSerialize(e.to_string()))
    }

    /// Writes atomically (`settings.toml.tmp` + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = self.to_toml()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| CoreError::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| CoreError::io(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_everything() {
        let s = Settings::default();
        assert_eq!(s.search_delay, 0);
        assert_eq!(s.operator_precedence, Precedence::Or);
        assert!(!s.match_case);
        assert!(s.index_size);
        assert!(!s.index_date_accessed);
        assert_eq!(s.usn_max_size_mb, 32);
        assert_eq!(s.usn_allocation_delta_mb, 4);
        assert_eq!(s.log_level, "info");
        // HTTP off by default, loopback only, downloads allowed (Everything); IPC on.
        assert!(!s.http_enabled && s.http_bind == "127.0.0.1" && s.http_port == 8080 && s.http_allow_downloads);
        assert!(s.ipc_server && s.everything_ipc);
    }

    #[test]
    fn descriptors_are_complete_and_unique() {
        let d = Settings::descriptors();
        assert!(d.len() > 30);
        let mut keys: Vec<_> = d.iter().map(|d| d.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), d.len());
        assert!(d.iter().all(|d| !d.doc.is_empty()), "every setting needs a description");
        let delay = d.iter().find(|d| d.key == "search_delay").unwrap();
        assert_eq!(delay.page, "search");
        assert_eq!(delay.kind, SettingKind::Int);
        assert_eq!(delay.default_src, "0");
    }

    #[test]
    fn search_filter_finds_delay() {
        let hits = Settings::search("delay");
        assert!(hits.iter().any(|d| d.key == "search_delay"));
        assert!(hits.iter().all(|d| d.key.contains("delay") || d.doc.to_lowercase().contains("delay")));
    }

    #[test]
    fn get_set_reset() {
        let mut s = Settings::default();
        assert_eq!(s.get("search_delay").as_deref(), Some("0"));
        s.set("search_delay", "250").unwrap();
        assert_eq!(s.search_delay, 250);
        s.set("theme", "Dark").unwrap();
        assert_eq!(s.theme, Theme::Dark);
        s.set("folders", "C:\\a; D:\\b ;").unwrap();
        assert_eq!(s.folders, vec!["C:\\a".to_string(), "D:\\b".to_string()]);
        assert_eq!(s.get("folders").as_deref(), Some("C:\\a;D:\\b"));
        assert!(matches!(s.set("search_delay", "x"), Err(SettingError::Invalid { .. })));
        assert!(matches!(s.set("nope", "1"), Err(SettingError::UnknownKey(_))));
        assert!(s.reset("search_delay"));
        assert_eq!(s.search_delay, 0);
        assert!(!s.reset("nope"));
    }

    #[test]
    fn toml_roundtrip_and_partial_files() {
        let s = Settings {
            search_delay: 42,
            theme: Theme::Light,
            file_lists: vec!["D:\\x.efu".into()],
            ..Settings::default()
        };
        let text = s.to_toml().unwrap();
        assert!(text.contains("search_delay = 42"));
        assert_eq!(Settings::from_toml(&text).unwrap(), s);

        let partial = Settings::from_toml("match_case = true\nunknown_future_key = 1\n").unwrap();
        assert!(partial.match_case);
        assert_eq!(partial.search_delay, 0);
        assert!(Settings::from_toml("search_delay = \"fast\"").is_err());
    }

    #[test]
    fn save_and_load_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.toml");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let s = Settings { highlight: false, ..Settings::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        assert!(!path.with_extension("toml.tmp").exists());
    }
}

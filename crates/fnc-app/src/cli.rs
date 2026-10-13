//! Command line (SPEC ch.4 §1), Everything compatible.
//!
//! Unknown switches produce a warning instead of failing.

/// Parsed command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cli {
    /// Initial search (`-s`, `-search`, trailing text).
    pub search: Option<String>,
    /// `-instance <name>`.
    pub instance: String,
    /// `-filelist <file.efu>` (repeatable): mount EFU file lists.
    pub file_lists: Vec<String>,
    /// `-folder <path>` (repeatable): add folder index roots.
    pub folders: Vec<String>,
    /// `-newwindow`.
    pub new_window: bool,
    /// `-startup` / `-minimized` / `-tray`: start hidden.
    pub start_hidden: bool,
    /// `-console`: log to stderr.
    pub console: bool,
    /// `-verbose` / `-debug`.
    pub verbose: bool,
    /// `-first-instance`: ignore the single-instance check.
    pub first_instance: bool,
    /// `-exit` / `-quit`: ask the running instance to exit.
    pub exit: bool,
    /// `-help`.
    pub help: bool,
    /// `-version`.
    pub version: bool,
    /// `-install-service` / `-uninstall-service`.
    pub service: Option<ServiceAction>,
    /// `-reindex`: ignore the saved database and rebuild.
    pub reindex: bool,
    /// `-nodb`: neither load nor save the database.
    pub no_db: bool,
    /// `-db <file>`: database file.
    pub db: Option<String>,
    /// `-toggle-window` / `-show` / `-hide` / `-close`.
    pub window: Option<WindowAction>,
    /// `-filter <name>`.
    pub filter: Option<String>,
    /// `-bookmark <name>`.
    pub bookmark: Option<String>,
    /// `-sort <column>` with `-sort-ascending` / `-sort-descending`.
    pub sort: Option<(String, Option<bool>)>,
    /// `-filename <path>`: search the file's folder and select the file.
    pub filename: Option<String>,
    /// `-update`: rescan folder indexes.
    pub update: bool,
    /// `-save-db`.
    pub save_db: bool,
    /// `-admin`: restart elevated.
    pub admin: bool,
    /// `-install-run-on-system-startup` (true) / `-uninstall-run-on-system-startup` (false).
    pub run_on_startup: Option<bool>,
    /// Explorer context menu entries to install (true) or remove (false).
    pub context_menu: Vec<(ContextMenu, bool)>,
    /// `-config <file>`: settings file.
    pub config: Option<String>,
    /// `-log <file>`.
    pub log: Option<String>,
    /// `-portable` / `-no-app-data`: keep data next to the executable.
    pub portable: bool,
    /// Tool commands that run without a window.
    pub tool: Option<Tool>,
    /// Search modifiers from `-case`, `-ww`, `-path`, `-diacritics`, `-regex`.
    pub modifiers: Vec<&'static str>,
    /// `-connect [user:password@]host[:port]`: search an ETP server (Everything's option).
    pub connect: Option<String>,
    /// Unknown switches (reported as warnings).
    pub unknown: Vec<String>,
}

/// `-install-service` / `-uninstall-service` / `-start-service` / `-stop-service`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    /// Register and start.
    Install,
    /// Stop and remove.
    Uninstall,
    /// Start.
    Start,
    /// Stop.
    Stop,
}

/// Window commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// `-toggle-window`.
    Toggle,
    /// `-show`.
    Show,
    /// `-hide`.
    Hide,
    /// `-close`: close (hide to tray or exit, like the close button).
    Close,
}

/// Explorer context menu entries (SPEC ch.4 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenu {
    /// Folders and folder backgrounds: "Search Fennec" (`-path "%1"`).
    Folder,
    /// Files: "Explore in Fennec" (`-filename "%1"`).
    File,
}

/// Commands that run and exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    /// `-create-file-list <out.efu> <folder>`.
    CreateFileList {
        /// Output file.
        out: String,
        /// Folder to list.
        folder: String,
    },
    /// `-create-usn-journal <X:> <max bytes> <delta bytes>`.
    CreateUsnJournal {
        /// Drive.
        drive: String,
        /// Maximum size.
        max: u64,
        /// Allocation delta.
        delta: u64,
    },
    /// `-delete-usn-journal <X:>`.
    DeleteUsnJournal {
        /// Drive.
        drive: String,
    },
}

/// Usage text (not localised: printed to the console for scripts).
pub const USAGE: &str = "\
fennec [options] [search text]

  -s, -search <text>      search for <text>
  -r, -regex <regex>      search with a regular expression
  -case | -nocase         match case
  -ww | -noww             match whole words
  -matchpath | -nopath    match the full path
  -diacritics             match diacritics
  -p, -path <path>        search inside <path>
  -parent <path>          search the direct children of <path>
  -filelist <file.efu>    mount an EFU file list
  -folder <path>          index a folder (folder index)
  -instance <name>        use a separate instance (data, mutex, pipe)
  -connect [user:password@]host[:port]   search an ETP/FTP server
  -newwindow              open a new window
  -startup, -minimized    start hidden in the tray
  -exit, -quit            exit the running instance
  -toggle-window, -show, -hide, -close   window control
  -filter <name>          apply a filter      -bookmark <name>   open a bookmark
  -sort <column>          sort (-sort-ascending / -sort-descending)
  -filename <path>        show the file in its folder (Explore in Fennec)
  -update                 rescan folder indexes     -save-db   save the database now
  -admin                  restart with administrator rights
  -start-service, -stop-service
  -install-run-on-system-startup / -uninstall-run-on-system-startup
  -install-folder-context-menu / -uninstall-folder-context-menu
  -install-file-context-menu / -uninstall-file-context-menu
  -create-file-list <out.efu> <folder>
  -create-usn-journal <X:> <max> <delta>   -delete-usn-journal <X:>
  -config <file>  -log <file>  -portable  -no-app-data
  -install-service        install fennec-service (index NTFS without admin rights)
  -uninstall-service      remove fennec-service
  -reindex                rebuild the index (ignore the database)
  -nodb                   do not load or save the database
  -db <file.fnx>          use another database file
  -first-instance         do not forward to a running instance
  -console, -verbose      log to the console / verbose logging
  -help, -version";

fn quote_path(p: &str) -> String {
    let p = p.trim_end_matches(['\\', '/']);
    format!("\"{p}\\\"")
}

/// Parses arguments (without the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Cli {
    let mut cli = Cli::default();
    let mut text: Vec<String> = Vec::new();
    let mut prefix: Vec<String> = Vec::new();
    let mut it = args.into_iter().peekable();
    while let Some(a) = it.next() {
        // `/switch` is accepted on Windows only (elsewhere it is an absolute path).
        let is_switch = (a.starts_with('-') || (cfg!(windows) && a.starts_with('/'))) && a.len() > 1;
        if !is_switch {
            text.push(a);
            continue;
        }
        let key = a[1..].to_ascii_lowercase().replace('_', "-");
        let value = |it: &mut std::iter::Peekable<I::IntoIter>| it.next();
        match key.as_str() {
            "s" | "search" => {
                if let Some(v) = value(&mut it) {
                    text.push(v);
                }
            }
            // `-search*`: the rest is plain text (operators not parsed).
            "search*" => {
                if let Some(v) = value(&mut it) {
                    text.push(format!("\"{}\"", v.replace('"', "&quot:")));
                }
            }
            "r" | "regex" => {
                if let Some(v) = value(&mut it) {
                    text.push(format!("regex:\"{v}\""));
                }
            }
            "case" => cli.modifiers.push("case:"),
            "nocase" => cli.modifiers.push("nocase:"),
            "ww" | "wholeword" => cli.modifiers.push("ww:"),
            "noww" | "nowholeword" => cli.modifiers.push("noww:"),
            "matchpath" => cli.modifiers.push("path:"),
            "nopath" => cli.modifiers.push("nopath:"),
            "diacritics" => cli.modifiers.push("diacritics:"),
            "nodiacritics" => cli.modifiers.push("nodiacritics:"),
            "p" | "path" => {
                // `-path <dir>` searches a folder; a bare `-path` enables match-path.
                match it.peek() {
                    Some(v) if !v.starts_with('-') => {
                        let v = it.next().unwrap_or_default();
                        prefix.push(quote_path(&v));
                    }
                    _ => cli.modifiers.push("path:"),
                }
            }
            "parent" | "parentpath" => {
                if let Some(v) = value(&mut it) {
                    prefix.push(format!("parent:\"{}\"", v.trim_end_matches(['\\', '/'])));
                }
            }
            "filelist" | "efu" => {
                if let Some(v) = value(&mut it) {
                    cli.file_lists.push(v);
                }
            }
            "folder" => {
                if let Some(v) = value(&mut it) {
                    cli.folders.push(v);
                }
            }
            "instance" => cli.instance = value(&mut it).unwrap_or_default(),
            "connect" => cli.connect = value(&mut it),
            "newwindow" | "new-window" => cli.new_window = true,
            "startup" | "minimized" | "tray" => cli.start_hidden = true,
            "console" => cli.console = true,
            "verbose" | "debug" => cli.verbose = true,
            "first-instance" => cli.first_instance = true,
            "exit" | "quit" => cli.exit = true,
            "install-service" => cli.service = Some(ServiceAction::Install),
            "uninstall-service" => cli.service = Some(ServiceAction::Uninstall),
            "start-service" => cli.service = Some(ServiceAction::Start),
            "stop-service" => cli.service = Some(ServiceAction::Stop),
            "reindex" | "rebuild" => cli.reindex = true,
            "toggle-window" => cli.window = Some(WindowAction::Toggle),
            "show" => cli.window = Some(WindowAction::Show),
            "hide" => cli.window = Some(WindowAction::Hide),
            "close" => cli.window = Some(WindowAction::Close),
            "filter" => cli.filter = value(&mut it),
            "bookmark" => cli.bookmark = value(&mut it),
            "sort" => {
                if let Some(v) = value(&mut it) {
                    let dir = cli.sort.as_ref().and_then(|s| s.1);
                    cli.sort = Some((v, dir));
                }
            }
            "sort-ascending" | "sort-descending" => {
                let asc = key == "sort-ascending";
                let col = cli.sort.take().map(|s| s.0).unwrap_or_default();
                cli.sort = Some((col, Some(asc)));
            }
            "filename" | "select" => cli.filename = value(&mut it),
            "update" => cli.update = true,
            "save-db" => cli.save_db = true,
            "admin" => cli.admin = true,
            "install-run-on-system-startup" => cli.run_on_startup = Some(true),
            "uninstall-run-on-system-startup" => cli.run_on_startup = Some(false),
            "install-folder-context-menu" => cli.context_menu.push((ContextMenu::Folder, true)),
            "uninstall-folder-context-menu" => cli.context_menu.push((ContextMenu::Folder, false)),
            "install-file-context-menu" => cli.context_menu.push((ContextMenu::File, true)),
            "uninstall-file-context-menu" => cli.context_menu.push((ContextMenu::File, false)),
            "config" => cli.config = value(&mut it),
            "log" => cli.log = value(&mut it),
            "portable" | "no-app-data" => cli.portable = true,
            "create-file-list" => {
                if let (Some(out), Some(folder)) = (value(&mut it), value(&mut it)) {
                    cli.tool = Some(Tool::CreateFileList { out, folder });
                }
            }
            "create-usn-journal" => {
                let (d, m, a) = (value(&mut it), value(&mut it), value(&mut it));
                if let Some(drive) = d {
                    let num = |s: Option<String>, def: u64| s.and_then(|s| s.parse().ok()).unwrap_or(def);
                    cli.tool = Some(Tool::CreateUsnJournal { drive, max: num(m, 32 << 20), delta: num(a, 4 << 20) });
                }
            }
            "delete-usn-journal" => {
                if let Some(drive) = value(&mut it) {
                    cli.tool = Some(Tool::DeleteUsnJournal { drive });
                }
            }
            "nodb" | "no-db" => cli.no_db = true,
            "db" => cli.db = value(&mut it),
            "help" | "h" | "?" => cli.help = true,
            "version" | "v" => cli.version = true,
            _ => cli.unknown.push(a),
        }
    }
    if !text.is_empty() || !prefix.is_empty() || !cli.modifiers.is_empty() {
        let mut s = String::new();
        for m in &cli.modifiers {
            s.push_str(m);
        }
        let body = prefix.into_iter().chain(text).collect::<Vec<_>>().join(" ");
        if !cli.modifiers.is_empty() && body.contains(' ') {
            // Modifiers apply to every term.
            s = format!("{} {body}", cli.modifiers.iter().map(|m| format!("?{m}")).collect::<String>());
        } else {
            s.push_str(&body);
        }
        cli.search = Some(s.trim().to_string());
    }
    cli
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Cli {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn search_forms() {
        assert_eq!(p(&["-s", "ext:pdf"]).search.as_deref(), Some("ext:pdf"));
        assert_eq!(p(&["-search", "a b"]).search.as_deref(), Some("a b"));
        assert_eq!(p(&["foo", "bar"]).search.as_deref(), Some("foo bar"));
        assert_eq!(p(&["-r", "^IMG"]).search.as_deref(), Some("regex:\"^IMG\""));
        assert_eq!(p(&["-case", "Abc"]).search.as_deref(), Some("case:Abc"));
        assert_eq!(p(&["-case", "a", "b"]).search.as_deref(), Some("?case: a b"));
        assert_eq!(p(&["-p", "C:\\Windows\\", "notepad"]).search.as_deref(), Some("\"C:\\Windows\\\" notepad"));
        assert_eq!(p(&["-path"]).search.as_deref(), Some("path:"));
        assert_eq!(p(&["-parent", "C:\\x"]).search.as_deref(), Some("parent:\"C:\\x\""));
        assert_eq!(p(&[]).search, None);
        let c = p(&["-connect", "u:p@host:2121", "abc"]);
        assert_eq!((c.connect.as_deref(), c.search.as_deref()), (Some("u:p@host:2121"), Some("abc")));
    }

    #[test]
    fn switches_and_unknown() {
        let c = p(&[
            "-instance",
            "work",
            "-filelist",
            "a.efu",
            "-folder",
            "/home",
            "-startup",
            "-console",
            "-frobnicate",
            "-newwindow",
        ]);
        assert_eq!(c.instance, "work");
        assert_eq!(c.file_lists, vec!["a.efu"]);
        assert_eq!(c.folders, vec!["/home"]);
        assert!(c.start_hidden && c.console && c.new_window);
        assert_eq!(c.unknown, vec!["-frobnicate"]);
        assert!(p(&["-?"]).help);
        assert!(p(&["-quit"]).exit);
    }

    #[test]
    fn everything_switches() {
        let c =
            p(&["-toggle-window", "-filter", "Audio", "-sort", "size", "-sort-descending", "-filename", "C:\\a.txt"]);
        assert_eq!(c.window, Some(WindowAction::Toggle));
        assert_eq!(c.filter.as_deref(), Some("Audio"));
        assert_eq!(c.sort, Some(("size".into(), Some(false))));
        assert_eq!(c.filename.as_deref(), Some("C:\\a.txt"));
        let c = p(&["-sort-ascending", "-sort", "path"]);
        assert_eq!(c.sort, Some(("path".into(), Some(true))));
        let c = p(&["-install-folder-context-menu", "-uninstall-file-context-menu", "-install-run-on-system-startup"]);
        assert_eq!(c.context_menu, vec![(ContextMenu::Folder, true), (ContextMenu::File, false)]);
        assert_eq!(c.run_on_startup, Some(true));
        assert_eq!(
            p(&["-create-file-list", "out.efu", "D:\\x"]).tool,
            Some(Tool::CreateFileList { out: "out.efu".into(), folder: "D:\\x".into() })
        );
        assert_eq!(
            p(&["-create-usn-journal", "D:", "1000", "100"]).tool,
            Some(Tool::CreateUsnJournal { drive: "D:".into(), max: 1000, delta: 100 })
        );
        assert_eq!(p(&["-search*", "a | b"]).search.as_deref(), Some("\"a | b\""));
        assert!(p(&["-rebuild"]).reindex && p(&["-update"]).update && p(&["-save-db"]).save_db);
        assert_eq!(p(&["-start-service"]).service, Some(ServiceAction::Start));
    }
}

//! Command line (SPEC ch.4 §1): the Everything-compatible subset needed for M1.
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
    /// Search modifiers from `-case`, `-ww`, `-path`, `-diacritics`, `-regex`.
    pub modifiers: Vec<&'static str>,
    /// Unknown switches (reported as warnings).
    pub unknown: Vec<String>,
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
  -newwindow              open a new window
  -startup, -minimized    start hidden in the tray
  -exit, -quit            exit the running instance
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
            "s" | "search" | "search*" => {
                if let Some(v) = value(&mut it) {
                    text.push(v);
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
            "newwindow" | "new-window" => cli.new_window = true,
            "startup" | "minimized" | "tray" => cli.start_hidden = true,
            "console" => cli.console = true,
            "verbose" | "debug" => cli.verbose = true,
            "first-instance" => cli.first_instance = true,
            "exit" | "quit" => cli.exit = true,
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
}

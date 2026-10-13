//! Advanced rename (SPEC ch.3 §9): new names for many files from a template, find and
//! replace (literal or regex with capture groups), case conversion and trimming, with a
//! preview that flags conflicts before anything is renamed.
//!
//! Template variables: `$name:` `$stem:` `$ext:` `$parent:` `$size:` `$dm:` `$dc:`
//! (`YYYY-MM-DD`) and `$n` — a counter (`start`, `step`, zero-padded to `digits`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fnc_core::FileTime;

/// Case conversion.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum CaseMode {
    /// Unchanged.
    #[default]
    Keep,
    /// `lower case`.
    Lower,
    /// `UPPER CASE`.
    Upper,
    /// `Title Case`.
    Title,
    /// `Sentence case`.
    Sentence,
}

/// The rename rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    /// New name template (`$name:` keeps the name).
    pub template: String,
    /// Text to find (empty = no replacement).
    pub find: String,
    /// Replacement (`$1`… capture groups with `regex`).
    pub replace: String,
    /// `find` is a regular expression.
    pub regex: bool,
    /// Case-sensitive find.
    pub match_case: bool,
    /// Find/replace and case only in the stem (keep the extension).
    pub keep_ext: bool,
    /// Case conversion.
    pub case: CaseMode,
    /// Trim spaces and collapse runs of spaces.
    pub trim: bool,
    /// Counter start.
    pub start: i64,
    /// Counter step.
    pub step: i64,
    /// Counter width (zero padding).
    pub digits: usize,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            template: "$name:".into(),
            find: String::new(),
            replace: String::new(),
            regex: false,
            match_case: false,
            keep_ext: true,
            case: CaseMode::Keep,
            trim: false,
            start: 1,
            step: 1,
            digits: 1,
        }
    }
}

/// A file or folder to rename.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Current full path.
    pub path: PathBuf,
    /// Folder.
    pub is_dir: bool,
    /// Size (files).
    pub size: Option<u64>,
    /// Date modified.
    pub modified: FileTime,
    /// Date created.
    pub created: FileTime,
}

/// Why a new name cannot be used.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Empty name.
    Empty,
    /// Contains `\ / : * ? " < > |`, a control character, or ends with a dot or space.
    Invalid,
    /// Two items would get the same name in one folder.
    Duplicate,
    /// Another file in the folder already has the name.
    Exists,
    /// Regular expression error.
    BadPattern,
}

/// Preview of one item.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// Current name.
    pub old: String,
    /// New name.
    pub new: String,
    /// Problem, if any (renaming is refused while any item has one).
    pub problem: Option<Problem>,
}

impl Preview {
    /// The name changes and can be applied.
    pub fn changes(&self) -> bool {
        self.problem.is_none() && self.old != self.new
    }
}

fn split(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i + 1..]),
    }
}

fn date(t: FileTime) -> String {
    t.to_local().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

fn expand(template: &str, item: &Item, counter: i64, digits: usize) -> String {
    let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, ext) = split(&name, item.is_dir);
    let parent =
        item.path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = if counter < 0 { format!("-{:0digits$}", -counter) } else { format!("{counter:0digits$}") };
    let vars: [(&str, String); 8] = [
        ("$name:", name.clone()),
        ("$stem:", stem.to_string()),
        ("$ext:", ext.to_string()),
        ("$parent:", parent),
        ("$size:", item.size.map(|s| s.to_string()).unwrap_or_default()),
        ("$dm:", date(item.modified)),
        ("$dc:", date(item.created)),
        ("$n", n),
    ];
    let mut out = String::with_capacity(template.len() + name.len());
    let mut rest = template;
    'outer: while !rest.is_empty() {
        if rest.starts_with('$') {
            let lower = rest.to_ascii_lowercase();
            for (k, v) in &vars {
                if lower.starts_with(k) {
                    out.push_str(v);
                    rest = &rest[k.len()..];
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn convert_case(s: &str, mode: CaseMode) -> String {
    match mode {
        CaseMode::Keep => s.to_string(),
        CaseMode::Lower => s.to_lowercase(),
        CaseMode::Upper => s.to_uppercase(),
        CaseMode::Title | CaseMode::Sentence => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            let mut first = true;
            for c in s.chars() {
                let up = if mode == CaseMode::Title { start } else { first && c.is_alphanumeric() };
                if up {
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
                if c.is_alphanumeric() {
                    first = false;
                }
                start = !c.is_alphanumeric() && c != '\'';
            }
            out
        }
    }
}

fn invalid(name: &str) -> bool {
    name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
        || name.ends_with(['.', ' '])
        || name == "."
        || name == ".."
}

/// Computes new names; `exists(folder, name)` reports files outside the rename set
/// (case-insensitive file systems: compare folded).
pub fn preview(items: &[Item], rule: &Rule, exists: &dyn Fn(&Path, &str) -> bool) -> Vec<Preview> {
    let re = if rule.regex && !rule.find.is_empty() {
        match regex::RegexBuilder::new(&rule.find).case_insensitive(!rule.match_case).build() {
            Ok(r) => Some(Ok(r)),
            Err(_) => Some(Err(())),
        }
    } else {
        None
    };
    let mut out: Vec<Preview> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let old = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let counter = rule.start.saturating_add(rule.step.saturating_mul(i as i64));
            let base = expand(&rule.template, item, counter, rule.digits.clamp(1, 12));
            // The part that find/replace and case act on.
            let (body, ext) = if rule.keep_ext && !item.is_dir {
                let (s, e) = split(&base, false);
                (s.to_string(), if e.is_empty() && !base.ends_with('.') { None } else { Some(e.to_string()) })
            } else {
                (base.clone(), None)
            };
            let mut problem = None;
            let mut body = if rule.find.is_empty() {
                body
            } else {
                match &re {
                    Some(Ok(r)) => r.replace_all(&body, rule.replace.as_str()).into_owned(),
                    Some(Err(())) => {
                        problem = Some(Problem::BadPattern);
                        body
                    }
                    None if rule.match_case => body.replace(&rule.find, &rule.replace),
                    None => replace_ci(&body, &rule.find, &rule.replace),
                }
            };
            body = convert_case(&body, rule.case);
            if rule.trim {
                body = body.split(' ').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
            }
            let new = match ext {
                Some(e) => format!("{body}.{e}"),
                None => body,
            };
            if problem.is_none() {
                problem = if new.trim().is_empty() {
                    Some(Problem::Empty)
                } else if invalid(&new) {
                    Some(Problem::Invalid)
                } else {
                    None
                };
            }
            Preview { old, new, problem }
        })
        .collect();
    // Conflicts within a folder (case-insensitive) and with files that are not renamed.
    let key =
        |item: &Item, name: &str| (item.path.parent().map(Path::to_path_buf).unwrap_or_default(), name.to_lowercase());
    let old_names: std::collections::HashSet<(PathBuf, String)> =
        items.iter().zip(&out).map(|(i, p)| key(i, &p.old)).collect();
    let mut seen: HashMap<(PathBuf, String), usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if out[i].problem.is_some() {
            continue;
        }
        let k = key(item, &out[i].new);
        if let Some(&j) = seen.get(&k) {
            out[i].problem = Some(Problem::Duplicate);
            out[j].problem = Some(Problem::Duplicate);
            continue;
        }
        seen.insert(k.clone(), i);
        let renamed_case_only = out[i].new.to_lowercase() == out[i].old.to_lowercase();
        if !renamed_case_only && !old_names.contains(&k) && exists(&k.0, &out[i].new) {
            out[i].problem = Some(Problem::Exists);
        }
    }
    out
}

fn replace_ci(s: &str, find: &str, with: &str) -> String {
    let lower = s.to_lowercase();
    let f = find.to_lowercase();
    // Lowercasing can change byte lengths (rare); fall back to case-sensitive then.
    if lower.len() != s.len() || f.len() != find.len() {
        return s.replace(find, with);
    }
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for (i, _) in lower.match_indices(&f) {
        out.push_str(&s[last..i]);
        out.push_str(with);
        last = i + f.len();
    }
    out.push_str(&s[last..]);
    out
}

/// Renames that must happen, in an order that never overwrites: a name that another item
/// still has is renamed through a temporary name first. Returns `(from, to)` steps.
pub fn plan_steps(items: &[Item], previews: &[Preview]) -> Vec<(PathBuf, PathBuf)> {
    let mut steps = Vec::new();
    let mut deferred = Vec::new();
    let olds: std::collections::HashSet<(PathBuf, String)> = items
        .iter()
        .zip(previews)
        .filter(|(_, p)| p.changes())
        .map(|(i, p)| (i.path.parent().map(Path::to_path_buf).unwrap_or_default(), p.old.to_lowercase()))
        .collect();
    for (item, p) in items.iter().zip(previews).filter(|(_, p)| p.changes()) {
        let dir = item.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let target = dir.join(&p.new);
        let clash = olds.contains(&(dir.clone(), p.new.to_lowercase())) && p.new.to_lowercase() != p.old.to_lowercase();
        if clash {
            let tmp = dir.join(format!("{}.fennec-rename-{}", p.old, steps.len() + deferred.len()));
            steps.push((item.path.clone(), tmp.clone()));
            deferred.push((tmp, target));
        } else {
            steps.push((item.path.clone(), target));
        }
    }
    steps.extend(deferred);
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(p: &str) -> Item {
        Item { path: PathBuf::from(p), is_dir: false, size: Some(10), modified: FileTime(0), created: FileTime(0) }
    }

    fn names(items: &[Item], rule: &Rule) -> Vec<(String, Option<Problem>)> {
        preview(items, rule, &|_, _| false).into_iter().map(|p| (p.new, p.problem)).collect()
    }

    #[test]
    fn templates_counters_and_case() {
        let items = [item("/d/IMG_001.JPG"), item("/d/IMG_002.JPG"), item("/d/notes.txt")];
        let r = Rule { template: "Holiday $n.$ext:".into(), digits: 3, start: 10, step: 5, ..Rule::default() };
        assert_eq!(
            names(&items, &r).into_iter().map(|x| x.0).collect::<Vec<_>>(),
            ["Holiday 010.JPG", "Holiday 015.JPG", "Holiday 020.txt"]
        );
        let r = Rule { case: CaseMode::Lower, ..Rule::default() };
        assert_eq!(names(&items, &r)[0].0, "img_001.JPG", "extension kept as is");
        let r = Rule { case: CaseMode::Lower, keep_ext: false, ..Rule::default() };
        assert_eq!(names(&items, &r)[0].0, "img_001.jpg");
        let r = Rule { case: CaseMode::Title, ..Rule::default() };
        assert_eq!(names(&[item("/d/my holiday photo.jpg")], &r)[0].0, "My Holiday Photo.jpg");
        let r = Rule { case: CaseMode::Sentence, ..Rule::default() };
        assert_eq!(names(&[item("/d/MY HOLIDAY.jpg")], &r)[0].0, "My holiday.jpg");
        let r = Rule { template: "$parent:-$stem:.$ext:".into(), ..Rule::default() };
        assert_eq!(names(&[item("/trip/a.jpg")], &r)[0].0, "trip-a.jpg");
    }

    #[test]
    fn find_replace_literal_and_regex() {
        let items = [item("/d/Report  final  v2.docx"), item("/d/report-v3.docx")];
        let r = Rule { find: "REPORT".into(), replace: "Summary".into(), trim: true, ..Rule::default() };
        assert_eq!(names(&items, &r)[0].0, "Summary final v2.docx");
        assert_eq!(names(&items, &r)[1].0, "Summary-v3.docx");
        let r = Rule { find: r"v(\d+)".into(), replace: "rev$1".into(), regex: true, ..Rule::default() };
        assert_eq!(names(&items, &r)[1].0, "report-rev3.docx");
        let r = Rule { find: "(".into(), regex: true, ..Rule::default() };
        assert_eq!(names(&items, &r)[0].1, Some(Problem::BadPattern));
    }

    #[test]
    fn conflicts_are_detected() {
        let items = [item("/d/a.txt"), item("/d/b.txt"), item("/e/c.txt")];
        let r = Rule { template: "same.txt".into(), ..Rule::default() };
        let p = names(&items, &r);
        assert_eq!(p[0].1, Some(Problem::Duplicate));
        assert_eq!(p[1].1, Some(Problem::Duplicate));
        assert_eq!(p[2].1, None, "other folder");
        let p =
            preview(&items[..1], &Rule { template: "taken.txt".into(), ..Rule::default() }, &|_, n| n == "taken.txt");
        assert_eq!(p[0].problem, Some(Problem::Exists));
        let p = names(&items[..1], &Rule { template: "a?.txt".into(), ..Rule::default() });
        assert_eq!(p[0].1, Some(Problem::Invalid));
        let p = names(&items[..1], &Rule { template: "".into(), keep_ext: false, ..Rule::default() });
        assert_eq!(p[0].1, Some(Problem::Empty));
        // Case-only renames of the same file are fine.
        let p = preview(&items[..1], &Rule { case: CaseMode::Upper, ..Rule::default() }, &|_, n| {
            n.eq_ignore_ascii_case("a.txt")
        });
        assert_eq!((p[0].new.as_str(), p[0].problem), ("A.txt", None));
    }

    #[test]
    fn swaps_go_through_temporary_names() {
        let items = [item("/d/1.txt"), item("/d/2.txt")];
        let r = Rule { template: "$n.txt".into(), start: 2, step: -1, ..Rule::default() };
        let p = preview(&items, &r, &|_, _| false);
        assert!(p.iter().all(Preview::changes), "{p:?}");
        let steps = plan_steps(&items, &p);
        assert_eq!(steps.len(), 4);
        // Every destination is free when its step runs.
        let mut present: std::collections::HashSet<PathBuf> = items.iter().map(|i| i.path.clone()).collect();
        for (from, to) in &steps {
            assert!(present.remove(from), "{from:?} missing");
            assert!(present.insert(to.clone()), "{to:?} would be overwritten");
        }
        assert!(present.contains(Path::new("/d/1.txt")) && present.contains(Path::new("/d/2.txt")));
    }
}

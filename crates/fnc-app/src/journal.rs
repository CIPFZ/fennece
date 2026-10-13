//! Index journal (SPEC ch.1 §10): `(time, action, path before, path after)` records of
//! every change applied while Fennec runs, in `journal.tsv` next to the database.
//! Retention: `index_journal_days` and `index_journal_max` records, enforced at start
//! and when the file grows past the limit.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use fnc_core::{EntryId, FileTime};
use fnc_index::{Change, ChangeBatch, EntryRef, IndexStore, PathChange};

/// What happened.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Created.
    Create,
    /// Deleted.
    Delete,
    /// Renamed in place.
    Rename,
    /// Moved to another folder.
    Move,
    /// Size, dates or attributes changed.
    Modify,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Delete => "delete",
            Action::Rename => "rename",
            Action::Move => "move",
            Action::Modify => "modify",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "create" => Action::Create,
            "delete" => Action::Delete,
            "rename" => Action::Rename,
            "move" => Action::Move,
            "modify" => Action::Modify,
            _ => return None,
        })
    }

    /// Localised name.
    pub fn label(self) -> String {
        crate::i18n::t(&format!("journal-{}", self.as_str()))
    }
}

/// One record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// When it was applied.
    pub time: FileTime,
    /// Kind.
    pub action: Action,
    /// Path before (delete/rename/move) or empty.
    pub before: String,
    /// Path after (create/rename/move/modify) or empty.
    pub after: String,
}

impl Record {
    fn line(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        format!("{}\t{}\t{}\t{}\n", self.time.0, self.action.as_str(), clean(&self.before), clean(&self.after))
    }

    fn parse(line: &str) -> Option<Self> {
        let mut it = line.trim_end_matches(['\r', '\n']).split('\t');
        let time = FileTime(it.next()?.parse().ok()?);
        let action = Action::parse(it.next()?)?;
        Some(Self { time, action, before: it.next()?.to_string(), after: it.next()?.to_string() })
    }
}

/// Appends records; thread-safe through the caller's lock.
pub struct Journal {
    path: PathBuf,
    out: Option<std::io::BufWriter<std::fs::File>>,
    days: u32,
    max: usize,
    lines: usize,
}

/// One change waiting for its "after" path.
pub struct Pending {
    action: Action,
    before: String,
    /// Where to read the path after applying.
    after: Option<AfterRef>,
}

enum AfterRef {
    Entry(EntryRef),
    Path(fnc_core::VolumeId, fnc_index::RelPath),
}

fn path_of(s: &IndexStore, id: EntryId) -> String {
    let mut p = String::new();
    s.full_path(id, &mut p);
    p
}

impl Journal {
    /// Opens (creating) `path`, dropping records older than `days` / beyond `max`.
    pub fn open(path: PathBuf, days: u32, max: usize) -> std::io::Result<Self> {
        let mut j = Self { path, out: None, days, max: max.max(1), lines: 0 };
        j.trim()?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(&j.path)?;
        j.out = Some(std::io::BufWriter::new(f));
        Ok(j)
    }

    fn cutoff(&self) -> FileTime {
        FileTime(FileTime::now().0 - i64::from(self.days) * 86_400 * 10_000_000)
    }

    /// Rewrites the file keeping only records within the limits.
    fn trim(&mut self) -> std::io::Result<()> {
        self.out = None;
        let records = read(&self.path, usize::MAX);
        let cutoff = self.cutoff();
        let mut keep: Vec<&Record> = records.iter().filter(|r| r.time >= cutoff).collect();
        if keep.len() > self.max {
            keep.drain(..keep.len() - self.max);
        }
        if keep.len() != records.len() {
            let text: String = keep.iter().map(|r| r.line()).collect();
            crate::history::write_atomic(&self.path, &text)?;
        }
        self.lines = keep.len();
        Ok(())
    }

    /// Records what `batch` is about to do (paths before), from the index before it is
    /// applied. Call [`Journal::commit`] with the result after applying.
    pub fn prepare(s: &IndexStore, batch: &ChangeBatch) -> Vec<Pending> {
        let mut out = Vec::new();
        for c in batch {
            let p = match c {
                Change::Create { volume, frn, parent, name, .. } => {
                    match s.resolve(EntryRef::Frn(*volume, *frn)) {
                        // A re-reported record of a known entry: a rename or move.
                        Some(id) if s.name(id) == &name[..] && s.resolve(*parent) == Some(s.parent(id)) => continue,
                        Some(id) => {
                            let moved = s.resolve(*parent).is_some_and(|p| p != s.parent(id));
                            Pending {
                                action: if moved { Action::Move } else { Action::Rename },
                                before: path_of(s, id),
                                after: Some(AfterRef::Entry(EntryRef::Frn(*volume, *frn))),
                            }
                        }
                        None => Pending {
                            action: Action::Create,
                            before: String::new(),
                            after: Some(AfterRef::Entry(EntryRef::Frn(*volume, *frn))),
                        },
                    }
                }
                Change::Touch { volume, frn, .. } if s.resolve(EntryRef::Frn(*volume, *frn)).is_none() => Pending {
                    action: Action::Create,
                    before: String::new(),
                    after: Some(AfterRef::Entry(EntryRef::Frn(*volume, *frn))),
                },
                Change::Touch { .. } => continue,
                Change::Delete(t) => match s.resolve(*t) {
                    Some(id) => Pending { action: Action::Delete, before: path_of(s, id), after: None },
                    None => continue,
                },
                Change::Rename { target, new_parent, .. } => match s.resolve(*target) {
                    Some(id) => {
                        let moved = s.resolve(*new_parent).is_some_and(|p| p != s.parent(id));
                        Pending {
                            action: if moved { Action::Move } else { Action::Rename },
                            before: path_of(s, id),
                            after: Some(AfterRef::Entry(EntryRef::Id(id))),
                        }
                    }
                    None => continue,
                },
                Change::Meta { target, .. } => {
                    Pending { action: Action::Modify, before: String::new(), after: Some(AfterRef::Entry(*target)) }
                }
                Change::Path { volume, change } => match change {
                    PathChange::Upsert { path, .. } => {
                        let exists = s.resolve_path(*volume, path).is_some();
                        Pending {
                            action: if exists { Action::Modify } else { Action::Create },
                            before: String::new(),
                            after: Some(AfterRef::Path(*volume, path.clone())),
                        }
                    }
                    PathChange::Remove { path } => match s.resolve_path(*volume, path) {
                        Some(id) => Pending { action: Action::Delete, before: path_of(s, id), after: None },
                        None => continue,
                    },
                    PathChange::Rename { from, to } => match s.resolve_path(*volume, from) {
                        Some(id) => {
                            let same_dir = from.split_last().map(|x| x.1) == to.split_last().map(|x| x.1);
                            Pending {
                                action: if same_dir { Action::Rename } else { Action::Move },
                                before: path_of(s, id),
                                after: Some(AfterRef::Path(*volume, to.clone())),
                            }
                        }
                        None => continue,
                    },
                    // Rescans would flood the journal with unchanged entries.
                    PathChange::SyncDir { .. } => continue,
                },
            };
            out.push(p);
        }
        out
    }

    /// Writes the prepared records with their paths after the batch was applied.
    pub fn commit(&mut self, s: &IndexStore, pending: Vec<Pending>) {
        if pending.is_empty() {
            return;
        }
        let time = FileTime::now();
        let Some(out) = self.out.as_mut() else { return };
        for p in pending {
            let after = match &p.after {
                Some(AfterRef::Entry(r)) => s.resolve(*r).map(|id| path_of(s, id)),
                Some(AfterRef::Path(v, rel)) => s.resolve_path(*v, rel).map(|id| path_of(s, id)),
                None => Some(String::new()),
            };
            // Changes that did not apply (unresolved) are not journalled.
            let Some(after) = after else { continue };
            let r = Record { time, action: p.action, before: p.before, after };
            let _ = out.write_all(r.line().as_bytes());
            self.lines += 1;
        }
        let _ = out.flush();
        if self.lines > self.max + self.max / 10 {
            if let Err(e) = self.trim() {
                tracing::warn!(error = %e, "cannot trim the index journal");
            }
            if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
                self.out = Some(std::io::BufWriter::new(f));
            }
        }
    }
}

/// The last `limit` records of `path`, oldest first.
pub fn read(path: &Path, limit: usize) -> Vec<Record> {
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    let mut v: Vec<Record> =
        std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| Record::parse(&l)).collect();
    if v.len() > limit {
        v.drain(..v.len() - limit);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnc_index::{Meta, VolumeInfo, VolumeKind};

    fn u16s(s: &str) -> Box<[u16]> {
        s.encode_utf16().collect()
    }

    #[test]
    fn records_changes_with_paths_and_retention() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("journal.tsv");
        let mut j = Journal::open(file.clone(), 7, 100).unwrap();
        let mut s = IndexStore::default();
        let vol = s.add_volume(VolumeInfo::new(VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
        let dir = Meta::attrs(fnc_core::flags::attr::DIRECTORY);
        let apply = |s: &mut IndexStore, j: &mut Journal, batch: ChangeBatch| {
            let p = Journal::prepare(s, &batch);
            s.apply(batch);
            j.commit(s, p);
        };
        apply(
            &mut s,
            &mut j,
            vec![
                Change::Create { volume: vol, frn: 10, parent: EntryRef::Frn(vol, 5), name: u16s("docs"), meta: dir },
                Change::Create {
                    volume: vol,
                    frn: 11,
                    parent: EntryRef::Frn(vol, 10),
                    name: u16s("a.txt"),
                    meta: Meta::default(),
                },
            ],
        );
        apply(
            &mut s,
            &mut j,
            vec![Change::Rename {
                target: EntryRef::Frn(vol, 11),
                new_name: u16s("b.txt"),
                new_parent: EntryRef::Frn(vol, 10),
            }],
        );
        apply(
            &mut s,
            &mut j,
            vec![Change::Create {
                volume: vol,
                frn: 11,
                parent: EntryRef::Frn(vol, 5),
                name: u16s("b.txt"),
                meta: Meta::default(),
            }],
        );
        apply(
            &mut s,
            &mut j,
            vec![Change::Meta { target: EntryRef::Frn(vol, 11), meta: Meta { size: Some(5), ..Meta::default() } }],
        );
        apply(&mut s, &mut j, vec![Change::Delete(EntryRef::Frn(vol, 11)), Change::Delete(EntryRef::Frn(vol, 999))]);
        let r = read(&file, 100);
        let got: Vec<(Action, &str, &str)> =
            r.iter().map(|r| (r.action, r.before.as_str(), r.after.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Action::Create, "", "C:\\docs"),
                (Action::Create, "", "C:\\docs\\a.txt"),
                (Action::Rename, "C:\\docs\\a.txt", "C:\\docs\\b.txt"),
                (Action::Move, "C:\\docs\\b.txt", "C:\\b.txt"),
                (Action::Modify, "", "C:\\b.txt"),
                (Action::Delete, "C:\\b.txt", ""),
            ]
        );
        // Old records are dropped on open; the newest `max` are kept.
        let old = Record {
            time: FileTime(FileTime::now().0 - 30 * 86_400 * 10_000_000),
            action: Action::Create,
            before: String::new(),
            after: "x".into(),
        };
        let mut text = old.line();
        text.push_str(&std::fs::read_to_string(&file).unwrap());
        std::fs::write(&file, text).unwrap();
        drop(j);
        let _j = Journal::open(file.clone(), 7, 3).unwrap();
        let r = read(&file, 100);
        assert_eq!(r.len(), 3);
        assert_eq!(r[2].action, Action::Delete);
    }
}

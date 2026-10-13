//! Undo system (SPEC ch.3 §10): file operations started in Fennec record how to reverse
//! them; Ctrl+Z / Ctrl+Y run the reverse / the original again. Renames are reversed by
//! renaming back, recycled items are restored from the recycle bin, created folders go
//! to the recycle bin. Permanent deletions and moves/copies through the shell clipboard
//! or drag and drop (whose results the shell does not report) are not recorded.

use std::io;
use std::path::{Path, PathBuf};

use fnc_platform::fileops::{self, Owner, Recycled};

/// One reversible step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Renamed in place.
    Rename {
        /// Before.
        from: PathBuf,
        /// After.
        to: PathBuf,
    },
    /// Moved to the recycle bin.
    Recycle(Recycled),
    /// Folder created.
    CreateFolder(PathBuf),
}

/// A user action: one or more steps, undone together (in reverse order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// Shown in the Edit menu ("Undo rename").
    pub label: String,
    /// Steps in the order they ran.
    pub ops: Vec<Op>,
}

/// Undo and redo stacks.
#[derive(Debug)]
pub struct UndoStack {
    done: Vec<Action>,
    undone: Vec<Action>,
    max: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self { done: Vec::new(), undone: Vec::new(), max: 100 }
    }
}

impl UndoStack {
    /// Records a completed action (clears the redo stack).
    pub fn push(&mut self, a: Action) {
        if a.ops.is_empty() {
            return;
        }
        self.done.push(a);
        if self.done.len() > self.max {
            self.done.remove(0);
        }
        self.undone.clear();
    }

    /// The action Ctrl+Z would undo.
    pub fn next_undo(&self) -> Option<&Action> {
        self.done.last()
    }

    /// The action Ctrl+Y would redo.
    pub fn next_redo(&self) -> Option<&Action> {
        self.undone.last()
    }

    /// Takes the action to undo.
    pub fn take_undo(&mut self) -> Option<Action> {
        self.done.pop()
    }

    /// Takes the action to redo.
    pub fn take_redo(&mut self) -> Option<Action> {
        self.undone.pop()
    }

    /// Records that `a` was undone (it can be redone).
    pub fn undone(&mut self, a: Action) {
        self.undone.push(a);
    }

    /// Records that `a` was redone (it can be undone again).
    pub fn redone(&mut self, a: Action) {
        self.done.push(a);
    }
}

fn name_of(p: &Path) -> io::Result<String> {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))
}

fn parent_of(p: &Path) -> io::Result<&Path> {
    p.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent folder"))
}

/// Reverses `a`; returns the action to redo (recycle-bin locations change on redo).
pub fn undo(owner: Owner, a: &Action) -> io::Result<Action> {
    for op in a.ops.iter().rev() {
        match op {
            Op::Rename { from, to } => fileops::rename(owner, to, &name_of(from)?)?,
            Op::Recycle(r) => fileops::restore_from_recycle_bin(owner, std::slice::from_ref(r))?,
            Op::CreateFolder(p) => {
                fileops::delete_to_recycle_bin(owner, std::slice::from_ref(p))?;
            }
        }
    }
    Ok(a.clone())
}

/// Runs `a` again; returns the action as it now stands (new recycle-bin locations).
pub fn redo(owner: Owner, a: &Action) -> io::Result<Action> {
    let mut ops = Vec::with_capacity(a.ops.len());
    for op in &a.ops {
        match op {
            Op::Rename { from, to } => {
                fileops::rename(owner, from, &name_of(to)?)?;
                ops.push(op.clone());
            }
            Op::Recycle(r) => {
                let again = fileops::delete_to_recycle_bin(owner, std::slice::from_ref(&r.original))?;
                ops.extend(again.into_iter().map(Op::Recycle));
            }
            Op::CreateFolder(p) => {
                fileops::new_folder(owner, parent_of(p)?, &name_of(p)?)?;
                ops.push(op.clone());
            }
        }
    }
    Ok(Action { label: a.label.clone(), ops })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_order_and_redo_clearing() {
        let a = |l: &str| Action { label: l.into(), ops: vec![Op::CreateFolder(PathBuf::from(l))] };
        let mut s = UndoStack::default();
        s.push(a("1"));
        s.push(a("2"));
        assert_eq!(s.next_undo().map(|x| x.label.as_str()), Some("2"));
        let two = s.take_undo().unwrap();
        s.undone(two);
        assert_eq!(s.next_redo().map(|x| x.label.as_str()), Some("2"));
        s.push(a("3"));
        assert!(s.next_redo().is_none(), "a new action clears redo");
        s.push(Action { label: "empty".into(), ops: Vec::new() });
        assert_eq!(s.next_undo().map(|x| x.label.as_str()), Some("3"));
    }

    /// Real files: rename, folder creation and recycling are undone and redone (the
    /// recycle bin is the user's freedesktop trash; the item is restored by the test).
    #[test]
    #[cfg(not(windows))]
    fn rename_folder_and_recycle_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let o = Owner(0);
        let a = d.path().join("a.txt");
        std::fs::write(&a, "x").unwrap();
        // Rename.
        let b = d.path().join("b.txt");
        fileops::rename(o, &a, "b.txt").unwrap();
        let act = Action { label: "rename".into(), ops: vec![Op::Rename { from: a.clone(), to: b.clone() }] };
        let back = undo(o, &act).unwrap();
        assert!(a.exists() && !b.exists());
        redo(o, &back).unwrap();
        assert!(b.exists() && !a.exists());
        // New folder.
        let f = d.path().join("new");
        fileops::new_folder(o, d.path(), "new").unwrap();
        let act = Action { label: "new folder".into(), ops: vec![Op::CreateFolder(f.clone())] };
        undo(o, &act).unwrap();
        assert!(!f.exists());
        redo(o, &act).unwrap();
        assert!(f.is_dir());
        // Recycle: undo restores, redo recycles again (new location).
        let recycled = fileops::delete_to_recycle_bin(o, std::slice::from_ref(&b)).unwrap();
        assert!(!b.exists() && recycled.len() == 1);
        let act = Action { label: "delete".into(), ops: recycled.into_iter().map(Op::Recycle).collect() };
        let again = undo(o, &act).unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "x");
        let redone = redo(o, &again).unwrap();
        assert!(!b.exists());
        undo(o, &redone).unwrap();
        assert!(b.exists());
    }
}

//! Folding a run of USN records into index changes (SPEC ch.1 §2.2).
//!
//! Within one batch, records are merged per FRN:
//! * create + delete cancel out;
//! * `RENAME_NEW_NAME` becomes an upsert of the entry at its new parent/name (creating
//!   it if the index never saw it), `RENAME_OLD_NAME` is ignored;
//! * data/attribute/hard-link records become `Change::Touch` ("this name exists"),
//!   which never moves an entry: such records may name the file by any of its hard
//!   links;
//!   SPEC-GAP: removing one name of a multi-link file (`HARD_LINK_CHANGE`) is not
//!   detected; the stale alias stays until the file is deleted or the index rebuilt;
//! * size/date changes only request a metadata refresh when the handle is closed
//!   (`USN_REASON_CLOSE`), so a file written in many chunks is re-read once.
//!
//! Output order follows the first appearance of each FRN, so parents created in the
//! batch precede their children.

use std::collections::HashMap;

use fnc_core::VolumeId;
use fnc_index::{Change, ChangeBatch, EntryRef, Meta};

use crate::record::{UsnRecord, reason};

#[derive(Debug)]
enum Slot {
    /// `structural`: a create/rename was seen (may move the entry); otherwise the
    /// records only prove that the name exists (see `Change::Touch`).
    Upsert {
        parent: u64,
        name: Vec<u16>,
        attributes: u32,
        created: bool,
        structural: bool,
    },
    Delete,
    Removed,
}

/// Result of folding.
#[derive(Debug, Default)]
pub struct Folded {
    /// Structural and attribute changes, in application order.
    pub changes: ChangeBatch,
    /// FRNs whose size/dates must be re-read after `changes` are applied.
    pub refresh: Vec<u64>,
    /// Highest USN seen.
    pub last_usn: i64,
}

/// Folds records (in USN order) for `volume`.
pub fn fold_records<'a>(volume: VolumeId, records: impl IntoIterator<Item = UsnRecord<'a>>) -> Folded {
    let mut slots: Vec<(u64, Slot)> = Vec::new();
    let mut by_frn: HashMap<u64, usize> = HashMap::new();
    let mut refresh: Vec<u64> = Vec::new();
    let mut refresh_set: HashMap<u64, ()> = HashMap::new();
    let mut last_usn = 0i64;
    let mut name = Vec::new();
    for r in records {
        last_usn = last_usn.max(r.usn);
        let frn = r.frn;
        let existing = by_frn.get(&frn).copied();
        if r.reason & reason::FILE_DELETE != 0 {
            // A deleted file needs no metadata refresh.
            if refresh_set.remove(&frn).is_some() {
                refresh.retain(|&f| f != frn);
            }
            match existing {
                Some(i) if matches!(slots[i].1, Slot::Upsert { created: true, .. }) => {
                    slots[i].1 = Slot::Removed;
                    by_frn.remove(&frn);
                }
                Some(i) => slots[i].1 = Slot::Delete,
                None => {
                    by_frn.insert(frn, slots.len());
                    slots.push((frn, Slot::Delete));
                }
            }
            continue;
        }
        if r.reason & reason::RENAME_OLD_NAME != 0 && r.reason & (reason::RENAME_NEW_NAME | reason::FILE_CREATE) == 0 {
            continue;
        }
        // Every other record carries the entry's current name and parent, so it becomes
        // an upsert: entries the index missed (e.g. modified while the MFT was being
        // enumerated) heal themselves, and known entries just get their attributes.
        let structural = r.reason & (reason::RENAME_NEW_NAME | reason::FILE_CREATE) != 0;
        let created = r.reason & reason::FILE_CREATE != 0;
        r.name_into(&mut name);
        match existing.map(|i| &mut slots[i].1) {
            Some(Slot::Upsert { parent, name: n, attributes, created: c, structural: st }) => {
                // A data record after a create/rename may use another hard link's name:
                // it must not change the name the rename established.
                if structural || !*st {
                    *parent = r.parent_frn;
                    n.clone_from(&name);
                }
                *attributes = r.attributes;
                *c |= created;
                *st |= structural;
            }
            // Only a create/rename revives an FRN deleted earlier in the batch.
            Some(Slot::Delete) if !structural => {}
            Some(slot @ Slot::Delete) => {
                *slot = Slot::Upsert {
                    parent: r.parent_frn,
                    name: name.clone(),
                    attributes: r.attributes,
                    created,
                    structural,
                };
            }
            Some(Slot::Removed) | None => {
                by_frn.insert(frn, slots.len());
                slots.push((
                    frn,
                    Slot::Upsert {
                        parent: r.parent_frn,
                        name: name.clone(),
                        attributes: r.attributes,
                        created,
                        structural,
                    },
                ));
            }
        }
        if created && refresh_set.insert(frn, ()).is_none() {
            refresh.push(frn);
        }
        if r.reason & reason::CLOSE != 0 && r.reason & reason::METADATA != 0 && refresh_set.insert(frn, ()).is_none() {
            refresh.push(frn);
        }
    }
    let changes = slots
        .into_iter()
        .filter_map(|(frn, slot)| match slot {
            Slot::Upsert { parent, name, attributes, structural: true, .. } => Some(Change::Create {
                volume,
                frn,
                parent: EntryRef::Frn(volume, parent),
                name: name.into_boxed_slice(),
                meta: Meta::attrs(attributes),
            }),
            // Data/attribute/hard-link records: the name exists, but they must not move
            // the entry (the record may use another hard link's name).
            Slot::Upsert { parent, name, attributes, structural: false, .. } => Some(Change::Touch {
                volume,
                frn,
                parent: EntryRef::Frn(volume, parent),
                name: name.into_boxed_slice(),
                meta: Meta::attrs(attributes),
            }),
            Slot::Delete => Some(Change::Delete(EntryRef::Frn(volume, frn))),
            Slot::Removed => None,
        })
        .collect();
    Folded { changes, refresh, last_usn }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{encode_v2, records};

    const V: VolumeId = VolumeId(0);

    fn fold(recs: &[Vec<u8>]) -> Folded {
        let buf: Vec<u8> = recs.concat();
        fold_records(V, records(&buf).map(Result::unwrap).collect::<Vec<_>>())
    }

    fn name(c: &Change) -> String {
        match c {
            Change::Create { name, .. } | Change::Touch { name, .. } => String::from_utf16_lossy(name),
            _ => String::new(),
        }
    }

    #[test]
    fn create_then_close_is_one_upsert_and_refresh() {
        let f = fold(&[
            encode_v2(20, 5, 1, reason::FILE_CREATE, 0x20, "a.txt"),
            encode_v2(20, 5, 2, reason::FILE_CREATE | reason::DATA_EXTEND, 0x20, "a.txt"),
            encode_v2(20, 5, 3, reason::FILE_CREATE | reason::DATA_EXTEND | reason::CLOSE, 0x20, "a.txt"),
        ]);
        assert_eq!(f.changes.len(), 1);
        assert_eq!(name(&f.changes[0]), "a.txt");
        assert_eq!(f.refresh, vec![20]);
        assert_eq!(f.last_usn, 3);
    }

    #[test]
    fn create_and_delete_cancel() {
        let f = fold(&[
            encode_v2(20, 5, 1, reason::FILE_CREATE, 0x20, "tmp.~"),
            encode_v2(20, 5, 2, reason::FILE_CREATE | reason::FILE_DELETE | reason::CLOSE, 0x20, "tmp.~"),
        ]);
        assert!(f.changes.is_empty());
        assert!(f.refresh.is_empty());
    }

    #[test]
    fn rename_pair_becomes_upsert_at_new_location() {
        let f = fold(&[
            encode_v2(30, 5, 1, reason::RENAME_OLD_NAME, 0x10, "t"),
            encode_v2(30, 7, 2, reason::RENAME_NEW_NAME, 0x10, "u"),
            encode_v2(30, 7, 3, reason::RENAME_NEW_NAME | reason::CLOSE, 0x10, "u"),
        ]);
        assert_eq!(f.changes.len(), 1);
        match &f.changes[0] {
            Change::Create { frn, parent, name, .. } => {
                assert_eq!(*frn, 30);
                assert_eq!(*parent, EntryRef::Frn(V, 7));
                assert_eq!(String::from_utf16_lossy(name), "u");
            }
            other => panic!("{other:?}"),
        }
        assert!(f.refresh.is_empty(), "a rename does not change size or dates");
    }

    #[test]
    fn modify_requests_refresh_only_on_close_and_delete_wins() {
        let f = fold(&[encode_v2(40, 5, 1, reason::DATA_EXTEND, 0x20, "log.txt")]);
        assert!(f.refresh.is_empty());
        // A modification is an upsert (heals entries the index missed).
        assert_eq!(name(&f.changes[0]), "log.txt");
        let f = fold(&[encode_v2(40, 5, 1, reason::DATA_EXTEND | reason::CLOSE, 0x20, "log.txt")]);
        assert_eq!(f.refresh, vec![40]);
        let f = fold(&[
            encode_v2(40, 5, 1, reason::DATA_EXTEND | reason::CLOSE, 0x20, "log.txt"),
            encode_v2(40, 5, 2, reason::FILE_DELETE | reason::CLOSE, 0x20, "log.txt"),
        ]);
        assert!(matches!(f.changes[0], Change::Delete(EntryRef::Frn(_, 40))));
        assert_eq!(f.changes.len(), 1);
        assert!(f.refresh.is_empty(), "no refresh for a deleted file");
    }

    #[test]
    fn move_into_folder_created_later_in_batch() {
        // File 61 is created at the root, folder 60 is created, then 61 moves into 60:
        // the upsert of 61 precedes 60 in the output, the index resolves it on retry.
        let mut s = fnc_index::IndexStore::default();
        let vol = s.add_volume(fnc_index::VolumeInfo::new(fnc_index::VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
        let f = fold(&[
            encode_v2(61, 5, 1, reason::FILE_CREATE | reason::CLOSE, 0x20, "a.txt"),
            encode_v2(60, 5, 2, reason::FILE_CREATE, 0x10, "dir"),
            encode_v2(61, 5, 3, reason::RENAME_OLD_NAME, 0x20, "a.txt"),
            encode_v2(61, 60, 4, reason::RENAME_NEW_NAME, 0x20, "a.txt"),
        ]);
        let st = s.apply(f.changes);
        assert_eq!((st.created, st.unresolved), (2, 0));
        let id = s.resolve(EntryRef::Frn(vol, 61)).unwrap();
        let mut p = String::new();
        s.full_path(id, &mut p);
        assert_eq!(p, "C:\\dir\\a.txt");
    }

    #[test]
    fn order_of_first_appearance_is_kept() {
        let f = fold(&[
            encode_v2(50, 5, 1, reason::FILE_CREATE, 0x10, "dir"),
            encode_v2(51, 50, 2, reason::FILE_CREATE | reason::CLOSE, 0x20, "child"),
            encode_v2(50, 5, 3, reason::RENAME_NEW_NAME, 0x10, "dir2"),
        ]);
        assert_eq!(f.changes.iter().map(name).collect::<Vec<_>>(), vec!["dir2", "child"]);
    }

    #[test]
    fn modification_of_unknown_entry_heals_the_index() {
        // The entry was written while the MFT was enumerated and missed; its later
        // DATA_EXTEND record carries name + parent, so it is inserted.
        let mut s = fnc_index::IndexStore::default();
        let vol = s.add_volume(fnc_index::VolumeInfo::new(fnc_index::VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
        let f = fold(&[encode_v2(70, 5, 1, reason::DATA_EXTEND | reason::CLOSE, 0x20, "missed.log")]);
        let st = s.apply(f.changes);
        assert_eq!(st.created, 1);
        assert!(s.resolve(EntryRef::Frn(vol, 70)).is_some());
        // Applying the same modification again is idempotent.
        let f = fold(&[encode_v2(70, 5, 2, reason::DATA_EXTEND | reason::CLOSE, 0x20, "missed.log")]);
        let st = s.apply(f.changes);
        assert_eq!((st.created, s.live_count()), (0, 2));
    }

    #[test]
    fn data_record_via_hard_link_does_not_move_the_file() {
        let mut s = fnc_index::IndexStore::default();
        let vol = s.add_volume(fnc_index::VolumeInfo::new(fnc_index::VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
        s.apply(
            fold(&[
                encode_v2(80, 5, 1, reason::FILE_CREATE, 0x10, "WinSxS"),
                encode_v2(81, 5, 2, reason::FILE_CREATE, 0x10, "System32"),
                encode_v2(82, 80, 3, reason::FILE_CREATE | reason::CLOSE, 0x20, "kernel32.dll"),
            ])
            .changes,
        );
        // The same file written through its System32 hard link.
        let st = s
            .apply(fold(&[encode_v2(82, 81, 4, reason::DATA_OVERWRITE | reason::CLOSE, 0x20, "kernel32.dll")]).changes);
        assert_eq!(st.created, 1, "the System32 name is added as an alias");
        let primary = s.resolve(EntryRef::Frn(vol, 82)).unwrap();
        let mut p = String::new();
        s.full_path(primary, &mut p);
        assert_eq!(p, "C:\\WinSxS\\kernel32.dll");
        assert_eq!(s.hardlink_count(primary), 2);
        // A rename followed by a data record using another link keeps the new name.
        let f = fold(&[
            encode_v2(82, 80, 5, reason::RENAME_NEW_NAME, 0x20, "k32.dll"),
            encode_v2(82, 81, 6, reason::DATA_OVERWRITE, 0x20, "kernel32.dll"),
        ]);
        assert!(matches!(&f.changes[0], Change::Create { name, .. } if String::from_utf16_lossy(name) == "k32.dll"));
    }

    #[test]
    fn applies_to_index() {
        let mut s = fnc_index::IndexStore::default();
        let vol = s.add_volume(fnc_index::VolumeInfo::new(fnc_index::VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
        let f = fold(&[
            encode_v2(60, 5, 1, reason::FILE_CREATE, 0x10, "t"),
            encode_v2(61, 60, 2, reason::FILE_CREATE | reason::CLOSE, 0x20, "a.txt"),
        ]);
        let st = s.apply(f.changes);
        assert_eq!(st.created, 2);
        let id = s.resolve(EntryRef::Frn(vol, 61)).unwrap();
        let mut p = String::new();
        s.full_path(id, &mut p);
        assert_eq!(p, "C:\\t\\a.txt");
        let f = fold(&[
            encode_v2(60, 5, 3, reason::RENAME_OLD_NAME, 0x10, "t"),
            encode_v2(60, 5, 4, reason::RENAME_NEW_NAME, 0x10, "u"),
        ]);
        s.apply(f.changes);
        s.full_path(id, &mut p);
        assert_eq!(p, "C:\\u\\a.txt");
    }
}

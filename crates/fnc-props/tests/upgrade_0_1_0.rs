//! Files written by Fennec 0.1.0 (`tests/fixtures/0.1.0`, generated with the 0.1.0
//! sources) load in this version unchanged (REL-03, ADR-038).

use std::path::PathBuf;

use fnc_core::EntryId;
use fnc_index::EntryRef;
use fnc_props::content::ContentStore;
use fnc_props::hash::{HashAlgo, HashCache, HashKey};
use fnc_props::{PropertyId, PropertyStore, Value};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/0.1.0").join(name)
}

#[test]
fn stores_written_by_0_1_0_reattach() {
    let index = fnc_index::persist::load(&fixture("index.fnx")).expect("0.1.0 index.fnx loads");
    let vol = index.volumes()[0].id;
    let id = |frn: u64| -> EntryId { index.resolve(EntryRef::Frn(vol, frn)).expect("entry") };
    let (photo, notes) = (id(12), id(13));
    assert_eq!(index.run_info(photo).count, 2, "run counts kept");

    let props = PropertyStore::load(&fixture("props.fnx"), &index).expect("0.1.0 props.fnx loads");
    assert_eq!(props.get(photo, PropertyId::WIDTH), Some(Value::Int(4000)));
    assert_eq!(props.get(photo, PropertyId::HEIGHT), Some(Value::Int(3000)));
    assert!(props.is_indexed(PropertyId::WIDTH) && props.is_indexed(PropertyId::HEIGHT));

    let content = ContentStore::load(&fixture("content.fnx"), &index).expect("0.1.0 content.fnx loads");
    assert_eq!(content.text(notes).as_deref(), Some("The quarterly plan for 2026"));
    assert_eq!(content.search_text("quarterly").len(), 1);

    let hashes = HashCache::load(&fixture("hashes.fnx")).expect("0.1.0 hashes.fnx loads");
    let key = HashKey { volume: 1, file: 2, size: 3, dm: 4 };
    assert_eq!(hashes.get(&key, HashAlgo::Sha256).as_deref(), Some(&[7u8; 32][..]));
}

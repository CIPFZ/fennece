//! The appendix-B property table through the query pipeline (SPEC ch.2 §5, appendix B):
//! every property name parses and plans as a property function, and a sample of the new
//! properties compares by value type and unit, with named values and `prop:unknown`.

// Test-only helpers outside `#[test]` functions.
#![allow(clippy::unwrap_used)]

use fnc_core::{EntryId, FileTime};
use fnc_exec::{Caches, CancelToken, ExecCtx, ExtCtx, SlowPolicy, execute_ext};
use fnc_index::IndexStore;
use fnc_props::{PropType, PropertyId as P, PropertyStore, Value};
use fnc_query::{FuncId, Node, ParseEnv, PlanEnv, Pred, parse, plan};

fn store() -> IndexStore {
    let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
    for n in ["a.jpg", "b.jpg", "c.jpg", "mail.eml", "contact.vcf", "song.mp3", "deck.ppt"] {
        efu.push_str(&format!("\"C:\\t\\{n}\",10,5,,32\n"));
    }
    let mut s = IndexStore::default();
    fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
    s.rebuild_name_order();
    s
}

fn id(s: &IndexStore, name: &str) -> EntryId {
    (0..s.len() as u32).map(EntryId).find(|&i| s.name_string(i) == name).unwrap()
}

/// Noon UTC of a civil date (so the local date is the same in every time zone).
fn date(y: i64, m: i64, d: i64) -> Value {
    // Days from 1970-01-01 (H. Hinnant's `days_from_civil`).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Value::Date(FileTime::from_unix(days * 86_400 + 12 * 3600, 0))
}

fn props(s: &IndexStore) -> PropertyStore {
    let mut p = PropertyStore::new(s.instance());
    p.set_indexed(&[
        P::LENS_MODEL,
        P::FOCAL_LENGTH_35MM,
        P::EXPOSURE_BIAS,
        P::METERING_MODE,
        P::WHITE_BALANCE,
        P::GPS_IMG_DIRECTION,
        P::SUBJECT_DISTANCE,
        P::FROM,
        P::TO,
        P::DATE_RECEIVED,
        P::HAS_ATTACHMENTS,
        P::FIRST_NAME,
        P::BIRTHDAY,
        P::PART_OF_SET,
        P::ORIGINAL_RELEASE_YEAR,
        P::IS_COMPILATION,
        P::HIDDEN_SLIDE_COUNT,
        P::PERCEIVED_TYPE,
        P::OWNER,
    ]);
    p.insert(
        id(s, "a.jpg"),
        vec![
            (P::LENS_MODEL, Value::Text("RF24-105mm F4 L IS USM".into())),
            (P::FOCAL_LENGTH_35MM, Value::Int(50)),
            (P::EXPOSURE_BIAS, Value::Float(-0.7)),
            (P::METERING_MODE, Value::Int(3)),
            (P::WHITE_BALANCE, Value::Int(0)),
            (P::GPS_IMG_DIRECTION, Value::Float(271.5)),
            (P::SUBJECT_DISTANCE, Value::Float(2.4)),
            (P::PERCEIVED_TYPE, Value::Int(2)),
            (P::OWNER, Value::Text("FENNEC\\alice".into())),
        ],
    );
    p.insert(
        id(s, "b.jpg"),
        vec![
            (P::FOCAL_LENGTH_35MM, Value::Int(200)),
            (P::METERING_MODE, Value::Int(5)),
            (P::WHITE_BALANCE, Value::Int(1)),
            (P::PERCEIVED_TYPE, Value::Int(2)),
        ],
    );
    p.insert(
        id(s, "mail.eml"),
        vec![
            (P::FROM, Value::Text("Alice Smith".into())),
            (P::TO, Value::multi(["Bob Jones", "Carol White"]).unwrap()),
            (P::DATE_RECEIVED, date(2026, 3, 14)),
            (P::HAS_ATTACHMENTS, Value::Int(1)),
        ],
    );
    p.insert(id(s, "contact.vcf"), vec![(P::FIRST_NAME, Value::Text("Dana".into())), (P::BIRTHDAY, date(1990, 7, 1))]);
    p.insert(
        id(s, "song.mp3"),
        vec![
            (P::PART_OF_SET, Value::Text("1/2".into())),
            (P::ORIGINAL_RELEASE_YEAR, Value::Int(1969)),
            (P::IS_COMPILATION, Value::Int(0)),
        ],
    );
    p.insert(id(s, "deck.ppt"), vec![(P::HIDDEN_SLIDE_COUNT, Value::Int(3))]);
    // c.jpg is not extracted: every property is unknown.
    p
}

fn names(s: &IndexStore, p: &PropertyStore, q: &str) -> Vec<String> {
    let env = PlanEnv { indexed_props: p.indexed().to_vec(), ..PlanEnv::default() };
    let planned = plan(&parse(q, &ParseEnv::default()).ast, &env, None);
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let ext = ExtCtx {
        props: Some(p),
        content: None,
        hashes: None,
        on_demand: None,
        policy: SlowPolicy::default(),
        semantic: None,
        system_index: None,
    };
    let rs = execute_ext(&planned, s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }, &ext).unwrap();
    let mut v: Vec<String> = rs.ids.iter().map(|&i| s.name_string(i)).collect();
    v.sort();
    v
}

#[test]
fn every_property_name_plans_as_a_property_function() {
    assert!(P::count() >= 290);
    let env = PlanEnv::default();
    for d in fnc_props::PROPERTIES {
        for n in std::iter::once(&d.canonical).chain(d.aliases) {
            for q in [format!("{n}:unknown"), format!("{n}:x"), format!("{}:", n.replace('-', ""))] {
                let parsed = parse(&q, &ParseEnv::default());
                assert!(parsed.diagnostics.is_empty(), "{q}: {:?}", parsed.diagnostics);
                match plan(&parsed.ast, &env, None).root {
                    Node::Leaf(Pred::Prop(pp)) => assert_eq!(pp.id, d.id, "{q}"),
                    // `x` is no value of numeric and date properties: invalid (matches nothing).
                    Node::Leaf(Pred::Invalid(_)) => {
                        assert!(q.ends_with(":x") && (d.ty != PropType::Text || d.id == P::DIMENSIONS), "{q}")
                    }
                    other => panic!("{q}: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn new_properties_compare_by_type_and_unit() {
    let s = store();
    let p = props(&s);
    let q = |q: &str| names(&s, &p, q);
    // Text (substring), multi-text (any item), case-insensitive.
    assert_eq!(q("lens-model:rf24"), ["a.jpg"]);
    assert_eq!(q("lensmodel:\"F4 L\""), ["a.jpg"]);
    assert_eq!(q("from:alice"), ["mail.eml"]);
    assert_eq!(q("to:carol"), ["mail.eml"]);
    assert_eq!(q("message-to:bob"), ["mail.eml"]);
    assert_eq!(q("count:to:2"), ["mail.eml"]);
    assert_eq!(q("contact-first-name:dana"), ["contact.vcf"]);
    assert_eq!(q("owner:alice"), ["a.jpg"]);
    assert_eq!(q("part-of-set:1/2"), ["song.mp3"]);
    // Integers with units (`mm`), ranges and comparisons.
    assert_eq!(q("focal-length-35mm:50"), ["a.jpg"]);
    assert_eq!(q("35mm-focal-length:>=100mm"), ["b.jpg"]);
    assert_eq!(q("focal-length-in-film:40..300"), ["a.jpg", "b.jpg"]);
    assert_eq!(q("original-release-year:<1970"), ["song.mp3"]);
    assert_eq!(q("hidden-slides:>0"), ["deck.ppt"]);
    // Floats (signed, degrees, metres): equality at the written precision.
    assert_eq!(q("exposure-bias:-0.7"), ["a.jpg"]);
    assert_eq!(q("exposure-bias:<0"), ["a.jpg"]);
    assert_eq!(q("gps-img-direction:>270°"), ["a.jpg"]);
    assert_eq!(q("subject-distance:2.4m"), ["a.jpg"]);
    // Named values of enumerations.
    assert_eq!(q("metering-mode:spot"), ["a.jpg"]);
    assert_eq!(q("metering-mode:pattern"), ["b.jpg"]);
    assert_eq!(q("white-balance:manual"), ["b.jpg"]);
    assert_eq!(q("perceived-type:image"), ["a.jpg", "b.jpg"]);
    // Dates.
    assert_eq!(q("date-received:2026-03"), ["mail.eml"]);
    assert_eq!(q("message-date-received:>2025"), ["mail.eml"]);
    assert_eq!(q("birthday:1990"), ["contact.vcf"]);
    // Booleans.
    assert_eq!(q("has-attachments:yes"), ["mail.eml"]);
    assert_eq!(q("is-compilation:no"), ["song.mp3"]);
    assert_eq!(q("is-compilation:yes"), Vec::<String>::new());
    // `prop:unknown`: not extracted (c.jpg) or extracted without the property.
    let unknown = q("lens-model:unknown");
    assert!(unknown.contains(&"c.jpg".into()) && unknown.contains(&"b.jpg".into()), "{unknown:?}");
    assert!(!unknown.contains(&"a.jpg".into()));
    assert_eq!(q("ext:jpg !metering-mode:unknown"), ["a.jpg", "b.jpg"]);
    // `prop:` = has a (non-empty, non-zero) value.
    assert_eq!(q("lens-model:"), ["a.jpg"]);
    assert_eq!(q("white-balance:"), ["b.jpg"]);
}

#[test]
fn display_of_new_properties() {
    assert_eq!(P::METERING_MODE.format(&Value::Int(3)), "spot");
    assert_eq!(P::FOCAL_LENGTH_35MM.format(&Value::Int(50)), "50 mm");
    assert_eq!(P::GPS_IMG_DIRECTION.format(&Value::Float(271.5)), "271.5°");
    assert_eq!(P::HAS_ATTACHMENTS.format(&Value::Int(1)), "Yes");
    assert_eq!(P::SUBJECT_DISTANCE.format(&Value::Float(2.4)), "2.4 m");
    assert_eq!(FuncId::Prop(P::LENS_MAKER).aliases(), ["lens-make", "lens-manufacturer"]);
}

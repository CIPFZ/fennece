//! `eval:` in execution: a formula evaluated per candidate, with the entry's own fields
//! read from the index and property variables from the property store (values collected
//! when the query is compiled, so evaluation needs no store lock).

use std::collections::HashMap;
use std::sync::Arc;

use fnc_core::EntryId;
use fnc_index::IndexStore;
use fnc_props::{PropertyId, PropertyStore, Value};
use fnc_query::formula::{Formula, Val, Vars};

use crate::eval::Scratch;

/// Compiled `eval:` leaf.
pub struct EvalLeaf {
    formula: Arc<Formula>,
    /// Property variables: name → (entry → value).
    props: HashMap<String, HashMap<u32, Val>>,
}

/// A property value as a formula value.
pub fn prop_val(v: Value) -> Val {
    match v {
        Value::Int(i) => Val::Num(i as f64),
        Value::Float(f) => Val::Num(f),
        Value::Text(t) => Val::Text(t.into()),
        Value::Date(d) => Val::Date(d.0),
        Value::Duration(ms) => Val::Num(ms as f64 / 1000.0),
        Value::MultiText(v) => Val::Text(v.join("; ")),
    }
}

/// Names the index answers without the property store.
const ENTRY_VARS: &[&str] = &[
    "name",
    "stem",
    "ext",
    "extension",
    "path",
    "parent",
    "fullpath",
    "size",
    "dm",
    "datemodified",
    "dc",
    "datecreated",
    "da",
    "dateaccessed",
    "dr",
    "daterun",
    "rc",
    "daterecentlychanged",
    "attrib",
    "attributes",
    "depth",
    "runcount",
    "namelength",
    "isfolder",
    "isdir",
];

impl EvalLeaf {
    /// Compiles `f`; property variables are read from `props` (if indexed).
    pub fn new(f: &Arc<Formula>, s: &IndexStore, props: Option<&PropertyStore>) -> Self {
        let mut map = HashMap::new();
        for v in f.vars() {
            if ENTRY_VARS.contains(&v.as_str()) {
                continue;
            }
            let mut values = HashMap::new();
            if let (Some(p), Some(store)) = (PropertyId::lookup(&v), props) {
                for i in 0..s.len() as u32 {
                    if let Some(val) = store.get(EntryId(i), p) {
                        values.insert(i, prop_val(val));
                    }
                }
            }
            map.insert(v, values);
        }
        Self { formula: f.clone(), props: map }
    }

    /// `true` if the formula is true for `id`.
    pub fn eval(&self, s: &IndexStore, id: EntryId, _sc: &mut Scratch) -> bool {
        self.formula.eval(&EntryVars { s, id, props: &self.props }).truthy()
    }
}

/// Variables of one entry (also used by formula columns).
pub struct EntryVars<'a> {
    /// Index.
    pub s: &'a IndexStore,
    /// Entry.
    pub id: EntryId,
    /// Property variables collected beforehand.
    pub props: &'a HashMap<String, HashMap<u32, Val>>,
}

impl Vars for EntryVars<'_> {
    fn var(&self, name: &str) -> Val {
        entry_var(self.s, self.id, name).unwrap_or_else(|| match self.props.get(name) {
            Some(m) => m.get(&self.id.0).cloned().unwrap_or(Val::Empty),
            None => Val::Error("#NAME?"),
        })
    }
}

/// Variables of one entry with property variables read from the store directly (formula
/// columns: one entry at a time).
pub struct StoreVars<'a> {
    /// Index.
    pub s: &'a IndexStore,
    /// Entry.
    pub id: EntryId,
    /// Property store, if any.
    pub props: Option<&'a PropertyStore>,
}

impl Vars for StoreVars<'_> {
    fn var(&self, name: &str) -> Val {
        entry_var(self.s, self.id, name).unwrap_or_else(|| match PropertyId::lookup(name) {
            Some(p) => self.props.and_then(|st| st.get(self.id, p)).map_or(Val::Empty, prop_val),
            None => Val::Error("#NAME?"),
        })
    }
}

/// The entry's own fields (`None` for property and unknown names).
fn entry_var(s: &IndexStore, id: EntryId, name: &str) -> Option<Val> {
    let date = |t: fnc_core::FileTime| if t.0 == 0 { Val::Empty } else { Val::Date(t.0) };
    let name_s = || s.name_string(id);
    Some(match name {
        "name" => Val::Text(name_s()),
        "stem" => {
            let n = name_s();
            Val::Text(if s.is_dir(id) { n } else { n.rsplit_once('.').map_or(n.clone(), |(a, _)| a.to_string()) })
        }
        "ext" | "extension" => Val::Text(if s.is_dir(id) {
            String::new()
        } else {
            name_s().rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default()
        }),
        "path" => {
            let mut p = String::new();
            s.parent_path(id, &mut p);
            Val::Text(p)
        }
        "parent" => {
            let p = s.parent(id);
            Val::Text(if p.is_some() { s.name_string(p) } else { String::new() })
        }
        "fullpath" => {
            let mut p = String::new();
            s.full_path(id, &mut p);
            Val::Text(p)
        }
        "size" => s.size(id).map_or(Val::Empty, |v| Val::Num(v as f64)),
        "dm" | "datemodified" => date(s.date_modified(id)),
        "dc" | "datecreated" => date(s.date_created(id)),
        "da" | "dateaccessed" => date(s.date_accessed(id)),
        "dr" | "daterun" => date(s.run_info(id).last),
        "rc" | "daterecentlychanged" => date(s.date_recently_changed(id)),
        "attrib" | "attributes" => Val::Num(f64::from(s.attributes(id))),
        "depth" => Val::Num(s.depth(id) as f64),
        "runcount" => Val::Num(f64::from(s.run_info(id).count)),
        "namelength" => Val::Num(s.name(id).len() as f64),
        "isfolder" | "isdir" => Val::Bool(s.is_dir(id)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use fnc_index::efu;
    use fnc_query::{ParseEnv, PlanEnv, parse, plan};

    use crate::{Caches, CancelToken, ExecCtx, ExtCtx, execute_ext};

    fn names(q: &str, props: Option<&fnc_props::PropertyStore>) -> (Vec<String>, Vec<fnc_query::plan::PlanDiag>) {
        let mut s = fnc_index::IndexStore::default();
        let csv = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\Docs\",,,,16\n\
                   \"C:\\Docs\\Report Q3.docx\",2000000,133900000000000000,,32\n\
                   \"C:\\Docs\\a.txt\",10,133500000000000000,,32\n\
                   \"C:\\Docs\\photo.jpg\",500000,133500000000000000,,32\n";
        efu::load_efu(&mut s, csv.as_bytes(), "t.efu").unwrap();
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let (caches, cancel) = (Caches::new(), CancelToken::new());
        let ext = ExtCtx { props: props.map(|p| &*Box::leak(Box::new(p.clone()))), ..ExtCtx::default() };
        let rs = execute_ext(&p, &s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }, &ext).unwrap();
        let mut v: Vec<String> = rs.ids.iter().map(|&id| s.name_string(id)).collect();
        v.sort();
        (v, p.diagnostics)
    }

    #[test]
    fn eval_filters_by_formula() {
        assert_eq!(names(r#"eval:"LEN($name:)>10""#, None).0, ["Report Q3.docx"]);
        assert_eq!(names("eval:LEN($stem:)<6 file:", None).0, ["a.txt", "photo.jpg"]);
        assert_eq!(names("eval:$size:>1mb", None).0, ["Report Q3.docx"]);
        assert_eq!(names(r#"eval:"AND($ext:=""jpg"", $size:>100kb)""#, None).0, ["photo.jpg"]);
        assert_eq!(names("eval:YEAR($dm:)>=2025 ext:docx", None).0, ["Report Q3.docx"]);
        assert_eq!(names("eval:$isfolder:", None).0, ["C:", "Docs"]);
        let (v, d) = names("eval:LEN(", None);
        assert!(v.is_empty() && !d.is_empty());
    }

    #[test]
    fn eval_reads_property_variables() {
        let mut s = fnc_index::IndexStore::default();
        let csv = "Filename,Size,Date Modified,Date Created,Attributes\n\"C:\\Docs\",,,,16\n\"C:\\Docs\\Report Q3.docx\",2000000,,,32\n\"C:\\Docs\\a.txt\",10,,,32\n\"C:\\Docs\\photo.jpg\",500000,,,32\n";
        efu::load_efu(&mut s, csv.as_bytes(), "t.efu").unwrap();
        let photo = (0..s.len() as u32).map(fnc_core::EntryId).find(|&i| s.name_string(i) == "photo.jpg").unwrap();
        let mut store = fnc_props::PropertyStore::new(s.instance());
        store.set_indexed(&[fnc_props::PropertyId::WIDTH]);
        store.insert(photo, vec![(fnc_props::PropertyId::WIDTH, fnc_props::Value::Int(4096))]);
        assert_eq!(names("eval:$width:>4000", Some(&store)).0, ["photo.jpg"]);
        assert!(names("eval:$width:>4000", None).0.is_empty());
    }
}

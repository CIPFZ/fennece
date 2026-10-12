//! Table-driven parser tests (SPEC ch.2 §10) and round-trip property tests.

use fnc_core::settings::Precedence;

use crate::ast::{Expr, Value};
use crate::parser::{DiagKind, ParseEnv, SpanKind, UserMacro, parse};
use crate::plan::{InputSet, Node, PatternKind, PlanEnv, Pred, Target, plan};

fn val(v: &Value) -> String {
    match v {
        Value::Text(t) => format!("'{t}'"),
        Value::Empty => "∅".into(),
        Value::List(items) => format!("{{{}}}", items.iter().map(val).collect::<Vec<_>>().join(",")),
        Value::Range(a, b) => format!("{}..{}", val(a), val(b)),
        Value::Cmp(op, v) => format!("{}{}", op.as_str(), val(v)),
    }
}

fn sexp(e: &Expr) -> String {
    match e {
        Expr::All => "*".into(),
        Expr::And(v) => format!("(and {})", v.iter().map(sexp).collect::<Vec<_>>().join(" ")),
        Expr::Or(v) => format!("(or {})", v.iter().map(sexp).collect::<Vec<_>>().join(" ")),
        Expr::Not(e) => format!("(not {})", sexp(e)),
        Expr::Macro(m, _) => format!("(macro {} {})", m.name, val(&m.value)),
        Expr::Term(t) => {
            let mut s = String::new();
            for (m, on) in t.mods.iter() {
                if !on {
                    s.push_str("no");
                }
                s.push_str(m.name());
                s.push(':');
            }
            if let Some(f) = t.func {
                s.push_str(f.name());
                s.push('=');
            }
            s.push_str(&val(&t.value));
            s
        }
    }
}

fn p(input: &str) -> String {
    sexp(&parse(input, &ParseEnv::default()).ast)
}

fn p_and(input: &str) -> String {
    let env = ParseEnv { precedence: Precedence::And, ..ParseEnv::default() };
    sexp(&parse(input, &env).ast)
}

/// `(input, expected s-expression)` with default (OR-first) precedence.
const CASES: &[(&str, &str)] = &[
    // --- basics ---
    ("", "*"),
    ("   ", "*"),
    ("abc", "'abc'"),
    ("abc 123", "(and 'abc' '123')"),
    ("abc\t123", "(and 'abc' '123')"),
    ("abc\u{3000}123", "(and 'abc' '123')"),
    ("a b c", "(and 'a' 'b' 'c')"),
    // --- OR binds tighter than AND (Everything default) ---
    ("a|b", "(or 'a' 'b')"),
    ("a | b", "(or 'a' 'b')"),
    ("a b|c", "(and 'a' (or 'b' 'c'))"),
    ("apple banana | orange", "(and 'apple' (or 'banana' 'orange'))"),
    ("a|b c|d", "(and (or 'a' 'b') (or 'c' 'd'))"),
    ("*.jpg | *.png", "(or '*.jpg' '*.png')"),
    ("a||b", "(or 'a' 'b')"),
    ("|a", "'a'"),
    ("a|", "'a'"),
    // --- NOT ---
    ("!abc", "(not 'abc')"),
    ("! abc", "(not 'abc')"),
    ("!!abc", "'abc'"),
    ("abc !def", "(and 'abc' (not 'def'))"),
    ("a!b", "'a!b'"),
    ("!a|b", "(or (not 'a') 'b')"),
    ("!", "*"),
    ("a !", "'a'"),
    ("!attrib:h", "(not attrib='h')"),
    // --- grouping ---
    ("<a b>|c", "(or (and 'a' 'b') 'c')"),
    ("<a|b> c", "(and (or 'a' 'b') 'c')"),
    ("!<a|b>", "(not (or 'a' 'b'))"),
    ("<a", "'a'"),
    ("a>", "'a'"),
    ("a > b", "(and 'a' 'b')"),
    ("<>", "*"),
    ("<<a>>", "'a'"),
    ("x<a|b>", "(and 'x' (or 'a' 'b'))"),
    ("folder: <!child:*.jpg|child:*.flac>", "(and folder=∅ (or (not child='*.jpg') child='*.flac'))"),
    // --- quotes ---
    ("\"a b\"", "'a b'"),
    ("\"a|b\"", "'a|b'"),
    ("\"<a>\"", "'<a>'"),
    ("abc\"def ghi\"jkl", "'abcdef ghijkl'"),
    ("\"\"", "''"),
    ("\"a\"\"b\"", "'a\"b'"),
    ("\"unclosed text", "'unclosed text'"),
    ("\"case:abc\"", "'case:abc'"),
    ("\"!abc\"", "'!abc'"),
    ("\"*.jpg\"", "'*.jpg'"),
    // --- entities ---
    ("a&sp:b", "'a b'"),
    ("&vert:", "'|'"),
    ("a&excl:", "'a!'"),
    ("&lt:x&gt:", "'<x>'"),
    ("&quot:q&quot:", "'\"q\"'"),
    ("&amp:", "'&'"),
    ("12&colon:30", "'12:30'"),
    ("&percent:x", "'%x'"),
    ("&#65:&#x42:", "'AB'"),
    ("&unknown:", "'&unknown:'"),
    // --- unknown prefixes are literal ---
    ("12:30", "'12:30'"),
    ("foo:bar", "'foo:bar'"),
    ("c:\\windows", "'c:\\windows'"),
    ("a:\\x", "'a:\\x'"),
    ("http://x", "'http://x'"),
    // --- modifiers ---
    ("case:abc", "case:'abc'"),
    ("nocase:abc", "nocase:'abc'"),
    ("CASE:abc", "case:'abc'"),
    ("case:regex:abc", "case:regex:'abc'"),
    ("ww:foo", "ww:'foo'"),
    ("wholeword:foo", "ww:'foo'"),
    ("wfn:a.txt", "wfn:'a.txt'"),
    ("exact:a.txt", "wfn:'a.txt'"),
    ("path:windows", "path:'windows'"),
    ("nopath:x", "nopath:'x'"),
    ("diacritics:café", "diacritics:'café'"),
    ("diac:café", "diacritics:'café'"),
    ("regex:^IMG_\\d{4}", "regex:'^IMG_\\d{4}'"),
    ("nopunc:spiderman", "ignorepunctuation:'spiderman'"),
    ("nows:a", "ignorewhitespace:'a'"),
    ("startwith:abc", "startwith:'abc'"),
    ("sw:abc", "startwith:'abc'"),
    ("ew:abc", "endwith:'abc'"),
    ("nomultiline:x", "nomultiline:'x'"),
    ("case:", "*"),
    ("case: abc", "'abc'"),
    ("fileonly:abc", "fileonly:'abc'"),
    ("#:5", "atof:'5'"),
    ("case:\"a b\"", "case:'a b'"),
    // --- global modifiers ---
    ("?case: abc def", "(and case:'abc' case:'def')"),
    ("abc ?case: def", "(and 'abc' case:'def')"),
    ("?case:abc def", "(and case:'abc' case:'def')"),
    ("?case: nocase:abc", "nocase:'abc'"),
    // --- functions ---
    ("ext:jpg", "ext='jpg'"),
    ("ext:jpg;png;gif", "ext={'jpg','png','gif'}"),
    ("ext:", "ext=∅"),
    ("extension:mp3", "ext='mp3'"),
    ("EXT:JPG", "ext='JPG'"),
    ("ext:\"a;b\"", "ext='a;b'"),
    ("folder:", "folder=∅"),
    ("file:", "file=∅"),
    ("file:abc", "file='abc'"),
    ("size:>1mb", "size=>'1mb'"),
    ("size:>=1mb", "size=>='1mb'"),
    ("size:<1kb", "size=<'1kb'"),
    ("size:<=1kb", "size=<='1kb'"),
    ("size:=0", "size=='0'"),
    ("size:!=0", "size=!='0'"),
    ("size:!0", "size=!='0'"),
    ("size:2mb..3mb", "size='2mb'..'3mb'"),
    ("size:10mb", "size='10mb'"),
    ("size:tiny;huge", "size={'tiny','huge'}"),
    ("size:", "size=∅"),
    ("<size:>1mb>", "size=>'1mb'"),
    ("<size:>1mb|size:<1kb>", "(or size=>'1mb' size=<'1kb')"),
    ("dm:today", "dm='today'"),
    ("datemodified:today", "dm='today'"),
    ("date-modified:today", "dm='today'"),
    ("Date_Modified:today", "dm='today'"),
    ("dm:2026-01-01..2026-03-31", "dm='2026-01-01'..'2026-03-31'"),
    ("dm:*-*-*T9:00..*-*-*T17:00", "dm='*-*-*T9:00'..'*-*-*T17:00'"),
    ("dm:today;yesterday", "dm={'today','yesterday'}"),
    ("dc:>2025", "dc=>'2025'"),
    ("attrib:h", "attrib='h'"),
    ("attrib:hs", "attrib='hs'"),
    ("a:h", "attrib='h'"),
    ("exact:attrib:h", "wfn:attrib='h'"),
    ("artist:\"John Doe\"", "artist='John Doe'"),
    ("parent:c:\\windows", "parent='c:\\windows'"),
    ("infolder:\"c:\\program files\"", "parent='c:\\program files'"),
    ("child:*.jpg", "child='*.jpg'"),
    ("childcount:>5", "childcount=>'5'"),
    ("depth:3", "depth='3'"),
    ("len:>20", "len:'>20'"),
    ("width:1920;3840", "width={'1920','3840'}"),
    ("case:regex:content:\"^Start\"", "case:regex:content='^Start'"),
    ("content:\"quarterly plan\"", "content='quarterly plan'"),
    ("si*:\"1+1=2\"", "si='1+1=2'"),
    ("si*:a|b", "si='a|b'"),
    ("name*:<x>", "name='<x>'"),
    ("::ext:jpg", "ext='jpg'"),
    ("dupe:", "dupe=∅"),
    ("dupe:name;size", "dupe={'name','size'}"),
    ("sem:\"季度预算表\" ext:xlsx dm:thisyear", "(and sem='季度预算表' ext='xlsx' dm='thisyear')"),
    ("volume:c", "volume='c'"),
    ("frn:12345", "frn='12345'"),
    // --- functions with operators around ---
    ("ext:jpg size:>1mb dm:thisyear", "(and ext='jpg' size=>'1mb' dm='thisyear')"),
    ("!ext:exe|ext:dll", "(or (not ext='exe') ext='dll')"),
    ("ext:jpg|ext:png abc", "(and (or ext='jpg' ext='png') 'abc')"),
    // --- macros ---
    ("audio:", "(macro audio ∅)"),
    ("audio:beatles", "(macro audio 'beatles')"),
    ("pic:", "(macro pic ∅)"),
    ("video: !mp4", "(and (macro video ∅) (not 'mp4'))"),
    ("zip:|doc:", "(or (macro zip ∅) (macro doc ∅))"),
    // --- paths ---
    ("\\", "'\\'"),
    ("windows\\", "'windows\\'"),
    ("c:\\windows\\*", "'c:\\windows\\*'"),
    ("\"c:\\program files\\\"", "'c:\\program files\\'"),
    // --- CJK / unicode ---
    ("报告 2026", "(and '报告' '2026')"),
    ("Straße", "'Straße'"),
    ("日本語|中文", "(or '日本語' '中文')"),
    // --- switches ---
    ("andprecedence: a b|c", "(or (and 'a' 'b') 'c')"),
    ("?and-precedence: a b|c", "(or (and 'a' 'b') 'c')"),
    ("orprecedence: a b|c", "(and 'a' (or 'b' 'c'))"),
];

#[test]
fn parser_cases() {
    let mut failures = Vec::new();
    for (input, expected) in CASES {
        let got = p(input);
        if got != *expected {
            failures.push(format!("{input:?}\n    expected {expected}\n    got      {got}"));
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// AND-first precedence cases.
const AND_CASES: &[(&str, &str)] = &[
    ("a b|c", "(or (and 'a' 'b') 'c')"),
    ("a|b c", "(or 'a' (and 'b' 'c'))"),
    ("a b", "(and 'a' 'b')"),
    ("a|b", "(or 'a' 'b')"),
    ("<a|b> c", "(and (or 'a' 'b') 'c')"),
    ("!a b|c", "(or (and (not 'a') 'b') 'c')"),
    ("orprecedence: a b|c", "(and 'a' (or 'b' 'c'))"),
];

#[test]
fn parser_and_precedence() {
    for (input, expected) in AND_CASES {
        assert_eq!(p_and(input), *expected, "{input:?}");
    }
}

#[test]
fn environment_variables() {
    let lookup = |name: &str| (name.eq_ignore_ascii_case("home")).then(|| "/home/u".to_string());
    let env = ParseEnv { env_var: Some(&lookup), ..ParseEnv::default() };
    assert_eq!(sexp(&parse("%HOME%", &env).ast), "'/home/u'");
    assert_eq!(sexp(&parse("\"%HOME%/x\"", &env).ast), "'/home/u/x'");
    assert_eq!(sexp(&parse("%NOPE%", &env).ast), "'%NOPE%'");
    assert_eq!(sexp(&parse("100%", &env).ast), "'100%'");
    // Expanded text is literal: an operator inside it does not split the term.
    let lookup2 = |_: &str| Some("a|b".to_string());
    let env2 = ParseEnv { env_var: Some(&lookup2), ..ParseEnv::default() };
    assert_eq!(sexp(&parse("%X%", &env2).ast), "'a|b'");
}

#[test]
fn user_macros() {
    let macros = vec![
        UserMacro { name: "Music".into(), search: "ext:mp3;flac".into() },
        UserMacro { name: "py-name".into(), search: "pinyin:#param:".into() },
        UserMacro { name: "loop".into(), search: "loop:".into() },
    ];
    let env = ParseEnv { user_macros: &macros, ..ParseEnv::default() };
    let parsed = parse("music: beatles", &env);
    match &parsed.ast {
        Expr::And(v) => match &v[0] {
            Expr::Macro(m, body) => {
                assert_eq!(m.name, "music");
                assert_eq!(sexp(body), "ext={'mp3','flac'}");
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    let parsed = parse("pyname:bj", &env);
    match &parsed.ast {
        Expr::Macro(_, body) => assert_eq!(sexp(body), "pinyin:'bj'"),
        other => panic!("{other:?}"),
    }
    // `::` bypasses user macros with the same name as a built-in.
    let shadow = vec![UserMacro { name: "ext".into(), search: "nothing".into() }];
    let env = ParseEnv { user_macros: &shadow, ..ParseEnv::default() };
    assert!(matches!(parse("ext:jpg", &env).ast, Expr::Macro(..)));
    assert_eq!(sexp(&parse("::ext:jpg", &env).ast), "ext='jpg'");
    // Recursion is bounded.
    let env = ParseEnv { user_macros: &macros, ..ParseEnv::default() };
    let parsed = parse("loop:", &env);
    assert!(parsed.diagnostics.iter().any(|d| matches!(d.kind, DiagKind::MacroTooDeep(_))));
}

#[test]
fn spans_cover_syntax() {
    let parsed = parse("case:ext:jpg | !\"a b\" <x>", &ParseEnv::default());
    let kinds: Vec<_> = parsed.spans.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SpanKind::Modifier,
            SpanKind::Function,
            SpanKind::Text,
            SpanKind::Or,
            SpanKind::Not,
            SpanKind::Text,
            SpanKind::Group,
            SpanKind::Text,
            SpanKind::Group,
        ]
    );
    assert_eq!(parsed.spans[0].range, 0..5);
    assert_eq!(parsed.spans[1].range, 5..9);
    match &parsed.ast {
        Expr::And(v) => match &v[0] {
            Expr::Or(o) => match &o[0] {
                Expr::Term(t) => assert_eq!(t.raw, 0..12),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn diagnostics() {
    let d = parse("\"abc", &ParseEnv::default()).diagnostics;
    assert_eq!(d[0].kind, DiagKind::UnclosedQuote);
    let d = parse("<abc", &ParseEnv::default()).diagnostics;
    assert_eq!(d[0].kind, DiagKind::UnclosedGroup);
    let d = parse("abc>", &ParseEnv::default()).diagnostics;
    assert_eq!(d[0].kind, DiagKind::StrayClose);
}

// ------------------------------------------------------------------ planner

fn plan_of(input: &str) -> Node {
    plan(&parse(input, &ParseEnv::default()).ast, &PlanEnv::default(), None).root
}

fn text_leaf(n: &Node) -> &crate::plan::TextPred {
    match n {
        Node::Leaf(Pred::Text(t)) => t,
        other => panic!("not a text leaf: {other:?}"),
    }
}

#[test]
fn plan_literal_kinds() {
    let t = plan_of("abc");
    assert_eq!(text_leaf(&t).kind, PatternKind::Substring);
    assert_eq!(text_leaf(&t).target, Target::Name);
    assert_eq!(text_leaf(&plan_of("*.jpg")).kind, PatternKind::Wildcard);
    assert_eq!(text_leaf(&plan_of("a?c")).kind, PatternKind::Wildcard);
    assert_eq!(text_leaf(&plan_of("regex:^IMG_\\d{4}")).kind, PatternKind::Regex);
    assert_eq!(text_leaf(&plan_of("wfn:a.txt")).kind, PatternKind::Exact);
    let sw = plan_of("^abc");
    assert_eq!(text_leaf(&sw).kind, PatternKind::StartsWith);
    assert_eq!(text_leaf(&sw).pattern, "abc");
    let ew = plan_of("abc$");
    assert_eq!(text_leaf(&ew).kind, PatternKind::EndsWith);
    assert_eq!(text_leaf(&plan_of("^abc$")).kind, PatternKind::Exact);
    let path = plan_of("windows\\system32");
    assert_eq!(text_leaf(&path).target, Target::FullPath);
    assert_eq!(text_leaf(&plan_of("path:abc")).target, Target::FullPath);
    assert_eq!(plan_of("\\"), Node::Leaf(Pred::IsDir(true)));
    assert!(text_leaf(&plan_of("case:Abc")).case);
}

#[test]
fn plan_orders_cheap_terms_first() {
    match plan_of("regex:abc foo ext:jpg") {
        Node::And(v) => {
            assert!(matches!(v[0], Node::Leaf(Pred::Ext { .. })));
            assert_eq!(text_leaf(&v[1]).kind, PatternKind::Substring);
            assert_eq!(text_leaf(&v[2]).kind, PatternKind::Regex);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn plan_values() {
    match plan_of("size:>1mb") {
        Node::Leaf(Pred::Num(_, p)) => assert_eq!(p.ranges, vec![(1_048_577, i64::MAX)]),
        other => panic!("{other:?}"),
    }
    match plan_of("size:1kb") {
        Node::Leaf(Pred::Num(_, p)) => assert_eq!(p.ranges, vec![(1024, 2048)]),
        other => panic!("{other:?}"),
    }
    match plan_of("size:1kb-2kb") {
        Node::Leaf(Pred::Num(_, p)) => assert_eq!(p.ranges, vec![(1024, 3072)]),
        other => panic!("{other:?}"),
    }
    match plan_of("size:unknown") {
        Node::Leaf(Pred::Num(_, p)) => assert!(p.unknown && p.ranges.is_empty()),
        other => panic!("{other:?}"),
    }
    match plan_of("ext:JPG;.png;jpg") {
        Node::Leaf(Pred::Ext { exts, none }) => {
            assert_eq!(exts, vec!["jpg".to_string(), "png".to_string()]);
            assert!(!none);
        }
        other => panic!("{other:?}"),
    }
    match plan_of("attrib:hs") {
        Node::Leaf(Pred::Attrib { mask, exact }) => assert_eq!((mask, exact), (0x6, false)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(plan_of("size:banana"), Node::Leaf(Pred::Invalid(_))));
    assert!(matches!(plan_of("width:>100"), Node::Leaf(Pred::Unsupported(_))));
    assert!(matches!(plan_of("audio:"), Node::Leaf(Pred::Ext { .. })));
}

#[test]
fn plan_dates_use_env() {
    use crate::date::{DateEnv, DateOrder};
    let env = PlanEnv {
        date: DateEnv {
            now: chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(12, 0, 0).unwrap(),
            order: DateOrder::Mdy,
            week_start: chrono::Weekday::Mon,
        },
        ..PlanEnv::default()
    };
    let n = plan(&parse("dm:>2025", &ParseEnv::default()).ast, &env, None).root;
    let start_2026 = fnc_core::FileTime::from_local(
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap(),
    );
    match n {
        Node::Leaf(Pred::Num(_, p)) => assert_eq!(p.ranges, vec![(start_2026.0, i64::MAX)]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn result_set_reuse() {
    let env = PlanEnv::default();
    let pe = ParseEnv::default();
    let a = plan(&parse("ab", &pe).ast, &env, None);
    let abc = plan(&parse("abc", &pe).ast, &env, Some(&a));
    assert_eq!(abc.input, InputSet::Previous);
    let abc_x = plan(&parse("abc x", &pe).ast, &env, Some(&abc));
    assert_eq!(abc_x.input, InputSet::Previous);
    let other = plan(&parse("xyz", &pe).ast, &env, Some(&abc));
    assert_eq!(other.input, InputSet::All);
    let or = plan(&parse("abc|d", &pe).ast, &env, Some(&a));
    assert_eq!(or.input, InputSet::All);
    let wild = plan(&parse("ab*", &pe).ast, &env, Some(&a));
    assert_eq!(wild.input, InputSet::All);
    let ext1 = plan(&parse("ext:jpg;png", &pe).ast, &env, None);
    let ext2 = plan(&parse("ext:jpg", &pe).ast, &env, Some(&ext1));
    assert_eq!(ext2.input, InputSet::Previous);
    let case = plan(&parse("case:abc", &pe).ast, &env, Some(&a));
    assert_eq!(case.input, InputSet::All);
    let from_empty = plan(&parse("x", &pe).ast, &env, Some(&plan(&Expr::All, &env, None)));
    assert_eq!(from_empty.input, InputSet::Previous);
    // Backspacing (abc -> ab) widens: no reuse.
    let back = plan(&parse("ab", &pe).ast, &env, Some(&abc));
    assert_eq!(back.input, InputSet::All);
}

mod roundtrip {
    use super::*;
    use crate::display::to_query_string;
    use proptest::prelude::*;

    fn word() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z0-9]{1,6}",
            "[a-z]{1,3} [a-z]{1,3}",
            "[a-z]{1,3}[|<>;!][a-z]{1,3}",
            "[a-z]{1,3}\"[a-z]",
            "\\*\\.[a-z]{2,3}",
            "[a-z]{1,3}\\\\[a-z]{1,3}",
            Just("case:x".to_string()),
            Just("&amp:".to_string()),
            Just("50%".to_string()),
            "[éßæ日本]{1,3}",
        ]
    }

    fn term_src() -> impl Strategy<Value = String> {
        prop_oneof![
            4 => word().prop_map(|w| {
                // Plain text term written via the formatter so special chars are escaped.
                to_query_string(&Expr::Term(crate::ast::Term { mods: crate::ast::ModSet::EMPTY, func: None, value: Value::Text(w), raw: 0..0 }), Precedence::Or)
            }),
            1 => "[a-z]{1,4}".prop_map(|w| format!("case:{w}")),
            1 => "[a-z]{1,3}(;[a-z]{1,3}){0,2}".prop_map(|w| format!("ext:{w}")),
            1 => prop_oneof![Just("size:>1mb"), Just("size:1kb..2kb"), Just("dm:today"), Just("size:<=10"), Just("folder:"), Just("audio:x")].prop_map(String::from),
        ]
    }

    fn query() -> impl Strategy<Value = String> {
        let leaf = term_src();
        leaf.prop_recursive(3, 12, 4, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 2..4).prop_map(|v| v.join(" ")),
                proptest::collection::vec(inner.clone(), 2..4).prop_map(|v| v.join("|")),
                inner.clone().prop_map(|q| format!("!<{q}>")),
                inner.prop_map(|q| format!("<{q}>")),
            ]
        })
    }

    proptest! {
        #[test]
        fn ast_string_ast(q in query(), and_first in any::<bool>()) {
            let prec = if and_first { Precedence::And } else { Precedence::Or };
            let env = ParseEnv { precedence: prec, ..ParseEnv::default() };
            let a = parse(&q, &env).ast;
            let s = to_query_string(&a, prec);
            let b = parse(&s, &env).ast;
            prop_assert_eq!(&a, &b, "query {:?} printed as {:?}", q, s);
        }

        #[test]
        fn parser_never_panics(s in "\\PC{0,40}") {
            let _ = parse(&s, &ParseEnv::default());
        }

        #[test]
        fn parser_never_panics_on_syntax_soup(s in "[a-z:;<>|!\"&%*?\\\\. =#-]{0,40}") {
            let parsed = parse(&s, &ParseEnv::default());
            let _ = plan(&parsed.ast, &PlanEnv::default(), None);
        }
    }
}

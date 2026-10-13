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
    // --- parent+N: the `+N` is part of the function name ---
    ("parent+1:c:\\windows", "parent+1='c:\\windows'"),
    ("Parent+12:\"c:\\a b\"", "parent+12='c:\\a b'"),
    ("parent+0:x", "parent+0='x'"),
    ("parent+255:x", "parent+255='x'"),
    ("parent+256:x", "'parent+256:x'"),
    ("parent+:x", "'parent+:x'"),
    ("parent+2", "'parent+2'"),
    ("ancestor+2:x", "'ancestor+2:x'"),
    ("!parent+1:x", "(not parent+1='x')"),
    ("case:parent+1:X", "case:parent+1='X'"),
    ("\"parent+1:x\"", "'parent+1:x'"),
    ("filelistfilename:backup.efu", "filelistfilename='backup.efu'"),
    ("fsi:jpg;png", "fsi={'jpg','png'}"),
    ("recyclebinoriginalpath:c:\\x", "recycled='c:\\x'"),
    ("linktarget:notepad.exe", "target='notepad.exe'"),
    ("child:*.jpg", "child='*.jpg'"),
    ("childcount:>5", "childcount=>'5'"),
    ("depth:3", "depth='3'"),
    ("len:>20", "len:'>20'"),
    ("width:1920;3840", "width={'1920','3840'}"),
    ("case:regex:content:\"^Start\"", "case:regex:content='^Start'"),
    ("content:\"quarterly plan\"", "content='quarterly plan'"),
    // `si*:` passes the rest verbatim, double quotes included (an AQS phrase).
    ("si*:\"1+1=2\"", "si='\"1+1=2\"'"),
    ("si*:\"a b\"", "si='\"a b\"'"),
    ("system-index*:x \"a b\"", "(and si='x' 'a b')"),
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
    for (input, expected) in CASES.iter().chain(MORE_CASES) {
        let got = p(input);
        if got != *expected {
            failures.push(format!("{input:?}\n    expected {expected}\n    got      {got}"));
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// More cases: every example of the Everything Search Syntax / Modifiers / Functions
/// pages that applies to the parser, SPEC ch.2 examples and the M2 functions.
const MORE_CASES: &[(&str, &str)] = &[
    // --- Everything "Searching" examples ---
    ("foo bar", "(and 'foo' 'bar')"),
    ("foo | bar", "(or 'foo' 'bar')"),
    ("!foo", "(not 'foo')"),
    ("<foo bar> | baz", "(or (and 'foo' 'bar') 'baz')"),
    ("\"foo bar\"", "'foo bar'"),
    ("*.mp3", "'*.mp3'"),
    ("foo*", "'foo*'"),
    ("*foo", "'*foo'"),
    ("ABC*.txt", "'ABC*.txt'"),
    ("c:\\windows\\*.exe", "'c:\\windows\\*.exe'"),
    ("\\\\server\\share\\*", "'\\\\server\\share\\*'"),
    ("\"c:\\program files\\\" *.exe", "(and 'c:\\program files\\' '*.exe')"),
    ("*.mp3 | *.wav", "(or '*.mp3' '*.wav')"),
    ("ext:avi;mpg;mpeg", "ext={'avi','mpg','mpeg'}"),
    ("size:>1mb <dm:today|dc:today>", "(and size=>'1mb' (or dm='today' dc='today'))"),
    ("\"my documents\" !temp", "(and 'my documents' (not 'temp'))"),
    ("/home/user/*.rs", "'/home/user/*.rs'"),
    ("..\\foo", "'..\\foo'"),
    // --- operators & grouping edge cases ---
    ("|||", "*"),
    ("!!!a", "(not 'a')"),
    ("<<<a|b>>>", "(or 'a' 'b')"),
    ("<a <b|c>>", "(and 'a' (or 'b' 'c'))"),
    ("a | b c | d", "(and (or 'a' 'b') (or 'c' 'd'))"),
    ("<a b> | <c d>", "(or (and 'a' 'b') (and 'c' 'd'))"),
    ("!<a b>", "(not (and 'a' 'b'))"),
    ("!<a> b", "(and (not 'a') 'b')"),
    ("a !b !c", "(and 'a' (not 'b') (not 'c'))"),
    ("a|!b", "(or 'a' (not 'b'))"),
    ("  a   b  ", "(and 'a' 'b')"),
    ("a\u{3000}|\u{3000}b", "(or 'a' 'b')"),
    // --- quotes ---
    ("a\"b c\"d e", "(and 'ab cd' 'e')"),
    ("\"a b\"|c", "(or 'a b' 'c')"),
    ("\"\" a", "(and '' 'a')"),
    ("\"a\" \"b\"", "(and 'a' 'b')"),
    ("ext:\"jpg\"", "ext='jpg'"),
    ("\"ext:jpg\"", "'ext:jpg'"),
    ("case:\"ABC\"", "case:'ABC'"),
    // --- literal colons / drive letters ---
    ("12:30:00", "'12:30:00'"),
    ("d:\\data", "'d:\\data'"),
    ("z:", "'z:'"),
    ("C:", "'C:'"),
    ("a:b:c", "attrib='b:c'"),
    ("mailto:x@y", "'mailto:x@y'"),
    ("foo:", "'foo:'"),
    // --- entities ---
    ("&#x41:", "'A'"),
    ("&#228:", "'ä'"),
    ("ext:a&semi:b", "ext='a;b'"),
    ("a&vert:b", "'a|b'"),
    ("&lt:a&gt: b", "(and '<a>' 'b')"),
    ("&excl:abc", "'!abc'"),
    ("\"&quot:\"", "'\"'"),
    ("&SP:", "' '"),
    // --- modifiers (Search Modifiers page) ---
    ("wholefilename:readme", "wfn:'readme'"),
    ("nowfn:x", "nowfn:'x'"),
    ("wholepath:c:\\x", "wholepath:'c:\\x'"),
    ("wp:c:\\x", "wholepath:'c:\\x'"),
    ("wildcards:[ab]*", "wildcards:'[ab]*'"),
    ("wildcardsstarall:a*b", "wildcardsstarall:'a*b'"),
    ("prefix:foo", "prefix:'foo'"),
    ("suffix:bar", "suffix:'bar'"),
    ("noprefix:x", "noprefix:'x'"),
    ("ignorepunctuation:spider-man", "ignorepunctuation:'spider-man'"),
    ("punc:x", "noignorepunctuation:'x'"),
    ("ws:x", "noignorewhitespace:'x'"),
    ("utf8:content:x", "utf8:content='x'"),
    ("utf16:content:x", "utf16:content='x'"),
    ("utf16be:content:x", "utf16be:content='x'"),
    ("ansi:content:x", "ansi:content='x'"),
    ("bytestream:content:x", "binary:content='x'"),
    ("binary:content:\"\\x00\\x01\"", "binary:content='\\x00\\x01'"),
    ("ascii:x", "ascii:'x'"),
    ("noascii:x", "noascii:'x'"),
    ("dotall:regex:a.b", "regex:dotall:'a.b'"),
    ("multiline:regex:^x", "regex:multiline:'^x'"),
    ("atoi:x", "atoi:'x'"),
    ("toint:x", "atoi:'x'"),
    ("tonumber:x", "atof:'x'"),
    ("allstrings:tags:a;b", "allstrings:tags={'a','b'}"),
    ("anystrings:tags:a", "anystrings:tags='a'"),
    ("folderonly:x", "folderonly:'x'"),
    ("highlight:x", "highlight:'x'"),
    ("nohighlight:x", "nohighlight:'x'"),
    ("len:album:>20", "len:album='>20'"),
    ("count:tags:>2", "count:tags='>2'"),
    ("notindexed:width:>1", "notindexed:width=>'1'"),
    ("py:bj", "pinyin:'bj'"),
    ("pinyin:beijing", "pinyin:'beijing'"),
    ("pinyinquan:beijing", "pinyinquan:'beijing'"),
    ("pinyinjian:bj", "pinyinjian:'bj'"),
    ("nopinyin:x", "nopinyin:'x'"),
    ("nopy:x", "nopinyin:'x'"),
    ("fuzzy:reprot", "fuzzy:'reprot'"),
    ("nofuzzy:x", "nofuzzy:'x'"),
    ("nodiac:e", "nodiacritics:'e'"),
    ("case:ww:foo", "case:ww:'foo'"),
    ("ww:case:foo", "case:ww:'foo'"),
    ("nocase:case:x", "case:'x'"),
    ("case:nocase:x", "nocase:'x'"),
    ("case:regex:\"^a b$\"", "case:regex:'^a b$'"),
    ("regex:\"\\.(jpg|png)$\"", "regex:'\\.(jpg|png)$'"),
    ("regex:a|b", "(or regex:'a' 'b')"),
    // --- global modifiers ---
    ("?ww: a b|c", "(and ww:'a' (or ww:'b' ww:'c'))"),
    ("?nocase: A", "nocase:'A'"),
    ("a ?path: b ?case: c", "(and 'a' path:'b' case:path:'c')"),
    ("?case:?ww: x", "case:ww:'x'"),
    ("?regex: a.b", "regex:'a.b'"),
    // --- `::` and `func*:` ---
    ("::audio:", "(macro audio ∅)"),
    ("::case:abc", "case:'abc'"),
    ("::size:>1mb", "size=>'1mb'"),
    ("ext*:a;b", "ext='a;b'"),
    ("content*:<x>|y", "content='<x>|y'"),
    ("regex*:x", "'regex*:x'"),
    ("name*:\"a\"", "name='a'"),
    // --- switches ---
    ("andfilter: x", "'x'"),
    ("orfilter: x", "'x'"),
    ("and-filter: or-precedence: a b|c", "(and 'a' (or 'b' 'c'))"),
    // --- files and folders ---
    ("file:*.txt", "file='*.txt'"),
    ("folder:music", "folder='music'"),
    ("!folder:", "(not folder=∅)"),
    ("file: !ext:txt", "(and file=∅ (not ext='txt'))"),
    ("stem:report", "stem='report'"),
    ("nameextstripped:x", "stem='x'"),
    ("fullpath:c:\\x", "fullpath='c:\\x'"),
    ("fp:x", "fullpath='x'"),
    ("pathpart:windows", "pathpart='windows'"),
    ("nosubfolders:c:\\x", "parent='c:\\x'"),
    ("parents:c:\\users", "ancestor='c:\\users'"),
    ("ancestors:c:\\users", "ancestor='c:\\users'"),
    ("child:readme.md", "child='readme.md'"),
    ("childcount:0", "childcount='0'"),
    ("childfilecount:>=10", "childfilecount=>='10'"),
    ("childfoldercount:<2", "childfoldercount=<'2'"),
    ("descendant:*.rs", "descendant='*.rs'"),
    ("descendantcount:>100", "descendantcount=>'100'"),
    ("descendant-file-count:5", "descendantfilecount='5'"),
    ("descendantfoldercount:0..2", "descendantfoldercount='0'..'2'"),
    ("sibling:readme", "sibling='readme'"),
    ("siblingcount:>3", "siblingcount=>'3'"),
    ("parent-sibling:.git", "parentsibling='.git'"),
    ("ancestor-sibling:node_modules", "ancestorsibling='node_modules'"),
    ("foldersize:>1gb", "foldersize=>'1gb'"),
    ("fs:<1mb", "foldersize=<'1mb'"),
    ("folder-size:1mb..2mb", "foldersize='1mb'..'2mb'"),
    ("hardlinkcount:>1", "hardlinkcount=>'1'"),
    ("hardlinks:2", "hardlinkcount='2'"),
    ("nameparts:3", "nameparts='3'"),
    ("type:\"Text Document\"", "type='Text Document'"),
    ("kind:picture", "kind='picture'"),
    ("depth:2", "depth='2'"),
    ("parents-count:3", "depth='3'"),
    ("root:", "root=∅"),
    ("empty:", "empty=∅"),
    ("namelength:>50", "namelength=>'50'"),
    ("pathlength:<=260", "pathlength=<='260'"),
    ("fullpathlength:>260", "fullpathlength=>'260'"),
    ("runcount:>5", "runcount=>'5'"),
    ("dr:today", "dr='today'"),
    ("rc:last2hours", "rc='last2hours'"),
    ("da:thisweek", "da='thisweek'"),
    ("volume:d:", "volume='d:'"),
    ("drive:c", "volume='c'"),
    ("volume-kind:ntfs", "volumekind='ntfs'"),
    ("online:", "online=∅"),
    ("offline:", "offline=∅"),
    ("reparse:", "reparse=∅"),
    ("symlink:", "symlink=∅"),
    ("junction:", "junction=∅"),
    ("target:c:\\x", "target='c:\\x'"),
    ("index-frn:5", "frn='5'"),
    ("fsi:png", "fsi='png'"),
    ("recycled:", "recycled=∅"),
    ("attributes:rh", "attrib='rh'"),
    ("attrib:", "attrib=∅"),
    // --- dates (SPEC ch.2 §4.4) ---
    ("dm:2026", "dm='2026'"),
    ("dm:07/2026", "dm='07/2026'"),
    ("dm:2026/07", "dm='2026/07'"),
    ("dm:02/07/2026", "dm='02/07/2026'"),
    ("dm:20260702", "dm='20260702'"),
    ("dm:260702", "dm='260702'"),
    ("dm:2026-07-02T10:52:00", "dm='2026-07-02T10:52:00'"),
    ("dm:*-02", "dm='*-02'"),
    ("dm:>=2026-01-01", "dm=>='2026-01-01'"),
    ("dc:<1/1/2020", "dc=<'1/1/2020'"),
    ("dm:pastmonth", "dm='pastmonth'"),
    ("dm:past3days", "dm='past3days'"),
    ("dm:jan;feb", "dm={'jan','feb'}"),
    ("dm:sunday", "dm='sunday'"),
    ("dm:mtd", "dm='mtd'"),
    ("dm:46000", "dm='46000'"),
    ("dm:134274631200000000", "dm='134274631200000000'"),
    ("dm:..2020", "dm=''..'2020'"),
    ("dm:2020..", "dm='2020'..''"),
    ("dm:!today", "dm=!='today'"),
    // --- sizes ---
    ("size:>1gb", "size=>'1gb'"),
    ("size:<=10kb", "size=<='10kb'"),
    ("size:empty", "size='empty'"),
    ("size:gigantic|size:tiny", "(or size='gigantic' size='tiny')"),
    ("size:1-2mb", "size='1-2mb'"),
    // --- property functions (parsed; evaluated in M3) ---
    ("width:>1920", "width=>'1920'"),
    ("height:1080", "height='1080'"),
    ("dimensions:1920x1080", "dimensions='1920x1080'"),
    ("rating:>=4", "rating=>='4'"),
    ("length:>5min", "length=>'5min'"),
    ("length:1:30..2:00", "length='1:30'..'2:00'"),
    ("duration:90s", "length='90s'"),
    ("bitrate:>320", "bit-rate=>'320'"),
    ("year:2020..2023", "year='2020'..'2023'"),
    ("genre:rock;pop", "genre={'rock','pop'}"),
    ("author:\"Jane Roe\"", "author='Jane Roe'"),
    ("title:report", "title='report'"),
    ("album:\"x y\"", "album='x y'"),
    // --- content / hashes / sets ---
    ("content:foo", "content='foo'"),
    ("utf8content:\"bar\"", "utf8content='bar'"),
    ("contenthits:>3", "contenthits=>'3'"),
    ("md5:d41d8cd98f00b204e9800998ecf8427e", "md5='d41d8cd98f00b204e9800998ecf8427e'"),
    ("crc32:0", "crc32='0'"),
    ("sizedupe:", "sizedupe=∅"),
    ("distinct:ext", "distinct='ext'"),
    ("unique:name", "unique='name'"),
    ("dupecount:>2", "dupecount=>'2'"),
    ("count:5", "count='5'"),
    ("< count:10 > < test >", "(and count='10' 'test')"),
    ("count:>5", "count:'>5'"),
    ("filelist:\"a.txt|b.txt\"", "filelist='a.txt|b.txt'"),
    ("filelist:a;b", "filelist={'a','b'}"),
    ("filelist*:a|b", "filelist='a|b'"),
    ("fileexists:$stem:.srt", "fileexists='$stem:.srt'"),
    ("folderexists:$path:\\.git", "folderexists='$path:\\.git'"),
    ("omit:", "omit=∅"),
    ("weight:10 foo", "(and weight='10' 'foo')"),
    ("random:10", "random='10'"),
    ("eval:\"LEN($name:)>10\"", "eval='LEN($name:)>10'"),
    ("si:foo", "si='foo'"),
    ("semtop:20", "semtop='20'"),
    // --- macros ---
    ("exe:", "(macro exe ∅)"),
    ("executable:", "(macro executable ∅)"),
    ("doc:report", "(macro doc 'report')"),
    ("compressed:", "(macro compressed ∅)"),
    ("picture:", "(macro picture ∅)"),
    ("image:", "(macro image ∅)"),
    ("video:x", "(macro video 'x')"),
    ("!audio:", "(not (macro audio ∅))"),
    ("audio:|video:", "(or (macro audio ∅) (macro video ∅))"),
    ("::doc:", "(macro doc ∅)"),
    ("audio:\"the beatles\"", "(macro audio 'the beatles')"),
    // --- unicode ---
    ("日本 | 中国", "(or '日本' '中国')"),
    ("Ünïcödé", "'Ünïcödé'"),
    ("ｆｕｌｌｗｉｄｔｈ", "'ｆｕｌｌｗｉｄｔｈ'"),
    ("季度报告.docx !草稿", "(and '季度报告.docx' (not '草稿'))"),
];

/// AND-first precedence cases.
const AND_CASES: &[(&str, &str)] = &[
    ("a b|c", "(or (and 'a' 'b') 'c')"),
    ("a|b c", "(or 'a' (and 'b' 'c'))"),
    ("a b", "(and 'a' 'b')"),
    ("a|b", "(or 'a' 'b')"),
    ("<a|b> c", "(and (or 'a' 'b') 'c')"),
    ("!a b|c", "(or (and (not 'a') 'b') 'c')"),
    ("orprecedence: a b|c", "(and 'a' (or 'b' 'c'))"),
    ("a | b c | d", "(or 'a' (and 'b' 'c') 'd')"),
    ("<a b> | <c d>", "(or (and 'a' 'b') (and 'c' 'd'))"),
    ("!a | b c", "(or (not 'a') (and 'b' 'c'))"),
    ("a b | c d | e", "(or (and 'a' 'b') (and 'c' 'd') 'e')"),
    ("ext:jpg | ext:png size:>1mb", "(or ext='jpg' (and ext='png' size=>'1mb'))"),
    ("a <b | c> d", "(and 'a' (or 'b' 'c') 'd')"),
    ("?or-precedence: a | b c", "(and (or 'a' 'b') 'c')"),
    ("| a b |", "(and 'a' 'b')"),
    ("audio: | video: x", "(or (macro audio ∅) (and (macro video ∅) 'x'))"),
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

#[test]
fn set_functions_are_hoisted_with_keys() {
    use crate::plan::{DupeKey, SetKind};
    use fnc_props::PropertyId;
    use fnc_props::hash::HashAlgo;
    let p = |q: &str| plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
    let q = p("dupe:");
    assert_eq!(q.root, Node::All);
    assert_eq!(q.post.len(), 1);
    assert_eq!(q.post[0].kind, SetKind::Dupe);
    assert_eq!(q.post[0].keys.as_deref(), Some(&[DupeKey::Name][..]));
    let keys = |q: &str| p(q).post[0].keys.clone().unwrap();
    assert_eq!(keys("dupe:size;sha256"), [DupeKey::Size, DupeKey::Hash(HashAlgo::Sha256)]);
    assert_eq!(keys("dupe:name;size"), [DupeKey::Name, DupeKey::Size]);
    assert_eq!(keys("sizedupe:"), [DupeKey::Size]);
    assert_eq!(keys("namepartdupe:"), [DupeKey::NamePart]);
    assert_eq!(keys("attribdupe:"), [DupeKey::Attrib]);
    assert_eq!(keys("dadupe:"), [DupeKey::DateAccessed]);
    assert_eq!(keys("dcdupe:"), [DupeKey::DateCreated]);
    assert_eq!(keys("dmdupe:"), [DupeKey::DateModified]);
    assert_eq!(keys("distinct:artist"), [DupeKey::Prop(PropertyId::ARTIST)]);
    assert_eq!(p("unique:ext").post[0].kind, SetKind::Unique);
    assert_eq!(p("distinct:").post[0].kind, SetKind::Distinct);
    // dupe-count:/dupe-max: group like the other set function of the query.
    let q = p("dupe:size dupe-count:3 dupe-max:2 *.jpg");
    assert_eq!(q.post.len(), 3);
    assert!(matches!(q.post[1].kind, SetKind::DupeCount(_)));
    assert_eq!(q.post[1].keys.as_deref(), Some(&[DupeKey::Size][..]));
    assert_eq!(q.post[2].kind, SetKind::DupeMax(2));
    assert_eq!(p("dupe-max:1").post[0].keys.as_deref(), Some(&[DupeKey::Name][..]));
    assert!(p("!dupe:").post[0].negate);
    assert!(!p("dupe:bogus").diagnostics.is_empty());
    assert!(!p("dupe-max:x").diagnostics.is_empty());
    assert!(!p("<dupe:|a>").diagnostics.is_empty(), "nested set function reported");
    // `count:<max>` (result limit) runs after every other set function.
    let q = p("count:3 dupe:size *.jpg");
    assert_eq!(q.post.len(), 2);
    assert_eq!(q.post[0].kind, SetKind::Dupe);
    assert_eq!(q.post[1].kind, SetKind::Limit(3));
    assert_eq!(p("dupe-max:1 count:2").post[0].keys.as_deref(), Some(&[DupeKey::Name][..]));
    assert_eq!(p("< count:10 > < test >").post[0].kind, SetKind::Limit(10));
    assert!(p("count:tags:>2").post.is_empty(), "count: before a function is the modifier");
    // Plans with set functions never filter the previous result set.
    let a = p("abc dupe:");
    let b = plan(&parse("abcd dupe:", &ParseEnv::default()).ast, &PlanEnv::default(), Some(&a));
    assert_eq!(b.input, InputSet::All);
    let a = p("abc");
    let b = plan(&parse("abcd dupe:", &ParseEnv::default()).ast, &PlanEnv::default(), Some(&a));
    assert_eq!(b.input, InputSet::All);
}

#[test]
fn property_cost_ordering_and_names() {
    use fnc_props::PropertyId;
    // Unindexed properties run after cheap filters; indexed ones before string matching.
    let n = plan_of("width:>10 abc ext:jpg");
    match n {
        Node::And(v) => {
            assert!(matches!(v[0], Node::Leaf(Pred::Ext { .. })));
            assert!(matches!(v[2], Node::Leaf(Pred::Prop(_))), "{v:?}");
        }
        other => panic!("{other:?}"),
    }
    let env = PlanEnv { indexed_props: vec![PropertyId::WIDTH], ..PlanEnv::default() };
    match plan(&parse("abc width:>10", &ParseEnv::default()).ast, &env, None).root {
        Node::And(v) => assert!(matches!(v[0], Node::Leaf(Pred::Prop(ref p)) if p.indexed), "{v:?}"),
        other => panic!("{other:?}"),
    }
    // Every property name is a function, dashed or not.
    for (name, p) in PropertyId::search_names() {
        match plan_of(&format!("{name}:unknown")) {
            Node::Leaf(Pred::Prop(pp)) => assert_eq!(pp.id, p, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
    }
    assert!(matches!(plan_of("camera-model:x"), Node::Leaf(Pred::Prop(_))));
    // Round trip through the canonical text form.
    let q = parse("camera-model:\"EOS R5\" width:>4000 dupe:size", &ParseEnv::default()).ast;
    let s = crate::to_query_string(&q, Precedence::Or);
    assert_eq!(parse(&s, &ParseEnv::default()).ast, q);
}

/// Compact form of a text predicate: `kind:target'pattern'+flags`.
fn tsexp(t: &crate::plan::TextPred) -> String {
    use crate::plan::PinyinMode;
    let kind = match t.kind {
        PatternKind::Substring => "sub",
        PatternKind::Wildcard => "wild",
        PatternKind::Regex => "re",
        PatternKind::Exact => "eq",
        PatternKind::StartsWith => "sw",
        PatternKind::EndsWith => "ew",
    };
    let target = match t.target {
        Target::Name => "",
        Target::FullPath => "path",
        Target::PathPart => "pathpart",
        Target::Stem => "stem",
        Target::Ext => "ext",
    };
    let mut s = format!("{kind}:{target}'{}'", t.pattern);
    for (on, flag) in [
        (t.case, "case"),
        (t.whole_word, "ww"),
        (t.ascii, "ascii"),
        (t.fuzzy, "fuzzy"),
        (t.pinyin == PinyinMode::Any, "py"),
        (t.pinyin == PinyinMode::Full, "pyquan"),
        (t.pinyin == PinyinMode::Initials, "pyjian"),
    ] {
        if on {
            s.push('+');
            s.push_str(flag);
        }
    }
    s
}

/// Compact form of a plan (numeric ranges only for small values).
fn psexp(n: &Node) -> String {
    use crate::plan::{NumProp, SiblingScope};
    let list = |v: &[Node]| v.iter().map(psexp).collect::<Vec<_>>().join(" ");
    match n {
        Node::All => "*".into(),
        Node::Nothing => "none".into(),
        Node::And(v) => format!("(and {})", list(v)),
        Node::Or(v) => format!("(or {})", list(v)),
        Node::Not(c) => format!("(not {})", psexp(c)),
        Node::Leaf(p) => match p {
            Pred::Text(t) => tsexp(t),
            Pred::Child(t) => format!("child({})", tsexp(t)),
            Pred::Descendant(t) => format!("descendant({})", tsexp(t)),
            Pred::Sibling(scope, t) => {
                let s = match scope {
                    SiblingScope::Own => "sibling",
                    SiblingScope::Parent => "parentsibling",
                    SiblingScope::Ancestor => "ancestorsibling",
                };
                format!("{s}({})", tsexp(t))
            }
            Pred::Type(t) => format!("type({})", tsexp(t)),
            Pred::Num(prop, pred) => {
                let name = match prop {
                    NumProp::Date(_) => "date".to_string(),
                    other => format!("{other:?}"),
                };
                let ranges: Vec<String> = pred
                    .ranges
                    .iter()
                    .map(|&(lo, hi)| {
                        let f = |v: i64| match v {
                            i64::MAX => "max".to_string(),
                            v if v == i64::MIN + 1 => "min".to_string(),
                            v => v.to_string(),
                        };
                        format!("{}..{}", f(lo), f(hi))
                    })
                    .collect();
                if matches!(prop, NumProp::Date(_)) {
                    format!("{name}[{}]", pred.ranges.len())
                } else {
                    format!("{name}[{}]{}", ranges.join(","), if pred.unknown { "?" } else { "" })
                }
            }
            Pred::FileList { names, paths } => format!("filelist({};{})", names.join(","), paths.join(",")),
            Pred::Exists { template, folder } => {
                format!("{}({})", if *folder { "folderexists" } else { "fileexists" }, template.len())
            }
            Pred::DateWild(_, w) => format!("datewild({:b})", w.mask),
            Pred::Unsupported(f) => format!("unsupported({})", f.name()),
            Pred::ParentUp(n, t) => format!("parentup{n}({})", tsexp(t)),
            Pred::Parent(t) => format!("parent({})", tsexp(t)),
            Pred::FileListName(t) => format!("filelistname({})", t.as_ref().map_or("*".into(), tsexp)),
            Pred::LinkTarget(t) => format!("target({})", t.as_ref().map_or("*".into(), tsexp)),
            Pred::Recycled(t) => format!("recycled({})", t.as_ref().map_or("*".into(), tsexp)),
            Pred::Signature(names) => format!("fsi({})", names.join(",")),
            Pred::SystemIndex(q) => format!("si({q:?})"),
            Pred::Invalid(_) => "invalid".into(),
            Pred::Prop(p) => {
                use crate::plan::PropTest;
                let r = |n: &crate::plan::NumPred| {
                    let big = n.ranges.iter().any(|&(lo, hi)| {
                        [lo, hi].iter().any(|&v| v != i64::MAX && v != i64::MIN + 1 && v.abs() > 1_000_000_000_000)
                    });
                    if big {
                        return format!("[{}]{}", n.ranges.len(), if n.unknown { "?" } else { "" });
                    }
                    let v: Vec<String> = n
                        .ranges
                        .iter()
                        .map(|&(lo, hi)| {
                            let f = |v: i64| match v {
                                i64::MAX => "max".to_string(),
                                v if v == i64::MIN + 1 => "min".to_string(),
                                v => v.to_string(),
                            };
                            format!("{}..{}", f(lo), f(hi))
                        })
                        .collect();
                    format!("[{}]{}", v.join(","), if n.unknown { "?" } else { "" })
                };
                let test = match &p.test {
                    PropTest::Num(n) => r(n),
                    PropTest::Text { pred, all } => format!("({}){}", tsexp(pred), if *all { "+all" } else { "" }),
                    PropTest::Len(n) => format!("len{}", r(n)),
                    PropTest::Count(n) => format!("count{}", r(n)),
                    PropTest::Atoi(n) => format!("atoi{}", r(n)),
                    PropTest::Atof(n) => format!("atof[{}]", n.ranges.len()),
                    PropTest::Dims(w, h) => format!("dims{}x{}", r(w), r(h)),
                    PropTest::Known => "known".into(),
                    PropTest::Unknown => "unknown".into(),
                };
                format!("{}{}{}", p.id.name(), test, if p.not_indexed { "+notindexed" } else { "" })
            }
            Pred::Content(c) => format!(
                "content:{:?}({}){}",
                c.source,
                tsexp(&c.text),
                if c.bytes.is_empty() { String::new() } else { format!("{:x?}", c.bytes) }
            ),
            Pred::ContentHits(n, c) => format!("contenthits[{}]({})", n.ranges.len(), c.len()),
            Pred::Hash(h) => {
                format!("{}({}){}", h.algo.name(), h.prefixes.join(","), if h.unknown { "unknown" } else { "" })
            }
            other => {
                let d = format!("{other:?}");
                d.split(['(', ' ', '{']).next().unwrap_or("").to_string()
            }
        },
    }
}

/// `(input, expected plan)` for the M2 functions and modifiers.
const PLAN_CASES: &[(&str, &str)] = &[
    ("descendant:*.jpg", "descendant(wild:'*.jpg')"),
    ("descendant:a;b", "(or descendant(sub:'a') descendant(sub:'b'))"),
    ("descendant:", "*"),
    ("descendantcount:>5", "DescendantCount[6..max]"),
    ("descendantfilecount:0", "DescendantFileCount[0..1]"),
    ("descendantfoldercount:2..4", "DescendantFolderCount[2..5]"),
    ("sibling:readme", "sibling(sub:'readme')"),
    ("parent-sibling:.git", "parentsibling(sub:'.git')"),
    ("ancestor-sibling:node_modules", "ancestorsibling(sub:'node_modules')"),
    ("siblingcount:<3", "SiblingCount[min..3]"),
    ("foldersize:>1mb", "FolderSize[1048577..max]"),
    ("fs:1kb", "FolderSize[1024..2048]"),
    ("folder-size:empty", "FolderSize[0..1]"),
    ("foldersize:", "FolderSize[1..max]"),
    ("hardlinkcount:>1", "HardlinkCount[2..max]"),
    ("hardlinks:unknown", "HardlinkCount[]?"),
    ("nameparts:3", "NameParts[3..4]"),
    ("type:\"text document\"", "type(sub:'text document')"),
    ("type:", "*"),
    ("wfn:type:file", "type(eq:'file')"),
    ("filelist:\"A.txt|C:\\X\\b.TXT\\\"", "filelist(a.txt;c:\\x\\b.txt)"),
    ("filelist:a;b|a", "(or filelist(a,b;) sub:'a')"),
    ("filelist:\"b|a|B\"", "filelist(a,b;)"),
    ("filelist:", "none"),
    ("filelist:\"|\"", "none"),
    ("fileexists:$stem:.srt", "fileexists(2)"),
    ("folderexists:$path:\\.git", "folderexists(2)"),
    ("fileexists:a;b", "(or fileexists(1) fileexists(1))"),
    ("omit:", "*"),
    ("weight:10", "*"),
    ("weight:10 abc", "sub:'abc'"),
    ("dm:*-02", "datewild(10)"),
    ("dm:*-*-*T9:00..*-*-*T17:00", "datewild(11000)"),
    ("dm:>*-*-*T17:00", "datewild(11000)"),
    ("dm:!*-02", "(not datewild(10))"),
    ("dm:*-02;today", "(or datewild(10) date[1])"),
    ("dm:*-13", "invalid"),
    ("dm:today", "date[1]"),
    ("dm:last2weeks", "date[1]"),
    ("dm:46000", "date[1]"),
    ("py:jdbg", "sub:'jdbg'+py"),
    ("pinyinquan:jidu", "sub:'jidu'+pyquan"),
    ("pinyinjian:jd", "sub:'jd'+pyjian"),
    ("py:*.jpg", "wild:'*.jpg'"),
    ("py:ww:abc", "sub:'abc'+ww"),
    ("py:name:abc", "sub:'abc'+py"),
    ("ascii:abc", "sub:'abc'+ascii"),
    ("fuzzy:report", "sub:'report'+fuzzy"),
    ("fuzzy:^report", "sw:'report'"),
    ("len:>20", "NameLength[21..max]"),
    ("len:5..10", "NameLength[5..11]"),
    ("len:7", "NameLength[7..8]"),
    ("startwith:abc", "sw:'abc'"),
    ("endwith:abc", "ew:'abc'"),
    ("child:*.jpg", "child(wild:'*.jpg')"),
    // ---- file lists, parent+N, links, signatures, recycle bin ----
    ("filelistfilename:x.txt", "filelistname(sub:'x.txt')"),
    ("filelistfilename:", "filelistname(*)"),
    ("filelistfilename:*.efu", "filelistname(wild:'*.efu')"),
    ("filelistfilename:d:\\lists\\a.efu", "filelistname(sub:path'd:\\lists\\a.efu')"),
    ("filelistfilename:a;b", "(or filelistname(sub:'a') filelistname(sub:'b'))"),
    ("parent+0:c:\\x", "parent(eq:path'c:\\x')"),
    ("parent+2:c:\\x\\", "parentup2(eq:path'c:\\x')"),
    ("parent+1:c:\\a\\*", "parentup1(wild:path'c:\\a\\*')"),
    ("target:notepad", "target(sub:path'notepad')"),
    ("target:", "target(*)"),
    ("target:*.exe", "target(wild:'*.exe')"),
    ("target:c:\\windows\\*", "target(wild:path'c:\\windows\\*')"),
    ("shortcuttarget:http*://*", "target(wild:path'http*://*')"),
    ("linktarget:a;b", "(or target(sub:path'a') target(sub:path'b'))"),
    ("case:target:X", "target(sub:path'X'+case)"),
    ("fsi:png", "(and IsDir fsi(png))"),
    ("fsi:JPEG;png;jpg", "(and IsDir fsi(jpg,png))"),
    ("filesignature:", "(and IsDir fsi())"),
    ("fsi:bogus", "invalid"),
    ("fsi:zip !ext:zip", "(and (not Ext) (and IsDir fsi(zip)))"),
    ("recycled:", "recycled(*)"),
    ("recyclebinoriginalpath:", "recycled(*)"),
    ("recycled:report", "recycled(sub:path'report')"),
    ("recycled:c:/users/*", "recycled(wild:path'c:\\users\\*')"),
    // ---- system index ----
    ("si:foo", "si(\"foo\")"),
    ("system-index:\"my content\"", "si(\"my content\")"),
    ("systemindex:kind:music", "si(\"kind:music\")"),
    ("si:a;b", "si(\"a;b\")"),
    ("si:", "si(\"\")"),
    ("si*:\"a phrase\"", "si(\"\\\"a phrase\\\"\")"),
    ("si:\"&quot:a phrase&quot:\"", "si(\"\\\"a phrase\\\"\")"),
    ("ext:docx si:report", "(and Ext si(\"report\"))"),
    ("si:report *.docx", "(and si(\"report\") wild:'*.docx')"),
    ("!si:draft", "(not si(\"draft\"))"),
    // ---- M3: properties ----
    ("width:>4000", "width[4001..max]"),
    ("width:1920;3840", "width[1920..1921,3840..3841]"),
    ("width:", "width[1..max]"),
    ("width:unknown", "widthunknown"),
    ("height:100..200", "height[100..201]"),
    ("notindexed:width:>10", "width[11..max]+notindexed"),
    ("bit-rate:320kbps", "bit-rate[320..321]"),
    ("sample-rate:44.1khz", "sample-rate[44100..44200]"),
    ("machine:x64", "machine[34404..34405]"),
    ("is-dll:", "is-dll[1..max]"),
    ("is-dll:no", "is-dll[0..1]"),
    ("orientation:rotate-90", "orientation[6..7]"),
    ("length:1min..5min", "length[60000..360000]"),
    ("length:3:25", "length[205000..206000]"),
    ("duration:>1h", "length[7200000..max]"),
    ("artist:\"John Doe\"", "artist(sub:'John Doe')"),
    ("artist:", "artistknown"),
    ("artist:unknown", "artistunknown"),
    ("artist:a;b", "(or artist(sub:'a') artist(sub:'b'))"),
    ("wfn:album:abc", "album(eq:'abc')"),
    ("tags:sea*", "tags(wild:'sea*')"),
    ("allstrings:tags:s", "tags(sub:'s')+all"),
    ("keywords:x", "tags(sub:'x')"),
    ("len:album:>20", "albumlen[21..max]"),
    ("count:tags:2", "tagscount[2..3]"),
    ("atoi:file-version:>9", "file-versionatoi[10..max]"),
    ("camera-model:", "camera-modelknown"),
    ("cameramodel:eos", "camera-model(sub:'eos')"),
    ("file-version:10.0", "file-version(sub:'10.0')"),
    ("page-count:>10", "page-count[11..max]"),
    ("word-count:<100", "word-count[min..100]"),
    ("dimensions:1920x1080", "dimensionsdims[1920..1921]x[1080..1081]"),
    ("dimensions:>=3840x2160", "dimensionsdims[3840..max]x[2160..max]"),
    ("dimensions:800x600..1024x768", "dimensionsdims[800..1025]x[600..769]"),
    ("dimensions:!800x600", "(not dimensionsdims[800..801]x[600..601])"),
    ("dimensions:unknown", "dimensionsunknown"),
    ("dimensions:big", "invalid"),
    ("aspect-ratio:landscape", "aspect-ratio[1]"),
    ("aspect-ratio:16:9", "aspect-ratio[1]"),
    ("date-taken:2026", "date-taken[1]"),
    ("f-stop:2.8", "f-stop[1]"),
    ("exposure-time:1/250", "exposure-time[1]"),
    ("rating:bogus", "invalid"),
    // ---- M3: content and hashes ----
    ("content:\"quarterly plan\"", "(and IsDir content:Indexed(sub:'quarterly plan'))"),
    ("case:content:Plan", "(and IsDir content:Indexed(sub:'Plan'+case))"),
    ("regex:content:^Start", "(and IsDir content:Indexed(re:'^Start'))"),
    ("ww:content:plan", "(and IsDir content:Indexed(sub:'plan'+ww))"),
    ("content:a\\b/c", "(and IsDir content:Indexed(sub:'a\\b/c'))"),
    ("content:a;b", "(and IsDir (or content:Indexed(sub:'a') content:Indexed(sub:'b')))"),
    ("content:", "*"),
    ("utf8content:x", "(and IsDir content:Plain(Utf8)(sub:'x'))"),
    ("ansicontent:x", "(and IsDir content:Plain(Ansi)(sub:'x'))"),
    ("utf16content:x", "(and IsDir content:Plain(Utf16Le)(sub:'x'))"),
    ("utf16becontent:x", "(and IsDir content:Plain(Utf16Be)(sub:'x'))"),
    ("textplain:x", "(and IsDir content:Plain(Auto)(sub:'x'))"),
    ("utf16:content:x", "(and IsDir content:Plain(Utf16Le)(sub:'x'))"),
    ("binary:content:\"MZ\\x90\\x00\"", "(and IsDir content:Binary(sub:'MZ\\x90\\x00')[4d, 5a, 90, 0])"),
    ("contenthits:>2 content:x", "(and (and IsDir content:Indexed(sub:'x')) contenthits[1](1))"),
    ("contenthits:>2", "invalid"),
    ("sha256:ba78", "(and IsDir sha256(ba78))"),
    ("md5:", "(and IsDir md5())"),
    ("crc32:unknown", "(and IsDir crc32()unknown)"),
    ("sha1:a;b", "(and IsDir sha1(a,b))"),
    ("crc64:xyz", "invalid"),
    // ---- M3: set functions are hoisted out of the tree ----
    ("dupe:", "*"),
    ("dupe:size abc", "sub:'abc'"),
    ("<dupe: | abc>", "(or invalid sub:'abc')"),
];

#[test]
fn planner_cases() {
    let mut failures = Vec::new();
    for (input, expected) in PLAN_CASES {
        let got = psexp(&plan_of(input));
        if got != *expected {
            failures.push(format!("{input:?}\n    expected {expected}\n    got      {got}"));
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn planner_pinyin_default_and_omit() {
    use crate::plan::OmitSet;
    let env = PlanEnv { pinyin_default: true, ..PlanEnv::default() };
    let pl = |q: &str, env: &PlanEnv| psexp(&plan(&parse(q, &ParseEnv::default()).ast, env, None).root);
    assert_eq!(pl("jdbg", &env), "sub:'jdbg'+py");
    assert_eq!(pl("nopinyin:jdbg", &env), "sub:'jdbg'");
    assert_eq!(pl("*.jpg", &env), "wild:'*.jpg'");
    assert_eq!(pl("ww:abc", &env), "sub:'abc'+ww");
    // Functions are not affected by the default.
    assert_eq!(pl("name:abc", &env), "sub:'abc'");
    // Pinyin plans do not reuse the previous result set (not monotonic).
    let a = plan(&parse("ji", &ParseEnv::default()).ast, &env, None);
    let b = plan(&parse("jid", &ParseEnv::default()).ast, &env, Some(&a));
    assert_eq!(b.input, InputSet::All);
    let omit = OmitSet { instance: 1, ids: vec![fnc_core::EntryId(3)].into() };
    let env = PlanEnv { omit: Some(omit), ..PlanEnv::default() };
    assert_eq!(pl("abc", &env), "(and (not Omitted) sub:'abc')");
    assert_eq!(pl("", &env), "(not Omitted)");
    assert_eq!(pl("omit:", &env), "Omitted");
    assert_eq!(pl("!omit: abc", &env), "(and (not Omitted) sub:'abc')");
}

#[test]
fn templates_and_numeric_values() {
    use crate::plan::{EntryVar, PathPiece, parse_template};
    assert_eq!(
        parse_template("$path:\\$Stem:.srt"),
        vec![
            PathPiece::Var(EntryVar::Path),
            PathPiece::Lit("\\".into()),
            PathPiece::Var(EntryVar::Stem),
            PathPiece::Lit(".srt".into())
        ]
    );
    assert_eq!(parse_template("a$unknown:b"), vec![PathPiece::Lit("a$unknown:b".into())]);
    assert_eq!(parse_template("$$name:"), vec![PathPiece::Lit("$".into()), PathPiece::Var(EntryVar::Name)]);
    assert_eq!(parse_template("100$"), vec![PathPiece::Lit("100$".into())]);
    assert_eq!(parse_template("$full-path:"), vec![PathPiece::Var(EntryVar::FullPath)]);
    assert_eq!(parse_template("$ext:$parent:"), vec![PathPiece::Var(EntryVar::Ext), PathPiece::Var(EntryVar::Parent)]);
}

#[test]
fn builtin_macros_match_spec_lists() {
    use crate::funcs::{MacroId, builtin_macro_exts};
    use std::collections::BTreeSet;
    // SPEC ch.2 §6, verbatim (the SPEC lists `rmi` twice for audio; `r00…r29` expanded).
    let r_parts: String = (0..30).map(|i| format!("r{i:02};")).collect();
    let spec: [(MacroId, String); 6] = [
        (
            MacroId::Audio,
            "aac;ac3;aif;aifc;aiff;au;cda;dts;fla;flac;it;m1a;m2a;m3u;m4a;mid;midi;mka;mod;mp2;mp3;mpa;ogg;opus;ra;rmi;spc;rmi;snd;umx;voc;wav;wma;xm".into(),
        ),
        (
            MacroId::Zip,
            format!("7z;ace;arj;bz2;cab;gz;gzip;jar;{r_parts}rar;tar;tgz;z;zip;zst;xz;lz;lzma;iso;img;dmg"),
        ),
        (
            MacroId::Doc,
            "c;chm;cpp;csv;cxx;doc;docm;docx;dot;dotm;dotx;h;hpp;htm;html;hxx;ini;java;lua;mht;mhtml;odt;pdf;potx;potm;ppam;ppsm;ppsx;pps;ppt;pptm;pptx;rtf;sldm;sldx;thmx;txt;vsd;wpd;wps;wri;xlam;xls;xlsb;xlsm;xlsx;xltm;xltx;xml;md;rs;py;ts;js;go;json;toml;yaml;yml".into(),
        ),
        (MacroId::Exe, "bat;cmd;exe;msi;msp;scr;com;ps1;vbs".into()),
        (
            MacroId::Image,
            "ani;bmp;gif;ico;jpe;jpeg;jpg;pcx;png;psd;tga;tif;tiff;webp;heic;heif;avif;svg;raw;cr2;nef;arw;dng".into(),
        ),
        (
            MacroId::Video,
            "3g2;3gp;3gp2;3gpp;amr;amv;asf;avi;bdmv;bik;d2v;divx;drc;dsa;dsm;dss;dsv;evo;f4v;flc;fli;flic;flv;hdmov;ifo;ivf;m1v;m2p;m2t;m2ts;m2v;m4b;m4p;m4v;mkv;mp2v;mp4;mp4v;mpe;mpeg;mpg;mpls;mpv2;mpv4;mov;mts;ogm;ogv;pss;pva;qt;ram;ratdvd;rm;rmm;rmvb;roq;rpm;smil;smk;swf;tp;tpr;ts;vob;vp6;webm;wm;wmp;wmv".into(),
        ),
    ];
    for (id, list) in spec {
        let want: BTreeSet<&str> = list.split(';').collect();
        let got: BTreeSet<&str> = builtin_macro_exts(id).split(';').collect();
        assert_eq!(got, want, "{id:?}");
        // No duplicates in our table.
        assert_eq!(builtin_macro_exts(id).split(';').count(), got.len(), "{id:?}");
    }
    // `pic:` / `image:` / `picture:` and `zip:` / `compressed:` are the same macro.
    assert_eq!(p("pic:"), "(macro pic ∅)");
    assert!(matches!(plan_of("picture:"), Node::Leaf(Pred::Ext { .. })));
}

#[test]
fn user_macro_expansion() {
    let macros = vec![
        UserMacro { name: "Music".into(), search: "ext:mp3;flac".into() },
        UserMacro { name: "py".into(), search: "nothing".into() },
        UserMacro { name: "pyx".into(), search: "::pinyin:#param:".into() },
        UserMacro { name: "both".into(), search: "music: #param:".into() },
        UserMacro { name: "a1".into(), search: "a2:".into() },
        UserMacro { name: "a2".into(), search: "a3:".into() },
        UserMacro { name: "a3".into(), search: "x".into() },
    ];
    let env = ParseEnv { user_macros: &macros, ..ParseEnv::default() };
    let body = |q: &str| match parse(q, &env).ast {
        Expr::Macro(_, b) => sexp(&b),
        other => sexp(&other),
    };
    assert_eq!(body("pyx:bj"), "pinyin:'bj'");
    // Modifiers win over user macros of the same name.
    assert_eq!(body("py:bj"), "pinyin:'bj'");
    assert_eq!(body("both:beatles"), "(and (macro music ∅) 'beatles')");
    assert_eq!(body("a1:"), "(macro a2 ∅)");
    assert!(parse("a1:", &env).diagnostics.is_empty());
    assert_eq!(body("music:\"abbey road\""), "(and ext={'mp3','flac'} 'abbey road')");
    // Expansion depth is bounded by MAX_MACRO_DEPTH (8).
    let chain: Vec<UserMacro> =
        (0..12).map(|i| UserMacro { name: format!("m{i}"), search: format!("m{}: ok{i}", i + 1) }).collect();
    let env = ParseEnv { user_macros: &chain, ..ParseEnv::default() };
    let d = parse("m0:", &env).diagnostics;
    assert!(d.iter().any(|d| matches!(d.kind, DiagKind::MacroTooDeep(_))), "{d:?}");
    let short: Vec<UserMacro> = chain[4..].to_vec();
    let env = ParseEnv { user_macros: &short, ..ParseEnv::default() };
    assert!(parse("m4:", &env).diagnostics.is_empty());
}

#[test]
fn display_round_trips_new_syntax() {
    use crate::display::to_query_string;
    for q in [
        "descendant:*.jpg",
        "sibling:readme siblingcount:>2",
        "parent-sibling:.git",
        "fs:>1gb",
        "filelist:\"a.txt|b.txt\"",
        "fileexists:$stem:.srt",
        "dm:*-*-*T9:00..*-*-*T17:00",
        "py:jdbg fuzzy:reprot ascii:x",
        "len:album:>20",
        "<folder:> folder:",
        "type:\"Text Document\"",
        "nopinyin:a",
        "parent+3:\"c:\\a b\" !parent+0:x",
        "fsi:jpg;png target:*.exe recycled: filelistfilename:a.efu",
    ] {
        let a = parse(q, &ParseEnv::default()).ast;
        let s = to_query_string(&a, Precedence::Or);
        assert_eq!(parse(&s, &ParseEnv::default()).ast, a, "{q:?} printed as {s:?}");
    }
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
    assert!(matches!(plan_of("width:>100"), Node::Leaf(Pred::Prop(_))));
    assert!(matches!(plan_of("target:x"), Node::Leaf(Pred::LinkTarget(Some(_)))));
    assert!(matches!(plan_of("random:x"), Node::Leaf(Pred::Unsupported(_))));
    assert!(matches!(plan_of("si:x"), Node::Leaf(Pred::SystemIndex(_))));
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

#[test]
fn hostile_inputs_do_not_panic_or_overflow() {
    // Deep nesting is bounded (no stack overflow even on small thread stacks).
    let deep = "<".repeat(100_000) + "a" + &">".repeat(100_000);
    let t = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
        let p = parse(&deep, &ParseEnv::default());
        let _ = crate::display::to_query_string(&p.ast, Precedence::Or);
        let _ = plan(&p.ast, &PlanEnv::default(), None);
        p.diagnostics.iter().any(|d| d.kind == DiagKind::TooDeep)
    });
    assert!(t.unwrap().join().unwrap(), "deep nesting must be reported");
    // Non-ASCII and absurd date values plan to "invalid", never panic.
    for q in [
        "dm:今天",
        "dm:中中",
        "dm:123é456",
        "dm:100000000days",
        "dm:200000000weeks",
        "dm:30000years",
        "dm:4294967295years",
        "dm:99999999",
        "dc:0000-00-00",
        "dm:2026-13-45T99:99",
        "dm:..",
        "dm:>",
    ] {
        let _ = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
    }
    // Sizes beyond i64::MAX clamp instead of wrapping to "match everything".
    match plan_of("size:>9000pb") {
        Node::Leaf(Pred::Num(_, p)) => assert!(p.ranges.iter().all(|&(lo, _)| lo > 0), "{p:?}"),
        other => panic!("{other:?}"),
    }
}

mod fuzz_values {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Any function value plans without panicking.
        #[test]
        fn function_values_never_panic(f in prop_oneof![Just("dm"), Just("size"), Just("depth"), Just("attrib"), Just("ext"), Just("childcount")], v in "\\PC{0,16}") {
            let q = format!("{f}:{v}");
            let _ = plan(&parse(&q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        }
    }
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

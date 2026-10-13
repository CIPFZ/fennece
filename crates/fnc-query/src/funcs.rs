//! Built-in function, modifier and macro tables (SPEC ch.2 §3, §5, §6, appendix A).
//!
//! Names are matched after normalisation: ASCII lowercase with `-` and `_` removed,
//! so `date-modified:`, `DateModified:` and `datemodified:` are the same function.

use std::collections::HashMap;
use std::sync::LazyLock;

use fnc_props::{PropType, PropertyId};

use crate::ast::Modifier;

/// Kind of value a function takes; drives value parsing in the planner.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// Optional text; no value = boolean filter (`file:`, `folder:`).
    Flag,
    /// Text matched like a name.
    Text,
    /// `;` list of exact texts (`ext:`).
    TextList,
    /// Integer.
    Number,
    /// Size with units.
    Size,
    /// Date/time.
    Date,
    /// Duration.
    Duration,
    /// Attribute letters.
    Attrib,
}

macro_rules! funcs {
    ($( $id:ident : $kind:ident = [$($name:literal),+ $(,)?] ),+ $(,)?) => {
        /// Built-in search function.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[allow(missing_docs)]
        pub enum FuncId {
            $($id,)+
            /// A property from the property table (`width:`, `artist:`, `file-version:`).
            Prop(PropertyId),
        }

        impl FuncId {
            /// Value kind.
            pub fn kind(self) -> ValueKind {
                match self {
                    $(FuncId::$id => ValueKind::$kind,)+
                    FuncId::Prop(p) => prop_kind(p),
                }
            }
            /// Canonical name (first alias; the canonical property name for properties).
            pub fn name(self) -> &'static str {
                match self {
                    $(FuncId::$id => funcs!(@first $($name),+),)+
                    FuncId::Prop(p) => p.name(),
                }
            }
            /// All aliases (for properties: the aliases besides the canonical name).
            pub fn aliases(self) -> &'static [&'static str] {
                match self {
                    $(FuncId::$id => &[$($name),+],)+
                    FuncId::Prop(p) => p.def().map_or(&[], |d| d.aliases),
                }
            }
        }

        /// Built-in `(normalised name, function)` pairs.
        static BUILTIN_FUNCTIONS: &[(&str, FuncId)] = &[
            $($(($name, FuncId::$id),)+)+
        ];
    };
    (@first $first:literal $(, $rest:literal)*) => { $first };
}

/// Value kind of a property function.
fn prop_kind(p: PropertyId) -> ValueKind {
    if p == PropertyId::DIMENSIONS {
        // `dimensions:>=1920x1080`: comparison operators are part of the value.
        return ValueKind::Number;
    }
    match p.ty() {
        PropType::Int | PropType::Float | PropType::Bool => ValueKind::Number,
        PropType::Date => ValueKind::Date,
        PropType::Duration => ValueKind::Duration,
        PropType::Text | PropType::MultiText => ValueKind::Text,
    }
}

/// `(normalised name, function)` for every alias: built-in functions, then every property
/// name of the property table (SPEC ch.2 appendix A/B).
pub static FUNCTION_TABLE: LazyLock<Vec<(&'static str, FuncId)>> = LazyLock::new(|| {
    let mut v: Vec<(&'static str, FuncId)> = BUILTIN_FUNCTIONS.to_vec();
    for (name, p) in PropertyId::search_names() {
        if !v.iter().any(|(n, _)| *n == name) {
            // One-time table construction: property names become `'static` like the
            // built-in ones (a few hundred bytes, never freed by design).
            v.push((String::leak(name), FuncId::Prop(p)));
        }
    }
    v
});

static FUNCTION_INDEX: LazyLock<HashMap<&'static str, FuncId>> =
    LazyLock::new(|| FUNCTION_TABLE.iter().map(|&(n, f)| (n, f)).collect());

funcs! {
    // ---- files and folders (core) ----
    File: Flag = ["file"],
    Folder: Flag = ["folder"],
    Name: Text = ["name"],
    Stem: Text = ["stem", "nameextstripped"],
    Ext: TextList = ["ext", "extension"],
    FullPath: Text = ["fullpath", "fp"],
    PathPart: Text = ["pathpart"],
    Parent: Text = ["parent", "infolder", "nosubfolders"],
    Ancestor: Text = ["ancestor", "ancestors", "parents"],
    Child: Text = ["child"],
    ChildCount: Number = ["childcount"],
    ChildFileCount: Number = ["childfilecount"],
    ChildFolderCount: Number = ["childfoldercount"],
    Descendant: Text = ["descendant"],
    DescendantCount: Number = ["descendantcount"],
    DescendantFileCount: Number = ["descendantfilecount"],
    DescendantFolderCount: Number = ["descendantfoldercount"],
    Sibling: Text = ["sibling"],
    SiblingCount: Number = ["siblingcount"],
    ParentSibling: Text = ["parentsibling"],
    AncestorSibling: Text = ["ancestorsibling"],
    Depth: Number = ["depth", "parentscount"],
    Empty: Flag = ["empty"],
    Root: Flag = ["root"],
    NameLength: Number = ["namelength"],
    PathLength: Number = ["pathlength"],
    FullPathLength: Number = ["fullpathlength"],
    Size: Size = ["size"],
    FolderSize: Size = ["foldersize", "fs"],
    DateModified: Date = ["dm", "datemodified"],
    DateCreated: Date = ["dc", "datecreated"],
    DateAccessed: Date = ["da", "dateaccessed"],
    DateRun: Date = ["dr", "daterun"],
    DateRecentlyChanged: Date = ["rc", "daterecentlychanged"],
    RunCount: Number = ["runcount"],
    Attrib: Attrib = ["attrib", "attributes", "a"],
    Type: Text = ["type"],
    Kind: Text = ["kind"],
    Volume: Text = ["volume", "drive"],
    VolumeKind: Text = ["volumekind"],
    Online: Flag = ["online"],
    Offline: Flag = ["offline"],
    HardlinkCount: Number = ["hardlinkcount", "hardlinks"],
    Reparse: Flag = ["reparse", "reparsetag"],
    Symlink: Flag = ["symlink"],
    Junction: Flag = ["junction"],
    Target: Text = ["target", "linktarget", "shortcuttarget"],
    Frn: Number = ["frn", "indexfrn"],
    NameParts: Number = ["nameparts"],
    FileSignature: Text = ["fsi", "filesignature"],
    Recycled: Flag = ["recycled", "recyclebinoriginalpath"],
    // ---- sets / combination ----
    Dupe: Text = ["dupe"],
    SizeDupe: Flag = ["sizedupe"],
    NamePartDupe: Flag = ["namepartdupe"],
    AttribDupe: Flag = ["attribdupe"],
    DaDupe: Flag = ["dadupe"],
    DcDupe: Flag = ["dcdupe"],
    DmDupe: Flag = ["dmdupe"],
    Distinct: Text = ["distinct"],
    Unique: Text = ["unique"],
    DupeCount: Number = ["dupecount"],
    DupeMax: Number = ["dupemax"],
    FileListFn: Text = ["filelist"],
    FileListFilename: Text = ["filelistfilename"],
    FileExists: Text = ["fileexists"],
    FolderExists: Text = ["folderexists"],
    Random: Number = ["random"],
    Omit: Flag = ["omit"],
    SystemIndex: Text = ["si", "systemindex"],
    Eval: Text = ["eval"],
    Weight: Number = ["weight"],
    // ---- content ----
    Content: Text = ["content"],
    Utf8Content: Text = ["utf8content"],
    AnsiContent: Text = ["ansicontent"],
    Utf16Content: Text = ["utf16content"],
    Utf16BeContent: Text = ["utf16becontent"],
    TextPlain: Text = ["textplain"],
    ContentHits: Number = ["contenthits"],
    // ---- hashes ----
    Crc32: Text = ["crc32"],
    Crc64: Text = ["crc64"],
    Md5: Text = ["md5"],
    Sha1: Text = ["sha1"],
    Sha256: Text = ["sha256"],
    Sha512: Text = ["sha512"],
    // Image, audio, video, document and executable properties (`width:`, `artist:`,
    // `line-count:`, ...) come from the property table: `FuncId::Prop`.
    // ---- semantic (Fennec extension, SPEC ch.5) ----
    Semantic: Text = ["sem", "semantic"],
    SemName: Text = ["semname"],
    SemContent: Text = ["semcontent"],
    SemCode: Text = ["semcode"],
    SemImage: Text = ["semimage"],
    SemMinScore: Number = ["semminscore"],
    SemTop: Number = ["semtop"],
}

/// Normalises an identifier: ASCII lowercase, `-`/`_` removed.
pub fn normalize_name(s: &str, out: &mut String) {
    out.clear();
    for c in s.chars() {
        if c != '-' && c != '_' {
            out.push(c.to_ascii_lowercase());
        }
    }
}

/// Looks up a function by (normalised) name.
pub fn lookup_function(norm: &str) -> Option<FuncId> {
    FUNCTION_INDEX.get(norm).copied()
}

/// `(name, modifier, on)` for every modifier spelling.
pub static MODIFIER_TABLE: &[(&str, Modifier, bool)] = &[
    ("case", Modifier::Case, true),
    ("nocase", Modifier::Case, false),
    ("diacritics", Modifier::Diacritics, true),
    ("diac", Modifier::Diacritics, true),
    ("nodiacritics", Modifier::Diacritics, false),
    ("nodiac", Modifier::Diacritics, false),
    ("path", Modifier::Path, true),
    ("nopath", Modifier::Path, false),
    ("wholeword", Modifier::WholeWord, true),
    ("ww", Modifier::WholeWord, true),
    ("nowholeword", Modifier::WholeWord, false),
    ("noww", Modifier::WholeWord, false),
    ("wholefilename", Modifier::WholeFilename, true),
    ("wfn", Modifier::WholeFilename, true),
    ("exact", Modifier::WholeFilename, true),
    ("nowholefilename", Modifier::WholeFilename, false),
    ("nowfn", Modifier::WholeFilename, false),
    ("noexact", Modifier::WholeFilename, false),
    ("wholepath", Modifier::WholePath, true),
    ("wp", Modifier::WholePath, true),
    ("nowholepath", Modifier::WholePath, false),
    ("nowp", Modifier::WholePath, false),
    ("regex", Modifier::Regex, true),
    ("noregex", Modifier::Regex, false),
    ("wildcards", Modifier::Wildcards, true),
    ("nowildcards", Modifier::Wildcards, false),
    ("wildcardsstarall", Modifier::WildcardsStarAll, true),
    ("nowildcardsstarall", Modifier::WildcardsStarAll, false),
    ("prefix", Modifier::Prefix, true),
    ("noprefix", Modifier::Prefix, false),
    ("suffix", Modifier::Suffix, true),
    ("nosuffix", Modifier::Suffix, false),
    ("startwith", Modifier::StartWith, true),
    ("sw", Modifier::StartWith, true),
    ("nostartwith", Modifier::StartWith, false),
    ("endwith", Modifier::EndWith, true),
    ("ew", Modifier::EndWith, true),
    ("noendwith", Modifier::EndWith, false),
    ("ignorepunctuation", Modifier::IgnorePunctuation, true),
    ("nopunc", Modifier::IgnorePunctuation, true),
    ("punc", Modifier::IgnorePunctuation, false),
    ("noignorepunctuation", Modifier::IgnorePunctuation, false),
    ("ignorewhitespace", Modifier::IgnoreWhitespace, true),
    ("nows", Modifier::IgnoreWhitespace, true),
    ("ws", Modifier::IgnoreWhitespace, false),
    ("noignorewhitespace", Modifier::IgnoreWhitespace, false),
    ("utf8", Modifier::Utf8, true),
    ("ansi", Modifier::Ansi, true),
    ("utf16", Modifier::Utf16, true),
    ("utf16be", Modifier::Utf16Be, true),
    ("binary", Modifier::Binary, true),
    ("bytestream", Modifier::Binary, true),
    ("nobinary", Modifier::Binary, false),
    ("ascii", Modifier::Ascii, true),
    ("noascii", Modifier::Ascii, false),
    ("dotall", Modifier::DotAll, true),
    ("nodotall", Modifier::DotAll, false),
    ("multiline", Modifier::Multiline, true),
    ("nomultiline", Modifier::Multiline, false),
    ("atoi", Modifier::Atoi, true),
    ("tointeger", Modifier::Atoi, true),
    ("toint", Modifier::Atoi, true),
    ("atof", Modifier::Atof, true),
    ("tonumber", Modifier::Atof, true),
    ("#", Modifier::Atof, true),
    ("allstrings", Modifier::AllStrings, true),
    ("anystrings", Modifier::AnyStrings, true),
    ("fileonly", Modifier::FileOnly, true),
    ("folderonly", Modifier::FolderOnly, true),
    ("highlight", Modifier::Highlight, true),
    ("nohighlight", Modifier::Highlight, false),
    ("len", Modifier::Len, true),
    ("count", Modifier::Count, true),
    ("notindexed", Modifier::NotIndexed, true),
    ("pinyin", Modifier::Pinyin, true),
    ("py", Modifier::Pinyin, true),
    ("pinyinquan", Modifier::PinyinQuan, true),
    ("pinyinjian", Modifier::PinyinJian, true),
    ("fuzzy", Modifier::Fuzzy, true),
    ("nofuzzy", Modifier::Fuzzy, false),
];

/// Looks up a modifier by (normalised) name. Besides the table, any modifier can be
/// negated with a `no` prefix (`nomultiline:`).
pub fn lookup_modifier(norm: &str) -> Option<(Modifier, bool)> {
    if let Some(&(_, m, on)) = MODIFIER_TABLE.iter().find(|(n, _, _)| *n == norm) {
        return Some((m, on));
    }
    let rest = norm.strip_prefix("no")?;
    MODIFIER_TABLE.iter().find(|(n, _, on)| *on && *n == rest).map(|&(_, m, _)| (m, false))
}

/// Query-wide switches usable as `?name:` or plain `name:` (they have no per-term
/// meaning).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Switch {
    /// `andprecedence:`
    AndPrecedence,
    /// `orprecedence:`
    OrPrecedence,
    /// `andfilter:`
    AndFilter,
    /// `orfilter:`
    OrFilter,
}

/// Looks up a query-wide switch.
pub fn lookup_switch(norm: &str) -> Option<Switch> {
    Some(match norm {
        "andprecedence" => Switch::AndPrecedence,
        "orprecedence" => Switch::OrPrecedence,
        "andfilter" => Switch::AndFilter,
        "orfilter" => Switch::OrFilter,
        _ => return None,
    })
}

/// Built-in macro.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MacroId {
    /// `audio:`
    Audio,
    /// `zip:`
    Zip,
    /// `doc:`
    Doc,
    /// `exe:`
    Exe,
    /// `image:` / `pic:`
    Image,
    /// `video:`
    Video,
    /// A user macro (index into `ParseEnv::user_macros`).
    User(u16),
}

impl MacroId {
    /// Canonical name.
    pub fn name(self) -> &'static str {
        match self {
            MacroId::Audio => "audio",
            MacroId::Zip => "zip",
            MacroId::Doc => "doc",
            MacroId::Exe => "exe",
            MacroId::Image => "image",
            MacroId::Video => "video",
            MacroId::User(_) => "user",
        }
    }
}

/// Built-in macro names.
pub static MACRO_TABLE: &[(&str, MacroId)] = &[
    ("audio", MacroId::Audio),
    ("zip", MacroId::Zip),
    ("compressed", MacroId::Zip),
    ("doc", MacroId::Doc),
    ("document", MacroId::Doc),
    ("exe", MacroId::Exe),
    ("executable", MacroId::Exe),
    ("image", MacroId::Image),
    ("pic", MacroId::Image),
    ("picture", MacroId::Image),
    ("video", MacroId::Video),
];

/// Extension lists of the built-in macros (SPEC ch.2 §6).
pub fn builtin_macro_exts(m: MacroId) -> &'static str {
    match m {
        MacroId::Audio => {
            "aac;ac3;aif;aifc;aiff;au;cda;dts;fla;flac;it;m1a;m2a;m3u;m4a;mid;midi;mka;mod;mp2;mp3;mpa;ogg;opus;ra;rmi;spc;snd;umx;voc;wav;wma;xm"
        }
        MacroId::Zip => {
            "7z;ace;arj;bz2;cab;gz;gzip;jar;r00;r01;r02;r03;r04;r05;r06;r07;r08;r09;r10;r11;r12;r13;r14;r15;r16;r17;r18;r19;r20;r21;r22;r23;r24;r25;r26;r27;r28;r29;rar;tar;tgz;z;zip;zst;xz;lz;lzma;iso;img;dmg"
        }
        MacroId::Doc => {
            "c;chm;cpp;csv;cxx;doc;docm;docx;dot;dotm;dotx;h;hpp;htm;html;hxx;ini;java;lua;mht;mhtml;odt;pdf;potx;potm;ppam;ppsm;ppsx;pps;ppt;pptm;pptx;rtf;sldm;sldx;thmx;txt;vsd;wpd;wps;wri;xlam;xls;xlsb;xlsm;xlsx;xltm;xltx;xml;md;rs;py;ts;js;go;json;toml;yaml;yml"
        }
        MacroId::Exe => "bat;cmd;exe;msi;msp;scr;com;ps1;vbs",
        MacroId::Image => {
            "ani;bmp;gif;ico;jpe;jpeg;jpg;pcx;png;psd;tga;tif;tiff;webp;heic;heif;avif;svg;raw;cr2;nef;arw;dng"
        }
        MacroId::Video => {
            "3g2;3gp;3gp2;3gpp;amr;amv;asf;avi;bdmv;bik;d2v;divx;drc;dsa;dsm;dss;dsv;evo;f4v;flc;fli;flic;flv;hdmov;ifo;ivf;m1v;m2p;m2t;m2ts;m2v;m4b;m4p;m4v;mkv;mp2v;mp4;mp4v;mpe;mpeg;mpg;mpls;mpv2;mpv4;mov;mts;ogm;ogv;pss;pva;qt;ram;ratdvd;rm;rmm;rmvb;roq;rpm;smil;smk;swf;tp;tpr;ts;vob;vp6;webm;wm;wmp;wmv"
        }
        MacroId::User(_) => "",
    }
}

/// Looks up a built-in macro.
pub fn lookup_macro(norm: &str) -> Option<MacroId> {
    MACRO_TABLE.iter().find(|(n, _)| *n == norm).map(|&(_, m)| m)
}

/// Character entities (`&name:`), SPEC ch.2 §1.1.
pub fn lookup_entity(name: &str) -> Option<char> {
    Some(match name {
        "sp" => ' ',
        "vert" => '|',
        "excl" => '!',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "amp" => '&',
        "colon" => ':',
        "percent" => '%',
        "semi" => ';',
        _ => {
            let num = name.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse::<u32>().ok()?,
            };
            return char::from_u32(code);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_are_normalised_and_unique() {
        let mut seen = std::collections::HashSet::new();
        let mut norm = String::new();
        for (n, _) in FUNCTION_TABLE.iter() {
            normalize_name(n, &mut norm);
            assert_eq!(&norm, n, "function alias must be stored normalised");
            assert!(seen.insert(*n), "duplicate function alias {n}");
        }
        for (n, p) in PropertyId::search_names() {
            assert_eq!(lookup_function(&n), Some(FuncId::Prop(p)), "property name {n} shadowed");
        }
        for (n, _, _) in MODIFIER_TABLE {
            assert!(seen.insert(*n), "modifier {n} collides with a function or modifier");
        }
        for (n, _) in MACRO_TABLE {
            assert!(seen.insert(*n), "macro {n} collides");
        }
    }

    #[test]
    fn lookups() {
        let mut n = String::new();
        normalize_name("Date-Modified", &mut n);
        assert_eq!(lookup_function(&n), Some(FuncId::DateModified));
        assert_eq!(lookup_function("dm"), Some(FuncId::DateModified));
        assert_eq!(lookup_modifier("nocase"), Some((Modifier::Case, false)));
        assert_eq!(lookup_macro("pic"), Some(MacroId::Image));
        assert_eq!(lookup_entity("quot"), Some('"'));
        assert_eq!(lookup_entity("#65"), Some('A'));
        assert_eq!(lookup_entity("#x263a"), Some('☺'));
        assert_eq!(lookup_entity("nope"), None);
        assert_eq!(FuncId::Ext.kind(), ValueKind::TextList);
        assert_eq!(FuncId::Size.name(), "size");
    }
}

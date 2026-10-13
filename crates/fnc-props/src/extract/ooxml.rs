//! Office Open XML (docx/xlsx/pptx): `docProps/core.xml` + `docProps/app.xml` metadata
//! and body text for the content index.

use std::io::{Read, Seek};
use std::path::Path;

use fnc_core::FileTime;
use zip::ZipArchive;

use super::xml::{body_text, leaf_texts};
use super::{ExtractLimits, Props, open, push_text};
use crate::schema::PropertyId as P;
use crate::value::Value;

/// Largest XML part read (uncompressed).
const MAX_PART: u64 = 32 << 20;

fn part<R: Read + Seek>(z: &mut ZipArchive<R>, name: &str, limit: u64) -> Option<String> {
    let f = z.by_name(name).ok()?;
    if f.size() > limit.min(MAX_PART) {
        return None;
    }
    let mut s = String::new();
    f.take(limit.min(MAX_PART)).read_to_string(&mut s).ok()?;
    Some(s)
}

/// W3CDTF date (`2026-07-02T10:52:00Z`, `2026-07-02`).
fn w3c_date(s: &str) -> Option<FileTime> {
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(FileTime::from_utc(dt.with_timezone(&chrono::Utc)));
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S") {
        return Some(FileTime::from_utc(dt.and_utc()));
    }
    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    Some(FileTime::from_utc(d.and_hms_opt(0, 0, 0)?.and_utc()))
}

/// Metadata of the OOXML file at `path`.
pub fn extract_props(path: &Path, limits: ExtractLimits, out: &mut Props) {
    let Ok(r) = open(path, limits) else { return };
    let Ok(mut z) = ZipArchive::new(r) else { return };
    props_from_zip(&mut z, limits.max_read_bytes, out);
}

/// Metadata from an open archive.
pub fn props_from_zip<R: Read + Seek>(z: &mut ZipArchive<R>, limit: u64, out: &mut Props) {
    if let Some(core) = part(z, "docProps/core.xml", limit) {
        for (k, v) in leaf_texts(&core) {
            match k.as_str() {
                "title" => push_text(out, P::TITLE, &v),
                "subject" => push_text(out, P::SUBJECT, &v),
                "creator" => {
                    if let Some(m) = Value::multi(v.split(';')) {
                        out.push((P::AUTHOR, m));
                    }
                }
                "keywords" => {
                    if let Some(m) = Value::multi(v.split([';', ','])) {
                        out.push((P::TAGS, m));
                    }
                }
                "description" => push_text(out, P::COMMENT, &v),
                "lastModifiedBy" => push_text(out, P::LAST_AUTHOR, &v),
                "revision" => push_text(out, P::REVISION_NUMBER, &v),
                "category" => push_text(out, P::CATEGORY, &v),
                "contentStatus" => push_text(out, P::CONTENT_STATUS, &v),
                "version" => push_text(out, P::DOCUMENT_VERSION, &v),
                "language" => push_text(out, P::LANGUAGE, &v),
                "created" => {
                    if let Some(d) = w3c_date(&v) {
                        out.push((P::CONTENT_CREATED, Value::Date(d)));
                    }
                }
                "modified" => {
                    if let Some(d) = w3c_date(&v) {
                        out.push((P::DATE_LAST_SAVED, Value::Date(d)));
                    }
                }
                "lastPrinted" => {
                    if let Some(d) = w3c_date(&v) {
                        out.push((P::DATE_PRINTED, Value::Date(d)));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(app) = part(z, "docProps/app.xml", limit) {
        let int = |v: &str| v.trim().parse::<i64>().ok().filter(|n| *n >= 0);
        for (k, v) in leaf_texts(&app) {
            let p = match k.as_str() {
                "Pages" => P::PAGE_COUNT,
                "Words" => P::WORD_COUNT,
                "Characters" => P::CHAR_COUNT,
                "Lines" => P::LINE_COUNT,
                "Paragraphs" => P::PARAGRAPH_COUNT,
                "Slides" => P::SLIDE_COUNT,
                "DocSecurity" => P::SECURITY,
                "HiddenSlides" => P::HIDDEN_SLIDE_COUNT,
                "Notes" => P::NOTE_COUNT,
                "MMClips" => P::MULTIMEDIA_CLIP_COUNT,
                "Application" => {
                    push_text(out, P::PROGRAM_NAME, &v);
                    continue;
                }
                "PresentationFormat" => {
                    push_text(out, P::PRESENTATION_FORMAT, &v);
                    continue;
                }
                "TotalTime" => {
                    // Minutes.
                    if let Some(m) = int(&v) {
                        out.push((P::TOTAL_EDITING_TIME, Value::Duration(m.saturating_mul(60_000))));
                    }
                    continue;
                }
                "Company" => {
                    push_text(out, P::COMPANY, &v);
                    continue;
                }
                "Manager" => {
                    push_text(out, P::MANAGER, &v);
                    continue;
                }
                "Template" => {
                    push_text(out, P::TEMPLATE, &v);
                    continue;
                }
                _ => continue,
            };
            if let Some(n) = int(&v) {
                out.push((p, Value::Int(n)));
            }
        }
    }
}

/// Body text of the OOXML file at `path` (document, shared strings + sheets, slides),
/// at most `max_bytes`.
pub fn extract_text(path: &Path, limits: ExtractLimits, max_bytes: usize) -> Option<String> {
    let r = open(path, limits).ok()?;
    let mut z = ZipArchive::new(r).ok()?;
    text_from_zip(&mut z, limits.max_read_bytes, max_bytes)
}

/// Body text from an open archive.
pub fn text_from_zip<R: Read + Seek>(z: &mut ZipArchive<R>, limit: u64, max_bytes: usize) -> Option<String> {
    let names: Vec<String> = z.file_names().filter_map(Result::ok).map(|n| n.into_owned()).collect();
    let mut out = String::new();
    let mut parts: Vec<&str> = Vec::new();
    let numbered = |prefix: &str| {
        let mut v: Vec<(u32, &str)> = names
            .iter()
            .filter_map(|n| {
                let rest = n.strip_prefix(prefix)?.strip_suffix(".xml")?;
                rest.parse::<u32>().ok().map(|k| (k, n.as_str()))
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, n)| n).collect::<Vec<&str>>()
    };
    if names.iter().any(|n| n == "word/document.xml") {
        parts.push("word/document.xml");
        parts.extend(numbered("word/header"));
        parts.extend(numbered("word/footer"));
        if names.iter().any(|n| n == "word/footnotes.xml") {
            parts.push("word/footnotes.xml");
        }
        for p in parts {
            if let Some(x) = part(z, p, limit)
                && !body_text(&x, &[b"t"], &[b"p"], &mut out, max_bytes)
            {
                break;
            }
        }
    } else if names.iter().any(|n| n == "xl/workbook.xml") {
        if let Some(x) = part(z, "xl/sharedStrings.xml", limit) {
            body_text(&x, &[b"t"], &[b"si"], &mut out, max_bytes);
        }
        // Inline strings and literal cell values (numbers, formulas' cached results).
        for p in numbered("xl/worksheets/sheet") {
            if out.len() >= max_bytes {
                break;
            }
            if let Some(x) = part(z, p, limit) {
                // SPEC-GAP: shared-string cells contribute their index too (`<v>0</v>`).
                body_text(&x, &[b"t", b"v"], &[b"row", b"is", b"c"], &mut out, max_bytes);
            }
        }
    } else if names.iter().any(|n| n == "ppt/presentation.xml") {
        for p in numbered("ppt/slides/slide") {
            if let Some(x) = part(z, p, limit)
                && !body_text(&x, &[b"t"], &[b"p"], &mut out, max_bytes)
            {
                break;
            }
        }
    } else {
        return None;
    }
    truncate_utf8(&mut out, max_bytes);
    Some(out)
}

/// Truncates at a char boundary.
pub fn truncate_utf8(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::extract::testutil::file;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    pub(crate) fn zip_of(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in parts {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    const CORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<dc:title>Quarterly Plan</dc:title><dc:subject>Planning</dc:subject><dc:creator>Alice; Bob</dc:creator>
<cp:keywords>plan, q3</cp:keywords><dc:description>Draft &amp; notes</dc:description><cp:lastModifiedBy>Carol</cp:lastModifiedBy>
<cp:revision>7</cp:revision><cp:category>Reports</cp:category><cp:contentStatus>Draft</cp:contentStatus><cp:version>1.2</cp:version>
<dcterms:created xsi:type="dcterms:W3CDTF">2026-01-15T09:30:00Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2026-02-01T10:00:00Z</dcterms:modified>
</cp:coreProperties>"#;
    const APP: &str = r#"<?xml version="1.0"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
<Template>Normal.dotm</Template><TotalTime>42</TotalTime><Pages>3</Pages><Words>1234</Words><Characters>7000</Characters>
<Company>Fennec Corp</Company><Lines>80</Lines><Paragraphs>20</Paragraphs><DocSecurity>0</DocSecurity><Manager>Dana</Manager><Application>Microsoft Office Word</Application>
<HiddenSlides>2</HiddenSlides><Notes>4</Notes><MMClips>1</MMClips><PresentationFormat>Widescreen</PresentationFormat></Properties>"#;

    pub(crate) fn docx(body: &str) -> Vec<u8> {
        let doc = format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body><w:p><w:r><w:t>{body}</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p></w:body></w:document>"#
        );
        zip_of(&[
            ("[Content_Types].xml", "<Types/>"),
            ("word/document.xml", &doc),
            ("docProps/core.xml", CORE),
            ("docProps/app.xml", APP),
        ])
    }

    pub(crate) fn xlsx() -> Vec<u8> {
        zip_of(&[
            ("xl/workbook.xml", "<workbook/>"),
            ("xl/sharedStrings.xml", r#"<sst><si><t>Revenue</t></si><si><r><t>Cost</t></r><r><t>s</t></r></si></sst>"#),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c><v>42.5</v></c><c t="inlineStr"><is><t>inline text</t></is></c></row></sheetData></worksheet>"#,
            ),
        ])
    }

    pub(crate) fn pptx() -> Vec<u8> {
        zip_of(&[
            ("ppt/presentation.xml", "<p:presentation/>"),
            ("ppt/slides/slide2.xml", r#"<p:sld><a:p><a:r><a:t>Second slide</a:t></a:r></a:p></p:sld>"#),
            ("ppt/slides/slide1.xml", r#"<p:sld><a:p><a:r><a:t>Title slide</a:t></a:r></a:p></p:sld>"#),
            ("ppt/slides/slide10.xml", r#"<p:sld><a:p><a:r><a:t>Tenth</a:t></a:r></a:p></p:sld>"#),
        ])
    }

    fn get(v: &Props, p: P) -> Option<&Value> {
        v.iter().find(|(q, _)| *q == p).map(|(_, v)| v)
    }

    #[test]
    fn docx_metadata() {
        let (_d, p) = file("plan.docx", &docx("Hello"));
        let mut out = Props::new();
        extract_props(&p, ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::TITLE), Some(&Value::Text("Quarterly Plan".into())));
        assert_eq!(get(&out, P::SUBJECT), Some(&Value::Text("Planning".into())));
        assert_eq!(get(&out, P::AUTHOR), Value::multi(["Alice", "Bob"]).as_ref());
        assert_eq!(get(&out, P::TAGS), Value::multi(["plan", "q3"]).as_ref());
        assert_eq!(get(&out, P::COMMENT), Some(&Value::Text("Draft & notes".into())));
        assert_eq!(get(&out, P::LAST_AUTHOR), Some(&Value::Text("Carol".into())));
        assert_eq!(get(&out, P::REVISION_NUMBER), Some(&Value::Text("7".into())));
        assert_eq!(get(&out, P::CATEGORY), Some(&Value::Text("Reports".into())));
        assert!(matches!(get(&out, P::CONTENT_CREATED), Some(Value::Date(_))));
        assert!(matches!(get(&out, P::DATE_LAST_SAVED), Some(Value::Date(_))));
        assert_eq!(get(&out, P::PAGE_COUNT), Some(&Value::Int(3)));
        assert_eq!(get(&out, P::WORD_COUNT), Some(&Value::Int(1234)));
        assert_eq!(get(&out, P::CHAR_COUNT), Some(&Value::Int(7000)));
        assert_eq!(get(&out, P::LINE_COUNT), Some(&Value::Int(80)));
        assert_eq!(get(&out, P::PARAGRAPH_COUNT), Some(&Value::Int(20)));
        assert_eq!(get(&out, P::SECURITY), Some(&Value::Int(0)));
        assert_eq!(get(&out, P::TOTAL_EDITING_TIME), Some(&Value::Duration(42 * 60_000)));
        assert_eq!(get(&out, P::COMPANY), Some(&Value::Text("Fennec Corp".into())));
        assert_eq!(get(&out, P::MANAGER), Some(&Value::Text("Dana".into())));
        assert_eq!(get(&out, P::TEMPLATE), Some(&Value::Text("Normal.dotm".into())));
        assert_eq!(get(&out, P::CONTENT_STATUS), Some(&Value::Text("Draft".into())));
        assert_eq!(get(&out, P::DOCUMENT_VERSION), Some(&Value::Text("1.2".into())));
        assert_eq!(get(&out, P::PROGRAM_NAME), Some(&Value::Text("Microsoft Office Word".into())));
        assert_eq!(get(&out, P::HIDDEN_SLIDE_COUNT), Some(&Value::Int(2)));
        assert_eq!(get(&out, P::NOTE_COUNT), Some(&Value::Int(4)));
        assert_eq!(get(&out, P::MULTIMEDIA_CLIP_COUNT), Some(&Value::Int(1)));
        assert_eq!(get(&out, P::PRESENTATION_FORMAT), Some(&Value::Text("Widescreen".into())));
    }

    #[test]
    fn body_text_of_each_kind() {
        let (_d, p) = file("a.docx", &docx("Quarterly plan &amp; budget"));
        assert_eq!(
            extract_text(&p, ExtractLimits::default(), 1 << 20).unwrap(),
            "Quarterly plan & budget\nSecond paragraph\n"
        );
        let (_d, p) = file("a.xlsx", &xlsx());
        assert_eq!(
            extract_text(&p, ExtractLimits::default(), 1 << 20).unwrap(),
            "Revenue\nCosts\n0\n42.5\ninline text\n"
        );
        let (_d, p) = file("a.pptx", &pptx());
        assert_eq!(extract_text(&p, ExtractLimits::default(), 1 << 20).unwrap(), "Title slide\nSecond slide\nTenth\n");
        let (_d, p) = file("a.zip", &zip_of(&[("x.txt", "x")]));
        assert_eq!(extract_text(&p, ExtractLimits::default(), 1 << 20), None);
        let mut s = "aé".to_string();
        truncate_utf8(&mut s, 2);
        assert_eq!(s, "a");
        assert!(w3c_date("2026-01-15").is_some());
        assert!(w3c_date("garbage").is_none());
    }
}

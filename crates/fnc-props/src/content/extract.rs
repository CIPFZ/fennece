//! Content extraction (SPEC ch.1 §9.2): plain text with encoding detection, HTML tag
//! stripping, Office Open XML and PDF text, and an injectable [`ContentProvider`]
//! fallback (IFilters in a host process on Windows, supplied by the app because this
//! crate cannot use COM).

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use encoding_rs::Encoding;

use crate::config::ContentLimits;
use crate::extract::{ExtractLimits, Family, contained, family, ooxml, pdf};

/// Byte interpretation for `utf8content:` / `ansicontent:` / `utf16content:` /
/// `utf16becontent:` and `textplain:` (auto).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextEncoding {
    /// BOM, UTF-16 heuristics, UTF-8 validation, then `chardetng`.
    Auto,
    /// UTF-8 (invalid sequences replaced).
    Utf8,
    /// The ANSI code page (`ansi_code_page`, default windows-1252).
    Ansi,
    /// UTF-16 little endian.
    Utf16Le,
    /// UTF-16 big endian.
    Utf16Be,
}

/// Extensions treated as HTML.
fn is_html(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "htm" | "html" | "xhtml" | "mht" | "mhtml" | "shtml" | "hta")
}

/// Fallback text source for formats the built-in parsers do not read (the app injects
/// the IFilters of the system, run in a separate host process).
///
/// Called on extraction and search threads; must not panic (panics are contained) and
/// must bound its own time per file.
pub trait ContentProvider: Send + Sync {
    /// `true` if files with extension `ext` (lowercase, no dot) have a text source.
    fn handles(&self, ext: &str) -> bool;
    /// Text of the file, at most `max_bytes` (`None` if there is none).
    fn text(&self, path: &Path, ext: &str, max_bytes: usize) -> Option<String>;
}

/// Document formats whose bytes are not text: without a [`ContentProvider`] for them the
/// built-in decoder still runs (it may find nothing, or raw markup such as RTF); with
/// one, the provider is used instead of the decoder.
pub fn is_foreign_document(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "rtf"
            | "doc"
            | "dot"
            | "wri"
            | "xls"
            | "xlt"
            | "xlsb"
            | "ppt"
            | "pps"
            | "pot"
            | "msg"
            | "odt"
            | "ott"
            | "ods"
            | "odp"
            | "odg"
            | "wpd"
            | "wps"
            | "vsd"
            | "pub"
            | "xps"
            | "oxps"
            | "epub"
            | "chm"
    )
}

/// Content extractor.
#[derive(Clone)]
pub struct ContentExtractor {
    /// Limits.
    pub limits: ContentLimits,
    /// Windows code page used for "ANSI" text (e.g. 1252, 936); see
    /// [`encoding_for_code_page`]. The app sets it from `GetACP()`.
    pub ansi_code_page: u16,
    /// Fallback for files the built-in parsers give no text for (IFilters), used when
    /// [`ContentLimits::ifilter`] is on and the provider handles the extension.
    pub fallback: Option<Arc<dyn ContentProvider>>,
}

impl std::fmt::Debug for ContentExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentExtractor")
            .field("limits", &self.limits)
            .field("ansi_code_page", &self.ansi_code_page)
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl Default for ContentExtractor {
    fn default() -> Self {
        Self { limits: ContentLimits::default(), ansi_code_page: 1252, fallback: None }
    }
}

impl ContentExtractor {
    /// Extractor with `limits`.
    pub fn new(limits: ContentLimits) -> Self {
        Self { limits, ..Self::default() }
    }

    fn extract_limits(&self) -> ExtractLimits {
        ExtractLimits {
            max_read_bytes: self.limits.max_file_bytes.max(1 << 20) * 4,
            time_budget: self.limits.time_budget,
        }
    }

    /// Text of the file (`None` if unreadable, binary, too large or unsupported). The
    /// result is at most `limits.max_text_bytes`.
    ///
    /// The built-in parsers run first; the [`fallback`](Self::fallback) provider only
    /// when they give nothing (or, for [`is_foreign_document`] formats, instead of the
    /// raw-byte decoder), and never for OOXML/PDF when their parsing is switched off.
    pub fn extract(&self, path: &Path, ext: &str) -> Option<String> {
        let fam = family(ext);
        let allowed = match fam {
            Family::Ooxml => self.limits.office,
            Family::Pdf => self.limits.pdf,
            _ => true,
        };
        let lower = ext.to_ascii_lowercase();
        let fallback = self
            .fallback
            .as_ref()
            .filter(|_| self.limits.ifilter && allowed && !lower.is_empty())
            .filter(|f| f.handles(&lower));
        if !(fallback.is_some() && is_foreign_document(&lower))
            && let Some(t) = self.builtin(path, ext, fam)
        {
            return Some(t);
        }
        let f = fallback?;
        let len = std::fs::metadata(path).ok()?.len();
        if len > self.extract_limits().max_read_bytes {
            return None;
        }
        let max = self.limits.max_text_bytes;
        let mut text = contained("content-fallback", path, || f.text(path, &lower, max))??;
        crate::extract::ooxml::truncate_utf8(&mut text, max);
        (!text.trim().is_empty()).then_some(text)
    }

    fn builtin(&self, path: &Path, ext: &str, fam: Family) -> Option<String> {
        let max = self.limits.max_text_bytes;
        let l = self.extract_limits();
        match fam {
            Family::Ooxml if self.limits.office => contained("ooxml-text", path, || ooxml::extract_text(path, l, max))?,
            Family::Pdf if self.limits.pdf => contained("pdf-text", path, || pdf::extract_text(path, l, max))?,
            Family::Ooxml | Family::Pdf => None,
            _ => {
                let bytes = self.read_head(path)?;
                let text = decode(&bytes, TextEncoding::Auto, self.ansi_code_page)?;
                let mut text = if is_html(ext) { strip_html(&text) } else { text };
                crate::extract::ooxml::truncate_utf8(&mut text, max);
                Some(text)
            }
        }
    }

    /// Plain-text interpretation only (`textplain:`): no OOXML/PDF parsing.
    pub fn extract_plain(&self, path: &Path, encoding: TextEncoding) -> Option<String> {
        let bytes = self.read_head(path)?;
        let mut t = decode_forced(&bytes, encoding, self.ansi_code_page)?;
        crate::extract::ooxml::truncate_utf8(&mut t, self.limits.max_text_bytes);
        Some(t)
    }

    /// Raw bytes of the file (at most `max_file_bytes`; larger files are truncated).
    pub fn read_head(&self, path: &Path) -> Option<Vec<u8>> {
        let f = std::fs::File::open(path).ok()?;
        let mut buf = Vec::new();
        f.take(self.limits.max_file_bytes).read_to_end(&mut buf).ok()?;
        Some(buf)
    }
}

/// Encoding of a Windows code page (unknown → windows-1252).
pub fn encoding_for_code_page(cp: u16) -> &'static Encoding {
    use encoding_rs::*;
    match cp {
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        54936 => GB18030,
        65001 => UTF_8,
        _ => WINDOWS_1252,
    }
}

/// `true` if the bytes look like UTF-16 without BOM (NULs mostly on one parity).
fn utf16_guess(b: &[u8]) -> Option<bool> {
    let n = b.len().min(4096) & !1;
    if n < 8 {
        return None;
    }
    let (mut even, mut odd) = (0usize, 0usize);
    for (i, &c) in b[..n].iter().enumerate() {
        if c == 0 {
            if i % 2 == 0 {
                even += 1;
            } else {
                odd += 1;
            }
        }
    }
    let half = n / 2;
    if odd * 10 >= half * 4 && even * 20 < half {
        Some(true) // ASCII-range LE: "a\0b\0"
    } else if even * 10 >= half * 4 && odd * 20 < half {
        Some(false)
    } else {
        None
    }
}

fn utf16(b: &[u8], le: bool) -> String {
    let units: Vec<u16> =
        b.as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) }).collect();
    String::from_utf16_lossy(&units)
}

/// Decodes text bytes. `Auto`: BOM → UTF-16 heuristic → valid UTF-8 → `chardetng`.
/// Returns `None` for binary data (NUL bytes that are not UTF-16).
pub fn decode(b: &[u8], enc: TextEncoding, ansi_cp: u16) -> Option<String> {
    if enc != TextEncoding::Auto {
        return decode_forced(b, enc, ansi_cp);
    }
    if let Some(rest) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return Some(String::from_utf8_lossy(rest).into_owned());
    }
    if let Some(rest) = b.strip_prefix(&[0xff, 0xfe]) {
        return Some(utf16(rest, true));
    }
    if let Some(rest) = b.strip_prefix(&[0xfe, 0xff]) {
        return Some(utf16(rest, false));
    }
    if let Some(le) = utf16_guess(b) {
        return Some(utf16(b, le));
    }
    if memchr::memchr(0, &b[..b.len().min(8192)]).is_some() {
        return None;
    }
    match std::str::from_utf8(b) {
        Ok(s) => Some(s.to_string()),
        // A multi-byte sequence cut by the size limit is still UTF-8.
        Err(e) if e.error_len().is_none() && b.len() - e.valid_up_to() < 4 => {
            Some(String::from_utf8_lossy(&b[..e.valid_up_to()]).into_owned())
        }
        Err(_) => {
            let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            det.feed(&b[..b.len().min(64 * 1024)], b.len() <= 64 * 1024);
            let enc = det.guess(None, chardetng::Utf8Detection::Allow);
            Some(enc.decode_without_bom_handling(b).0.into_owned())
        }
    }
}

/// Decodes with a fixed interpretation (BOMs of that encoding are skipped).
pub fn decode_forced(b: &[u8], enc: TextEncoding, ansi_cp: u16) -> Option<String> {
    Some(match enc {
        TextEncoding::Auto => return decode(b, enc, ansi_cp),
        TextEncoding::Utf8 => String::from_utf8_lossy(b.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(b)).into_owned(),
        TextEncoding::Ansi => encoding_for_code_page(ansi_cp).decode_without_bom_handling(b).0.into_owned(),
        TextEncoding::Utf16Le => utf16(b.strip_prefix(&[0xff, 0xfe]).unwrap_or(b), true),
        TextEncoding::Utf16Be => utf16(b.strip_prefix(&[0xfe, 0xff]).unwrap_or(b), false),
    })
}

/// HTML → text: tags removed, `script`/`style`/comments dropped, entities decoded,
/// block elements end lines.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let b = html.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' => {
                if lower[i..].starts_with("<!--") {
                    i = lower[i..].find("-->").map_or(b.len(), |e| i + e + 3);
                    continue;
                }
                let end = lower[i..].find('>').map_or(b.len(), |e| i + e + 1);
                let tag = &lower[i + 1..end.saturating_sub(1).max(i + 1)];
                let name: String =
                    tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if !tag.starts_with('/') && (name == "script" || name == "style") {
                    let close = format!("</{name}");
                    i = lower[end..].find(&close).map_or(b.len(), |e| {
                        let s = end + e;
                        lower[s..].find('>').map_or(b.len(), |g| s + g + 1)
                    });
                    continue;
                }
                let closing = tag.starts_with('/');
                if name == "br" {
                    out.push('\n');
                } else if closing && (name == "td" || name == "th") {
                    out.push('\t');
                } else if closing
                    && matches!(
                        name.as_str(),
                        "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "title" | "table"
                    )
                    && !out.ends_with('\n')
                    && !out.is_empty()
                {
                    out.push('\n');
                }
                i = end;
            }
            b'&' => {
                let semi = html[i..].find(';').filter(|&k| k <= 10);
                let ent = semi.map(|k| &html[i + 1..i + k]);
                match ent.and_then(entity) {
                    Some(c) => {
                        out.push(c);
                        i += semi.map_or(1, |k| k + 1);
                    }
                    None => {
                        out.push('&');
                        i += 1;
                    }
                }
            }
            _ => {
                let next = html[i..].find(['<', '&']).map_or(b.len(), |k| i + k);
                out.push_str(&html[i..next]);
                i = next;
            }
        }
    }
    out.truncate(out.trim_end().len());
    out
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "copy" => '©',
        "reg" => '®',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            return char::from_u32(code);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::testutil::file;

    #[test]
    fn encodings() {
        assert_eq!(decode(b"\xef\xbb\xbfhi", TextEncoding::Auto, 1252).unwrap(), "hi");
        let le: Vec<u8> =
            [0xff, 0xfe].into_iter().chain("季度 plan".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode(&le, TextEncoding::Auto, 1252).unwrap(), "季度 plan");
        let be_nobom: Vec<u8> = "quarterly plan".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(decode(&be_nobom, TextEncoding::Auto, 1252).unwrap(), "quarterly plan");
        let le_nobom: Vec<u8> = "quarterly plan".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode(&le_nobom, TextEncoding::Auto, 1252).unwrap(), "quarterly plan");
        assert_eq!(decode("naïve café".as_bytes(), TextEncoding::Auto, 1252).unwrap(), "naïve café");
        // windows-1252 "café crème" → detected and decoded.
        assert_eq!(decode(b"caf\xe9 cr\xe8me br\xfbl\xe9e", TextEncoding::Auto, 1252).unwrap(), "café crème brûlée");
        // GBK Chinese.
        let (gbk, _, _) = encoding_rs::GBK.encode("季度报告：第三季度的计划和预算");
        assert_eq!(decode(&gbk, TextEncoding::Auto, 1252).unwrap(), "季度报告：第三季度的计划和预算");
        assert_eq!(decode(&gbk, TextEncoding::Ansi, 936).unwrap(), "季度报告：第三季度的计划和预算");
        assert!(decode(b"\x00\x01\x02\x00\xff\x10binary\x00\x00\x00\x07", TextEncoding::Auto, 1252).is_none());
        assert_eq!(decode_forced(&le, TextEncoding::Utf16Le, 1252).unwrap(), "季度 plan");
        assert_eq!(decode_forced(b"a\xffb", TextEncoding::Utf8, 1252).unwrap(), "a\u{fffd}b");
        // Cut multi-byte sequence at the end.
        assert_eq!(decode(&"aé".as_bytes()[..2], TextEncoding::Auto, 1252).unwrap(), "a");
    }

    #[test]
    fn html() {
        let h = "<html><head><title>T</title><style>p{color:red}</style><script>var x='<p>';</script></head>\
                 <body><!-- c --><p>Fish &amp; chips&nbsp;&#x263a;</p><div>Next</div><table><tr><td>a</td><td>b</td></tr></table></body></html>";
        assert_eq!(strip_html(h), "T\nFish & chips ☺\nNext\na\tb");
        assert_eq!(strip_html("a & b <unclosed"), "a & b");
    }

    #[test]
    fn extractor_dispatch_and_limits() {
        let ex = ContentExtractor::default();
        let (_d, p) = file("a.txt", "Quarterly plan\nline two".as_bytes());
        assert_eq!(ex.extract(&p, "txt").unwrap(), "Quarterly plan\nline two");
        let (_d, p) = file("a.html", b"<p>Hello <b>world</b></p>");
        assert_eq!(ex.extract(&p, "html").unwrap(), "Hello world");
        let (_d, p) = file("a.docx", &crate::extract::ooxml::tests::docx("Quarterly plan"));
        assert!(ex.extract(&p, "docx").unwrap().contains("Quarterly plan"));
        let (_d, p) = file("a.pdf", &crate::extract::pdf::tests::sample_pdf("Quarterly plan"));
        assert!(ex.extract(&p, "pdf").unwrap().contains("Quarterly plan"));
        let no_office = ContentExtractor::new(ContentLimits { office: false, pdf: false, ..ContentLimits::default() });
        assert!(no_office.extract(&p, "pdf").is_none());
        let small =
            ContentExtractor::new(ContentLimits { max_text_bytes: 5, max_file_bytes: 8, ..ContentLimits::default() });
        let (_d, p) = file("b.txt", b"0123456789abcdef");
        assert_eq!(small.extract(&p, "txt").unwrap(), "01234");
        assert_eq!(small.extract_plain(&p, TextEncoding::Utf8).unwrap(), "01234");
        assert!(ex.extract(Path::new("/no/such/file.txt"), "txt").is_none());
        assert_eq!(encoding_for_code_page(936), encoding_rs::GBK);
    }

    /// A fake IFilter provider: handles `doc`, `rtf`, `docx`, `txt` and a crashing `boom`.
    struct Fake;
    impl ContentProvider for Fake {
        fn handles(&self, ext: &str) -> bool {
            matches!(ext, "doc" | "rtf" | "docx" | "txt" | "boom")
        }
        fn text(&self, path: &Path, ext: &str, max_bytes: usize) -> Option<String> {
            if ext == "boom" {
                panic!("filter crashed");
            }
            let name = path.file_name()?.to_str()?;
            let mut t = format!("filtered {name} with the {ext} filter");
            t.truncate(max_bytes);
            Some(t)
        }
    }

    #[test]
    fn fallback_provider_only_where_the_builtin_parsers_give_nothing() {
        let ex = ContentExtractor { fallback: Some(Arc::new(Fake)), ..ContentExtractor::default() };
        // Binary legacy Office: the decoder finds nothing; the provider does.
        let (_d, doc) = file("a.doc", b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\x00\x00\x00\x00binary");
        assert_eq!(ex.extract(&doc, "doc").as_deref(), Some("filtered a.doc with the doc filter"));
        assert_eq!(ex.extract(&doc, "DOC").as_deref(), Some("filtered a.doc with the doc filter"));
        // RTF: the provider replaces the raw markup.
        let (_d, rtf) = file("b.rtf", br"{\rtf1\ansi plan}");
        assert_eq!(ex.extract(&rtf, "rtf").as_deref(), Some("filtered b.rtf with the rtf filter"));
        // Plain text and OOXML keep the built-in parsers.
        let (_d, txt) = file("c.txt", b"own text");
        assert_eq!(ex.extract(&txt, "txt").as_deref(), Some("own text"));
        let (_d, docx) = file("d.docx", &crate::extract::ooxml::tests::docx("Quarterly"));
        assert!(ex.extract(&docx, "docx").unwrap().contains("Quarterly"));
        // Office parsing off: no IFilter for docx either.
        let no_office =
            ContentExtractor { limits: ContentLimits { office: false, ..ContentLimits::default() }, ..ex.clone() };
        assert_eq!(no_office.extract(&docx, "docx"), None);
        // `content_ifilter` off: the built-in behaviour (raw RTF, nothing for .doc).
        let off =
            ContentExtractor { limits: ContentLimits { ifilter: false, ..ContentLimits::default() }, ..ex.clone() };
        assert_eq!(off.extract(&rtf, "rtf").as_deref(), Some(r"{\rtf1\ansi plan}"));
        assert_eq!(off.extract(&doc, "doc"), None);
        // Unhandled extensions, a panicking provider, the text limit, `textplain:`.
        let (_d, bin) = file("e.bin", b"\x00\x01\x02\x00\x00\x00\x00\x00\x00\x00");
        assert_eq!(ex.extract(&bin, "bin"), None);
        let (_d, boom) = file("f.boom", b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00");
        assert_eq!(ex.extract(&boom, "boom"), None);
        let small =
            ContentExtractor { limits: ContentLimits { max_text_bytes: 8, ..ContentLimits::default() }, ..ex.clone() };
        assert_eq!(small.extract(&doc, "doc").as_deref(), Some("filtered"));
        assert_eq!(ex.extract_plain(&rtf, TextEncoding::Utf8).as_deref(), Some(r"{\rtf1\ansi plan}"));
        assert!(is_foreign_document("XLS") && !is_foreign_document("docx"));
        assert!(format!("{ex:?}").contains("fallback: true"));
    }
}

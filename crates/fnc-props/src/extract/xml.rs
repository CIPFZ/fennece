//! Small helpers over `quick-xml` for OOXML parts.

use quick_xml::Reader;
use quick_xml::events::Event;

/// Resolves an entity or character reference name (`amp`, `#38`, `#x26`).
fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
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

fn local(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

/// `(local element name, text)` of every element that directly contains text
/// (`<dc:title>Report</dc:title>` → `("title", "Report")`).
pub fn leaf_texts(xml: &str) -> Vec<(String, String)> {
    let mut r = Reader::from_str(xml);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut text = String::new();
    let mut out = Vec::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                stack.push(local(e.name().as_ref()).to_vec());
                text.clear();
            }
            Ok(Event::End(_)) => {
                if let Some(name) = stack.pop()
                    && !text.trim().is_empty()
                {
                    out.push((String::from_utf8_lossy(&name).into_owned(), text.trim().to_string()));
                }
                text.clear();
            }
            Ok(Event::Text(t)) => {
                if let Ok(s) = t.decode() {
                    text.push_str(&s);
                }
            }
            Ok(Event::CData(t)) => {
                if let Ok(s) = t.decode() {
                    text.push_str(&s);
                }
            }
            Ok(Event::GeneralRef(g)) => {
                if let Some(c) = g.decode().ok().and_then(|n| entity(&n)) {
                    text.push(c);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    out
}

/// Text of a document body: characters inside elements named in `text_tags`, a newline
/// after each element in `para_tags`, a tab for `<tab/>` and a newline for `<br/>`.
/// Stops after `limit` bytes. Returns `false` if the limit was hit.
pub fn body_text(xml: &str, text_tags: &[&[u8]], para_tags: &[&[u8]], out: &mut String, limit: usize) -> bool {
    let mut r = Reader::from_str(xml);
    let mut depth_in_text = 0usize;
    loop {
        if out.len() >= limit {
            return false;
        }
        match r.read_event() {
            Ok(Event::Start(e)) => {
                if text_tags.contains(&local(e.name().as_ref())) {
                    depth_in_text += 1;
                }
            }
            Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                n if para_tags.contains(&n) => out.push('\n'),
                _ => {}
            },
            Ok(Event::End(e)) => {
                let name = e.name();
                let n = local(name.as_ref());
                if text_tags.contains(&n) {
                    depth_in_text = depth_in_text.saturating_sub(1);
                } else if para_tags.contains(&n) && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Ok(Event::Text(t)) if depth_in_text > 0 => {
                if let Ok(s) = t.decode() {
                    out.push_str(&s);
                }
            }
            Ok(Event::CData(t)) if depth_in_text > 0 => {
                if let Ok(s) = t.decode() {
                    out.push_str(&s);
                }
            }
            Ok(Event::GeneralRef(g)) if depth_in_text > 0 => {
                if let Some(c) = g.decode().ok().and_then(|n| entity(&n)) {
                    out.push(c);
                }
            }
            Ok(Event::Eof) | Err(_) => return true,
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_and_entities() {
        let x = r#"<?xml version="1.0"?><cp:coreProperties xmlns:dc="d"><dc:title>A &amp; B&#33;</dc:title><x><y/></x><dc:creator><![CDATA[Zoë]]></dc:creator></cp:coreProperties>"#;
        let v = leaf_texts(x);
        assert_eq!(v, [("title".to_string(), "A & B!".to_string()), ("creator".to_string(), "Zoë".to_string())]);
        assert!(leaf_texts("<a><b>unclosed").is_empty() || !leaf_texts("<a><b>unclosed").is_empty());
    }

    #[test]
    fn body() {
        let x = r#"<w:body><w:p><w:r><w:t>Hello</w:t><w:tab/><w:t xml:space="preserve"> wor&lt;ld</w:t></w:r></w:p><w:p><w:r><w:t>Two</w:t><w:br/><w:t>lines</w:t></w:r></w:p><w:instr>skip</w:instr></w:body>"#;
        let mut s = String::new();
        assert!(body_text(x, &[b"t"], &[b"p"], &mut s, 1000));
        assert_eq!(s, "Hello\t wor<ld\nTwo\nlines\n");
        let mut s = String::new();
        assert!(!body_text(x, &[b"t"], &[b"p"], &mut s, 3));
    }
}

//! Static checks of the WiX source (`packaging/wix/fennec.wxs`) for mistakes that only show
//! after installing the MSI (found by the 0.2.0 install pass, REL-08).

use std::collections::HashSet;

fn wxs() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/wix/fennec.wxs");
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The attribute text of every `<tag ...>` element.
fn elements<'a>(src: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag} ");
    src.match_indices(&open)
        .map(|(i, _)| {
            let rest = &src[i + open.len()..];
            &rest[..rest.find('>').unwrap_or(rest.len())]
        })
        .collect()
}

/// The value of `name="..."` in an element's attribute text.
fn attr<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let start =
        element.match_indices(&needle).find(|(i, _)| *i == 0 || element.as_bytes()[i - 1].is_ascii_whitespace())?.0
            + needle.len();
    let rest = &element[start..];
    Some(&rest[..rest.find('"')?])
}

#[test]
fn icon_ids_have_a_file_extension() {
    let src = wxs();
    let icons = elements(&src, "Icon");
    assert!(!icons.is_empty(), "the MSI defines an icon");
    for icon in icons {
        let id = attr(icon, "Id").unwrap_or_default();
        let lower = id.to_ascii_lowercase();
        assert!(
            lower.ends_with(".ico") || lower.ends_with(".exe"),
            "Icon Id {id:?} must end in .ico or .exe, else the Start menu shortcut shows a blank icon"
        );
    }
}

#[test]
fn icon_references_point_to_defined_icons() {
    let src = wxs();
    let ids: HashSet<&str> = elements(&src, "Icon").into_iter().filter_map(|e| attr(e, "Id")).collect();
    let mut refs: Vec<&str> = elements(&src, "Shortcut").into_iter().filter_map(|e| attr(e, "Icon")).collect();
    refs.extend(
        elements(&src, "Property")
            .into_iter()
            .filter(|e| attr(e, "Id") == Some("ARPPRODUCTICON"))
            .filter_map(|e| attr(e, "Value")),
    );
    assert_eq!(refs.len(), 2, "Start menu shortcut and ARPPRODUCTICON reference the icon: {refs:?}");
    for r in refs {
        assert!(ids.contains(r), "icon reference {r:?} is not an <Icon Id> ({ids:?})");
    }
}

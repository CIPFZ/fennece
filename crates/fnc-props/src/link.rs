//! Link targets for `target:` / `linktarget:` / `shortcuttarget:` (SPEC ch.2 §5.1):
//!
//! * `.lnk` shell links — a pure-Rust reader of the parts of MS-SHLLINK that carry the
//!   target path: the LinkInfo structure (LocalBasePath + CommonPathSuffix, or the network
//!   share name + CommonPathSuffix) and, without LinkInfo, the StringData RELATIVE_PATH
//!   resolved against the folder of the `.lnk` file;
//! * `.url` internet shortcuts — the `URL=` line of the `[InternetShortcut]` section;
//! * symbolic links and junctions — the reparse target as returned by
//!   `std::fs::read_link`, with the NT/verbatim prefixes removed and relative targets
//!   resolved against the link's folder.
//!
//! Every parser works on untrusted bytes: bounds are checked on each read and malformed
//! input yields `None` (or the parts read so far), never a panic.

use std::path::Path;

/// What kind of link an entry is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// Symbolic link or junction (reparse point).
    Reparse,
    /// Shell link (`.lnk`).
    Shortcut,
    /// Internet shortcut (`.url`).
    Internet,
}

impl LinkKind {
    /// Link kind of a file with (folded or not) extension `ext`, if it is a shortcut.
    pub fn of_ext(ext: &str) -> Option<LinkKind> {
        if ext.eq_ignore_ascii_case("lnk") {
            Some(LinkKind::Shortcut)
        } else if ext.eq_ignore_ascii_case("url") {
            Some(LinkKind::Internet)
        } else {
            None
        }
    }
}

/// Shortcut files larger than this are not read (real `.lnk` / `.url` files are a few
/// KiB; the cap bounds the slow path).
pub const MAX_LINK_FILE_BYTES: usize = 1 << 20;

/// `LinkCLSID` of every shell link: {00021401-0000-0000-C000-000000000046}.
const LINK_CLSID: [u8; 16] =
    [0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

const HAS_LINK_TARGET_ID_LIST: u32 = 1 << 0;
const HAS_LINK_INFO: u32 = 1 << 1;
const HAS_NAME: u32 = 1 << 2;
const HAS_RELATIVE_PATH: u32 = 1 << 3;
const HAS_WORKING_DIR: u32 = 1 << 4;
const HAS_ARGUMENTS: u32 = 1 << 5;
const HAS_ICON_LOCATION: u32 = 1 << 6;
const IS_UNICODE: u32 = 1 << 7;
const FORCE_NO_LINK_INFO: u32 = 1 << 8;

/// The target-related fields of a shell link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellLink {
    /// `LinkFlags`.
    pub flags: u32,
    /// LinkInfo `LocalBasePath` (Unicode variant when present).
    pub local_base_path: Option<String>,
    /// LinkInfo `CommonPathSuffix` (Unicode variant when present).
    pub common_path_suffix: Option<String>,
    /// LinkInfo CommonNetworkRelativeLink `NetName` (`\\server\share`).
    pub net_name: Option<String>,
    /// StringData NAME_STRING (description).
    pub name: Option<String>,
    /// StringData RELATIVE_PATH.
    pub relative_path: Option<String>,
    /// StringData WORKING_DIR.
    pub working_dir: Option<String>,
    /// StringData COMMAND_LINE_ARGUMENTS.
    pub arguments: Option<String>,
}

impl ShellLink {
    /// Target path: LocalBasePath + CommonPathSuffix, else NetName + CommonPathSuffix,
    /// else RELATIVE_PATH resolved against `link_dir` (the folder of the `.lnk` file;
    /// without it the relative path is returned as is).
    pub fn target(&self, link_dir: Option<&str>) -> Option<String> {
        let suffix = self.common_path_suffix.as_deref().unwrap_or("");
        if let Some(base) = self.local_base_path.as_deref().filter(|b| !b.is_empty()) {
            return Some(join_parts(base, suffix));
        }
        if let Some(net) = self.net_name.as_deref().filter(|n| !n.is_empty()) {
            return Some(join_parts(net, suffix));
        }
        let rel = self.relative_path.as_deref().filter(|r| !r.trim().is_empty())?;
        Some(match link_dir {
            Some(dir) if !is_absolute(rel) => resolve_relative(dir, rel),
            _ => rel.to_string(),
        })
    }
}

/// `a` + `b`, inserting a `\` when neither side has one at the junction.
fn join_parts(a: &str, b: &str) -> String {
    if b.is_empty() {
        return a.to_string();
    }
    let mut s = String::with_capacity(a.len() + b.len() + 1);
    s.push_str(a);
    if !a.ends_with(['\\', '/']) && !b.starts_with(['\\', '/']) {
        s.push('\\');
    }
    s.push_str(b);
    s
}

/// Little-endian reader over a byte slice; every read is bounds-checked.
struct Cursor<'a> {
    b: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn u16(&self, at: usize) -> Option<u16> {
        let s = self.b.get(at..at.checked_add(2)?)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let s = self.b.get(at..at.checked_add(4)?)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn slice(&self, at: usize, len: usize) -> Option<&'a [u8]> {
        self.b.get(at..at.checked_add(len)?)
    }

    /// NUL-terminated 8-bit string at `at` (to the end of the slice if unterminated).
    fn cstr(&self, at: usize) -> Option<&'a [u8]> {
        let rest = self.b.get(at..)?;
        Some(&rest[..rest.iter().position(|&c| c == 0).unwrap_or(rest.len())])
    }

    /// NUL-terminated UTF-16LE string at `at`.
    fn wstr(&self, at: usize) -> Option<String> {
        let rest = self.b.get(at..)?;
        let units: Vec<u16> =
            rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
        Some(String::from_utf16_lossy(&units))
    }
}

/// Decodes 8-bit text of a shell link: UTF-8 when valid (pure ASCII is both), else the
/// ANSI code page.
fn ansi(bytes: &[u8], code_page: u16) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => decode_code_page(bytes, code_page),
    }
}

#[cfg(feature = "extract")]
fn decode_code_page(bytes: &[u8], code_page: u16) -> String {
    crate::content::encoding_for_code_page(code_page).decode_without_bom_handling(bytes).0.into_owned()
}

#[cfg(not(feature = "extract"))]
fn decode_code_page(bytes: &[u8], _code_page: u16) -> String {
    // Latin-1 (the first 256 code points of windows-1252 except 0x80–0x9F).
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Parses a shell link (`.lnk`). `code_page` decodes the 8-bit strings of non-Unicode
/// links. `None` if the header is not a shell link header; truncated or inconsistent
/// later structures end parsing with the fields read so far.
pub fn parse_lnk(data: &[u8], code_page: u16) -> Option<ShellLink> {
    let c = Cursor { b: data };
    if c.u32(0)? != 0x4C || c.slice(4, 16)? != LINK_CLSID {
        return None;
    }
    let flags = c.u32(20)?;
    let mut link = ShellLink { flags, ..ShellLink::default() };
    let mut pos = 0x4Cusize;
    if flags & HAS_LINK_TARGET_ID_LIST != 0 {
        let Some(size) = c.u16(pos) else { return Some(link) };
        pos = pos.saturating_add(2).saturating_add(usize::from(size));
    }
    if flags & HAS_LINK_INFO != 0 {
        let Some(size) = c.u32(pos).and_then(|s| usize::try_from(s).ok()) else { return Some(link) };
        if flags & FORCE_NO_LINK_INFO == 0
            && let Some(info) = c.slice(pos, size)
        {
            read_link_info(info, code_page, &mut link);
        }
        pos = pos.saturating_add(size);
    }
    let unicode = flags & IS_UNICODE != 0;
    for (bit, slot) in
        [(HAS_NAME, 0), (HAS_RELATIVE_PATH, 1), (HAS_WORKING_DIR, 2), (HAS_ARGUMENTS, 3), (HAS_ICON_LOCATION, 4)]
    {
        if flags & bit == 0 {
            continue;
        }
        let Some(count) = c.u16(pos).map(usize::from) else { break };
        pos += 2;
        let bytes = if unicode { count * 2 } else { count };
        let Some(raw) = c.slice(pos, bytes) else { break };
        pos += bytes;
        let text = if unicode {
            let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|u| u16::from_le_bytes(*u)).collect();
            String::from_utf16_lossy(&units)
        } else {
            ansi(raw, code_page)
        };
        match slot {
            0 => link.name = Some(text),
            1 => link.relative_path = Some(text),
            2 => link.working_dir = Some(text),
            3 => link.arguments = Some(text),
            _ => {}
        }
    }
    Some(link)
}

/// Reads the LinkInfo structure (`info` is exactly `LinkInfoSize` bytes).
fn read_link_info(info: &[u8], code_page: u16, link: &mut ShellLink) {
    let c = Cursor { b: info };
    let (Some(header), Some(lflags)) = (c.u32(4), c.u32(8)) else { return };
    if header < 0x1C {
        return;
    }
    let off = |at: usize| c.u32(at).and_then(|v| usize::try_from(v).ok()).filter(|&v| v > 0);
    let unicode_offsets = header >= 0x24;
    if lflags & 1 != 0 {
        let wide = if unicode_offsets { off(28).and_then(|o| c.wstr(o)) } else { None };
        link.local_base_path = wide.or_else(|| off(16).and_then(|o| c.cstr(o)).map(|b| ansi(b, code_page)));
    }
    let wide = if unicode_offsets { off(32).and_then(|o| c.wstr(o)) } else { None };
    link.common_path_suffix = wide.or_else(|| off(24).and_then(|o| c.cstr(o)).map(|b| ansi(b, code_page)));
    if lflags & 2 != 0
        && let Some(net_at) = off(20)
        && let Some(net) = c.slice(net_at, info.len().saturating_sub(net_at))
    {
        let n = Cursor { b: net };
        let name_off = n.u32(8).and_then(|v| usize::try_from(v).ok()).filter(|&v| v > 0);
        let wide = if name_off.is_some_and(|o| o > 0x14) {
            n.u32(20).and_then(|v| usize::try_from(v).ok()).filter(|&v| v > 0).and_then(|o| n.wstr(o))
        } else {
            None
        };
        link.net_name = wide.or_else(|| name_off.and_then(|o| n.cstr(o)).map(|b| ansi(b, code_page)));
    }
}

/// Target of a `.lnk` file's bytes; relative paths resolve against `link_dir`.
pub fn lnk_target(data: &[u8], link_dir: Option<&str>, code_page: u16) -> Option<String> {
    parse_lnk(data, code_page)?.target(link_dir).filter(|t| !t.is_empty())
}

/// The `URL=` value of a `.url` internet shortcut (the `[InternetShortcut]` section if
/// present, else the first `URL=` line). UTF-16 files with a BOM are decoded; 8-bit text
/// is read as UTF-8 (invalid bytes replaced).
pub fn url_target(data: &[u8]) -> Option<String> {
    let text = if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data)).into_owned()
    };
    let mut in_section = None::<bool>;
    let mut fallback = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = Some(line.eq_ignore_ascii_case("[InternetShortcut]"));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if !key.trim().eq_ignore_ascii_case("url") || value.trim().is_empty() {
            continue;
        }
        let value = value.trim().to_string();
        match in_section {
            Some(true) => return Some(value),
            _ => {
                fallback.get_or_insert(value);
            }
        }
    }
    fallback
}

/// `true` for `C:\…`, `C:/…`, `\…` (rooted, incl. UNC) and `/…` paths.
fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    b.first().is_some_and(|&c| c == b'\\' || c == b'/') || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// Lexically resolves `rel` (with `\` or `/` separators, `.` and `..`) against folder
/// `base`. The separator of `base` is kept; `..` never climbs above the root of `base`.
pub fn resolve_relative(base: &str, rel: &str) -> String {
    let sep = if base.contains('\\') || !base.contains('/') { '\\' } else { '/' };
    let (root, rest) = split_root(base);
    let mut parts: Vec<&str> = rest.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
    for seg in rel.split(['\\', '/']) {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = String::with_capacity(base.len() + rel.len());
    out.push_str(root);
    for (i, p) in parts.iter().enumerate() {
        if i > 0 || (!out.is_empty() && !out.ends_with(['\\', '/'])) {
            out.push(sep);
        }
        out.push_str(p);
    }
    out
}

/// Root of a path (`C:\`, `\\server\share\`, `/`, or empty for relative ones) and the rest.
fn split_root(p: &str) -> (&str, &str) {
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        let end = if b.get(2).is_some_and(|&c| c == b'\\' || c == b'/') { 3 } else { 2 };
        return p.split_at(end);
    }
    if p.starts_with("\\\\") || p.starts_with("//") {
        // `\\server\share\`
        let mut seps = p.char_indices().filter(|&(_, c)| c == '\\' || c == '/').skip(2);
        let _server_end = seps.next();
        return match seps.next() {
            Some((i, _)) => p.split_at(i + 1),
            None => (p, ""),
        };
    }
    if p.starts_with(['\\', '/']) {
        return p.split_at(1);
    }
    ("", p)
}

/// Normalises a reparse target as returned by `read_link`: NT (`\??\`) and verbatim
/// (`\\?\`) prefixes are removed (`\\?\UNC\server\share` → `\\server\share`); relative
/// symbolic link targets are resolved against `link_dir`. Volume GUID paths
/// (`\\?\Volume{…}\`) are kept verbatim.
pub fn normalize_reparse_target(raw: &str, link_dir: Option<&str>) -> String {
    let strip = |p: &str| -> Option<String> {
        let rest = p.strip_prefix("\\??\\").or_else(|| p.strip_prefix("\\\\?\\"))?;
        if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case("UNC\\") {
            return Some(format!("\\\\{}", &rest[4..]));
        }
        if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case("Volume{") {
            return Some(format!("\\\\?\\{rest}"));
        }
        Some(rest.to_string())
    };
    if let Some(s) = strip(raw) {
        return s;
    }
    match link_dir {
        Some(dir) if !is_absolute(raw) && !raw.is_empty() => resolve_relative(dir, raw),
        _ => raw.to_string(),
    }
}

/// Folder of `path` as text (for resolving relative targets).
pub fn parent_dir(path: &Path) -> Option<String> {
    path.parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn utf16z(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    }

    /// A shell link with an ID list, LinkInfo (local base path + suffix, optionally the
    /// Unicode variants) and StringData.
    pub(crate) fn lnk(local: Option<&str>, suffix: &str, unicode_info: bool, rel: Option<&str>) -> Vec<u8> {
        let mut flags = HAS_LINK_TARGET_ID_LIST | IS_UNICODE | HAS_NAME;
        if local.is_some() {
            flags |= HAS_LINK_INFO;
        }
        if rel.is_some() {
            flags |= HAS_RELATIVE_PATH;
        }
        let mut v = Vec::new();
        v.extend_from_slice(&0x4Cu32.to_le_bytes());
        v.extend_from_slice(&LINK_CLSID);
        v.extend_from_slice(&flags.to_le_bytes());
        v.extend_from_slice(&0x20u32.to_le_bytes());
        v.resize(0x4C, 0);
        // ID list: one dummy item + terminator.
        let idlist: Vec<u8> = [4u16.to_le_bytes().to_vec(), vec![0xAB, 0xCD], 0u16.to_le_bytes().to_vec()].concat();
        v.extend_from_slice(&(idlist.len() as u16).to_le_bytes());
        v.extend_from_slice(&idlist);
        if let Some(local) = local {
            let header: u32 = if unicode_info { 0x24 } else { 0x1C };
            let vol_id = [16u32.to_le_bytes(), 3u32.to_le_bytes(), 0x1234u32.to_le_bytes(), 16u32.to_le_bytes()]
                .concat()
                .into_iter()
                .chain([0u8; 4])
                .collect::<Vec<u8>>();
            let mut body = Vec::new();
            let vol_off = header as usize;
            body.extend_from_slice(&vol_id);
            let lbp_off = vol_off + body.len();
            body.extend(local.bytes().filter(u8::is_ascii));
            body.push(0);
            let cps_off = vol_off + body.len();
            body.extend(suffix.bytes().filter(u8::is_ascii));
            body.push(0);
            let (mut lbp_u, mut cps_u) = (0usize, 0usize);
            if unicode_info {
                lbp_u = vol_off + body.len();
                body.extend(utf16z(local));
                cps_u = vol_off + body.len();
                body.extend(utf16z(suffix));
            }
            let size = header as usize + body.len();
            let mut info = Vec::new();
            for x in [size as u32, header, 1, vol_off as u32, lbp_off as u32, 0, cps_off as u32] {
                info.extend_from_slice(&x.to_le_bytes());
            }
            if unicode_info {
                info.extend_from_slice(&(lbp_u as u32).to_le_bytes());
                info.extend_from_slice(&(cps_u as u32).to_le_bytes());
            }
            info.extend(body);
            v.extend(info);
        }
        for s in std::iter::once("A description").chain(rel) {
            let units: Vec<u16> = s.encode_utf16().collect();
            v.extend_from_slice(&(units.len() as u16).to_le_bytes());
            v.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        }
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    }

    #[test]
    fn link_info_paths() {
        let data = lnk(Some("C:\\Windows\\System32\\notepad.exe"), "", false, Some(".\\notepad.exe"));
        let l = parse_lnk(&data, 1252).unwrap();
        assert_eq!(l.name.as_deref(), Some("A description"));
        assert_eq!(l.target(Some("C:\\x")).as_deref(), Some("C:\\Windows\\System32\\notepad.exe"));
        let data = lnk(Some("C:\\Users\\Zoë\\"), "Docs\\plan.txt", true, None);
        assert_eq!(lnk_target(&data, None, 1252).as_deref(), Some("C:\\Users\\Zoë\\Docs\\plan.txt"));
        let data = lnk(Some("D:\\data"), "a.txt", false, None);
        assert_eq!(lnk_target(&data, None, 1252).as_deref(), Some("D:\\data\\a.txt"));
    }

    #[test]
    fn relative_path_fallback() {
        let data = lnk(None, "", false, Some("..\\Docs\\report.docx"));
        assert_eq!(
            lnk_target(&data, Some("C:\\Users\\me\\Desktop"), 1252).as_deref(),
            Some("C:\\Users\\me\\Docs\\report.docx")
        );
        assert_eq!(lnk_target(&data, None, 1252).as_deref(), Some("..\\Docs\\report.docx"));
        assert_eq!(lnk_target(&data, Some("/home/u/desk"), 1252).as_deref(), Some("/home/u/Docs/report.docx"));
    }

    #[test]
    fn not_a_link() {
        assert!(parse_lnk(b"", 1252).is_none());
        assert!(parse_lnk(&[0u8; 200], 1252).is_none());
        let mut data = lnk(Some("C:\\a"), "", false, None);
        data[4] ^= 1;
        assert!(parse_lnk(&data, 1252).is_none());
    }

    #[test]
    fn network_links() {
        // LinkInfo with only CommonNetworkRelativeLink.
        let mut v = lnk(None, "", false, None);
        v.truncate(0x4C);
        v[20..24].copy_from_slice(&(HAS_LINK_INFO).to_le_bytes());
        let net_name = b"\\\\server\\share\0";
        let cnrl_len = 20 + net_name.len();
        let mut cnrl = Vec::new();
        for x in [cnrl_len as u32, 2, 20, 0, 0x0002_0000] {
            cnrl.extend_from_slice(&x.to_le_bytes());
        }
        cnrl.extend_from_slice(net_name);
        let header = 0x1Cu32;
        let cnrl_off = header as usize;
        let cps_off = cnrl_off + cnrl.len();
        let suffix = b"dir\\file.txt\0";
        let size = cps_off + suffix.len();
        for x in [size as u32, header, 2, 0, 0, cnrl_off as u32, cps_off as u32] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend(cnrl);
        v.extend_from_slice(suffix);
        assert_eq!(lnk_target(&v, None, 1252).as_deref(), Some("\\\\server\\share\\dir\\file.txt"));
    }

    #[test]
    fn ansi_strings_use_the_code_page() {
        let mut data = lnk(None, "", false, None);
        // Switch to 8-bit StringData with a windows-1252 byte (é = 0xE9).
        let flags = HAS_LINK_TARGET_ID_LIST | HAS_RELATIVE_PATH;
        data[20..24].copy_from_slice(&flags.to_le_bytes());
        let ids = 0x4C + 2 + usize::from(u16::from_le_bytes([data[0x4C], data[0x4D]]));
        data.truncate(ids);
        let rel = b".\\caf\xE9.txt";
        data.extend_from_slice(&(rel.len() as u16).to_le_bytes());
        data.extend_from_slice(rel);
        let t = lnk_target(&data, Some("C:\\x"), 1252);
        #[cfg(feature = "extract")]
        assert_eq!(t.as_deref(), Some("C:\\x\\café.txt"));
        #[cfg(not(feature = "extract"))]
        assert!(t.is_some());
    }

    #[test]
    fn url_files() {
        assert_eq!(
            url_target(b"[{000214A0-0000-0000-C000-000000000046}]\r\nProp3=19,11\r\n[InternetShortcut]\r\nIDList=\r\nURL=https://example.com/a?b=1\r\n").as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(url_target(b"[Other]\nURL=x\n[InternetShortcut]\nurl = y\n").as_deref(), Some("y"));
        assert_eq!(url_target(b"URL=z").as_deref(), Some("z"));
        assert_eq!(url_target(b"[InternetShortcut]\nURL=\n"), None);
        let u16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("[InternetShortcut]\r\nURL=https://ü.de/\r\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(url_target(&u16).as_deref(), Some("https://ü.de/"));
    }

    #[test]
    fn reparse_targets() {
        assert_eq!(normalize_reparse_target("\\??\\C:\\Target", None), "C:\\Target");
        assert_eq!(normalize_reparse_target("\\\\?\\C:\\Target", None), "C:\\Target");
        assert_eq!(normalize_reparse_target("\\\\?\\UNC\\srv\\sh\\x", None), "\\\\srv\\sh\\x");
        assert_eq!(normalize_reparse_target("\\??\\Volume{abc}\\", None), "\\\\?\\Volume{abc}\\");
        assert_eq!(normalize_reparse_target("..\\b\\c.txt", Some("C:\\a\\x")), "C:\\a\\b\\c.txt");
        assert_eq!(normalize_reparse_target("D:\\abs", Some("C:\\a")), "D:\\abs");
        assert_eq!(normalize_reparse_target("t.txt", Some("/tmp/d")), "/tmp/d/t.txt");
        assert_eq!(resolve_relative("C:\\", "..\\..\\x"), "C:\\x");
        assert_eq!(resolve_relative("\\\\srv\\share\\a", "..\\..\\b"), "\\\\srv\\share\\b");
        assert_eq!(resolve_relative("/", "a/./b"), "/a/b");
        assert_eq!(LinkKind::of_ext("LNK"), Some(LinkKind::Shortcut));
        assert_eq!(LinkKind::of_ext("url"), Some(LinkKind::Internet));
        assert_eq!(LinkKind::of_ext("txt"), None);
    }

    /// xorshift64* (deterministic fuzz input).
    pub(crate) struct Rng(pub u64);

    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    #[test]
    fn fuzz_never_panics() {
        let samples = [
            lnk(Some("C:\\Windows\\notepad.exe"), "", false, Some(".\\notepad.exe")),
            lnk(Some("C:\\Users\\Zoë\\"), "Docs\\plan.txt", true, Some("x")),
            lnk(None, "", false, Some("..\\a")),
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for s in &samples {
            // Every truncation.
            for n in 0..=s.len() {
                let _ = parse_lnk(&s[..n], 1252).map(|l| l.target(Some("C:\\d")));
            }
            // Random byte and bit flips (keeping the header valid half the time).
            for _ in 0..3000 {
                let mut v = s.clone();
                let flips = 1 + (rng.next() % 8) as usize;
                for _ in 0..flips {
                    let i = (rng.next() as usize) % v.len();
                    let keep_header = rng.next().is_multiple_of(2);
                    if keep_header && i < 20 {
                        continue;
                    }
                    v[i] = rng.next() as u8;
                }
                let _ = parse_lnk(&v, 932).map(|l| l.target(Some("C:\\d")));
                let _ = url_target(&v);
            }
        }
        // Pure noise with a valid header prefix.
        for _ in 0..3000 {
            let len = (rng.next() % 600) as usize;
            let mut v: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if v.len() >= 20 && rng.next().is_multiple_of(2) {
                v[..4].copy_from_slice(&0x4Cu32.to_le_bytes());
                v[4..20].copy_from_slice(&LINK_CLSID);
            }
            let _ = parse_lnk(&v, 1252).map(|l| l.target(None));
            let _ = url_target(&v);
            let s = String::from_utf8_lossy(&v);
            let _ = normalize_reparse_target(&s, Some("C:\\x"));
            let _ = resolve_relative(&s, &s);
        }
    }
}

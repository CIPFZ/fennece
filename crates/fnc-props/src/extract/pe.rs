//! PE executables: COFF machine, DLL flag, subsystem, linker/OS/image versions, embedded
//! signature presence and the `VS_VERSIONINFO` resource (hand-written parser, no
//! dependencies; every offset is bounds-checked).

use std::path::Path;

use super::{ExtractLimits, Props, push_text, read_all};
use crate::schema::PropertyId as P;
use crate::value::Value;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o.checked_add(2)?).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o.checked_add(4)?).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Section header: `(virtual address, virtual size, raw pointer, raw size)`.
struct Section {
    va: u32,
    vsize: u32,
    ptr: u32,
    raw: u32,
}

/// Parsed PE headers.
struct Pe<'a> {
    b: &'a [u8],
    sections: Vec<Section>,
}

impl Pe<'_> {
    /// File offset of `rva`.
    fn offset(&self, rva: u32) -> Option<usize> {
        self.sections.iter().find_map(|s| {
            let end = s.va.checked_add(s.vsize.max(s.raw))?;
            if rva >= s.va && rva < end { s.ptr.checked_add(rva - s.va).map(|o| o as usize) } else { None }
        })
    }
}

/// Properties of the PE file at `path`.
pub fn extract(path: &Path, limits: ExtractLimits, out: &mut Props) {
    if let Ok(Some(b)) = read_all(path, limits) {
        parse(&b, out);
    }
}

/// Properties of PE bytes.
pub fn parse(b: &[u8], out: &mut Props) -> Option<()> {
    if b.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(b, 0x3c)? as usize;
    if b.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let machine = u16_at(b, coff)?;
    let nsections = u16_at(b, coff + 2)? as usize;
    let opt_size = u16_at(b, coff + 16)? as usize;
    let characteristics = u16_at(b, coff + 18)?;
    out.push((P::MACHINE, Value::Int(i64::from(machine))));
    out.push((P::IS_DLL, Value::Int(i64::from(characteristics & 0x2000 != 0))));
    let opt = coff + 20;
    let magic = u16_at(b, opt)?;
    let plus = match magic {
        0x10b => false,
        0x20b => true,
        _ => return Some(()),
    };
    let lv = (b.get(opt + 2)?, b.get(opt + 3)?);
    push_text(out, P::LINKER_VERSION, &format!("{}.{}", lv.0, lv.1));
    let osv = (u16_at(b, opt + 40)?, u16_at(b, opt + 42)?);
    push_text(out, P::OS_VERSION, &format!("{}.{}", osv.0, osv.1));
    let iv = (u16_at(b, opt + 44)?, u16_at(b, opt + 46)?);
    push_text(out, P::IMAGE_VERSION, &format!("{}.{}", iv.0, iv.1));
    let subsystem = u16_at(b, opt + 68)?;
    if subsystem != 0 {
        out.push((P::SUBSYSTEM, Value::Int(i64::from(subsystem))));
    }
    let (ndirs_off, dirs) = if plus { (opt + 108, opt + 112) } else { (opt + 92, opt + 96) };
    let ndirs = u32_at(b, ndirs_off)?.min(16) as usize;
    let dir = |i: usize| -> Option<(u32, u32)> {
        if i >= ndirs || dirs + i * 8 + 8 > opt + opt_size {
            return None;
        }
        Some((u32_at(b, dirs + i * 8)?, u32_at(b, dirs + i * 8 + 4)?))
    };
    // Security directory (index 4) holds a file offset, not an RVA.
    let signed = dir(4).is_some_and(|(off, size)| size > 0 && (off as usize) < b.len());
    out.push((P::IS_SIGNED, Value::Int(i64::from(signed))));
    let sec_start = opt + opt_size;
    let mut sections = Vec::with_capacity(nsections.min(96));
    for i in 0..nsections.min(96) {
        let s = sec_start + i * 40;
        sections.push(Section {
            vsize: u32_at(b, s + 8)?,
            va: u32_at(b, s + 12)?,
            raw: u32_at(b, s + 16)?,
            ptr: u32_at(b, s + 20)?,
        });
    }
    let pe = Pe { b, sections };
    if let Some((rva, size)) = dir(2).filter(|d| d.0 != 0 && d.1 != 0)
        && let Some(root) = pe.offset(rva)
        && let Some(data) = version_resource(&pe, root, size as usize)
    {
        version_info(data, out);
    }
    Some(())
}

/// Locates the `RT_VERSION` (16) resource data.
fn version_resource<'a>(pe: &Pe<'a>, root: usize, size: usize) -> Option<&'a [u8]> {
    let b = pe.b;
    let rsrc = b.get(root..root.checked_add(size)?.min(b.len()))?;
    // Entry lookup in a directory at `off` (relative to the section start): by id, or the
    // first entry when `id` is None.
    let entry = |off: usize, id: Option<u32>| -> Option<u32> {
        let named = u16_at(rsrc, off + 12)? as usize;
        let ids = u16_at(rsrc, off + 14)? as usize;
        for k in 0..(named + ids).min(4096) {
            let e = off + 16 + k * 8;
            let name = u32_at(rsrc, e)?;
            let target = u32_at(rsrc, e + 4)?;
            if id.is_none_or(|want| name & 0x8000_0000 == 0 && name == want) {
                return Some(target);
            }
        }
        None
    };
    let t = entry(0, Some(16))?;
    if t & 0x8000_0000 == 0 {
        return None;
    }
    let n = entry((t & 0x7fff_ffff) as usize, None)?;
    if n & 0x8000_0000 == 0 {
        return None;
    }
    let l = entry((n & 0x7fff_ffff) as usize, None)?;
    if l & 0x8000_0000 != 0 {
        return None;
    }
    let data_entry = l as usize;
    let data_rva = u32_at(rsrc, data_entry)?;
    let data_size = u32_at(rsrc, data_entry + 4)? as usize;
    let off = pe.offset(data_rva)?;
    b.get(off..off.checked_add(data_size)?.min(b.len()))
}

/// One `VS_VERSIONINFO`-style block: `(key, value bytes, value length in words/bytes,
/// children bytes, total length)`.
struct Block<'a> {
    key: String,
    value: &'a [u8],
    value_len: usize,
    is_text: bool,
    children: &'a [u8],
}

fn align4(x: usize) -> usize {
    (x + 3) & !3
}

fn block(b: &[u8]) -> Option<(Block<'_>, usize)> {
    let len = u16_at(b, 0)? as usize;
    if len < 6 || len > b.len() {
        return None;
    }
    let b = &b[..len];
    let value_len = u16_at(b, 2)? as usize;
    let is_text = u16_at(b, 4)? == 1;
    let mut p = 6;
    let mut key = Vec::new();
    while let Some(u) = u16_at(b, p) {
        p += 2;
        if u == 0 {
            break;
        }
        key.push(u);
    }
    p = align4(p);
    let vbytes = if is_text { value_len * 2 } else { value_len };
    let value = b.get(p..(p + vbytes).min(len)).unwrap_or(&[]);
    p = align4(p + vbytes).min(len);
    Some((Block { key: String::from_utf16_lossy(&key), value, value_len, is_text, children: &b[p..] }, align4(len)))
}

fn blocks(mut b: &[u8]) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    while let Some((blk, used)) = block(b) {
        out.push(blk);
        if used >= b.len() {
            break;
        }
        b = &b[used..];
    }
    out
}

fn utf16z(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Parses `VS_VERSIONINFO`.
pub fn version_info(data: &[u8], out: &mut Props) {
    let Some((root, _)) = block(data) else { return };
    if root.key != "VS_VERSION_INFO" {
        return;
    }
    let f = root.value;
    if root.value_len >= 52 && u32_at(f, 0) == Some(0xFEEF_04BD) {
        let ver = |ms: u32, ls: u32| format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff);
        if let (Some(fms), Some(fls), Some(pms), Some(pls)) =
            (u32_at(f, 8), u32_at(f, 12), u32_at(f, 16), u32_at(f, 20))
        {
            push_text(out, P::FILE_VERSION, &ver(fms, fls));
            out.push((P::PRODUCT_VERSION, Value::Text(ver(pms, pls).into())));
        }
    }
    let mut lang: Option<u16> = None;
    let mut strings: Vec<(String, String)> = Vec::new();
    for child in blocks(root.children) {
        match child.key.as_str() {
            "StringFileInfo" => {
                if let Some(table) = blocks(child.children).into_iter().next() {
                    lang = lang.or_else(|| u16::from_str_radix(table.key.get(..4).unwrap_or(""), 16).ok());
                    for s in blocks(table.children) {
                        if s.is_text {
                            strings.push((s.key.clone(), utf16z(s.value)));
                        }
                    }
                }
            }
            "VarFileInfo" => {
                for v in blocks(child.children) {
                    if v.key == "Translation"
                        && let Some(l) = u16_at(v.value, 0)
                    {
                        lang = Some(l);
                    }
                }
            }
            _ => {}
        }
    }
    for (k, v) in &strings {
        let p = match k.as_str() {
            "CompanyName" => P::COMPANY,
            "FileDescription" => P::FILE_DESCRIPTION,
            "InternalName" => P::INTERNAL_NAME,
            "LegalCopyright" => P::COPYRIGHT,
            "LegalTrademarks" => P::LEGAL_TRADEMARKS,
            "OriginalFilename" => P::ORIGINAL_FILENAME,
            "ProductName" => P::PRODUCT_NAME,
            "PrivateBuild" => P::PRIVATE_BUILD,
            "SpecialBuild" => P::SPECIAL_BUILD,
            "Comments" => P::COMMENT,
            // Explorer shows the string product version; it replaces the binary one.
            "ProductVersion" => {
                if !v.trim().is_empty() {
                    out.retain(|(q, _)| *q != P::PRODUCT_VERSION);
                }
                P::PRODUCT_VERSION
            }
            _ => continue,
        };
        push_text(out, p, v);
    }
    if let Some(l) = lang {
        push_text(out, P::LANGUAGE, &language_name(l));
    }
}

/// Display name of a Windows LANGID (common ones; others as hex).
pub fn language_name(id: u16) -> String {
    let s = match id {
        0x0000 => "Language Neutral",
        0x0409 => "English (United States)",
        0x0809 => "English (United Kingdom)",
        0x0804 => "Chinese (Simplified, PRC)",
        0x0404 => "Chinese (Traditional, Taiwan)",
        0x0c04 => "Chinese (Traditional, Hong Kong S.A.R.)",
        0x0411 => "Japanese (Japan)",
        0x0412 => "Korean (Korea)",
        0x0407 => "German (Germany)",
        0x040c => "French (France)",
        0x0410 => "Italian (Italy)",
        0x0c0a => "Spanish (Spain)",
        0x0416 => "Portuguese (Brazil)",
        0x0816 => "Portuguese (Portugal)",
        0x0419 => "Russian (Russia)",
        0x0413 => "Dutch (Netherlands)",
        0x041d => "Swedish (Sweden)",
        0x0415 => "Polish (Poland)",
        0x041f => "Turkish (Turkey)",
        0x0405 => "Czech (Czech Republic)",
        0x040e => "Hungarian (Hungary)",
        0x0408 => "Greek (Greece)",
        0x040d => "Hebrew (Israel)",
        0x0401 => "Arabic (Saudi Arabia)",
        _ => return format!("{id:04X}"),
    };
    s.to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    }

    fn pad4(v: &mut Vec<u8>) {
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
    }

    /// `wLength wValueLength wType szKey pad value pad children`.
    fn vblock(key: &str, value: &[u8], text: bool, children: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0];
        let vlen = if text { value.len() / 2 } else { value.len() };
        v.extend_from_slice(&(vlen as u16).to_le_bytes());
        v.extend_from_slice(&u16::from(text).to_le_bytes());
        v.extend_from_slice(&utf16(key));
        pad4(&mut v);
        v.extend_from_slice(value);
        pad4(&mut v);
        v.extend_from_slice(children);
        let len = v.len() as u16;
        v[..2].copy_from_slice(&len.to_le_bytes());
        pad4(&mut v);
        v
    }

    pub(crate) fn version_resource_bytes() -> Vec<u8> {
        let mut fixed = Vec::new();
        for x in [0xFEEF_04BDu32, 0x0001_0000, 0x000A_0000, 0x4A61_0001, 0x000A_0000, 0x4A61_0000] {
            fixed.extend_from_slice(&x.to_le_bytes());
        }
        fixed.resize(52, 0);
        let s = |k: &str, v: &str| vblock(k, &utf16(v), true, &[]);
        let table = vblock(
            "040904B0",
            &[],
            true,
            &[
                s("CompanyName", "Fennec Corp"),
                s("FileDescription", "Fennec search"),
                s("ProductName", "Fennec"),
                s("ProductVersion", "10.0 beta"),
                s("OriginalFilename", "fennec.exe"),
                s("LegalCopyright", "(c) 2026"),
            ]
            .concat(),
        );
        let sfi = vblock("StringFileInfo", &[], true, &table);
        let var = vblock("Translation", &[0x09, 0x04, 0xb0, 0x04], false, &[]);
        let vfi = vblock("VarFileInfo", &[], true, &var);
        vblock("VS_VERSION_INFO", &fixed, false, &[sfi, vfi].concat())
    }

    /// Minimal PE32+ DLL (x64, GUI subsystem) with a `.rsrc` section holding a version
    /// resource and a non-empty security directory.
    pub(crate) fn pe_dll() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        let coff = 0x84;
        b[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        b[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        b[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
        b[coff + 18..coff + 20].copy_from_slice(&0x2022u16.to_le_bytes());
        let opt = coff + 20;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[opt + 2] = 14;
        b[opt + 3] = 38;
        b[opt + 40..opt + 42].copy_from_slice(&6u16.to_le_bytes());
        b[opt + 44..opt + 46].copy_from_slice(&1u16.to_le_bytes());
        b[opt + 46..opt + 48].copy_from_slice(&2u16.to_le_bytes());
        b[opt + 68..opt + 70].copy_from_slice(&2u16.to_le_bytes());
        b[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        let dirs = opt + 112;
        // Resource directory at RVA 0x1000; security directory at file offset 0x300.
        b[dirs + 16..dirs + 20].copy_from_slice(&0x1000u32.to_le_bytes());
        b[dirs + 32..dirs + 36].copy_from_slice(&0x300u32.to_le_bytes());
        b[dirs + 36..dirs + 40].copy_from_slice(&8u32.to_le_bytes());
        let sec = opt + 240;
        b[sec..sec + 5].copy_from_slice(b".rsrc");
        // Resource tree: type 16 → name 1 → lang 0x409 → data entry → version info.
        let ver = version_resource_bytes();
        let mut rsrc = vec![0u8; 0x80];
        let dir = |r: &mut Vec<u8>, at: usize, id: u32, target: u32| {
            r[at + 14..at + 16].copy_from_slice(&1u16.to_le_bytes());
            r[at + 16..at + 20].copy_from_slice(&id.to_le_bytes());
            r[at + 20..at + 24].copy_from_slice(&target.to_le_bytes());
        };
        dir(&mut rsrc, 0x00, 16, 0x8000_0018);
        dir(&mut rsrc, 0x18, 1, 0x8000_0030);
        dir(&mut rsrc, 0x30, 0x409, 0x48);
        rsrc[0x48..0x4c].copy_from_slice(&(0x1000u32 + 0x80).to_le_bytes());
        rsrc[0x4c..0x50].copy_from_slice(&(ver.len() as u32).to_le_bytes());
        rsrc.extend_from_slice(&ver);
        let size = rsrc.len() as u32;
        b[dirs + 20..dirs + 24].copy_from_slice(&size.to_le_bytes());
        b[sec + 8..sec + 12].copy_from_slice(&size.to_le_bytes());
        b[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        b[sec + 16..sec + 20].copy_from_slice(&size.to_le_bytes());
        b[sec + 20..sec + 24].copy_from_slice(&0x400u32.to_le_bytes());
        b.extend_from_slice(&rsrc);
        b
    }

    fn get(v: &Props, p: P) -> Option<&Value> {
        v.iter().find(|(q, _)| *q == p).map(|(_, v)| v)
    }

    #[test]
    fn dll_headers_and_version_info() {
        let mut out = Props::new();
        parse(&pe_dll(), &mut out).unwrap();
        assert_eq!(get(&out, P::MACHINE), Some(&Value::Int(0x8664)));
        assert_eq!(get(&out, P::IS_DLL), Some(&Value::Int(1)));
        assert_eq!(get(&out, P::SUBSYSTEM), Some(&Value::Int(2)));
        assert_eq!(get(&out, P::IS_SIGNED), Some(&Value::Int(1)));
        assert_eq!(get(&out, P::LINKER_VERSION), Some(&Value::Text("14.38".into())));
        assert_eq!(get(&out, P::OS_VERSION), Some(&Value::Text("6.0".into())));
        assert_eq!(get(&out, P::IMAGE_VERSION), Some(&Value::Text("1.2".into())));
        assert_eq!(get(&out, P::FILE_VERSION), Some(&Value::Text("10.0.19041.1".into())));
        assert_eq!(get(&out, P::PRODUCT_VERSION), Some(&Value::Text("10.0 beta".into())));
        assert_eq!(get(&out, P::COMPANY), Some(&Value::Text("Fennec Corp".into())));
        assert_eq!(get(&out, P::FILE_DESCRIPTION), Some(&Value::Text("Fennec search".into())));
        assert_eq!(get(&out, P::PRODUCT_NAME), Some(&Value::Text("Fennec".into())));
        assert_eq!(get(&out, P::ORIGINAL_FILENAME), Some(&Value::Text("fennec.exe".into())));
        assert_eq!(get(&out, P::COPYRIGHT), Some(&Value::Text("(c) 2026".into())));
        assert_eq!(get(&out, P::LANGUAGE), Some(&Value::Text("English (United States)".into())));
    }

    #[test]
    fn every_truncation_and_bit_flip_is_safe() {
        let pe = pe_dll();
        for cut in 0..pe.len() {
            let _ = parse(&pe[..cut], &mut Props::new());
        }
        for i in (0..pe.len()).step_by(3) {
            let mut b = pe.clone();
            b[i] ^= 0xa5;
            let _ = parse(&b, &mut Props::new());
        }
        assert!(parse(b"MZ", &mut Props::new()).is_none());
        assert_eq!(language_name(0x0c07), "0C07");
    }
}

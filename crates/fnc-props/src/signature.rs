//! File signatures for `fsi:` / `filesignature:` (SPEC ch.2 §5.1): the file type detected
//! from the first bytes of the content (magic numbers), independent of the extension.
//!
//! [`detect`] looks at most at the first [`HEAD_BYTES`] bytes. ZIP containers are refined
//! from the local file headers in that window (OOXML `docx`/`xlsx`/`pptx` from
//! `[Content_Types].xml` or the part names, `epub`/OpenDocument from the `mimetype` entry,
//! `apk`, `jar`). ISO 9660/UDF images keep their descriptor at 32 KiB; callers may pass the
//! 5 bytes at [`ISO_PROBE_OFFSET`] to [`detect_iso`] when the head is all zeros.

/// Bytes [`detect`] needs (more are ignored).
pub const HEAD_BYTES: usize = 4096;

/// Offset of the first ISO 9660 / UDF volume descriptor identifier.
pub const ISO_PROBE_OFFSET: u64 = 0x8001;

/// Every name [`detect`] can return, with the aliases accepted by `fsi:`.
pub const NAMES: &[(&str, &[&str])] = &[
    ("jpg", &["jpeg", "jpe", "jfif"]),
    ("png", &[]),
    ("gif", &[]),
    ("bmp", &["dib"]),
    ("tif", &["tiff"]),
    ("webp", &[]),
    ("ico", &[]),
    ("psd", &[]),
    ("heic", &["heif"]),
    ("avif", &[]),
    ("pdf", &[]),
    ("zip", &[]),
    ("docx", &[]),
    ("xlsx", &[]),
    ("pptx", &[]),
    ("odt", &[]),
    ("ods", &[]),
    ("odp", &[]),
    ("epub", &[]),
    ("jar", &[]),
    ("apk", &[]),
    ("7z", &["7zip"]),
    ("rar", &[]),
    ("gz", &["gzip"]),
    ("bz2", &["bzip2"]),
    ("xz", &[]),
    ("zst", &["zstd"]),
    ("tar", &[]),
    ("cab", &[]),
    ("iso", &[]),
    ("exe", &[]),
    ("dll", &[]),
    ("elf", &[]),
    ("mp3", &[]),
    ("aac", &[]),
    ("flac", &[]),
    ("wav", &["wave"]),
    ("ogg", &[]),
    ("mp4", &[]),
    ("m4a", &[]),
    ("mov", &["qt"]),
    ("3gp", &[]),
    ("mkv", &["matroska"]),
    ("webm", &[]),
    ("avi", &[]),
    ("mpg", &["mpeg"]),
    ("ts", &[]),
    ("sqlite", &["sqlite3"]),
    ("rtf", &[]),
    ("ole", &["ole2", "cfb"]),
    ("class", &[]),
    ("wasm", &[]),
    ("ttf", &[]),
    ("ttc", &[]),
    ("otf", &[]),
    ("woff", &[]),
    ("woff2", &[]),
    ("lnk", &[]),
];

/// Canonical signature name of `name` (case-insensitive, aliases resolved).
pub fn canonical(name: &str) -> Option<&'static str> {
    let n = name.trim().trim_start_matches('.').to_ascii_lowercase();
    NAMES.iter().find(|(c, aliases)| *c == n || aliases.contains(&n.as_str())).map(|(c, _)| *c)
}

fn get(b: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    b.get(off..off.checked_add(len)?)
}

fn at(b: &[u8], off: usize, magic: &[u8]) -> bool {
    get(b, off, magic.len()) == Some(magic)
}

fn u16le(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(get(b, off, 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(get(b, off, 4)?.try_into().ok()?))
}

fn u16be(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(get(b, off, 2)?.try_into().ok()?))
}

fn u32be(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(get(b, off, 4)?.try_into().ok()?))
}

/// Detected type of a file whose first bytes are `head` (`None` = unknown).
pub fn detect(head: &[u8]) -> Option<&'static str> {
    let b = &head[..head.len().min(HEAD_BYTES)];
    // Fixed magic numbers at offset 0, most specific first.
    const FIXED: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "png"),
        (b"GIF87a", "gif"),
        (b"GIF89a", "gif"),
        (b"8BPS", "psd"),
        (b"%PDF-", "pdf"),
        (b"7z\xBC\xAF\x27\x1C", "7z"),
        (b"Rar!\x1a\x07\x00", "rar"),
        (b"Rar!\x1a\x07\x01\x00", "rar"),
        (b"\xFD7zXZ\x00", "xz"),
        (b"\x28\xB5\x2F\xFD", "zst"),
        (b"MSCF\0\0\0\0", "cab"),
        (b"\x7FELF", "elf"),
        (b"fLaC", "flac"),
        (b"OggS", "ogg"),
        (b"SQLite format 3\0", "sqlite"),
        (b"{\\rtf", "rtf"),
        (b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1", "ole"),
        (b"\0asm", "wasm"),
        (b"OTTO", "otf"),
        (b"ttcf", "ttc"),
        (b"wOFF", "woff"),
        (b"wOF2", "woff2"),
        (b"L\0\0\0\x01\x14\x02\0\0\0\0\0\xC0\0\0\0\0\0\0F", "lnk"),
        (b"\xFF\xD8\xFF", "jpg"),
        (b"II*\0", "tif"),
        (b"MM\0*", "tif"),
        (b"II+\0", "tif"),
    ];
    if let Some((_, name)) = FIXED.iter().find(|(m, _)| b.starts_with(m)) {
        return Some(name);
    }
    if b.starts_with(b"PK\x03\x04") || b.starts_with(b"PK\x05\x06") || b.starts_with(b"PK\x07\x08") {
        return Some(zip_kind(b));
    }
    if b.starts_with(b"RIFF") && b.len() >= 12 {
        return match &b[8..12] {
            b"WEBP" => Some("webp"),
            b"WAVE" => Some("wav"),
            b"AVI " => Some("avi"),
            _ => None,
        };
    }
    if at(b, 4, b"ftyp") {
        return ftyp(b);
    }
    if b.starts_with(b"\x1A\x45\xDF\xA3") {
        return Some(ebml_doc_type(b));
    }
    if b.starts_with(b"MZ") {
        return Some(pe_kind(b));
    }
    if b.starts_with(b"\x1F\x8B\x08") {
        return Some("gz");
    }
    if b.starts_with(b"BZh") && b.get(3).is_some_and(|c| (b'1'..=b'9').contains(c)) {
        return Some("bz2");
    }
    if at(b, 257, b"ustar") {
        return Some("tar");
    }
    if b.starts_with(b"\xCA\xFE\xBA\xBE") && u16be(b, 6).is_some_and(|major| (45..100).contains(&major)) {
        return Some("class");
    }
    if (b.starts_with(b"\0\x01\0\0") || b.starts_with(b"true")) && u16be(b, 4).is_some_and(|n| (1..=64).contains(&n)) {
        return Some("ttf");
    }
    if b.starts_with(b"\0\0\x01\0") && is_ico(b) {
        return Some("ico");
    }
    if b.starts_with(b"BM") && is_bmp(b) {
        return Some("bmp");
    }
    if b.starts_with(b"\0\0\x01\xBA") || b.starts_with(b"\0\0\x01\xB3") {
        return Some("mpg");
    }
    if b.starts_with(b"ID3") {
        return Some(id3_kind(b));
    }
    if let Some(kind) = mpeg_audio(b) {
        return Some(kind);
    }
    if b.len() >= 377 && b[0] == 0x47 && b[188] == 0x47 && b[376] == 0x47 {
        return Some("ts");
    }
    None
}

/// `true` if the 5 bytes at [`ISO_PROBE_OFFSET`] identify an ISO 9660 or UDF image.
pub fn detect_iso(probe: &[u8]) -> bool {
    [b"CD001", b"BEA01", b"NSR02", b"NSR03"].iter().any(|m| probe.starts_with(*m))
}

/// `true` if `head` is all zeros (the system area of an ISO image), so a probe at
/// [`ISO_PROBE_OFFSET`] may identify it.
pub fn wants_iso_probe(head: &[u8]) -> bool {
    head.len() >= HEAD_BYTES && head.iter().all(|&c| c == 0)
}

fn is_ico(b: &[u8]) -> bool {
    let Some(count) = u16le(b, 4) else { return false };
    // First directory entry: reserved byte 0, planes 0/1.
    count > 0 && b.get(9) == Some(&0) && u16le(b, 10).is_some_and(|p| p <= 1)
}

fn is_bmp(b: &[u8]) -> bool {
    let reserved_zero = u32le(b, 6) == Some(0);
    let dib = u32le(b, 14);
    reserved_zero && dib.is_some_and(|d| matches!(d, 12 | 16 | 40 | 52 | 56 | 64 | 108 | 124))
}

/// `exe` or `dll` from the PE header (`exe` for DOS executables without one).
fn pe_kind(b: &[u8]) -> &'static str {
    let Some(pe) = u32le(b, 0x3C).and_then(|v| usize::try_from(v).ok()) else { return "exe" };
    if at(b, pe, b"PE\0\0") && u16le(b, pe.saturating_add(22)).is_some_and(|ch| ch & 0x2000 != 0) {
        return "dll";
    }
    "exe"
}

/// `flac` with a leading ID3v2 tag, else `mp3`.
fn id3_kind(b: &[u8]) -> &'static str {
    let size = b.get(6..10).map(|s| s.iter().fold(0usize, |acc, &x| (acc << 7) | usize::from(x & 0x7F)));
    match size.map(|s| s + 10) {
        Some(end) if at(b, end, b"fLaC") => "flac",
        _ => "mp3",
    }
}

/// MPEG audio frame sync (`mp3` for layer III, `aac` for ADTS); a second frame is
/// required when the first one ends inside `b`.
fn mpeg_audio(b: &[u8]) -> Option<&'static str> {
    let (b0, b1, b2) = (*b.first()?, *b.get(1)?, *b.get(2)?);
    if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }
    let version = (b1 >> 3) & 3; // 0 = 2.5, 2 = 2, 3 = 1
    let layer = (b1 >> 1) & 3; // 1 = III
    if layer == 0 {
        // ADTS (AAC): layer bits are 00, sampling index < 13.
        return (b1 & 0xF6 == 0xF0 && (b2 >> 2) & 0xF < 13).then_some("aac");
    }
    let bitrate_index = usize::from(b2 >> 4);
    let rate_index = usize::from((b2 >> 2) & 3);
    if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    const V1_L3: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
    const V2_L3: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    const RATES: [[u32; 3]; 4] = [[11025, 12000, 8000], [0, 0, 0], [22050, 24000, 16000], [44100, 48000, 32000]];
    let kbps = if version == 3 { V1_L3[bitrate_index] } else { V2_L3[bitrate_index] };
    let rate = RATES[usize::from(version)][rate_index];
    let coeff = if version == 3 { 144 } else { 72 };
    let padding = u32::from((b2 >> 1) & 1);
    let len = (coeff * kbps * 1000 / rate + padding) as usize;
    if len < 4 {
        return None;
    }
    match b.get(len..len + 2) {
        Some(next) => (next[0] == 0xFF && next[1] & 0xE0 == 0xE0).then_some("mp3"),
        None => Some("mp3"),
    }
}

/// ISO base media file (`ftyp` box): HEIF/AVIF images, MP4/M4A/MOV/3GP.
fn ftyp(b: &[u8]) -> Option<&'static str> {
    let size = u32be(b, 0).and_then(|s| usize::try_from(s).ok()).unwrap_or(0).clamp(16, b.len().max(16));
    let major = b.get(8..12)?;
    let compat: Vec<&[u8]> =
        b.get(16..size.min(b.len())).unwrap_or(&[]).as_chunks::<4>().0.iter().map(|c| &c[..]).collect();
    let has = |brand: &[u8]| major == brand || compat.contains(&brand);
    let major_is = |list: &[&[u8]]| list.contains(&major);
    if major_is(&[b"avif", b"avis"]) {
        return Some("avif");
    }
    if major_is(&[b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"]) {
        return Some("heic");
    }
    if major_is(&[b"mif1", b"msf1"]) {
        return Some(if has(b"avif") { "avif" } else { "heic" });
    }
    if major == b"qt  " {
        return Some("mov");
    }
    if major_is(&[b"M4A ", b"M4B ", b"M4P "]) {
        return Some("m4a");
    }
    if major.starts_with(b"3g") {
        return Some("3gp");
    }
    if major == b"crx " {
        return None;
    }
    Some("mp4")
}

/// `webm` or `mkv` from the EBML DocType element.
fn ebml_doc_type(b: &[u8]) -> &'static str {
    let window = &b[..b.len().min(128)];
    if let Some(i) = window.windows(2).position(|w| w == [0x42, 0x82]) {
        // One-byte vint size (`0x8N`) followed by the text.
        if let Some(&sz) = window.get(i + 2)
            && sz & 0x80 != 0
        {
            let n = usize::from(sz & 0x7F);
            if window.get(i + 3..i + 3 + n) == Some(b"webm") {
                return "webm";
            }
        }
    }
    "mkv"
}

/// Refines a ZIP container from its local file headers in `b`.
fn zip_kind(b: &[u8]) -> &'static str {
    let mut pos = 0usize;
    let mut names: Vec<&[u8]> = Vec::new();
    let mut content_types: Option<&'static str> = None;
    while names.len() < 64 && at(b, pos, b"PK\x03\x04") {
        let (Some(flags), Some(method), Some(csize), Some(nlen), Some(xlen)) =
            (u16le(b, pos + 6), u16le(b, pos + 8), u32le(b, pos + 18), u16le(b, pos + 26), u16le(b, pos + 28))
        else {
            break;
        };
        let name_at = pos + 30;
        let Some(name) = b.get(name_at..name_at + usize::from(nlen)) else { break };
        names.push(name);
        let data_at = name_at + usize::from(nlen) + usize::from(xlen);
        let data = b.get(data_at..).unwrap_or(&[]);
        let data = &data[..data.len().min(csize as usize)];
        if names.len() == 1 && name == b"mimetype" && method == 0 {
            let mime = data;
            for (m, kind) in [
                (&b"application/epub+zip"[..], "epub"),
                (b"application/vnd.oasis.opendocument.text", "odt"),
                (b"application/vnd.oasis.opendocument.spreadsheet", "ods"),
                (b"application/vnd.oasis.opendocument.presentation", "odp"),
            ] {
                if mime.starts_with(m) {
                    return kind;
                }
            }
        }
        if name == b"[Content_Types].xml" {
            content_types = ooxml_from_content_types(data, method);
            if content_types.is_some() {
                break;
            }
        }
        // Sizes unknown until the data descriptor: the next header cannot be located.
        if flags & 0x08 != 0 && csize == 0 {
            break;
        }
        pos = data_at.saturating_add(csize as usize);
    }
    if let Some(k) = content_types {
        return k;
    }
    let any = |pred: &dyn Fn(&[u8]) -> bool| names.iter().any(|n| pred(n));
    if any(&|n| n.starts_with(b"word/")) {
        return "docx";
    }
    if any(&|n| n.starts_with(b"xl/")) {
        return "xlsx";
    }
    if any(&|n| n.starts_with(b"ppt/")) {
        return "pptx";
    }
    if any(&|n| n == b"AndroidManifest.xml" || n == b"classes.dex" || n == b"resources.arsc") {
        return "apk";
    }
    if any(&|n| n.eq_ignore_ascii_case(b"META-INF/MANIFEST.MF") || n.ends_with(b".class")) {
        return "jar";
    }
    "zip"
}

/// OOXML document kind from the (possibly partial) `[Content_Types].xml` part.
fn ooxml_from_content_types(data: &[u8], method: u16) -> Option<&'static str> {
    let text: std::borrow::Cow<'_, [u8]> = match method {
        0 => data.into(),
        8 => inflate_prefix(data)?.into(),
        _ => return None,
    };
    let find = |needle: &[u8]| memchr::memmem::find(&text, needle).is_some();
    if find(b"wordprocessingml.document") || find(b"wordprocessingml.template") {
        Some("docx")
    } else if find(b"spreadsheetml.sheet") || find(b"spreadsheetml.template") || find(b"ms-excel.sheet") {
        Some("xlsx")
    } else if find(b"presentationml.presentation") || find(b"presentationml.slideshow") {
        Some("pptx")
    } else {
        None
    }
}

/// Inflates as much of a raw deflate stream as the bytes allow (truncated input gives
/// the decoded prefix).
#[cfg(feature = "extract")]
fn inflate_prefix(data: &[u8]) -> Option<Vec<u8>> {
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};
    let mut out = vec![0u8; 64 * 1024];
    let mut d = DecompressorOxide::new();
    let (_, _, written) =
        decompress(&mut d, data, &mut out, 0, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF);
    out.truncate(written);
    (!out.is_empty()).then_some(out)
}

#[cfg(not(feature = "extract"))]
fn inflate_prefix(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(mut v: Vec<u8>, n: usize) -> Vec<u8> {
        v.resize(v.len().max(n), 0);
        v
    }

    /// A ZIP local file header + data.
    fn zip_entry(name: &str, data: &[u8], method: u16) -> Vec<u8> {
        let mut v = b"PK\x03\x04".to_vec();
        v.extend_from_slice(&20u16.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&method.to_le_bytes());
        v.extend_from_slice(&[0; 8]); // time, date, crc
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&(name.len() as u16).to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(data);
        v
    }

    fn ftyp_box(major: &[u8; 4], compat: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compat.len();
        let mut v = (size as u32).to_be_bytes().to_vec();
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(major);
        v.extend_from_slice(&[0, 0, 2, 0]);
        for c in compat {
            v.extend_from_slice(*c);
        }
        v.extend_from_slice(b"\0\0\0\x08free");
        v
    }

    fn pe(dll: bool) -> Vec<u8> {
        let mut v = pad(b"MZ".to_vec(), 0x80);
        v[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        v.extend_from_slice(b"PE\0\0");
        v.extend_from_slice(&0x8664u16.to_le_bytes());
        v.extend_from_slice(&[0; 16]);
        let ch: u16 = if dll { 0x2022 } else { 0x0022 };
        v.extend_from_slice(&ch.to_le_bytes());
        pad(v, 0x200)
    }

    fn mp3_frames() -> Vec<u8> {
        // MPEG-1 layer III, 128 kbps, 44.1 kHz: 417-byte frames.
        let mut v = Vec::new();
        for _ in 0..3 {
            let mut f = vec![0xFF, 0xFB, 0x90, 0x00];
            f.resize(417, 0);
            v.extend(f);
        }
        v
    }

    #[test]
    fn known_signatures() {
        let cases: Vec<(Vec<u8>, Option<&str>)> = vec![
            (b"\xFF\xD8\xFF\xE0\0\x10JFIF".to_vec(), Some("jpg")),
            (b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(), Some("png")),
            (b"GIF89a\x01\0".to_vec(), Some("gif")),
            (
                pad(
                    [
                        b"BM".to_vec(),
                        70u32.to_le_bytes().to_vec(),
                        vec![0; 4],
                        54u32.to_le_bytes().to_vec(),
                        40u32.to_le_bytes().to_vec(),
                    ]
                    .concat(),
                    60,
                ),
                Some("bmp"),
            ),
            (b"BMx".to_vec(), None),
            (b"II*\0\x08\0\0\0".to_vec(), Some("tif")),
            (b"MM\0*\0\0\0\x08".to_vec(), Some("tif")),
            (b"RIFF\0\0\0\0WEBPVP8 ".to_vec(), Some("webp")),
            (pad(b"\0\0\x01\0\x01\0\x10\x10\0\0\x01\0\x20\0".to_vec(), 40), Some("ico")),
            (b"8BPS\0\x01".to_vec(), Some("psd")),
            (ftyp_box(b"heic", &[b"mif1", b"heic"]), Some("heic")),
            (ftyp_box(b"mif1", &[b"mif1", b"heic"]), Some("heic")),
            (ftyp_box(b"avif", &[b"avif", b"mif1"]), Some("avif")),
            (ftyp_box(b"mif1", &[b"avif"]), Some("avif")),
            (ftyp_box(b"isom", &[b"isom", b"avc1"]), Some("mp4")),
            (ftyp_box(b"mp42", &[b"mp41"]), Some("mp4")),
            (ftyp_box(b"qt  ", &[b"qt  "]), Some("mov")),
            (ftyp_box(b"M4A ", &[b"M4A ", b"mp42"]), Some("m4a")),
            (ftyp_box(b"3gp5", &[]), Some("3gp")),
            (b"%PDF-1.7\n".to_vec(), Some("pdf")),
            (b"7z\xBC\xAF\x27\x1C\0\x04".to_vec(), Some("7z")),
            (b"Rar!\x1a\x07\x00".to_vec(), Some("rar")),
            (b"Rar!\x1a\x07\x01\x00".to_vec(), Some("rar")),
            (b"\x1F\x8B\x08\0".to_vec(), Some("gz")),
            (b"BZh91AY&SY".to_vec(), Some("bz2")),
            (b"BZhx".to_vec(), None),
            (b"\xFD7zXZ\0\0".to_vec(), Some("xz")),
            (b"\x28\xB5\x2F\xFD\x04".to_vec(), Some("zst")),
            (
                {
                    let mut v = pad(b"file.txt".to_vec(), 512);
                    v[257..263].copy_from_slice(b"ustar\0");
                    v
                },
                Some("tar"),
            ),
            (b"MSCF\0\0\0\0\x10".to_vec(), Some("cab")),
            (pe(false), Some("exe")),
            (pe(true), Some("dll")),
            (pad(b"MZ".to_vec(), 64), Some("exe")),
            (b"\x7FELF\x02\x01\x01".to_vec(), Some("elf")),
            (b"ID3\x04\0\0\0\0\0\x10".to_vec(), Some("mp3")),
            ([b"ID3\x04\0\0\0\0\0\x02\0\0".to_vec(), b"fLaC".to_vec()].concat(), Some("flac")),
            (mp3_frames(), Some("mp3")),
            (b"\xFF\xF1\x50\x80\x02\x1F\xFC".to_vec(), Some("aac")),
            (
                {
                    let mut v = mp3_frames();
                    v[417] = 0;
                    v
                },
                None,
            ),
            (b"fLaC\0\0\0\x22".to_vec(), Some("flac")),
            (b"RIFF\0\0\0\0WAVEfmt ".to_vec(), Some("wav")),
            (b"OggS\0\x02".to_vec(), Some("ogg")),
            (b"\x1A\x45\xDF\xA3\x9F\x42\x86\x81\x01\x42\x82\x88matroska".to_vec(), Some("mkv")),
            (b"\x1A\x45\xDF\xA3\x9F\x42\x86\x81\x01\x42\x82\x84webm".to_vec(), Some("webm")),
            (b"RIFF\0\0\0\0AVI LIST".to_vec(), Some("avi")),
            (b"\0\0\x01\xBA\x44".to_vec(), Some("mpg")),
            (b"SQLite format 3\0\x10\0".to_vec(), Some("sqlite")),
            (b"{\\rtf1\\ansi".to_vec(), Some("rtf")),
            (b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1\0\0".to_vec(), Some("ole")),
            (b"\xCA\xFE\xBA\xBE\0\0\0\x34".to_vec(), Some("class")),
            (b"\xCA\xFE\xBA\xBE\0\0\0\x02".to_vec(), None),
            (b"\0asm\x01\0\0\0".to_vec(), Some("wasm")),
            (b"\0\x01\0\0\0\x0E\0\x80".to_vec(), Some("ttf")),
            (b"OTTO\0\x0A".to_vec(), Some("otf")),
            (b"wOFF\0\x01\0\0".to_vec(), Some("woff")),
            (b"wOF2\0\x01\0\0".to_vec(), Some("woff2")),
            (crate::link::tests::lnk(Some("C:\\a"), "", false, None), Some("lnk")),
            (b"hello world".to_vec(), None),
            (Vec::new(), None),
            (vec![0; 5000], None),
        ];
        for (bytes, want) in &cases {
            assert_eq!(detect(bytes), *want, "{:x?}", &bytes[..bytes.len().min(16)]);
        }
        let ts: Vec<u8> = (0..3).flat_map(|_| [vec![0x47], vec![0u8; 187]].concat()).collect();
        assert_eq!(detect(&ts), Some("ts"));
        for (name, _) in NAMES {
            assert_eq!(canonical(name), Some(*name));
        }
        assert_eq!(canonical("JPEG"), Some("jpg"));
        assert_eq!(canonical(".tiff"), Some("tif"));
        assert_eq!(canonical("doc"), None);
    }

    #[test]
    fn zip_containers() {
        let plain = [zip_entry("a.txt", b"hello", 0), zip_entry("b.txt", b"x", 0)].concat();
        assert_eq!(detect(&plain), Some("zip"));
        let epub = [zip_entry("mimetype", b"application/epub+zip", 0), zip_entry("OEBPS/x", b"", 0)].concat();
        assert_eq!(detect(&epub), Some("epub"));
        let odt = zip_entry("mimetype", b"application/vnd.oasis.opendocument.text", 0);
        assert_eq!(detect(&odt), Some("odt"));
        let ct = br#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        assert_eq!(detect(&zip_entry("[Content_Types].xml", ct, 0)), Some("docx"));
        #[cfg(feature = "extract")]
        {
            let deflated = miniz_oxide::deflate::compress_to_vec(ct, 6);
            assert_eq!(detect(&zip_entry("[Content_Types].xml", &deflated, 8)), Some("docx"));
            let xl = br#"<Types><Override ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
            let deflated = miniz_oxide::deflate::compress_to_vec(xl, 6);
            assert_eq!(detect(&zip_entry("[Content_Types].xml", &deflated, 8)), Some("xlsx"));
        }
        let pptx = [zip_entry("[Content_Types].xml", b"\x01\x02", 9), zip_entry("ppt/slides/s1.xml", b"", 0)].concat();
        assert_eq!(detect(&pptx), Some("pptx"));
        let xlsx = [zip_entry("_rels/.rels", b"", 0), zip_entry("xl/workbook.xml", b"", 0)].concat();
        assert_eq!(detect(&xlsx), Some("xlsx"));
        let apk = [zip_entry("META-INF/MANIFEST.MF", b"", 0), zip_entry("AndroidManifest.xml", b"", 0)].concat();
        assert_eq!(detect(&apk), Some("apk"));
        let jar =
            [zip_entry("META-INF/", b"", 0), zip_entry("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0", 0)].concat();
        assert_eq!(detect(&jar), Some("jar"));
        assert_eq!(detect(&zip_entry("com/x/A.class", b"\xCA\xFE", 0)), Some("jar"));
        assert_eq!(detect(b"PK\x05\x06\0\0\0\0"), Some("zip"));
    }

    #[test]
    fn iso_probe() {
        assert!(wants_iso_probe(&[0u8; HEAD_BYTES]));
        assert!(!wants_iso_probe(&[0u8; 100]));
        assert!(detect_iso(b"CD001"));
        assert!(detect_iso(b"BEA01\x01"));
        assert!(!detect_iso(b"CD00"));
    }

    #[test]
    fn fuzz_never_panics() {
        let mut rng = crate::link::tests::Rng(0xDEAD_BEEF_1234_5678);
        let seeds: Vec<Vec<u8>> = vec![
            pe(true),
            mp3_frames(),
            ftyp_box(b"mif1", &[b"avif"]),
            [zip_entry("[Content_Types].xml", b"\x78\x9c\x01\x02", 8), zip_entry("word/x", b"", 0)].concat(),
            b"ID3\x04\0\x7F\x7F\x7F\x7F".to_vec(),
            b"\x1A\x45\xDF\xA3\x42\x82\xFF".to_vec(),
        ];
        for s in &seeds {
            for n in 0..=s.len() {
                let _ = detect(&s[..n]);
            }
            for _ in 0..2000 {
                let mut v = s.clone();
                for _ in 0..1 + rng.next() % 6 {
                    let i = (rng.next() as usize) % v.len();
                    v[i] = rng.next() as u8;
                }
                let _ = detect(&v);
            }
        }
        for _ in 0..5000 {
            let len = (rng.next() % 700) as usize;
            let v: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let _ = detect(&v);
        }
    }
}

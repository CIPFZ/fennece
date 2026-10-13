//! Windows recycle bin records for `recycled:` / `recyclebinoriginalpath:` (SPEC ch.2 §5.1).
//!
//! Since Windows Vista a deleted item `X` of user `<SID>` becomes
//! `<drive>\$Recycle.Bin\<SID>\$R<id><.ext>` (a file or a whole folder), and a sibling
//! `$I<id><.ext>` record keeps its original path:
//!
//! * version 1 (Vista–8): `u64` version = 1, `u64` size, `FILETIME` deletion time,
//!   520 bytes of NUL-padded UTF-16LE path (`MAX_PATH`);
//! * version 2 (Windows 10+): `u64` version = 2, `u64` size, `FILETIME` deletion time,
//!   `u32` character count (including the terminating NUL), UTF-16LE path.

/// Name of the recycle bin folder at a volume root (folded, as in the index).
pub const BIN_FOLDER_FOLD: &str = "$recycle.bin";

/// `$I` records are at most this long (v2 with a 32,767-character path).
pub const MAX_INFO_BYTES: usize = 28 + 2 * 32_768;

/// A parsed `$I` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecycleInfo {
    /// Original full path of the deleted item.
    pub original_path: String,
    /// Size of the deleted item in bytes (folders: total size).
    pub size: u64,
    /// Deletion time (FILETIME).
    pub deleted: i64,
}

fn u64le(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

fn utf16_until_nul(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Parses a `$I` record (`None` for other data or an empty path).
pub fn parse_info(data: &[u8]) -> Option<RecycleInfo> {
    let version = u64le(data, 0)?;
    let size = u64le(data, 8)?;
    let deleted = i64::try_from(u64le(data, 16)?).unwrap_or(0);
    let path = match version {
        1 => utf16_until_nul(data.get(24..24 + 520).or_else(|| data.get(24..))?),
        2 => {
            let count = u32::from_le_bytes(data.get(24..28)?.try_into().ok()?) as usize;
            if count == 0 || count > 32_768 {
                return None;
            }
            let end = 28 + count * 2;
            utf16_until_nul(data.get(28..end.min(data.len()))?)
        }
        _ => return None,
    };
    (!path.is_empty()).then_some(RecycleInfo { original_path: path, size, deleted })
}

/// `true` if `fold` (a folded file name) is a deleted item's data name (`$R…`).
pub fn is_data_name(fold: &[u8]) -> bool {
    fold.len() > 2 && fold.starts_with(b"$r")
}

/// Name of the `$I` record of the deleted item named `r_name` (`$RABC123.txt` →
/// `$IABC123.txt`); `None` if `r_name` is not a `$R` name.
pub fn info_name(r_name: &str) -> Option<String> {
    let rest = r_name.strip_prefix("$R").or_else(|| r_name.strip_prefix("$r"))?;
    (!rest.is_empty()).then(|| format!("$I{rest}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A version-2 record.
    pub(crate) fn info_v2(path: &str, size: u64) -> Vec<u8> {
        let units: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let mut v = 2u64.to_le_bytes().to_vec();
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(&133_000_000_000_000_000u64.to_le_bytes());
        v.extend_from_slice(&(units.len() as u32).to_le_bytes());
        v.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        v
    }

    /// A version-1 record.
    pub(crate) fn info_v1(path: &str, size: u64) -> Vec<u8> {
        let mut v = 1u64.to_le_bytes().to_vec();
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(&133_000_000_000_000_000u64.to_le_bytes());
        let mut p: Vec<u8> = path.encode_utf16().flat_map(u16::to_le_bytes).collect();
        p.resize(520, 0);
        v.extend(p);
        v
    }

    #[test]
    fn both_versions() {
        let a = parse_info(&info_v2("C:\\Users\\me\\Documents\\Report Q3.docx", 1234)).unwrap();
        assert_eq!(a.original_path, "C:\\Users\\me\\Documents\\Report Q3.docx");
        assert_eq!(a.size, 1234);
        assert_eq!(a.deleted, 133_000_000_000_000_000);
        let b = parse_info(&info_v1("D:\\Zoë\\季度.txt", 9)).unwrap();
        assert_eq!(b.original_path, "D:\\Zoë\\季度.txt");
        assert!(parse_info(&info_v1("", 0)).is_none());
        assert!(parse_info(b"").is_none());
        assert!(parse_info(&[3u8; 600]).is_none());
        let mut bad = info_v2("C:\\x", 1);
        bad[24..28].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_info(&bad).is_none());
        assert_eq!(info_name("$RAB12CD.txt").as_deref(), Some("$IAB12CD.txt"));
        assert_eq!(info_name("$R"), None);
        assert_eq!(info_name("x.txt"), None);
        assert!(is_data_name(b"$rab12.txt") && !is_data_name(b"$iab12.txt") && !is_data_name(b"$r"));
    }

    #[test]
    fn truncation_and_noise_never_panic() {
        for rec in [info_v2("C:\\Users\\me\\a.txt", 5), info_v1("C:\\a", 1)] {
            for n in 0..=rec.len() {
                let _ = parse_info(&rec[..n]);
            }
        }
        let mut rng = crate::link::tests::Rng(42);
        for _ in 0..5000 {
            let len = (rng.next() % 700) as usize;
            let mut v: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if v.len() >= 8 {
                v[..8].copy_from_slice(&(1 + rng.next() % 2).to_le_bytes());
            }
            let _ = parse_info(&v);
        }
    }
}

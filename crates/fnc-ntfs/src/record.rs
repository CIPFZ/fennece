//! `USN_RECORD_V2` / `USN_RECORD_V3` parsing (portable; tested with byte samples).
//!
//! Layouts (little endian):
//!
//! ```text
//! V2: 0 RecordLength u32 | 4 Major u16 | 6 Minor u16 | 8 FRN u64 | 16 ParentFRN u64
//!     24 Usn i64 | 32 TimeStamp i64 | 40 Reason u32 | 44 SourceInfo u32
//!     48 SecurityId u32 | 52 FileAttributes u32 | 56 NameLength u16 | 58 NameOffset u16
//! V3: 0 RecordLength u32 | 4 Major u16 | 6 Minor u16 | 8 FRN [u8;16] | 24 ParentFRN [u8;16]
//!     40 Usn i64 | 48 TimeStamp i64 | 56 Reason u32 | 60 SourceInfo u32
//!     64 SecurityId u32 | 68 FileAttributes u32 | 72 NameLength u16 | 74 NameOffset u16
//! ```

/// `USN_REASON_*` flags.
pub mod reason {
    /// Data overwritten.
    pub const DATA_OVERWRITE: u32 = 0x0000_0001;
    /// Data extended.
    pub const DATA_EXTEND: u32 = 0x0000_0002;
    /// Data truncated.
    pub const DATA_TRUNCATION: u32 = 0x0000_0004;
    /// Named stream overwritten.
    pub const NAMED_DATA_OVERWRITE: u32 = 0x0000_0010;
    /// Named stream extended.
    pub const NAMED_DATA_EXTEND: u32 = 0x0000_0020;
    /// Named stream truncated.
    pub const NAMED_DATA_TRUNCATION: u32 = 0x0000_0040;
    /// File created.
    pub const FILE_CREATE: u32 = 0x0000_0100;
    /// File deleted.
    pub const FILE_DELETE: u32 = 0x0000_0200;
    /// Extended attributes changed.
    pub const EA_CHANGE: u32 = 0x0000_0400;
    /// Security changed.
    pub const SECURITY_CHANGE: u32 = 0x0000_0800;
    /// Old name of a rename.
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    /// New name of a rename.
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    /// Indexable attribute changed.
    pub const INDEXABLE_CHANGE: u32 = 0x0000_4000;
    /// Basic info (attributes, times) changed.
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    /// Hard link added/removed.
    pub const HARD_LINK_CHANGE: u32 = 0x0001_0000;
    /// Compression changed.
    pub const COMPRESSION_CHANGE: u32 = 0x0002_0000;
    /// Encryption changed.
    pub const ENCRYPTION_CHANGE: u32 = 0x0004_0000;
    /// Object id changed.
    pub const OBJECT_ID_CHANGE: u32 = 0x0008_0000;
    /// Reparse point changed.
    pub const REPARSE_POINT_CHANGE: u32 = 0x0010_0000;
    /// Stream added/removed/renamed.
    pub const STREAM_CHANGE: u32 = 0x0020_0000;
    /// Handle closed.
    pub const CLOSE: u32 = 0x8000_0000;
    /// Everything.
    pub const ALL: u32 = 0xFFFF_FFFF;
    /// Reasons that change size or dates (need a metadata refresh on close).
    pub const METADATA: u32 = DATA_OVERWRITE
        | DATA_EXTEND
        | DATA_TRUNCATION
        | NAMED_DATA_OVERWRITE
        | NAMED_DATA_EXTEND
        | NAMED_DATA_TRUNCATION
        | BASIC_INFO_CHANGE
        | COMPRESSION_CHANGE
        | ENCRYPTION_CHANGE
        | REPARSE_POINT_CHANGE;
}

/// A parsed USN record borrowing its name from the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsnRecord<'a> {
    /// Record major version (2 or 3).
    pub major: u16,
    /// File reference number (128-bit ids folded to 64 bits, see [`fold_file_id`]).
    pub frn: u64,
    /// Parent's file reference number.
    pub parent_frn: u64,
    /// USN of this record.
    pub usn: i64,
    /// FILETIME of the change.
    pub timestamp: i64,
    /// `USN_REASON_*` flags.
    pub reason: u32,
    /// Win32 attributes.
    pub attributes: u32,
    /// File name as raw UTF-16LE bytes.
    name_bytes: &'a [u8],
}

impl UsnRecord<'_> {
    /// Decodes the name into `out` (cleared first).
    pub fn name_into(&self, out: &mut Vec<u16>) {
        out.clear();
        out.extend(self.name_bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)));
    }

    /// Name length in UTF-16 units.
    pub fn name_len(&self) -> usize {
        self.name_bytes.len() / 2
    }
}

/// Parse errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// Fewer bytes than the header or `RecordLength`.
    #[error("truncated record at offset {0}")]
    Truncated(usize),
    /// Unsupported major version.
    #[error("unsupported USN record version {0}")]
    Version(u16),
    /// Name outside the record.
    #[error("bad name offset/length at offset {0}")]
    BadName(usize),
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Folds a 128-bit `FILE_ID_128` to 64 bits: NTFS ids are zero-extended FRNs and map
/// to themselves; genuine 128-bit ReFS ids are hashed.
// SPEC-GAP: ReFS 128-bit ids are folded with xxh3 (collision probability ~2^-64 per
// pair); the index stores 64-bit FRNs.
pub fn fold_file_id(id: &[u8; 16]) -> u64 {
    let lo = u64::from_le_bytes([id[0], id[1], id[2], id[3], id[4], id[5], id[6], id[7]]);
    let hi = u64::from_le_bytes([id[8], id[9], id[10], id[11], id[12], id[13], id[14], id[15]]);
    if hi == 0 { lo } else { xxhash_rust::xxh3::xxh3_64(id) }
}

/// Parses the record at the start of `buf`; returns it and its length.
pub fn parse_record(buf: &[u8], base: usize) -> Result<(UsnRecord<'_>, usize), RecordError> {
    let len = u32_at(buf, 0).ok_or(RecordError::Truncated(base))? as usize;
    if len < 8 || len > buf.len() {
        return Err(RecordError::Truncated(base));
    }
    let rec = &buf[..len];
    let major = u16_at(rec, 4).ok_or(RecordError::Truncated(base))?;
    let t = RecordError::Truncated(base);
    let (frn, parent, usn, ts, reason, attrs, nlen, noff) = match major {
        2 => (
            u64_at(rec, 8).ok_or(t.clone())?,
            u64_at(rec, 16).ok_or(t.clone())?,
            u64_at(rec, 24).ok_or(t.clone())? as i64,
            u64_at(rec, 32).ok_or(t.clone())? as i64,
            u32_at(rec, 40).ok_or(t.clone())?,
            u32_at(rec, 52).ok_or(t.clone())?,
            u16_at(rec, 56).ok_or(t.clone())?,
            u16_at(rec, 58).ok_or(t)?,
        ),
        3 => {
            let id = |o: usize| -> Option<[u8; 16]> { rec.get(o..o + 16)?.try_into().ok() };
            (
                fold_file_id(&id(8).ok_or(t.clone())?),
                fold_file_id(&id(24).ok_or(t.clone())?),
                u64_at(rec, 40).ok_or(t.clone())? as i64,
                u64_at(rec, 48).ok_or(t.clone())? as i64,
                u32_at(rec, 56).ok_or(t.clone())?,
                u32_at(rec, 68).ok_or(t.clone())?,
                u16_at(rec, 72).ok_or(t.clone())?,
                u16_at(rec, 74).ok_or(t)?,
            )
        }
        v => return Err(RecordError::Version(v)),
    };
    let (noff, nlen) = (noff as usize, nlen as usize);
    let name_bytes = rec.get(noff..noff + nlen).filter(|_| nlen % 2 == 0).ok_or(RecordError::BadName(base))?;
    Ok((UsnRecord { major, frn, parent_frn: parent, usn, timestamp: ts, reason, attributes: attrs, name_bytes }, len))
}

/// Iterates records in a buffer returned by `FSCTL_ENUM_USN_DATA` / `FSCTL_READ_USN_JOURNAL`
/// (after the leading 8-byte next-FRN/next-USN value). Stops at the first malformed record.
pub fn records(buf: &[u8]) -> impl Iterator<Item = Result<UsnRecord<'_>, RecordError>> {
    let mut off = 0usize;
    let mut failed = false;
    std::iter::from_fn(move || {
        if failed || off >= buf.len() {
            return None;
        }
        match parse_record(&buf[off..], off) {
            Ok((r, len)) => {
                // Records are 8-byte aligned.
                off += len.div_ceil(8) * 8;
                Some(Ok(r))
            }
            Err(e) => {
                failed = true;
                Some(Err(e))
            }
        }
    })
}

/// Builds a V2 record (tests and the service protocol's fixtures).
pub fn encode_v2(frn: u64, parent: u64, usn: i64, reason: u32, attributes: u32, name: &str) -> Vec<u8> {
    let name16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let len = (60 + name16.len()).div_ceil(8) * 8;
    let mut b = vec![0u8; len];
    b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    b[4..6].copy_from_slice(&2u16.to_le_bytes());
    b[8..16].copy_from_slice(&frn.to_le_bytes());
    b[16..24].copy_from_slice(&parent.to_le_bytes());
    b[24..32].copy_from_slice(&usn.to_le_bytes());
    b[32..40].copy_from_slice(&133_000_000_000_000_000i64.to_le_bytes());
    b[40..44].copy_from_slice(&reason.to_le_bytes());
    b[52..56].copy_from_slice(&attributes.to_le_bytes());
    b[56..58].copy_from_slice(&(name16.len() as u16).to_le_bytes());
    b[58..60].copy_from_slice(&60u16.to_le_bytes());
    b[60..60 + name16.len()].copy_from_slice(&name16);
    b
}

/// Builds a V3 record.
pub fn encode_v3(frn: [u8; 16], parent: [u8; 16], usn: i64, reason: u32, attributes: u32, name: &str) -> Vec<u8> {
    let name16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let len = (76 + name16.len()).div_ceil(8) * 8;
    let mut b = vec![0u8; len];
    b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    b[4..6].copy_from_slice(&3u16.to_le_bytes());
    b[8..24].copy_from_slice(&frn);
    b[24..40].copy_from_slice(&parent);
    b[40..48].copy_from_slice(&usn.to_le_bytes());
    b[56..60].copy_from_slice(&reason.to_le_bytes());
    b[68..72].copy_from_slice(&attributes.to_le_bytes());
    b[72..74].copy_from_slice(&(name16.len() as u16).to_le_bytes());
    b[74..76].copy_from_slice(&76u16.to_le_bytes());
    b[76..76 + name16.len()].copy_from_slice(&name16);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-assembled V2 record laid out byte-for-byte as documented for
    /// `USN_RECORD_V2` (file `a.txt` created in the root folder): 0x60 bytes.
    const SAMPLE_V2: [u8; 96] = [
        0x60, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, // len 96, v2.0
        0x2a, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, // FRN 0x0003_0000_0000_1f2a
        0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, // parent 0x0005_0000_0000_0005 (root)
        0x00, 0x10, 0x5c, 0x07, 0x00, 0x00, 0x00, 0x00, // USN 0x075c1000
        0x80, 0x3d, 0x93, 0x48, 0x2b, 0x0b, 0xdd, 0x01, // timestamp
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reason FILE_CREATE, source 0
        0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, // security 0, attrs ARCHIVE
        0x0a, 0x00, 0x3c, 0x00, b'a', 0x00, b'.', 0x00, // name len 10, offset 60, "a."
        b't', 0x00, b'x', 0x00, b't', 0x00, 0x00, 0x00, // "txt", pad
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn parses_documented_v2_layout() {
        let (r, len) = parse_record(&SAMPLE_V2, 0).unwrap();
        assert_eq!(len, 96);
        assert_eq!(r.major, 2);
        assert_eq!(r.frn, 0x0003_0000_0000_1f2a);
        assert_eq!(r.parent_frn, 0x0005_0000_0000_0005);
        assert_eq!(r.usn, 0x075c_1000);
        assert_eq!(r.reason, reason::FILE_CREATE);
        assert_eq!(r.attributes, 0x20);
        let mut n = Vec::new();
        r.name_into(&mut n);
        assert_eq!(String::from_utf16(&n).unwrap(), "a.txt");
    }

    #[test]
    fn roundtrip_encoded_records_and_iteration() {
        let mut buf = encode_v2(10, 5, 100, reason::FILE_CREATE, 0x10, "dir");
        buf.extend(encode_v2(11, 10, 101, reason::FILE_CREATE | reason::CLOSE, 0x20, "文件.txt"));
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&12u64.to_le_bytes());
        let mut pid = [0u8; 16];
        pid[..8].copy_from_slice(&10u64.to_le_bytes());
        buf.extend(encode_v3(id, pid, 102, reason::RENAME_NEW_NAME, 0x20, "b"));
        let recs: Vec<_> = records(&buf).collect::<Result<_, _>>().unwrap();
        assert_eq!(recs.len(), 3);
        let mut n = Vec::new();
        recs[1].name_into(&mut n);
        assert_eq!(String::from_utf16(&n).unwrap(), "文件.txt");
        assert_eq!(recs[2].frn, 12, "zero-extended 128-bit id maps to the 64-bit FRN");
        assert_eq!(recs[2].parent_frn, 10);
        assert_eq!(recs[2].major, 3);
    }

    #[test]
    fn refs_ids_are_hashed() {
        let mut id = [7u8; 16];
        id[15] = 1;
        assert_ne!(fold_file_id(&id), u64::from_le_bytes([7; 8]));
        assert_eq!(fold_file_id(&id), fold_file_id(&id));
    }

    #[test]
    fn malformed_input() {
        assert_eq!(parse_record(&[1, 2, 3], 0), Err(RecordError::Truncated(0)));
        let mut bad = encode_v2(1, 2, 3, 0, 0, "x");
        bad[4] = 9;
        assert_eq!(parse_record(&bad, 0), Err(RecordError::Version(9)));
        let mut bad = encode_v2(1, 2, 3, 0, 0, "x");
        bad[56] = 200; // name length beyond the record
        assert_eq!(parse_record(&bad, 0), Err(RecordError::BadName(0)));
        let mut buf = encode_v2(1, 2, 3, 0, 0, "ok");
        buf.extend([0xff, 0xff, 0, 0]);
        let v: Vec<_> = records(&buf).collect();
        assert!(v[0].is_ok());
        assert!(v[1].is_err());
        assert_eq!(v.len(), 2);
    }
}

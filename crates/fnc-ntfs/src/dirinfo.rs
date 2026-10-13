//! Parsing `FILE_ID_BOTH_DIR_INFO` buffers returned by
//! `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` (portable; tested with
//! hand-built buffers). Unlike `FindFirstFileExW`, each entry carries its 64-bit file
//! id, which lets the size/date back-fill recognise extra hard-link names.
//!
//! ```text
//! 0 NextEntryOffset u32 | 4 FileIndex u32 | 8 CreationTime i64 | 16 LastAccessTime i64
//! 24 LastWriteTime i64 | 32 ChangeTime i64 | 40 EndOfFile i64 | 48 AllocationSize i64
//! 56 FileAttributes u32 | 60 FileNameLength u32 (bytes) | 64 EaSize u32
//! 68 ShortNameLength i8 | 70 ShortName [u16; 12] | 96 FileId i64 | 104 FileName [u16]
//! ```

use fnc_core::FileTime;
use fnc_core::flags::attr;
use fnc_index::Meta;

/// Byte offsets of the fields used (checked against the `windows` crate layout in `win`).
pub mod offset {
    /// `NextEntryOffset`
    pub const NEXT: usize = 0;
    /// `CreationTime`
    pub const CREATION: usize = 8;
    /// `LastAccessTime`
    pub const ACCESS: usize = 16;
    /// `LastWriteTime`
    pub const WRITE: usize = 24;
    /// `EndOfFile`
    pub const SIZE: usize = 40;
    /// `FileAttributes`
    pub const ATTRIBUTES: usize = 56;
    /// `FileNameLength`
    pub const NAME_LEN: usize = 60;
    /// `FileId`
    pub const FILE_ID: usize = 96;
    /// `FileName`
    pub const NAME: usize = 104;
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn i64_at(b: &[u8], o: usize) -> Option<i64> {
    Some(i64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Calls `f(name, meta, file_id)` for every entry in `buf` except `.` and `..`.
/// Stops at the first malformed entry.
pub fn for_each_entry(buf: &[u8], name: &mut Vec<u16>, mut f: impl FnMut(&[u16], Meta, u64)) {
    let mut off = 0usize;
    loop {
        let Some(rec) = buf.get(off..) else { return };
        let (Some(next), Some(name_len)) = (u32_at(rec, offset::NEXT), u32_at(rec, offset::NAME_LEN)) else { return };
        let Some(raw) = rec.get(offset::NAME..offset::NAME + name_len as usize) else { return };
        name.clear();
        name.extend(raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)));
        let dot = u16::from(b'.');
        if name.as_slice() != [dot] && name.as_slice() != [dot, dot] {
            let attributes = u32_at(rec, offset::ATTRIBUTES).unwrap_or(0);
            let is_dir = attributes & attr::DIRECTORY != 0;
            let time = |o| i64_at(rec, o).map(FileTime);
            let meta = Meta {
                attributes: Some(attributes),
                size: if is_dir { None } else { i64_at(rec, offset::SIZE).map(|s| s.max(0) as u64) },
                date_modified: time(offset::WRITE),
                date_created: time(offset::CREATION),
                date_accessed: time(offset::ACCESS),
            };
            f(name, meta, i64_at(rec, offset::FILE_ID).unwrap_or(0) as u64);
        }
        if next == 0 {
            return;
        }
        off += next as usize;
    }
}

/// Builds one entry (tests).
pub fn encode_entry(name: &str, attributes: u32, size: i64, write_time: i64, file_id: u64, last: bool) -> Vec<u8> {
    let n: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let len = (offset::NAME + n.len()).div_ceil(8) * 8;
    let mut b = vec![0u8; len];
    let next = if last { 0 } else { len as u32 };
    b[offset::NEXT..offset::NEXT + 4].copy_from_slice(&next.to_le_bytes());
    b[offset::WRITE..offset::WRITE + 8].copy_from_slice(&write_time.to_le_bytes());
    b[offset::SIZE..offset::SIZE + 8].copy_from_slice(&size.to_le_bytes());
    b[offset::ATTRIBUTES..offset::ATTRIBUTES + 4].copy_from_slice(&attributes.to_le_bytes());
    b[offset::NAME_LEN..offset::NAME_LEN + 4].copy_from_slice(&(n.len() as u32).to_le_bytes());
    b[offset::FILE_ID..offset::FILE_ID + 8].copy_from_slice(&file_id.to_le_bytes());
    b[offset::NAME..offset::NAME + n.len()].copy_from_slice(&n);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries_and_skips_dots() {
        let mut buf = encode_entry(".", attr::DIRECTORY, 0, 0, 5, false);
        buf.extend(encode_entry("..", attr::DIRECTORY, 0, 0, 5, false));
        buf.extend(encode_entry(
            "kernel32.dll",
            attr::ARCHIVE,
            765_000,
            133_000_000_000_000_000,
            0x0001_0000_0000_1234,
            false,
        ));
        buf.extend(encode_entry("文件夹", attr::DIRECTORY, 4096, 1, 0x99, true));
        let mut got = Vec::new();
        for_each_entry(&buf, &mut Vec::new(), |n, m, id| got.push((String::from_utf16_lossy(n), m, id)));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "kernel32.dll");
        assert_eq!(got[0].1.size, Some(765_000));
        assert_eq!(got[0].1.date_modified, Some(FileTime(133_000_000_000_000_000)));
        assert_eq!(got[0].2, 0x0001_0000_0000_1234);
        assert_eq!(got[1].0, "文件夹");
        assert_eq!(got[1].1.size, None, "folders have no size");
    }

    #[test]
    fn truncated_buffers_stop_cleanly() {
        let buf = encode_entry("abc.txt", 0, 1, 1, 1, false);
        let mut n = 0;
        for_each_entry(&buf[..buf.len() - 4], &mut Vec::new(), |_, _, _| n += 1);
        assert_eq!(n, 0);
        for_each_entry(&[0u8; 3], &mut Vec::new(), |_, _, _| n += 1);
        assert_eq!(n, 0);
    }
}

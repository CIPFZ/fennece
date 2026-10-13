//! Result export (File → Export, Copy as CSV/EFU; SPEC ch.4 §7) through the es-format
//! writers of `fnc_ipc::export`, so the app, fes and HTTP produce identical files.

use std::io::{self, Write};
use std::path::Path;

use fnc_core::EntryId;
use fnc_exec::{Caches, TypeNames};
use fnc_index::IndexStore;
use fnc_ipc::export::{ExportOptions, Format, export_entries};
use fnc_ipc::proto::Column;

use crate::columns::ColumnId;

/// Format chosen by the file extension (`.efu`, `.csv`, `.tsv`, `.json`, `.m3u`, `.m3u8`;
/// anything else: one full path per line).
pub fn format_for(path: &Path) -> Format {
    path.extension().and_then(|e| e.to_str()).and_then(Format::parse).unwrap_or(Format::Txt)
}

/// The IPC column showing the same data as a result-list column.
pub fn ipc_column(c: ColumnId) -> Option<Column> {
    Some(match c {
        ColumnId::Name => Column::Name,
        ColumnId::Path => Column::Path,
        ColumnId::FullPath => Column::FullPath,
        ColumnId::Size => Column::Size,
        ColumnId::Extension => Column::Extension,
        ColumnId::Type => Column::Type,
        ColumnId::DateModified => Column::DateModified,
        ColumnId::DateCreated => Column::DateCreated,
        ColumnId::DateAccessed => Column::DateAccessed,
        ColumnId::Attributes => Column::Attributes,
        ColumnId::RunCount => Column::RunCount,
        ColumnId::DateRun => Column::DateRun,
        ColumnId::DateRecentlyChanged => Column::DateRecentlyChanged,
        ColumnId::FileListFilename => Column::FileListName,
        _ => return None,
    })
}

/// Options for exporting to `format` with the visible `columns` (EFU and the path lists
/// ignore them: their columns are fixed).
pub fn options(format: Format, columns: &[ColumnId]) -> ExportOptions {
    ExportOptions { columns: columns.iter().copied().filter_map(ipc_column).collect(), ..ExportOptions::new(format) }
}

fn caches(types: Option<TypeNames>) -> Caches {
    let c = Caches::new();
    c.set_type_names(types);
    c
}

/// Writes `ids` (live entries only, in order) to `out`; returns the number of rows.
pub fn write<W: Write>(
    out: W,
    opts: &ExportOptions,
    store: &IndexStore,
    ids: &[EntryId],
    types: Option<TypeNames>,
) -> io::Result<u64> {
    export_entries(out, opts, store, ids, &caches(types))
}

/// Writes `ids` to the file `path` in the format of its extension.
pub fn write_file(
    path: &Path,
    columns: &[ColumnId],
    store: &IndexStore,
    ids: &[EntryId],
    types: Option<TypeNames>,
) -> io::Result<u64> {
    let mut f = io::BufWriter::new(std::fs::File::create(path)?);
    let n = write(&mut f, &options(format_for(path), columns), store, ids, types)?;
    f.flush()?;
    Ok(n)
}

/// The rows as text (clipboard).
pub fn to_text(format: Format, store: &IndexStore, ids: &[EntryId], types: Option<TypeNames>) -> String {
    let mut buf = Vec::new();
    let _ = write(&mut buf, &ExportOptions::new(format), store, ids, types);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
        \"C:\\Data\",,133000000000000000,,16\n\
        \"C:\\Data\\a, \"\"b\"\".txt\",12,133000000000000001,132000000000000000,33\n\
        \"C:\\Data\\测试\",,133000000000000002,,16\n\
        \"C:\\Data\\测试\\ü.iso\",700000000,133000000000000003,,32\n";

    fn entries(s: &IndexStore) -> Vec<(String, Option<u64>, i64, i64, u32)> {
        let mut v: Vec<_> = (0..s.len() as u32)
            .map(EntryId)
            .filter(|&i| s.is_live(i) && !s.flags(i).contains(fnc_core::EntryFlags::ROOT))
            .map(|i| {
                let mut p = String::new();
                s.full_path(i, &mut p);
                let size = if s.is_dir(i) { None } else { s.size(i) };
                (p, size, s.date_modified(i).0, s.date_created(i).0, s.attributes(i))
            })
            .collect();
        v.sort();
        v
    }

    /// INT-07: export EFU, mount it again, identical entries.
    #[test]
    fn int07_efu_roundtrip() {
        let mut a = IndexStore::default();
        fnc_index::efu::load_efu(&mut a, EFU.as_bytes(), "in.efu").unwrap();
        let ids: Vec<EntryId> = (0..a.len() as u32)
            .map(EntryId)
            .filter(|&i| a.is_live(i) && !a.flags(i).contains(fnc_core::EntryFlags::ROOT))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.efu");
        assert_eq!(format_for(&out), Format::Efu);
        let n = write_file(&out, &[ColumnId::Name, ColumnId::Size], &a, &ids, None).unwrap();
        assert_eq!(n, 4);
        let mut b = IndexStore::default();
        fnc_index::efu::load_efu_file(&mut b, &out).unwrap();
        assert_eq!(entries(&a), entries(&b));
    }

    #[test]
    fn formats_by_extension_and_columns() {
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, EFU.as_bytes(), "in.efu").unwrap();
        let ids: Vec<EntryId> = (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i) && !s.is_dir(i)).collect();
        assert_eq!(format_for(Path::new("x.TXT")), Format::Txt);
        assert_eq!(format_for(Path::new("x.lst")), Format::Txt);
        assert_eq!(format_for(Path::new("x.m3u8")), Format::M3u8);
        let csv = {
            let mut b = Vec::new();
            write(&mut b, &options(Format::Csv, &[ColumnId::Name, ColumnId::Size]), &s, &ids, None).unwrap();
            String::from_utf8(b).unwrap()
        };
        assert!(csv.starts_with("Name,Size\r\n"), "{csv}");
        assert!(csv.contains("\"a, \"\"b\"\".txt\",12"), "{csv}");
        let txt = to_text(Format::Txt, &s, &ids, None);
        assert!(txt.contains("C:\\Data\\测试\\ü.iso\r\n"), "{txt}");
        assert!(
            to_text(Format::Efu, &s, &ids, None).starts_with("Filename,Size,Date Modified,Date Created,Attributes")
        );
    }
}

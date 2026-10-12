# fnc-service — `fennec-service` (M2)

Windows service (LocalSystem) exposing exactly three read-only RPCs — `QueryJournal`, `EnumMft`,
`ReadUsn` — over `\\.\pipe\Fennec.<instance>.svc` so non-elevated users get NTFS indexing (IDX-10).

**Invariants (planned)**: no arbitrary file reads; pipe DACL limited to Interactive + Administrators.

# fnc-platform

Platform abstraction (SPEC ch.0 §4, ch.1 §12).

**Responsibilities**
- `VolumeSource` contract (enumerate / watch / fetch_metadata) implemented by `fnc-ntfs` and `walk::FolderSource`.
- Portable folder walker (synthetic FRNs, no symlink following).
- Shell actions: open, reveal in file manager (`ShellExecuteW`, `explorer /select,`, `xdg-open`, `open -R`).
- Single instance: named mutex `Local\Fennec.<instance>` (Windows) + local socket `Fennec.<instance>.cmd`
  carrying forwarded command lines.
- Elevation check.

**Invariants**: `unsafe` only for Win32 calls, each block with a safety comment.

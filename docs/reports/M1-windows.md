## M1 Windows 实机验证报告（2026-10-13）
环境: Windows Server 2022 (10.0.20348)，Xeon Platinum 8255C 10 vCPU，32 GB，C: NTFS 33 GB / D: NTFS 275 GB，中文系统（默认微软拼音），管理员；
  Rust 1.99.0 + VS 2022 Build Tools 原生编译（rustup/crates 走 USTC 镜像）。
构建/测试: 原生 `cargo clippy --workspace --all-targets -D warnings` ✓；`cargo test --workspace` 全绿 ✓；
  新增 `ntfs_end_to_end`（真实卷，管理员）✓。
验收（真实 NTFS 卷 + 真实桌面）:
  IDX-01 → 通过：C:+D: 36.0 万条目，MFT 枚举后 1.7 s 可搜索，大小/日期补齐完成 3.8 s（预算 60 s）。
  IDX-02 → 通过：新建文件 → 搜索可见 2.6–31 ms（预算 1 s）；写入后大小刷新 1.5–49 ms；删除 → 消失 2.4 ms。
  IDX-03 → 通过：重命名含 100,000 子项的目录 → 新路径前缀结果正确 23 ms（预算 2 s），旧路径 0 命中。
  UI-01  → 通过：100 万结果翻页 UI 帧 CPU 时间 mean 2.9 ms / p95 3.3 ms（kittest，release）。
  UI-02  → 通过（同 Linux 用例，Windows 上全绿）。
  UI-06  → 通过：Esc 隐藏到托盘后，在记事本前台时按 Ctrl+` → 窗口可见且置前 ~105 ms，后续键入直接进入搜索框。
  其他实测: 托盘左键隐藏/显示、右键菜单“显示 Fennec/退出”、退出后 close_to_tray 设置保留；单实例转发 `-s` 与 `-exit`
  （退出请求 → 进程结束 135 ms）；Ctrl+Enter 打开资源管理器定位；列头排序；USN 实时新增在 GUI 中即时出现；中文 IME 输入、
  中文界面、中文文件名搜索与高亮（YaHei 回退字体）。
基准（Windows，1 M 合成条目，p95）: `a` 6.3 / `abc` 5.3 / `*.jpg` 4.0 / `regex:^IMG_\d{4}` 44.8 ms；名称排序 45 ms；
  索引堆 104.7 MiB（同 Linux）→ bench/results-windows.md。
在 Windows 上发现并修复的问题（均有回归测试）:
  1. `$Extend`（MFT 记录 11）不在 FSCTL_ENUM_USN_DATA 输出中，其子项（$RmMetadata/$Txf…）落入 $Orphans → 合成 $Extend；实测孤儿 0。
  2. 硬链接只得到一个名称：`C:\Windows\System32\kernel32.dll` 等 3.6 万个名称缺失 → 补齐阶段改用 FileIdBothDirectoryInfo，
     识别额外名称为 HARDLINK_ALIAS；USN 数据类记录改为 Touch，不再在硬链接间“搬家”（ADR-010）。
  3. 中文 Windows 上界面为英文（只读 LANG）→ 改为 GetUserDefaultUILanguage。
  4. 补齐阶段把所有条目记入“最近更改”（rc: 语义错误）且新增别名行使各列容量翻倍：296k 条目索引堆 91 → 48 MiB。
  5. 点击列头后搜索框失焦。
仍待处理 / 观察:
  - 进程常驻：296k 条目时 private 194 MB，其中空索引基线 135 MB 来自 GUI 栈（OpenGL 驱动 + 19 MB 的 CJK 字体），索引本身 48 MiB；
    SPEC §6.1 的 150 MB 对 100 万条目若按整进程计会超出 → M2 评估 wgpu/DX12 后端与字体子集化。
  - 删除多链接文件的单个名称不检测（SPEC-GAP）；同名结果的次序按条目 ID 而非路径。
  - IDX-04/05/10、UI-03/04/05/07/08 属 M2。

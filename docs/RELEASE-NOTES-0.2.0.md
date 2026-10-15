# Fennec 0.2.0 发布说明

Fennec 是用 Rust + egui 实现的 Windows 本地文件搜索引擎，以 voidtools Everything 1.5 为功能基准。0.2.0 是第一个面向日常试用的版本：不增加新搜索功能，重点是稳定性、可诊断性、升级与安装。

## 下载：两个包

| 包 | 文件 | 内容 |
|---|---|---|
| 标准包（推荐）| `fennec-0.2.0-x64-standard.msi` / `fennec-0.2.0-x64-standard-portable.zip` | 全部 Everything 式功能；**不含**语义搜索与 llama.cpp |
| 实验包 | `fennec-0.2.0-x64-experimental.msi` / `fennec-0.2.0-x64-experimental-portable.zip` | 标准包 + 语义搜索（`sem:`，EmbeddingGemma-2，模型约 300 MB 首次启用时下载并校验 SHA-256）|

发布包由 GitHub Actions（`.github/workflows/release.yml`）在推送 tag 时于 Windows 上构建；校验：`SHA256SUMS.txt`（`sha256sum -c` 格式）。两个 MSI 共用升级码：安装其中一个会替换另一个；从 0.1.0 直接升级即可。仅 x64；未签名（Windows SmartScreen 可能提示）。

## 功能概览

- 索引：NTFS/ReFS 经 MFT 枚举 + USN 日志实时更新（管理员直连，或普通用户经 MSI 安装的 `FennecService`）；文件夹索引、EFU 文件列表；属性（305 个）、内容（含隔离进程 IFilter）、哈希、查重。
- 搜索语法：Everything 1.5 的运算符、通配符、修饰符、函数与宏，`si:`（Windows Search）、`eval:`、拼音、`regex:` 等；差异见下文"已知差异"。
- 界面：虚拟结果表、多级排序、属性列与自定义列、预览、缩略图、侧栏、标签页与会话、查找栏、高级重命名、撤销/重做、筛选器与书签、托盘与全局热键、9 种界面语言、深色与高对比度主题。
- 集成：`fes`（es.exe 兼容命令行）、官方 `es.exe`/Everything SDK 兼容层、原生 IPC 与 C/Rust SDK、HTTP 服务器、ETP/FTP 服务器与客户端（默认关闭、仅回环、必须设密码）、资源管理器右键菜单、导入导出。

## 0.2.0 新增与修复

- 崩溃日志：所有程序（fennec、fes、fennec-service、IFilter 宿主）崩溃时写 `logs\crash-<时间>.log`（版本、系统、线程、调用栈、最近 20 条搜索、索引状态；选项 `crash_redact_queries` 可只记录长度与哈希）；下次启动状态栏提示"上次异常退出，查看日志"。
- 升级兼容：所有数据文件带格式版本；旧版本可读则直接加载，无法读取的旧版本、损坏或更新版本的文件先备份为 `*.v<N>.bak` / `*.corrupt.bak` 再重建并提示，从不覆盖（ADR-038）。0.1.0 的数据目录可直接使用（筛选器、书签、历史、运行次数、设置均保留）。
- 关闭语义搜索（或空闲 10 分钟卸载）后内存回到开启前水平（此前打开第二个窗口或后台标签时不释放）。
- "链接目标""文件签名""回收站原位置"三列可排序：点击列头先提示需读取的文件数与预计时间，确认后后台读取（可取消）再排序，结果缓存于 `files.fnx`。
- 持续文件变动下：运行中压缩不再被饿死（此前删除的条目持续堆积）；索引变化引起的重新搜索限速且不打断进行中的搜索，窗口隐藏/最小化时不再重新搜索（此前每秒 50 个变化时占用 3–4 个核）。
- 大小/日期补齐不再跟随目录联接/符号链接（此前把链接目标的子项错误加入链接下）。
- 便携版放在不可写目录时改用 `%LOCALAPPDATA%\Fennec\portable` 并在状态栏常驻警告（此前静默退出）。
- 卸载：删除程序、服务、右键菜单、自启项与 `%ProgramData%\Fennec`；`%APPDATA%\Fennec` 默认保留，卸载界面勾选或 `msiexec /x … REMOVEUSERDATA=1` 时删除。
- `fennec -version` / `-help` 在命令行窗口中可见；HTTP `Server` 头为 `Fennec/0.2.0`，Everything IPC 报告版本 `1.4.1.200`。

## 升级说明

- 从 0.1.0：直接安装 0.2.0 MSI（自动卸载旧版，保留功能选择与数据）；便携版把旧 `data\` 复制到新目录即可。
- 降级到 0.1.0：0.2.0 写的数据文件格式版本未变，0.1.0 仍可读取（新增的 `files.fnx` 会被忽略）。
- 首次启动若某个数据文件被备份重建，状态栏会提示文件名；原文件保留在同目录。

## 已知差异（与 Everything 1.5）

完整列表见 `docs/SPEC.md` 第 2 章 §9。主要几项：

- `weight:` 解析后忽略（未实现相关度排序）。
- Everything 1.5 的 IPC3 管道协议未实现；`es.exe` 自动回退到旧协议，功能不受影响。
- 插件 DLL 不支持（永不实现）。
- ETP：传输中的 `ABOR` 在传输结束后才应答；`OPTS UTF8 OFF` 后仍发送 UTF-8；客户端不支持路径重写。
- 正则用 Rust `regex`（回溯特性回退 `fancy-regex`）；`%VAR%` 展开结果视为字面文本；`fileexists:` 只查索引。

## 已知问题

- 语义搜索（仅实验包）：CPU 上索引吞吐约 2 块/秒（SPEC 要求 ≥ 20）；`semimage:` 未实现（提示不可用）。
- 窗口可见且搜索框为空（显示全部条目）时，若磁盘持续大量变动（每秒数十个文件），每次限速刷新都会重新排序全部条目，约占 0.7 个核；隐藏到托盘或输入任意搜索词后接近 0。
- 只构建 x64，未做代码签名。
- 卸载或升级时若 Fennec 仍在**其他用户的会话**中运行（多人共用一台电脑时），Windows Installer 会要求重启：请先在所有会话中退出 Fennec（托盘 → 退出）。同一会话中运行的 Fennec 会被安装程序自动关闭。
- 安装程序界面只有英文，资源管理器右键菜单项为英文（"Search Fennec"）；安装结束页没有"启动 Fennec"选项。

## 反馈

崩溃时请附上 `%APPDATA%\Fennec\logs\crash-*.log`（便携版在 `data\logs\`）与同目录的 `fennec.<日期>.log`。

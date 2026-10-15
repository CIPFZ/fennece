# AGENT.md — Fennec 开发代理工作手册（v2，接续开发版）

> 用法：把本文件全文作为开发 AI 代理的首条系统/任务提示。代理工作在 GitHub 仓库 `git@github.com:CIPFZ/fennece.git`（`master`，起点提交 `38ee0ab`）。本文件应替换仓库中的 `docs/AGENT.md`，并作为该次工作的首个提交。
>
> 与 v1 的区别：v1 面向从零搭建（M0–M6）；v2 面向一个已完成全部里程碑、约 8 万行、563 个测试的代码库，目标是发布 0.2.0 并进入试用期。你不是在创造，你是在接手。

---

## 1. 你的角色

你是 Fennec 项目的首席工程师兼唯一实现者。Fennec 是用 Rust + egui 从零实现的 Windows 本地文件搜索引擎，以 voidtools Everything 1.5 为功能基准，并带有基于 EmbeddingGemma-2 的本地语义搜索（当前处于暂缓状态）。你对代码质量、性能、测试与文档负全责；人类评审者只做验收与决策，不替你补坑。

需求来源唯一：`docs/SPEC.md`。`[MUST]` 必须实现并通过验收用例；`[SHOULD]` 默认实现，放弃须写 ADR；`[MAY]` 仅在明确要求时实现。SPEC 未写明之处，以 Everything 1.5 实际行为为准（voidtools 官方 Search Syntax / Search Functions / Search Modifiers），再以"最不令 Everything 老用户惊讶"为准。

## 2. 项目现状（接手时必须知道的事实）

### 2.1 已完成

| 阶段 | 内容 | 验收报告 |
|---|---|---|
| M0–M6 | SPEC 第 6 章规划的全部里程碑，Windows Server 2022 实机验收 | `docs/reports/M0.md` … `M6.md`，`M1-windows.md` |
| after-M6 | 补齐差异表：ETP/FTP 服务器与客户端（ADR-036）、`si:` 系统索引、`target:`/`fsi:`/`recycled:`/`parent+N:`/`filelistfilename:`、运行中压缩与重建、服务每连接鉴权、HTTP/ETP 密码 PBKDF2、拼音排序、305 个属性、IFilter 隔离宿主（ADR-035） | `docs/reports/after-M6.md` |

质量基线：Linux 542 测试全绿、Windows 原生 520 测试全绿（不含 `fnc-semantic`）、`fmt`/`clippy -D warnings`/`cargo-deny` 通过；workspace 级 `unwrap_used`/`expect_used` = deny 且非测试代码为零。100 万条基准全部在 SPEC 第 0 章 §6.1 预算内，名称扫描比 M3 快 22–40%。

### 2.2 未完成或有缺陷（这是你的工作清单来源）

| 项 | 状态 | 处置 |
|---|---|---|
| 语义索引吞吐 ≈ 2 块/秒（SPEC 要求 ≥ 20）| 未达标 | 暂缓；见 §8 阶段 B |
| `semimage:` | 未做（llama.cpp 绑定不支持多模态）| 暂缓 |
| 关闭语义搜索后进程内存不回落 | 缺陷 | **0.2.0 必修**（§7）|
| 读文件的三列（链接目标/文件签名/回收站原位置）不可排序 | 差异 | 0.2.0 实现"确认后后台读取再排序" |
| ETP `ABOR` 延迟应答、`OPTS UTF8 OFF` 仍发 UTF-8、客户端路径改写 | 差异 | 不做，留在差异表 |
| `weight:` 解析后忽略 | 差异 | 不做 |
| IPC3 管道（Everything 1.5 新协议）| 未做 | 不做，es.exe 自动回退旧协议 |
| 插件 DLL ABI | 不做 | 永不 |
| 全局 panic hook / 崩溃日志 | 不存在（只有局部 `catch_unwind`）| **0.2.0 必做** |
| 安装包 | MSI/便携版仍是 M6 时的 0.1.0 | **0.2.0 重打** |
| `unsafe` 实际出现在 `fnc-ntfs` `fnc-platform` `fnc-ipc` `fnc-sdk` `fnc-service` | 与 v1 铁律 5 不符 | 本版已修订铁律（§4 第 5 条），需补 ADR |

### 2.3 仓库地图

```text
crates/
  fnc-core      数据模型、字符串池、折叠键、自然排序、Settings 派生宏 (fnc-macros)、日志、数据目录
  fnc-index     列式 IndexStore、children CSR、FRN 表、变更应用、压缩、快速排序、.fnx 持久化 (persist.rs: 魔数 "FNX1" + version u32)
  fnc-query     词法/语法/AST、函数/修饰符/宏表、日期/大小/时长、代价规划器
  fnc-exec      匹配器、并行扫描、排序、高亮、EngineHandle、拼音/模糊
  fnc-props     属性(305)/内容/哈希/查重，IFilter 宿主
  fnc-semantic  llama.cpp 嵌入、分块、usearch、后台索引器（feature `semantic`，默认开启）
  fnc-ntfs      MFT 枚举、USN 监视、服务客户端
  fnc-platform  VolumeSource/Shell/热键/托盘、Everything IPC 兼容 (compat.rs)、Windows Search
  fnc-ipc       原生管道协议、后端、导出
  fnc-http      axum 服务器        fnc-etp   ETP/FTP 服务器与客户端
  fnc-sdk       Rust + C ABI       fnc-fes   fes CLI        fnc-service  Windows 服务
  fnc-app       fennec GUI（约 2 万行，含 kittest 测试）
tools/   gen-fixtures  bench  gen-icons  sem-eval  sem-corpus.py
packaging/  build.ps1  wix/          third_party/  vendored llama.cpp 绑定
docs/    SPEC.md  AGENT.md  CHANGELOG.md  adr/(001–036)  reports/  screenshots/
.github/workflows/ci.yml   Linux+Windows check、Windows cross-clippy、cargo-deny
```

## 3. 开始前必读（按顺序，总计约 1 小时）

1. 本文件全文。
2. `docs/reports/after-M6.md` 与 `docs/reports/M6.md`：最近两份验收报告，了解当前行为与已知差异。
3. `docs/SPEC.md` 第 0 章（命名空间、性能预算 §6.1、安全 §6.3）、第 6 章 §4–§5（测试策略、编码规范）、第 2 章 §9 差异表。
4. `docs/CHANGELOG.md` 最近 50 行，`docs/adr/` 的标题列表（不必逐篇读，涉及某模块时再读对应 ADR）。
5. 本地执行一次 `cargo test --workspace`（Linux）确认起点绿；若在 Windows 机器上，另跑 `cargo test --workspace --no-default-features`。

读完后用不超过 40 行写出"我对当前阶段的理解与计划"，然后立即开始，不等待批准。

## 4. 铁律（违反任一条即视为交付失败）

1. 字面搜索路径的性能与资源是最高优先级。任何改动若使 SPEC 第 0 章 §6.1 指标回退超过 10%，必须回滚或先修复。语义/属性/内容/HTTP/ETP 关闭时必须零开销。
2. UI 线程永不阻塞超过 8 ms。索引、查询、排序、提取、网络、文件读取全部在后台线程；每次按键取消上一查询。
3. NTFS/ReFS 索引只走 `FSCTL_ENUM_USN_DATA` + `FSCTL_READ_USN_JOURNAL`；目录遍历只用于文件夹索引与非 Windows 后端。
4. 热路径（匹配、排序比较、路径拼接）零分配；已有 dhat 断言测试，不得删除或放宽。
5. `unsafe` 只允许出现在 `fnc-ntfs`、`fnc-platform`、`fnc-ipc`、`fnc-sdk`、`fnc-service`，每处附 `// SAFETY:` 论证；新增 crate 需要 `unsafe` 必须先写 ADR。非测试代码禁止 `unwrap()`/`expect()`（已 deny，不得加 `#[allow]` 绕过）。
6. 文件删除/重命名/移动一律通过 `IFileOperation`；HTTP/ETP 默认关闭、默认回环、启用必须有密码；服务进程只暴露只读元数据 RPC。
7. 搜索语法以 Everything 1.5 兼容为目标；任何有意差异写入 SPEC 第 2 章 §9，不得静默偏离。
8. 不引入 nightly 特性；不引入未在 SPEC 第 6 章 §6 列出的重量级依赖（> 1 万行或带 C 构建）而不先写 ADR（参考 ADR-009/014/020）。
9. 每次提交：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿；Windows 相关改动在 Windows 实机验证并在报告中写明机器与结果。
10. 不臆造 Everything 的行为。拿不准查官方文档；查不到则在实现处留 `// SPEC-GAP:` 并在报告中列出。
11. **接手期特有**：不做"顺手重构"。不重命名已有模块、不调整 crate 边界、不替换已有依赖，除非当前任务明确需要且写入提交说明。现有 563 个测试是你的安全网，不得为通过而修改其断言。

## 5. 工作方式

### 5.1 Git 约定

- 直接在 `master` 工作（单人项目）；每个可验收单元一个提交，提交信息格式沿用：`[0.2.0][fnc-app] 全局 panic hook 与崩溃日志 (REL-02)`。
- 推送频率：每个单元完成即推送；每天结束前必须推送。
- 发版：打 tag `v0.2.0`，GitHub Release 附两个安装包与 SHA256。
- 不 force-push `master`。

### 5.2 迭代单元

1. 列出要满足的 SPEC 条款与验收用例编号（本阶段用 §7 的 REL-xx 编号）。
2. 先写测试（红）→ 实现（绿）→ 跑相关基准。
3. 走完 §6 自检清单后提交，`docs/CHANGELOG.md` 追加一行到 `## 0.2.0` 节；架构决策写 `docs/adr/NNN-标题.md`（下一个编号从 037 开始，≤ 60 行）。

### 5.3 遇到问题时

- SPEC 内部矛盾：以更具体条款为准，报告中指出位置。
- Windows 专属逻辑无法在 Linux CI 测试：`#[cfg(windows)]` 隔离 + 纯函数部分提供跨平台测试（仓库里 USN 解析的做法）。
- 需要人类决策：以 `DECISION NEEDED:` 开头列出问题、选项、建议与影响，继续做不受阻塞的部分。§9 已列出的决定不要重复询问。
- 发现报告与代码不符：以代码为准，在报告中标注"修正：…"。

### 5.4 禁止

- 把未完成功能标为完成；用 TODO 替代必做项；删减或放宽验收用例/测试断言；提交调试输出或大段注释代码；无基准数字声称"性能足够"。

## 6. 每次提交前自检清单

```text
[ ] 覆盖的 SPEC 条款 / REL 编号写入提交说明
[ ] fmt / clippy -D warnings / test 全绿（Linux；涉及 Windows 的在 Windows 也跑）
[ ] 新增 pub 项有 rustdoc；新增设置项带 #[setting] 元数据并出现在"高级"页
[ ] 热路径无新增分配（dhat 测试仍绿）
[ ] 基准：相关 criterion 数字贴入提交说明，与 bench/results*.md 基线对比
[ ] 无 unwrap/expect/println!/未论证的 unsafe；未新增 #[allow(clippy::…)]
[ ] 可配置项默认值与 Everything 一致
[ ] UI 文案走 Fluent（9 种语言文件都补了 key，哪怕先用英文占位）
[ ] CHANGELOG 已更新；需要时 ADR 已写；差异表（SPEC 第 2 章 §9）同步
[ ] 推送后确认 GitHub Actions 已触发且全部通过（CI；打 tag 时还有 Release）
```

## 7. 当前阶段：发布 Fennec 0.2.0

目标不是新功能，而是让人类可以每天用它替代 Everything，并在出问题时给出可定位的反馈。预计 1–2 周。

### 7.1 任务与验收（REL-xx）

| 编号 | 任务 | 验收 |
|---|---|---|
| REL-01 | 版本号升到 0.2.0（workspace 与所有 crate、MSI、`-version` 输出、HTTP `Server` 头、关于对话框）| `fennec -version` / `fes -version` 输出 0.2.0 |
| REL-02 | 全局 panic hook：所有二进制（fennec / fes / fennec-service / ifilter-host）安装 hook，写入 `logs/crash-<时间>.log`：版本、OS、backtrace、线程名、最近 20 条查询（环形缓冲，脱敏开关默认关）、索引状态摘要；下次启动检测到崩溃文件时在状态栏提示"上次异常退出，查看日志"并可一键打开日志目录 | 注入 `-debug-panic` 隐藏开关触发，验证文件内容与提示 |
| REL-03 | 升级兼容：确认 `.fnx` 头 `version u32` 的检查路径——旧版本号可加载则加载，否则自动重建并提示；`settings.toml` 增加 `schema_version` 字段，缺失视为 1，提供迁移函数骨架；`content.fnx`、属性库、向量库同样处理 | 用 0.1.0 产生的数据目录启动 0.2.0 不崩、不丢筛选器/书签/历史；单元测试覆盖"旧版本/损坏/未来版本"三种头 |
| REL-04 | 修复"关闭语义搜索后内存不回落"：定位持有者（怀疑 `OnceLock`/静态持有 llama context 或 usearch 索引），实现显式 `unload()`；空闲卸载（SPEC 第 5 章 §4）同样验证 | Windows 上报告开启→关闭前后 RSS 数字；开启→空闲 10 min 后 RSS 数字 |
| REL-05 | 24 小时 soak 脚本 `tools/soak/`（PowerShell + Rust 小工具）：启动 fennec + 服务，持续以 50 变更/秒制造 USN 事件（创建/重命名/删除，含 1 万子项目录），每 5 分钟采样 RSS/CPU/句柄数/索引条目数，触发运行中压缩；产出 CSV 与 `docs/reports/soak-0.2.0.md`（含趋势图或表）| RSS 24 h 内增长 < 10%；句柄数稳定；无崩溃文件 |
| REL-06 | 读文件的三列（链接目标、文件签名、回收站原位置）可排序：点击列头时若存在未读取行，弹出"需读取 N 个文件（预计 X 秒），继续？"；确认后后台读取（可取消、状态栏进度）完成再排序；结果缓存至属性库 | kittest 测试 + Windows 实测 1 万行 |
| REL-07 | 构建矩阵与产物：`packaging/build.ps1` 产出两组：标准包（`--no-default-features`，不含语义）MSI + 便携 zip；experimental 包（含 `semantic`）MSI + 便携 zip；文件名含版本与 variant；产出 `SHA256SUMS.txt` | 四个文件 + 校验和 |
| REL-08 | 干净 Windows VM 验证（两组包各一遍）：安装 → 服务安装 → 普通用户启动可索引 NTFS → 搜索/打开/右键/删除撤销 → `fes` → es.exe 兼容 → 卸载后 `Program Files`/`%APPDATA%`/服务/注册表右键菜单无残留（`%APPDATA%` 数据保留需有卸载时勾选项）；便携版在无写权限目录下的行为 | `docs/reports/0.2.0-install.md` 逐项打勾并附截图 |
| REL-09 | 性能回归：在 Windows 基准机重跑 `bench` 全套与 100 万条 fixtures，更新 `bench/results-windows.md`；对比 M6 基线 | 无指标回退 > 10% |
| REL-10 | 文档：`docs/adr/037-unsafe-scope.md`（五个 crate 各处 unsafe 的必要性）；`docs/RELEASE-NOTES-0.2.0.md`（功能概览、已知差异=SPEC 第 2 章 §9、已知问题=§2.2、升级说明、两包区别）；`README.md` 状态行更新；SPEC 第 6 章 §3 追加"0.2.0 发布阶段"行 | 文件存在且内容与实际一致 |
| REL-11 | 发布：tag `v0.2.0`，GitHub Release 上传四个包 + `SHA256SUMS.txt` + release notes | Release 页可下载 |

### 7.2 本阶段不做

ETP `ABOR`/`OPTS UTF8`/路径改写、`semimage:`、语义索引提速、任何新搜索函数或 UI 功能、跨平台后端、重构。

### 7.3 阶段报告

完成 REL-01…11 后按 §10 格式提交 `docs/reports/0.2.0.md`，并在其末尾给出"建议试用关注点"（告诉人类试用者重点验证什么）。

## 8. 后续阶段（0.2.0 之后，等人类试用反馈后再启动，不要提前做）

| 阶段 | 内容 | 进入条件 |
|---|---|---|
| A 试用修复 | 人类一周试用反馈的缺陷与体验问题；每条反馈对应一个提交与一个回归测试 | 收到反馈清单 |
| B 语义搜索回归 | 第一步是半天 profiling spike（是否攒 batch、`n_batch`/线程数、是否按本机指令集编译 `GGML_NATIVE`/AVX2、是否每次重建 context），产出数字再定方案；目标 ≥ 20 块/秒；然后评估 `semimage:` 路径（换绑定或 ORT）| 人类明确说"回到语义搜索" |
| C 打磨 | ETP 小差异、IPC3、三列排序体验、更多语言 | 人类指定 |

## 9. 已做出的决定（不要重复询问）

- 语义搜索暂缓；0.2.0 以"标准包不含语义 + experimental 包含语义"双产物发布。
- ETP/HTTP 必须设密码；ETP 不做 TLS；远程结果每次最多 1 万条；`si:` 上限 10 万项 / 20 s 超时 / 30 s 复用。
- 自定义列空值/错误排在最后；属性列未知值升序在前。
- `weight:` 保持解析后忽略；插件 DLL ABI 永不实现；IPC3 不在 0.2.0 范围。
- `unsafe` 允许范围扩大为五个 crate（铁律 5），需补 ADR-037。
- 读文件列排序采用"确认后后台读取再排序"而非即时排序。

待人类决定（在首次报告中提出一次即可）：仓库名 `fennece` 是否改为 `fennec`；`%APPDATA%` 数据在卸载时默认保留还是删除。

## 10. 报告格式

```text
## <阶段名> 报告（<日期>，起点提交 → 终点提交）
完成: <REL/SPEC 编号 → 一句话>
验收: <编号 → 通过/失败 + 数字/截图路径>
基准: <指标 → 本次 / 目标 / 上次基线>
Windows 实机: <机器/系统/测试内容/结果>
差异表变更: <SPEC 第 2 章 §9 新增或删除条目>
SPEC-GAP: <文件:行 → 说明>
DECISION NEEDED: <问题 → 选项 → 建议 → 影响>
下一步: <建议>
```

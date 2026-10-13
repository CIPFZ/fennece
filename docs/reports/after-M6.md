## M6 之后：补齐差异表中的未达标 / 未做项（2026-10-14）
范围：用户要求暂不处理语义搜索（llama.cpp / EmbeddingGemma-2），先补齐基本与核心功能。
环境：Linux 构建机 + Windows Server 2022 实机（中文系统）。
构建/测试：Linux `fmt --check` ✓、`clippy --workspace --all-targets -D warnings` ✓、`cargo test --workspace` 542 项全绿；
  Windows 原生 `fmt --check` ✓、`clippy`（不含 fnc-semantic/sem-eval/bench，fnc-app 无默认特性）✓、`cargo test` 520 项全绿；Windows 目标交叉 clippy ✓。

| 项 | 之前 | 现在 | 验证 |
|---|---|---|---|
| 名称子串扫描 | 比 M3 慢 8 % | 每个词一个 `memmem` 查找器；`abc` −30 %、`a b c` −40 % | Windows 成对基准 |
| 删除后的索引空洞 | 只在重启时压缩 | 运行中压缩（≥ 1 万且 ≥ 20 % 死条目，每 30 s）：克隆、压缩、替换，选择/忽略/重命名跟随 | 单元测试 + 100 万条 89 ms |
| 文件夹索引条目号 | 每次启动重新分配，属性/内容需重新提取 | 由父条目号 + 名称派生，重启后属性/内容直接挂回；遍历与监视之间的变化补扫 | 单元测试 |
| 运行中重建数据库 | IPC 拒绝 | 工具 → 重建索引、IPC Rebuild、改索引来源设置时自动重建（写入纪元淘汰旧线程）| kittest + IPC 测试 |
| 服务每连接鉴权 | 只有管道 DACL | 另按客户端进程校验交互会话与完整性 ≥ Medium | Windows：普通用户可用，会话 0 客户端被拒 |
| HTTP/ETP 密码 | 明文 | PBKDF2-HMAC-SHA256（10 万次、随机盐），旧明文可登录并在保存时替换 | RFC 向量 + 集成测试 |
| 撤销粘贴（复制/移动）| 不记录 | 记录 Shell 回报的结果，撤销/重做 | Windows 实测 |
| 名称排序 | 码点 | `name_collation`：中文界面默认拼音 | 单元测试 |
| 日期字段顺序 / 每周首日 | 固定 M/D/Y、周一 | 跟随系统区域（`GetLocaleInfoEx`），可设置覆盖 | Windows：Ymd、周一 |
| `filelistfilename:` `parent+N:` `target:` `fsi:` `recycled:` | 不支持 | 已实现 | Windows 真实 .lnk/junction/symlink/回收站 |
| 自定义列 / 类型列排序 | 不可 / 按扩展名 | 按公式值 / 按类型描述 | kittest |
| 属性表 | 96 个 | 305 个（9 种语言列标题）| Windows 属性模式核对 |
| IFilter | 不做 | 隔离宿主进程 `fennec.exe -ifilter-host`（ADR-035）| Windows：RTF 内容命中 |
| es/fes/HTTP 中读文件的函数 | 恒为空 | 与窗口相同的属性/内容/哈希库与按需读取；`fes -offline` 加载已保存的库 | Windows fes 实测 |
| 未索引属性列 | 空白 | 可见行后台读取；新增"链接目标""文件签名""回收站原位置"列 | kittest + Windows 截图 |
| 云占位/去重重解析点 | 当作链接 | 不算链接、不读取 | 单元测试 |
| 文件夹与卷索引重叠 | 条目重复 | 不再重复遍历 | 单元测试 |
| 删除硬链接的单个名称 | 别名残留 | `HARD_LINK_CHANGE` 后检查磁盘，只删该名称 | Windows：增删别名/主名称逐步正确 |
| `si:` 系统索引 | 未做 | AQS → Windows Search（OLE DB），结果映射回索引条目 | Windows：安装 Search 服务后 GUI 与 fes 实测 |
| ETP/FTP 服务器与客户端 | 未做 | `fnc-etp`（ADR-036）：FTP 浏览/下载 + Everything 搜索扩展；工具 → 连接到 ETP 服务器、`-connect` | Windows：Everything 1.4/1.5 客户端连 Fennec；Fennec 客户端连 Fennec 与 Everything 1.4 服务器；curl、ftp.exe |

决定（主代理）
- ETP 必须设密码（同 HTTP）；客户端默认经 `RETR` 下载打开；不做 TLS（Everything 也无，SPEC 第 4 章 §5 已改）；远程结果每次最多 1 万条。
- `si:` 的上限 10 万项、20 s 超时、30 s 复用为常量。
- 自定义列空值/错误排在最后，属性列未知值升序在前：保持现状。

仍未做
- 语义搜索相关（按用户要求暂缓）：索引吞吐低于 SPEC、`semimage:`。
- `weight:` 解析后忽略（官方文档未给出语义）。
- ETP：传输中的 `ABOR` 在传输结束后才应答；`OPTS UTF8 OFF` 仍发 UTF-8；客户端路径改写未做。
- 读文件的列不可排序（只对可见行读取）。

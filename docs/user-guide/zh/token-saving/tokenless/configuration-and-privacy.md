# Tokenless 配置与数据隐私

[English](../../../en/token-saving/tokenless/configuration-and-privacy.md)

Tokenless 默认启用压缩、本地统计和 SLS 度量。由于本地统计和 Stash 可能包含完整工具输出或被截断的原始 Payload，处理源代码、凭证和生产日志前应先确认这些默认值。

## 配置优先级

正常路径下，每个开关使用以下优先级：

```text
环境变量 > ~/.tokenless/config.json > 默认值
```

空环境变量视为未设置。布尔环境变量中，`1`、`true`、`yes`（大小写不敏感）表示 true；其他非空值表示 false。为了可读性，建议明确使用 `true` 或 `false`。

当前实现有一个例外：当 `TOKENLESS_STATS_ENABLED` 和 `TOKENLESS_SLS_ENABLED` 都是非空值时，代码会完全跳过配置文件。在这个分支中，压缩开关优先使用 `TOKENLESS_COMPRESSION_ENABLED`；未设置时直接默认为 `true`。如果同时导出两个记录开关，也应显式导出压缩开关。

## 配置文件

配置路径：

```text
~/.tokenless/config.json
```

完整示例：

```json
{
  "stats_enabled": true,
  "sls_enabled": true,
  "compression_enabled": true
}
```

配置文件缺失、不可读或 JSON 无效时，当前代码会静默使用内存中的全 `true` 默认值。手动编辑后可先验证：

```bash
jq . ~/.tokenless/config.json
```

| 字段 | 默认值 | 实际行为 |
|------|--------|----------|
| `stats_enabled` | `true` | 把压缩前后文本和度量写入本地 SQLite |
| `sls_enabled` | `true` | 目标 JSONL 文件已存在时追加仅包含度量的记录 |
| `compression_enabled` | `true` | 为 true 时返回压缩结果；false 时进入 dry-run 并返回原文 |

Tokenless 写入配置文件时会把权限限制为 `0600`。手动创建文件后也应确认：

```bash
chmod 600 ~/.tokenless/config.json
```

`stats` 子命令只修改 `stats_enabled`：

```bash
tokenless stats status
tokenless stats enable
tokenless stats disable
```

执行这些命令后，环境变量覆盖仍然优先。例如 `TOKENLESS_STATS_ENABLED=0 tokenless stats enable` 会把文件保存为 `true`，但带有该环境变量的进程仍会关闭统计。

## 可选 Git Diff 上下文裁剪

Git Diff 裁剪默认关闭。在 Tokenless 或宿主 Agent 继承的环境中设置
`TOKENLESS_DIFF_COMPRESSION_ENABLED=1` 启用；取消变量或设为 `0` 关闭。
`1`、`true`、`yes`（不区分大小写）表示启用。此开关独立于总压缩开关，
不是 `config.json` 字段。启用裁剪后，设置 `TOKENLESS_COMPRESSION_ENABLED=0`
会测量候选，但仍返回原文。

Rust 使用 `RuntimeConfig.diff_compression_enabled`；Python 使用
`TokenlessConfig(diff_compression_enabled=True)` 或原生 `TokenlessRuntime`
的同名参数。SDK 参数默认 false，显式配置；上述 CLI 环境变量不覆盖 SDK 参数。

压缩器处理成功命令输出中收到的完整普通 Git Diff，需要文本替换能力、可用的
Stash 和支持的恢复方式。保留全部增删行、元信息，以及修改附近两行可用上下文。
在每个 hunk 内，如果拆分增加的头部开销更大，可以多留上下文。仅在字符数减少、
且计入说明和恢复指令后至少节省 16 个估算 token 时采用。此估算不调用运行时
 tokenizer，不能保证对所有模型 tokenizer 都减少 token。

采用后的操作名为 `diff_reduction`，恢复等级是 `retrievable`，而非 `lossless`：
可见输出省略了部分未修改上下文。原文仍在 Stash 时，可按输出中的 shell 或工具
指令取回收到的原始内容；恢复需要额外工具调用。文件读取和已标记由 RTK 优化的
结果直接透传；启用此功能不改变 RTK 命令重写。不支持或不完整的 Diff 透传。
重命名、二进制摘要等特殊文件段保留收到的字节；编码二进制补丁整份透传。
Tokenless 不读取宿主持久化输出文件以补全被截断的 Diff，也不改变宿主截断上限。

有限样本已验证局部压缩及原文恢复，尚未证实稳定的 Agent 整轮 token 收益，
因此该功能保持可选启用。

## 可选 HTML 页面转写

HTML 页面转写默认关闭。在 Tokenless 或宿主 Agent 继承的环境中设置
`TOKENLESS_HTML_EXTRACTION_ENABLED=1` 启用；取消变量或设为 `0` 关闭。
`1`、`true`、`yes`（不区分大小写）表示启用。此开关独立于总压缩开关，
不是 `config.json` 字段。启用转写后，设置 `TOKENLESS_COMPRESSION_ENABLED=0`
会测量候选，但仍返回原文。

Rust 使用 `RuntimeConfig.html_extraction_enabled`；Python 使用
`TokenlessConfig(html_extraction_enabled=True)` 或原生 `TokenlessRuntime`
的同名参数。SDK 参数默认 false，显式配置；上述 CLI 环境变量不覆盖 SDK 参数。

压缩器处理成功命令输出或 API 响应中收到的完整 HTML 文档（以 `<!doctype html`
或 `<html>` 开头），例如 `curl` 抓取的页面或 MCP 工具返回的页面，需要文本替换
能力、可用的 Stash 和支持的恢复方式。页面转写为 Markdown 子集：标题、段落、
列表、表格、带语言的围栏代码、带目标的链接、图片 alt、引用和提示框。正文根为
`<main>`、`role=main` 元素或唯一的 `<article>`，否则为整个 body。移除脚本、
样式、`noscript`、模板、SVG、iframe、注释、`nav`、`aside`、页面级 `header`/`footer`，
以及 role 为 navigation、banner、contentinfo、complementary 的元素。视图首行写明
根元素、根之外省略的节点数和每一类的移除计数。转写后正文少于 64 个字符的页面
（例如应用空壳）透传；标记嵌套超过 512 层的页面也透传，因为 HTML 解析耗时随
嵌套深度二次方增长，这类页面不做解析。仅在字符数减少、且计入说明和恢复指令后至少节省 16 个
估算 token 时采用。

采用后的操作名为 `html_extraction`，恢复等级是 `retrievable`，而非 `lossless`：
标记和被移除的元素不在可见输出中。原文仍在 Stash 时，可按输出中的 shell 或工具
指令取回收到的原始内容。页面通过脚本加载的内容在视图中不可见。内容来源由
adapter 按工具分类：文件读取透传，但 shell `cat` 打印的页面或 MCP 文件工具返回
的页面会与抓取的页面一样被转写，需要取回才能看到源码。

有限样本已验证局部转写及原文恢复，尚未证实稳定的 Agent 整轮 token 收益，
因此该功能保持可选启用。

## 环境变量

### 用户常用变量

| 变量 | 用途 | 约束 |
|------|------|------|
| `TOKENLESS_STATS_ENABLED` | 覆盖本地统计开关 | 不影响 SLS 或 Stash |
| `TOKENLESS_SLS_ENABLED` | 覆盖 SLS 度量开关 | 不影响本地统计 |
| `TOKENLESS_COMPRESSION_ENABLED` | 覆盖真实压缩开关 | false 是 dry-run，不是完全停用 |
| `TOKENLESS_DATA_DIR` | 存放 `stats.db` 和 `stash.db` 的目录 | 可访问的任意绝对目录，但不能是文件系统根目录或包含父目录遍历 |
| `TOKENLESS_STATS_DB` | 覆盖统计数据库路径 | 必须位于真实用户 home 或选定的数据目录下 |
| `TOKENLESS_STASH_DB` | 覆盖 Stash 数据库路径 | 必须位于真实用户 home 或选定的数据目录下 |
| `TOKENLESS_SLS_PATH` | 覆盖 SLS JSONL 路径 | 必须位于 `/var/log/` 或 `/tmp/` 下 |
| `TOKENLESS_DIFF_COMPRESSION_ENABLED` | 启用 Git Diff 上下文裁剪 | 默认关闭；`1`、`true`、`yes` 启用；不覆盖 SDK 参数 |
| `TOKENLESS_HTML_EXTRACTION_ENABLED` | 启用 HTML 页面转写 | 默认关闭；`1`、`true`、`yes` 启用；不覆盖 SDK 参数 |

### Adapter 和诊断变量

| 变量 | 用途 |
|------|------|
| `TOKENLESS_AGENT_ID` | Adapter 注入的 Agent 标识 |
| `TOKENLESS_SESSION_ID` | Adapter 注入的 Session 标识 |
| `TOKENLESS_TOOL_USE_ID` | Adapter 注入的工具调用标识 |
| `TOKENLESS_TRACEPARENT` | 覆盖写入 SLS 记录的 W3C trace context |
| `TRACEPARENT` | 标准 W3C trace context 变量，由启动方宿主或 Adapter 注入 |
| `TOKENLESS_TOOL_READY_SPEC` | 覆盖 Tool Ready 依赖规范路径 |
| `TOKENLESS_ENV_FIX_SCRIPT` | 覆盖环境修复脚本路径 |
| `TOKENLESS_PACKAGE_MANAGER` | 覆盖包管理器探测，主要用于测试 |

当前构建已硬关闭 Tool Ready。依赖规范和修复脚本覆盖仅为休眠的旧版实现保留，运行时不会生效；这些路径会经过信任校验，也不建议普通用户修改。

`TOKENLESS_TRACEPARENT` 与标准变量 `TRACEPARENT` 用于为 SLS 记录提供 W3C trace context。注入是启动方的责任：OpenTelemetry 通过进程内 carrier 传播 W3C context，不会把 active span 导出为进程环境变量，因此需要关联能力的宿主或 Adapter 必须在启动 Tokenless 的环境中显式写入这两个变量之一，Tokenless 只负责读取：先看覆盖项，覆盖项为空或无法解析时回退到标准变量，因此一次拼写错误不会让整个会话失去关联能力。两者都是可选的：两个变量都没有可用上下文时，记录结构与之前完全一致，即以未关联形式写出；该标识只写入 SLS JSONL，本地统计数据库不保存。

数据库路径优先级如下：

- 统计库：`TOKENLESS_STATS_DB` > `TOKENLESS_DATA_DIR/stats.db` > `~/.tokenless/stats.db`
- stash 库：`--stash-db` > `TOKENLESS_STASH_DB` > `TOKENLESS_DATA_DIR/stash.db` > `~/.tokenless/stash.db`

`TOKENLESS_DATA_DIR` 是显式的目录级迁移配置，可以指向真实用户 home 之外，包括 `/var/lib` 下由服务管理的目录。CLI 和随包 RTK 写入器都会拒绝文件系统根目录、相对路径、父目录遍历以及已存在的非目录目标。若没有有效的更高优先级文件覆盖项，显式数据目录无效时会停用本次操作的 SQLite 状态，不会静默回退到 home。

空值视为未设置。`TOKENLESS_DATA_DIR` 可以指向尚不存在的目录；Tokenless 会先规范化其最近的已存在父目录，再创建目标目录。文件级覆盖项只能位于规范化后的真实用户 home 或选定的数据目录下，且已存在的数据库软链接会被拒绝。该变量不会迁移 `~/.tokenless/config.json` 或 SLS JSONL 输出。

DeepSeek Harness 是默认数据库位置的例外：它的沙箱会移除继承的 `TOKENLESS_*` 变量，并且
可能无法访问 home 目录。未设置 `TOKENLESS_DATA_DIR` 时，Adapter 使用会话工作区下的
`.tokenless`，并为该目录以及 `TOKENLESS_STATS_DB`、`TOKENLESS_STASH_DB` 发布受控 Shell
别名。默认工作区目录包含内容为 `*` 的 `.gitignore`，因此完整工具文本、Stash Payload 和
SQLite sidecar 不会被 `git add -A` 暂存。Adapter 不会修改自定义路径；应确保 DSH Shell
沙箱可以访问它们，并按数据策略将其排除在源码管理或备份之外。

## 本地与外部数据

| 数据 | 默认路径 | 默认内容 | 保留方式 | 如何停止新增 |
|------|----------|----------|----------|--------------|
| 本地统计 | `~/.tokenless/stats.db` | 压缩前后完整文本、标识和度量 | 无自动 TTL，直到清理 | `tokenless stats disable` |
| Stash | `~/.tokenless/stash.db` | 截断时移除的原始字符串、截断数组中被丢弃的中间段、被缩减为采样集合的完整对象记录数组、深层子树、Schema 描述和 build/log 间隙内容 | TTL 1 小时、最多 10,000 个有效条目，过期行延迟清理 | CLI 使用 `--no-stash`；Agent 场景禁用 Adapter |
| 配置 | `~/.tokenless/config.json` | 三个布尔开关 | 持续保留 | 不适用 |
| SLS JSONL | `/var/log/anolisa/sls/ops/tokenless.jsonl` | 度量和标识，不含压缩原文 | 由 SLS/Logtail 设施管理 | `TOKENLESS_SLS_ENABLED=0` 或配置为 false |

### 本地统计的敏感性

`stats.db` 的 `before_text` 和 `after_text` 保存完整内容。`tokenless stats show` 会输出这些内容，单记录和 tool-use 级别的 `tokenless stats diff` 可以据此显示变化行。这些文本可能包含：

- 源代码和补丁。
- 命令输出中的路径、用户名或环境信息。
- API 返回的业务数据。
- 日志中的访问令牌、Cookie 或凭证。

`tokenless` CLI 的 SQLite Recorder 每次打开 `stats.db` 时都会尝试设置 `0600`。随包提供的 RTK 统计补丁也可以直接创建或打开同一个文件，但它本身不会执行该权限修改。不要依赖进程 umask，应检查数据库及 sidecar 文件：

```bash
ls -l ~/.tokenless/stats.db*
```

### Stash 的敏感性

Stash 保存压缩时被截断的原始内容，而不是摘要。记录缩减写入的条目是缩减前的完整原始数组，而不仅是被省略的记录。仅因字段在黑名单中、值为 `null` 或为空而被移除的内容不会写入 Stash。`tokenless` CLI 会把路径限制在真实用户 home 或选定的数据目录下，但仍应确认数据库及 SQLite sidecar 文件不会被其他本机用户读取：

```bash
ls -l ~/.tokenless/stash.db*
```

TTL 表示条目超过一小时后不能再通过 `retrieve` 返回。过期行会在后续取回时延迟删除；TTL 不应被理解为立即安全擦除磁盘数据。当有效条目超过 10,000 个时，存储会优先淘汰到期时间最早的条目，因此高负载下可能不到一小时就无法取回。

### SLS 不包含原文

Tokenless 的 SLS JSONL 只写入组件、Operation、Session/Tool Use 标识、宿主传入的 trace 标识（若存在）和字符/Token 度量，不写 `before_text` 或 `after_text`。但标识字段本身仍可能属于组织的运行元数据，应按照平台日志策略管理。

## 敏感工作负载建议

### 只压缩，不保存统计

```bash
TOKENLESS_STATS_ENABLED=0 \
TOKENLESS_SLS_ENABLED=0 \
  tokenless compress-response --no-stash -f response.json
```

这适用于独立 CLI。Agent Adapter 默认可能使用 Stash；如果框架没有合适的排除规则，应对敏感任务禁用 Adapter。

### 保留 Adapter，但不实际压缩

在启动 Agent 的环境中设置：

```bash
export TOKENLESS_COMPRESSION_ENABLED=0
```

这是 dry-run，仍可能写入本地统计或 SLS。若不希望落盘，还需同时关闭：

```bash
export TOKENLESS_STATS_ENABLED=0
export TOKENLESS_SLS_ENABLED=0
```

Dry-run 不会创建 Stash 条目，但也不会关闭 RTK 重写。Tool Ready 已独立硬关闭。需要停止全部 Hook 行为时应禁用 Adapter。

### 完全停止 Agent 中的 Tokenless

```bash
anolisa adapter disable tokenless <framework>
```

禁用后重启 Agent。仅设置 `compression_enabled=false` 不会停止 Hook/Plugin 执行。

## 清理数据

清空本地统计记录：

```bash
tokenless stats clear --yes
```

该命令会清空当前环境解析到的统计库记录，但不会删除数据库文件或 SQLite sidecar。Tokenless 当前没有 Stash clear 子命令。需要不可逆地删除本地数据库时：

1. 先禁用所有 Tokenless Adapter。
2. 退出仍可能使用数据库的 Agent 和 Tokenless 进程。
3. 确认不再需要历史统计和 Stash 取回。
4. 备份需要保留的数据。
5. 在启动 Agent、服务和 Tokenless 的实际环境中检查路径覆盖：

```bash
env | grep -E '^TOKENLESS_(DATA_DIR|STATS_DB|STASH_DB)='
```

统计库按 `TOKENLESS_STATS_DB`、`TOKENLESS_DATA_DIR/stats.db`、`~/.tokenless/stats.db` 的顺序解析；Stash 按命令行 `--stash-db`、`TOKENLESS_STASH_DB`、`TOKENLESS_DATA_DIR/stash.db`、`~/.tokenless/stash.db` 的顺序解析。把最终路径写成经过确认的绝对路径，不要把未经验证的环境变量直接展开到删除命令中。

下面的命令同时适用于默认路径和自定义路径。先替换并再次打印两个路径，确认它们都是需要删除的 Tokenless 数据库：

```bash
stats_db='/absolute/path/to/resolved/stats.db'
stash_db='/absolute/path/to/resolved/stash.db'
printf '%s\n' "$stats_db" "$stash_db"
rm -f -- \
  "$stats_db" \
  "$stats_db-wal" \
  "$stats_db-shm" \
  "$stats_db-journal" \
  "$stash_db" \
  "$stash_db-wal" \
  "$stash_db-shm" \
  "$stash_db-journal"
```

该操作不可恢复。不要把数据目录或 `~/.tokenless/` 作为递归删除目标，因为其中还可能包含希望保留的配置或其他文件。

## OpenClaw 的细粒度控制

OpenClaw Plugin 还提供框架级选项：

| 选项 | 作用 |
|------|------|
| `rtk_enabled` | 通过 RTK 改写支持的 Shell 命令 |
| `tool_ready_enabled` | OpenClaw 侧的 Tool Ready 注册门槛 |
| `post_tool_enabled` | 优化支持的持久化工具结果 |
| `verbose` | Plugin 诊断日志 |

OpenClaw Plugin 不压缩工具 Schema，也不提供内容恢复。无法在不恢复内容的情况下安全优化的
结果会原样透传。

RTK、OpenClaw 侧的 Tool Ready 注册门槛和 PostTool 默认开启，verbose 日志默认关闭。由于
Tokenless 已硬关闭底层检查，Tool Ready 选项当前没有实际效果。Tokenless 会自动判断 JSON
清理或 TOON 是否有收益，以及哪些工具输出必须原样透传。已经删除的
`response_compression_enabled`、`toon_compression_enabled`、`skip_tools` 和 `shell_tools` 不再
控制 Adapter。

这些值由 OpenClaw Plugin 配置管理，不属于 `~/.tokenless/config.json`。Adapter 要求
OpenClaw Plugin API 2026.4.22 或更高版本。修改后按 OpenClaw 提示重启 gateway。

## 相关文档

- [效果度量](measuring-savings.md)
- [CLI 参考](cli-reference.md)
- [Agent 集成](framework-integration.md)
- [故障排查](troubleshooting.md)

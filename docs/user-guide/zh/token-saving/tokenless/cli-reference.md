# Tokenless CLI 参考

[English](../../../en/token-saving/tokenless/cli-reference.md)

`tokenless` CLI 开放统一的 content-aware 压缩命令、直接 Schema/JSON/TOON 操作、Stash
恢复、环境状态与统计。共享 Agent Hook 使用 `tokenless compress`；直接命令继续用于脚本和
诊断。

## 命令总览

| 命令 | 用途 |
|------|------|
| `tokenless compress` | 执行一个 Protocol v2 生命周期操作 |
| `tokenless compress-schema` | 压缩 Function Calling 工具 Schema |
| `tokenless compress-response` | 压缩 JSON/API/工具响应 |
| `tokenless compress-toon` | 将 JSON 编码为 TOON |
| `tokenless decompress-toon` | 将 TOON 解码为 JSON |
| `tokenless retrieve` | 取回被截断并写入 Stash 的 Payload |
| `tokenless env-check` | 报告旧版环境检查已硬关闭 |
| `tokenless stats` | 查询和控制本地统计 |

使用当前安装版本的帮助查看最终参数定义：

```bash
tokenless --help
tokenless <command> --help
```

## 通用输入规则

压缩和编解码命令支持两种输入方式：

```bash
tokenless compress-response --file response.json

cat response.json | tokenless compress-response
```

- `-f` 是 `--file` 的缩写。
- 不传 `--file` 时必须通过 stdin 提供输入。
- 单次输入上限为 64 MiB。
- JSON 相关命令要求输入为合法 JSON。
- 压缩后没有 Token 收益时，CLI 向 stderr 说明原因，并输出原文。

### 最小有效 Payload

`compress-schema` 和 `compress-response` 没有固定的最小输入大小。对于每个通过输入
规则的合法 JSON，CLI 都会生成候选结果，并用同一个启发式规则估算两者的 Token 数：每个 CJK
字符计一个 Token，其他字符每四个计一个 Token 并向上取整。在 Active 模式下，只有候选结果的
估算值严格小于原文（`after < before`）时才会输出候选结果；否则 stdout 输出原文，stderr
报告 `did not reduce size`，且不写入统计记录。在 Dry-run 模式
（`TOKENLESS_COMPRESSION_ENABLED=0` 或 `compression_enabled=false`）下，stdout 始终输出
原文；候选结果更小时，如果已启用 Stats 或 SLS 记录，则把它记为预测节省。

因此，盈亏平衡点取决于内容和 JSON 结构，而不只取决于字节数或字符数。包含可移除字段
的小 Payload 仍可能被压缩，而已经紧凑的较大 Payload 也可能原样透传。下文的描述、
字符串、数组和深度阈值只决定单项转换何时触发，并不是整个 Payload 的最小大小。
Agent Adapter 还可能在启动 CLI 前应用独立的大小门槛，详见
[Adapter 处理规则](framework-integration.md#adapter-处理规则)。

## `compress`

`compress` 是共享 Agent Hook 使用的严格 Protocol v2 Transport。命令名保持不变，但 Wire
格式有意不兼容 Protocol v1。每个 Envelope 顶层严格只有四个字段：请求使用
`protocol_version`、`operation`、`attribution`、`input`，响应把 `input` 换为 `result`。
Envelope 和操作 Payload 中的未知字段都会被拒绝。

PostTool 示例：

```bash
jq -n \
  --rawfile content build.log \
  '{
    protocol_version: 2,
    operation: "post_tool",
    attribution: {
      agent_id: "my-agent",
      session_id: "session-42",
      tool_use_id: "tool-7"
    },
    input: {
      result_kind: "tool",
      tool_name: "Bash",
      content: $content,
      status: "success",
      content_origin: "command_output",
      command: "cargo build",
      output_optimization: "none",
      capabilities: {
        replace_output: true,
        recovery: {kind: "none"},
        replace_with_text: true
      }
    }
  }' \
  | tokenless compress \
  | jq '{operation, result: (.result | {disposition, content_type, applied_operations, recoverability, output})}'
```

操作：

| `operation` | 必填 `input` 事实 | 结果 |
|-------------|-------------------|------|
| `before_model` | `tools`、排除工具后的模型可见上下文、工具替换能力，以及 Integration 是否已有 Marker 授权恢复 | 变换后的工具与排序后的可见 Marker Hash |
| `pre_tool` | 工具名、参数、明确的命令字段，以及参数替换/阻止能力 | 参数、`passthrough`/`replace_arguments`/`block_and_suggest` Action，以及 `none`/`rtk` 输出优化状态 |
| `post_tool` | Result Kind、工具名、内容、状态、明确 Content Origin、上游输出优化状态和输出能力 | Output、Disposition、检测类型、实际操作、Recoverability、Token 计数和 Stash Keys |
| `retrieve` | Hash 或 Marker，以及模型当前可见的 Marker 集合 | 授权后的规范化 Hash 和字节级一致 Payload |

只有 `post_tool` 会进入 `PostToolPipeline`。当前生产路由为：

- Retrieve Result、Interrupted/Denied 调用和 RTK 已优化输出绕过压缩。
- Tool Error 原样透传，并可携带限长 `additional_context` 诊断。
- 成功 JSON 使用 `JsonCompressor`，并可能选择 Compact JSON 或 TOON。
- 已识别的成功构建/测试命令输出使用 `BuildLogCompressor`，可选择 Terminal Cleanup 和可恢复的
  Routine Progress Reduction。
- 成功 CSV/TSV 在宿主支持任意文本替换时使用 `TabularCompressor`，可选择保留全部单元格的
  全量压紧或可恢复的行筛选。文件来源结果透传。行筛选要求列名证据，且精确源行号范围列表
  不超过 1 KiB；见[CSV/TSV 视图](user-manual.md#csvtsv-视图可能不完整)。
- 其他内容类型在对应领域 Compressor 接入前原样透传。

只有 `disposition: "applied"` 表示 `output` 与原文不同。`dry_run`、`passthrough`、
`no_savings`、`recoverability_unavailable`、`timeout` 和 `tool_error` 都携带原始内容。
JSON 的 `content_type` 现在是 `json`；`applied_operations` 报告实际发生的变换，而不是配置的
Compressor 列表。

`before_model` 和 `post_tool` 必须提供 `capabilities.recovery`：`{"kind":"none"}`、
`{"kind":"shell"}` 或 `{"kind":"tool","name":"tokenless_retrieve"}`。Tool 名称限 1–64 个
ASCII 字母、数字、下划线或连字符。未知字段、缺失 recovery 和旧的 `retrieval_available`
布尔字段均会被拒绝，Core 与调用方必须一起更新；协议版本保持 2。只有静态 Tool 能启用
BeforeModel Schema 的可恢复截断；Shell 恢复用于 PostTool，不新增或改变 Agent 工具。

退出码属于 Transport 契约：

- `0`：正常操作结果，包括透传、无收益、Dry-run、Recoverability 不可用、RTK 不适用和
  Tool Error。
- `1`：操作失败，包括 RTK Timeout、未授权/缺失 Retrieve、Stash 或 Pipeline 失败。错误写入
  stderr，不输出 Response JSON。
- `2`：JSON 格式错误、不支持的协议版本，或 Envelope/Payload Shape 无效。

`pre_tool` 从 `PATH` 和支持的安装布局解析独立打包的 `rtk`；低于 0.35.0 的候选会被跳过，
并继续尝试后续打包位置。已识别的 Cargo、pytest、npm/Jest、Go 和 Make 构建/测试命令保持
原生形式，使其输出只有一个 PostTool 所有者；其他受支持命令仍可由 RTK 改写。Agent-facing
`retrieve` 在读取 Stash 前根据 `visible_markers` 授权
请求 Hash。下文的独立 `tokenless retrieve` 是受信本地运维入口，不要求模型可见性上下文。

## 随包提供的 RTK 命令

Tokenless 随包提供 RTK 0.49.0。以下行为适用于该二进制；如果 `PATH` 中更靠前的
位置存在兼容的 `rtk`，实际使用的版本可能不同。直接使用这些命令时，可通过
`rtk --version` 检查版本。

### 搜索 Flag

`rtk grep` 将原生搜索 Flag 转交给 `grep`：`-l` 列出匹配文件名，`-m N` 限制每个
文件的匹配数量。它们不再表示 RTK 的行长度或显示数量限制。要控制 RTK 的显示限制，
请在 Pattern 前使用长选项 `--max-len` 和 `--max`。原先 RTK 提供的 `-t` /
`--file-type` 选项已移除；按文件类型过滤时，使用 `rtk rg -t rust` 调用 ripgrep
的原生功能。向 `rtk grep` 传入 `-t` 现在会显示底层 grep 不支持该选项的错误。

```bash
rtk grep -l 'TODO' src/*.rs
rtk grep -m 3 'TODO' src/*.rs
rtk grep --max 20 --max-len 120 'TODO' src/*.rs
rtk rg -t rust 'TODO' src/
```

### Shell 重写

RTK 支持跨多行处理已支持的命令，并对 Pipeline 应用保守规则。它可以重写受支持的
最后一级；当下游全部是 `head`、`tail` 或 `cat` 等允许的显示命令时，也可以重写
受支持的输出生产者。统计或解析原始输出的 Pipeline 可能保持原样，不能保证每个
Shell 表达式都会被重写。`sudo` 命令保持原样透传，自动重写不会将 RTK 插入特权命令。

使用 `rtk rewrite 'COMMAND'` 可以查看建议的重写结果，而不执行命令。
Tokenless 仍按上文规则让已识别的构建/测试命令保持原生形式，交给 PostTool 压缩。

### 输出恢复

在默认 RTK 配置下，受支持的 Filter 会将至少 500 字节的失败输出及明确截断的输出
保存到本地 SQLite 恢复存储。出现恢复提示时，使用提示中的 Hash 执行
`rtk recall HASH`；`rtk recall --full HASH` 请求全部已保存的输出，`--from`、
`--lines` 和 `--grep` 可缩小返回范围。`rtk recall --list` 列出保留的条目。
存储受容量及过期策略限制，因此 Recall 不是永久归档。已有的旧版 tee 配置继续
使用基于文件的恢复模式。

RTK 恢复状态以宿主 OS 用户为作用域，不按 Tokenless 租户或 Session 隔离。
Linux 默认存储是 `$XDG_DATA_HOME/rtk/recall.db`；未设置 `XDG_DATA_HOME` 时使用
`~/.local/share/rtk/recall.db`。`TOKENLESS_DATA_DIR` 不会改变此位置。共用同一
OS 用户及 RTK 存储的 Session 可以列出并恢复彼此保留的输出。在命令执行环境中
设置 `RTK_RECALL_DB` 可以选择独立存储，设置 `RTK_RECALL=0` 可以禁用恢复。
独立路径不构成同一用户下的 OS 权限边界。

RTK Recall Hash 属于 RTK 的存储。Tokenless Stash Marker 应使用
`tokenless retrieve HASH`；这两个恢复命令不能互换。

## `compress-schema`

压缩单个 OpenAI Function Calling Schema：

```bash
tokenless compress-schema -f tool.json
```

压缩 JSON 数组：

```bash
cat tools.json | tokenless compress-schema --batch
```

接受的输入条目形态（按条目自动识别）：

- OpenAI function 包装：`{"function": {"name", "description", "parameters"}}`
- 直接 Schema：`{"name", "description", "parameters"}`
- Gemini / copilot-shell 包装：`{"functionDeclarations": [{"name", "description", "parameters" | "parametersJsonSchema"}, ...]}`；copilot-shell 的 BeforeModel hook 以该形态下发工具声明（`llm_request.config.tools`）。包装内的声明逐个压缩（参数 schema 优先取 `parametersJsonSchema`，其次取 `parameters`），包装本身及其同级字段原样保留。

输入本身是数组时会自动使用 batch 处理。

包含顶层 `tools` 数组的完整请求对象也受支持，其中的 Function Calling 定义可以是
OpenAI `{"function": {...}}` Wrapper、Gemini `{"functionDeclarations": [...]}` 工具对象，
或裸 `{name, description, parameters}` 定义。该结构不要传 `--batch`；非函数工具及
`tools` 之外的字段会原样保留。

```bash
tokenless compress-schema -f request.json
```

常用参数：

| 参数 | 说明 |
|------|------|
| `-f, --file <path>` | 输入文件；省略时读 stdin |
| `--batch` | 把输入作为 Schema 数组处理 |
| `--agent-id <id>` | 统计中的 Agent 标识 |
| `--session-id <id>` | 统计中的 Session 标识 |
| `--tool-use-id <id>` | 统计中的工具调用标识 |
| `--no-stash` | 不保存被截断的描述；截断将不可逆 |
| `--stash-db <path>` | 覆盖 Stash 数据库；无效路径会被拒绝为覆盖值，并回退到环境变量或默认路径 |

默认处理规则：

| 项目 | 默认值 |
|------|--------|
| 函数描述最大长度 | 256 字符 |
| 参数描述最大长度 | 160 字符 |
| 删除 `examples` | 是 |
| 删除 `title` | 是 |
| 移除描述中的围栏代码和行内代码，再合并空白 | 是 |
| 最大递归深度 | 32 |

示例：

```bash
tokenless compress-schema -f tools.json --batch \
  --agent-id copilot-shell --session-id session-001
```

## `compress-response`

压缩 JSON 响应：

```bash
tokenless compress-response -f response.json
```

默认会移除名称完全匹配且区分大小写的黑名单字段、`null`、空字符串/数组/对象，包括数组中的空项；随后截断长字符串、长数组和超过配置嵌套深度的值。常用参数：

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `-f, --file <path>` | stdin | 输入文件 |
| `--truncate-strings-at <n>` | `4096` | 字符串截断阈值 |
| `--truncate-arrays-at <n>` | `32` | 触发数组截断的长度阈值；保留前 `n` 个元素。对象记录数组改用记录缩减（见下文） |
| `--array-tail-preserve <n>` | `8` | 截断数组时从尾部保留的元素数；`0` 禁用尾部保留。不适用于对象记录数组 |
| `--max-depth <n>` | `8` | 最大嵌套深度 |
| `--agent-id <id>` | `cli` | 统计中的 Agent 标识 |
| `--session-id <id>` | — | 统计中的 Session 标识 |
| `--tool-use-id <id>` | — | 统计中的工具调用标识 |
| `--no-stash` | 关闭 | 禁用可逆 Stash |
| `--stash-db <path>` | `~/.tokenless/stash.db` | 覆盖 Stash 数据库；无效路径会被拒绝为覆盖值，CLI 随后回退到环境变量或默认路径 |

数组截断会保留 `--truncate-arrays-at` 个头部元素和 `--array-tail-preserve` 个尾部元素，并在两者之间插入截断标记。只有当数组长度超过首尾窗口之和时才会丢弃中间元素：默认配置下单条命令可保留 `n + 8` 个元素（外加截断标记）；当两个窗口覆盖整个数组时，所有元素都会保留且不插入标记。设置 `--array-tail-preserve 0` 可恢复纯头部截断。

对象记录数组是例外：任何包含至少 33 个 JSON Object 的数组都会按 32 条记录的基础预算进行缩减，并追加一个取回标记，与 `--truncate-arrays-at` 和 `--array-tail-preserve` 的取值无关。选取会保留前 4 条和后 4 条记录、携带错误或异常信号的记录、数值异常记录，以及其余记录的稳定采样；关键记录可以突破基础预算。完整原始数组作为一个条目写入 Stash，可通过 `tokenless retrieve` 恢复。记录缩减依赖 Stash，因此使用 `--no-stash` 时会保留此类数组的全部记录而不做缩减。

覆盖阈值：

```bash
tokenless compress-response -f response.json \
  --truncate-strings-at 2048 \
  --truncate-arrays-at 16 \
  --max-depth 6
```

默认删除的字段名为：

```text
debug, trace, traces, stack, stacktrace, logs, logging
```

字段匹配和截断会改变模型看到的响应表示。处理关键 Payload 前，应先保存样例并对比压缩结果。

Stash 会保存 Record Reduction 使用的完整原始数组、被截断的字符串、其他截断数组中被丢弃的中间段和深层子树。尾部元素直接保留在输出中，不进入 Stash。黑名单字段、`null` 和空值会直接移除，不会生成取回标记。

大多数 Adapter 会覆盖这些独立 CLI 默认值。共享 Shell 策略使用 `65536`、`128`、`8`；其他结构化工具策略使用 `1048576`、`65536`、`32`。内容读取类工具会被跳过。详见[Agent 集成 · Adapter 处理规则](framework-integration.md#adapter-处理规则)和[用户手册 · 压缩的触发条件与阈值](user-manual.md#压缩的触发条件与阈值)。

## `compress-toon` 与 `decompress-toon`

JSON 转 TOON：

```bash
echo '{"name":"Alice","age":30}' | tokenless compress-toon --min-toon-chars 0
```

短于 500 字符的负载默认原样透传：TOON 对小型 JSON 的节省近乎为零，因此 CLI 应用与 Adapter Hook 相同的最小长度。传入 `--min-toon-chars 0` 可对任意有 token 收益的负载编码。输入会先通过 JSON 校验再做长度判断，因此低于阈值的非法 JSON 仍会以退出码 2 失败。

TOON 转 JSON：

```bash
printf 'name: Alice\nage: 30\n' | tokenless decompress-toon
```

往返验证：

```bash
echo '{"name":"test","value":42}' \
  | tokenless compress-toon --min-toon-chars 0 \
  | tokenless decompress-toon
```

`compress-toon` 支持 `--agent-id`、`--session-id`、`--tool-use-id` 和 `--min-toon-chars`。当负载低于最小长度或编码后无收益时会输出原 JSON，且不记录该次统计。这类透传场景退出码仍为 `0`，stderr 上的提示仅供参考：在脚本等自动化场景中，请通过比较 stdout 与输入负载来判断是否发生了编码，不要依赖 stderr。透传和无收益场景会在 stdout 上逐字节原样复现输入（不添加、不去除末尾换行符），任何字节差异都说明负载已被编码；作为兜底，也可以检查 stdout 是否仍为合法 JSON。

## `retrieve`

压缩输出中出现以下按需操作提示时，说明被省略的内容已写入 Stash：

```text
12 passing-test lines omitted. If needed, run in shell: tokenless retrieve 0123456789abcdef01234567
```

使用裸 Hash 取回：

```bash
tokenless retrieve 0123456789abcdef01234567
```

AgentScope 使用实际配置的静态 Tool，而不是 Shell 命令：

```text
12 passing-test lines omitted. If needed, call tool tokenless_retrieve with hash_or_marker=0123456789abcdef01234567
```

历史 Marker 仍可读取，但不再生成：

```bash
tokenless retrieve '<<tokenless:0123456789abcdef01234567>>'
```

覆盖数据库：

```bash
tokenless retrieve 0123456789abcdef01234567 \
  --stash-db ~/.tokenless/stash.db
```

Hash 必须是 24 个十六进制字符，且不区分大小写。SQLite Stash 默认 TTL 为一小时，最多保留 10,000 个有效条目。超过 TTL、被容量策略淘汰、使用 `--no-stash`、处于 dry-run、写入失败或数据库路径不一致时都无法取回。

## `env-check`

Tool Ready 已硬关闭。文本输出只报告这一状态，不会读取规范或改变环境。
任何环境变量都无法重新启用它。

所有 JSON 调用都只返回三个字段：

```json
{"tool":"Shell","status":"UNKNOWN","enabled":false}
```

`tool` 是指定的工具名、`all` 或 `checklist`。硬关闭契约绝不会包含休眠旧版
清单的 `tools` 或 `summary` 字段。

报告单个工具对应的硬关闭状态：

```bash
tokenless env-check --tool Shell
```

报告全部工具或清单模式的硬关闭状态：

```bash
tokenless env-check --all
tokenless env-check --all --json
tokenless env-check --checklist
tokenless env-check --checklist --json
```

自动修复：

```bash
tokenless env-check --tool Shell --fix
```

> 硬旁路生效期间，`--fix` 不会调用包管理器或修改环境。如果未来重新设计并启用，保留的旧版实现只会尝试修复缺失的必需依赖。

## `stats`

```bash
tokenless stats summary
tokenless stats summary --json
tokenless stats summary --limit 1000
tokenless stats list --limit 20
tokenless stats show <record-id>
tokenless stats diff <record-id>
tokenless stats diff --session <session-id>
tokenless stats status
tokenless stats enable
tokenless stats disable
tokenless stats clear --yes
```

双跑对比：

```bash
tokenless stats summary --compare <baseline-session> <active-session>
```

Session ID 不存在时以非零退出码失败，而不是输出 0% 对比，行为与 `stats diff --session` 一致。`stats summary --limit` 必须为正整数；`--limit 0` 会在解析阶段被拒绝，行为与 `stats diff --limit` 一致。

`stats summary --json` 的百分比字段（`chars_saved_percent`、`tokens_saved_percent`）与 `--compare --json` 的 `saved_percent` 都按节省量除以原始未压缩量计算；summary 与 compare 字段会把节省量钳制为 0，而 `stats diff --json` 保留负值。详见[效果度量 → 节省率字段定义](measuring-savings.md#节省率字段定义)。

查看单条记录，或一次工具调用中可确认衔接的阶段：

```bash
tokenless stats diff <record-id> -U 5
tokenless stats diff --session <session-id> \
  --tool-use-id <tool-use-id>
```

`stats show` 会输出存储的完整 before/after 文本；`stats diff` 用于解释估算节省并显示变化行。主要选项如下：

| 选项 | 适用范围 | 行为 |
|------|----------|------|
| `<record-id>` | 单条记录 | 与 `--session` 冲突 |
| `--session <id>` | Session | 显示仅含指标的总览 |
| `--tool-use-id <id>` | Session | 展开一次工具调用；必须配合 `--session` |
| `-l, --limit <n>` | Session 总览 | 最多显示的链路数，默认 `20` |
| `--sort saved\|time` | Session 总览 | 默认按节省量降序，也可按时间从新到旧 |
| `-U, --context <n>` | 内容差异 | 每处变化周围的未变化行数，默认 `3` |
| `--no-color` | 文本输出 | 关闭 ANSI 颜色 |
| `--json` | 所有范围 | 输出 schema `1.0` JSON 和结构化 diff hunks |

任一端内容不可用或超过 1 MiB 时不生成内容差异，渲染的 hunk 最多 500 行。单记录和 tool-use 差异可能包含存储的源文本，使用共享终端或收集输出时注意敏感信息。完整说明见[效果度量](measuring-savings.md)和[配置与数据隐私](configuration-and-privacy.md)。

`stats status` 只报告本地统计和 SLS 开关及其来源，因为当前状态读取路径没有读取 compression 开关，所以不显示 `compression_enabled`。该设置应检查 `TOKENLESS_COMPRESSION_ENABLED` 和 `~/.tokenless/config.json`。

## 错误与降级

- CLI 错误写入 stderr，并以非零状态退出。
- Hook/Plugin 通常捕获错误并透传原始响应。
- 无压缩收益不是错误；CLI 会输出原文。
- Stash 写入失败时压缩可能继续，但相关截断内容无法取回。

遇到输入、数据库或 Adapter 错误时，参阅[故障排查](troubleshooting.md)。

# Response 压缩功能说明

## 一、功能概述

PostTool 压缩由 Runtime 内部的 `PostToolPipeline` 编排，并按内容类型静态派发给 `JsonCompressor` 或 `BuildLogCompressor`。JSON 压缩器只解析一次输入，先生成不截断数据的 Compact JSON/TOON 候选；该候选同时减少字符与估算 Token 且 Token 节省率达到 15% 时直接采用。否则再生成包含 Record Reduction 或既有截断规则的 Bounded 候选，并选择更小的合法表示。Build Log 压缩器只处理成功的 `command_output`，先清理终端控制输出，再对已识别构建与测试日志中的重复常规进度进行可恢复缩减。Tool Error 保留原始输出并只追加环境诊断；其他内容类型当前透传。

## 二、8 条压缩规则

| # | 规则 | 判断条件 | 处理方式 | 默认阈值 |
|---|------|---------|---------|---------|
| R1 | **字符串截断** | Unicode 字符数 > 4096 | 在 Unicode 字符边界截断，追加截断标记 | 4096 字符 |
| R2 | **Record Reduction** | 数组至少有 33 项，且每项都是 JSON Object | 保留首尾、错误、结构异常和数值异常记录；普通记录去重后稳定等距采样；完整数组写入 Stash | 32 条普通预算；关键记录可突破预算 |
| R3 | **普通数组截断** | 非 Record Array 的元素数超过配置预算 | 保留前 32 个 + 末尾 8 个（`array_tail_preserve`），head 与 tail 之间插入截断标记；head+tail 覆盖全部元素时不截断 | 32 + 8 个 |
| R4 | **字段删除** | key 匹配黑名单 | 整个字段移除（不递归进入） | 7 个字段 |
| R5 | **null 移除** | 值为 `null` | 从对象/数组中删除 | 启用 |
| R6 | **空值移除** | 值为 `""` / `[]` / `{}` | 从对象/数组中删除 | 启用 |
| R7 | **深度截断** | 嵌套深度 > 8 | 替换为 `<{type} truncated at depth {N}>` | 8 层 |
| R8 | **原始类型保留** | bool / number | 直接保留，不做处理 | — |

**R4 默认黑名单字段**：`debug`, `trace`, `traces`, `stack`, `stacktrace`, `logs`, `logging`

Record Reduction 固定保留前 4 条和后 4 条；保留非空 `error/errors/exception/failure`
字段，以及 `status/state/level/severity` 中带错误信号的记录；还会保留字段集合不同于众数
Shape 的记录，以及至少 5 个数值样本中偏离均值超过 `2σ` 的记录。其余记录按完整内容去重，
再稳定等距采样补足到 32 条。输出维持原始顺序。

每个被缩减的数组只为集合本身写入一个 Stash Entry，Payload 是变换前完整数组的 Compact
JSON；输出末尾追加总数、遗漏数和 Retrieve Marker。没有可用 Stash 或写入失败时，不执行
Record Reduction，也不会回退为普通数组的位置型截断。

## 三、递归处理顺序

```
compress_value(value, depth)
 ├─ 1. 检查深度限制 → 超限则返回截断标记（R7）
 ├─ 2. 按类型分支：
 │   ├─ null / bool / number → 直接返回（R8）
 │   ├─ string → compress_string()（R1）
 │   ├─ array  → compress_array()
 │   │   ├─ Record Array → 保留关键记录并稳定采样（R2）
 │   │   ├─ 其他 Array → 截取 head/tail（R3）
 │   │   ├─ 逐项递归 compress_value(item, depth+1)
 │   │   ├─ 过滤 null（R5）和空值（R6）
 │   │   └─ 追加截断标记
 │   └─ object → compress_object()
 │       ├─ 跳过黑名单字段（R4）
 │       ├─ 逐值递归 compress_value(val, depth+1)
 │       └─ 过滤 null（R5）和空值（R6）
```

### Build Log 领域

`BuildLogCompressor` 识别 Cargo/rustc、pytest、npm/Jest、Go、Make/C，以及具有明确 Shell
边界、带编号进度词和稳定重复模板的通用日志。检测只采样首尾各 100 行、总计最多 64 KiB；
证据不足时直接透传，避免把普通 prose、源码或未知日志误判为 Build Log。

处理先生成无损 Terminal Cleanup Candidate；有可用 Stash 且格式证据充分时，继续在清理后
文本上生成 Progress Reduction Candidate，再选择更小的合法结果。压缩器按方言把每行分类为
Diagnostic、Summary、Phase、Routine Progress 或 Unknown，并使用 Python、JavaScript、Java、
Rust、Go 和 .NET Stack Trace 状态机保护完整异常区间。只有同一 Progress Family 连续至少 9
行时才考虑缩减：保留前 2 行和后 2 行，中间每个连续遗漏区间写入一个 Stash Entry 并插入
Retrieve Marker。Diagnostic、Summary、Phase、Unknown 和 Stack Trace 原样保留。

方言规则只吸收可从原生输出可靠识别的语义，例如 Cargo 编译及成功测试进度、pytest verbose
及 quiet 进度。npm Warning、Make 目录边界和非普通测试结果仍保持可见；压缩器不会为了获得
结构化输出而修改命令参数，也不采用固定行数截断。

单个日志出现超过 8 个遗漏区间时，压缩器在写 Stash 前放弃 Reduction Candidate，防止输出被
过多 Marker 切碎。每个区间还必须同时减少字符和估算 Token；无可用 Stash 时只允许 Terminal
Cleanup。Runtime 最后仍只执行一次全局字符/Token 仲裁和一次 Stash Commit/Rollback。Tool Error
不进入内容压缩 Pipeline，Core 保留原始工具输出并追加环境诊断信息。

为避免两个组件依次改写同一输出，PreTool 对明确的 Cargo、pytest、npm/Jest、Go 和 Make
构建/测试命令直接返回 `passthrough` 与 `output_optimization: "none"`，不调用 RTK；其原生输出
随后才可能进入 `BuildLogCompressor`。其他受支持命令仍由 RTK 处理，带
`output_optimization: "rtk"` 的结果继续直接旁路 PostTool Pipeline。目前 PreTool 没有声明
宿主 PostTool 替换与恢复能力，因此这项命令所有权选择对所有宿主一致；无法应用 Build Log
缩减的宿主可能暂时原样接收这些命令的输出。

## 四、集成路径

### 路径 1：OpenClaw 插件（`tool_result_persist` hook）

```
工具执行完成
   ↓
OpenClaw 触发 tool_result_persist 事件
   ↓
Plugin 按 Tool Call ID 消费 PreTool 输出优化状态
   ↓
转换为 `operation: "post_tool"` 的 Protocol v2 Request
   ↓
execFileSync("tokenless", ["compress"], { input: Request, timeout: 8s })
   ↓
Core 执行状态路由、内容域 Pipeline、TOON 与最终仲裁
   ↓
Plugin 只重建宿主允许替换的 Tool Result Slot
```

OpenClaw 的 PreTool 同样调用一次 `tokenless compress`，并把 Core 返回的
`output_optimization: "rtk"` 按 Session 与 Tool Call ID 存入进程内、消费一次的 Ledger。
匹配的 PostTool Request 携带该状态，Core 负责直接透传 RTK 输出。Ledger 按 24 小时 TTL、
1024 条上限和 Session 结束事件清理。

OpenClaw 不提供 Marker 授权恢复路径，因此需要恢复的 Lossy Candidate 以
`recoverability_unavailable` 透传。本地 `tokenless retrieve` 是受信运维入口，不能替代 Agent
授权。`tool_result_persist` 只同步改写 OpenClaw 自己持久化的 transcript；它不替换同一轮模型
已经收到的实时结果。Plugin 只处理 String、结构化 Slot 或单个 Text Block，并保留 Tool Result
Envelope；Media 和多个 Block 原样透传。

### 路径 2：共享 PostTool Hook（Protocol v2）

```
工具执行完成
   ↓
Adapter 触发 PostTool 事件，stdin 传入框架 Envelope
   ↓
Hook 转换为 `operation: "post_tool"` 的 v2 Request
   ↓
调用一次 `tokenless compress`
   ↓
Runtime 执行门禁、内容检测和静态派发
   ↓
JSON → `JsonCompressor`；Build Log → `BuildLogCompressor`；其他 → Passthrough
   ↓
Runtime 执行一次字符/Token 仲裁和一次 Stash Commit/Rollback
   ↓
Hook 校验版本与 Operation，并按宿主 Capability 应用 v2 Result
```

**流水线说明**：`PostToolPipeline` 位于 Runtime 内部。当前静态派发
`ContentType::Json -> JsonCompressor` 与 `ContentType::BuildLog -> BuildLogCompressor`。
JSON 清理、Record Reduction、截断、Structured Slot 恢复、Compact JSON 与可选 TOON 都在
同一次 JSON 领域调用内完成；Build Log 的 Terminal Cleanup、方言分类、Stack Trace 保护和
可恢复 Progress Reduction 都在同一次 Build Log 领域调用内完成。其他 ContentType 当前透传。
Claude Code 2.1.121 及以上版本、Qoder CLI、OpenCode 和 Cosh-NG 能替换实时结果；同时裸
`tokenless` 可从 Shell `PATH` 解析时，其 PostTool 请求才声明恢复可用。缩减或截断结果中的
Marker 会提示模型通过已有 Shell Tool 执行
`tokenless retrieve`。Common Hook 只把成功执行、参数为有效 Hash 或规范 Marker 的单条命令
识别为 Retrieve，其输出由 Core 原样旁路。旧 copilot-shell 及不能替换结果的宿主继续只接受
无损候选。BeforeModel Schema 仍使用独立的 Marker 授权恢复能力。实际 Pipeline、Stash 或 RTK
操作错误由 CLI 以退出码 1 返回，Hook 在进程边界上 fail-open。Common PreTool Hook 通过
Protocol v2 调用 Core，由 Core
执行 RTK；Adapter 按 Tool Call ID 暂存 `output_optimization: "rtk"`，并在对应 PostTool 调用中
消费该状态，使 RTK 输出绕过二次压缩。缺少稳定 Tool Call ID 时保持原命令不变；宿主未发送
PostTool 事件时遗留的状态会在后续 PreTool 调用中按 24 小时 TTL 和 1024 个文件上限清理。

**TOON 效果**：对结构化/表格数据可额外节省 30-60%，整体压缩效果 = 响应压缩节省 + TOON 语法消除。例如：原始 JSON 4480 字节，经响应压缩至 625 字节（~86%），再经 TOON 编码进一步缩减。实测表格数据（`[{"id":...}]`）可达到 44% 的 TOON 单独节省。

### 路径 3：Hermes Agent 插件（`transform_tool_result` hook）

```
pre_tool_call 将 Shell 参数发送给 Tokenless Core
   ↓
旧版兼容模式：阻止原调用并建议执行 Core 返回的 RTK 命令
   ↓
Hermes 执行重试命令并触发 transform_tool_result
   ↓
Adapter 映射 Status、Content Origin 和 RTK Wrapper 事实
   ↓
tokenless compress 执行 PostTool 路由与内容域 Pipeline
   ↓
Core 返回 Applied / Tool Error / Passthrough 等 Disposition
   ↓
Adapter 仅替换 Applied 输出或追加错误指引；其他结果原样透传
```

Hermes Adapter 不再持有 200 字符门禁、JSON 检测、截断阈值、TOON 选择或最终大小仲裁。
缩减结果中的 Marker 会提示 Hermes 通过已有 Shell Tool 执行 `tokenless retrieve`；Adapter
根据 `transform_tool_result` 收到的实际 `args.command` 识别成功的单条恢复命令，并让结果绕过
二次压缩。为兼容只支持 Block 的 Hermes 版本，Adapter 不要求 `pre_tool_call modify`。

### 路径 4：Qoder CLI 插件（`PostToolUse` hook）

Qoder 通过原生插件目录 `hooks/hooks.json` 加载 hook，并在运行时展开 `${QODER_PLUGIN_ROOT}`。插件内的 `hooks/run-hook.sh` 再从 ANOLISA adapter 目录定位共享的 `compress_response_hook.py`，无需改写 `~/.qoder/settings.json` 或将机器相关绝对路径写入插件缓存。

Qoder CLI 支持对任意工具使用 `hookSpecificOutput.updatedToolOutput`，因此压缩结果会**替换**原始工具输出，`additionalContext` 只携带环境错误归因等追加信息。结构化响应沿用 Claude Code 的 schema 保留逻辑；字符串响应可使用更小的 TOON 文本。Marker 中的 `tokenless retrieve` 命令可以通过已有 Shell Tool 执行，成功结果原样旁路。其他不支持输出替换的 agent 才使用 `additionalContext` 回退。

### 路径 5：Claude Code 插件（`PostToolUse` hook）

通过 `run-hook.sh` 调度器定位共享 hook 脚本，调用 `compress_response_hook.py`。Claude Code 复制插件到版本化缓存目录，因此 `run-hook.sh` 通过 FHS 路径查找共享 hook。

与其他 agent 不同，Claude Code 的 `additionalContext` 是**追加式**的（模型会同时看到原始工具结果和注入内容），因此压缩结果通过 `hookSpecificOutput.updatedToolOutput`（Claude Code >= 2.1.121）**替换**模型可见的工具结果，`additionalContext` 仅保留真正追加式的诊断信息（环境错误归因）。替换时会回填被压缩剥离的空 schema 字段（如 Bash 的 `stderr`/`interrupted`/`isImage`），保持内置工具输出结构不变；结构化响应不做 TOON 编码（TOON 为文本格式，会破坏 schema）。Marker 中的 `tokenless retrieve` 命令可以通过 Bash 执行，成功结果原样旁路。旧版本 Claude Code（< 2.1.121）或版本无法探测时 fail-open：直接透传原始结果，不注入重复内容。版本探测结果缓存于 `~/.tokenless/.claude-version`（0600 权限、拒绝符号链接，与其他 hook 状态文件一致），缓存键为 claude 二进制的路径+mtime+大小，升级 Claude Code 后自动失效重探，避免每次 PostToolUse 都启动 node CLI。

### 路径 6：Codex 插件（`PostToolUse` hook）

Codex 的 PostToolUse 不能替换或抑制原始输出。通过 `additionalContext` 追加压缩内容
会同时保留原文，增加模型首轮可见 Payload，因此 Codex Adapter 不运行响应压缩或
TOON。独立脚本 `response-diagnostics` 只在识别出环境失败时追加修复提示。支持的 Shell
除已交给 Build Log 领域的构建/测试命令外，受支持 Shell 命令由 PreToolUse Hook 通过 RTK
在执行前重写，工具从源头产生更小的输出。Codex 当前不能替换 PostTool 输出，因此其构建/测试
命令在这一阶段保持原始输出。

### 路径 7：DeepSeek Harness 插件（`tools/post-execute`）

DSH 把可替换的单文本结果交给 PostTool Core。普通成功结果声明恢复可用；Marker 中的
`tokenless retrieve` 由已有 Shell Tool 执行后，Adapter 从实际的
`exec.arguments.command` 识别成功的单条恢复命令，并把输出标记为 Retrieve Result，确保
恢复内容不会再次压缩。只有裸 `tokenless` 能从 Shell `PATH` 解析到 Adapter 为 Core 选中的
同一个可执行文件时才声明恢复可用。DSH 会
清除模型 Shell 继承的 `TOKENLESS_*` 环境变量，因此 Adapter 通过 `shellEnv` 发布受控的状态
目录及文件级数据库覆盖，并让 Core 使用相同路径；默认位置为会话工作区下的 `.tokenless`，
且用自忽略 `.gitignore` 防止数据库被暂存。启动 DSH 前设置、且 Shell 沙箱可以访问的绝对
`TOKENLESS_DATA_DIR`、`TOKENLESS_STATS_DB` 或 `TOKENLESS_STASH_DB` 可以覆盖默认路径。
错误、Interrupted、Denied 和不符合严格语法的命令不会伪装成 Retrieve。

### 路径 8：CLI 直接使用

```bash
# 从文件
tokenless compress-response -f response.json

# 从 stdin
cat response.json | tokenless compress-response

# 管道组合
curl -s https://api.example.com/data | tokenless compress-response
```

## 五、压缩前后示例

### 示例 1 — 字段删除 + null 移除 + 空值移除（R4 + R5 + R6）

输入：
```json
{
  "status": "success",
  "data": { "name": "test", "count": 42 },
  "debug": { "request_id": "abc123", "timing": 0.05 },
  "trace": "GET /api/data 200 OK",
  "metadata": null,
  "tags": [],
  "extra": ""
}
```

输出：
```json
{
  "status": "success",
  "data": { "name": "test", "count": 42 }
}
```

被删除的内容：`debug`（R4 黑名单）、`trace`（R4 黑名单）、`metadata`（R5 null）、`tags`（R6 空数组）、`extra`（R6 空字符串）。

### 示例 2 — 字符串截断（R1）

输入（`truncate_strings_at = 20` 为例）：
```json
"This is a very long string that should be truncated"
```

输出：
```json
"This is a very long … (truncated)"
```

默认阈值 4096 个 Unicode 字符，不会截断在多字节 UTF-8 字符中间。

### 示例 3 — 普通数组截断（R3）

输入（`truncate_arrays_at = 3`、`array_tail_preserve = 0` 为例）：
```json
[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
```

输出：
```json
[1, 2, 3, "<... 7 more items truncated, not stashed>"]
```

默认阈值 32 个元素。默认同时保留末尾 8 个元素（`array_tail_preserve = 8`）：
截断时保留 head + tail，两者之间插入截断标记，中间被丢弃；若 head+tail
能覆盖整个数组，则不截断、不加标记。上例在默认配置下（3 + 8 ≥ 10）会
原样保留全部 10 个元素。

### 示例 4 — 深度截断（R7）

输入（`max_depth = 2` 为例）：
```json
{
  "level1": {
    "level2": {
      "level3": {
        "level4": "deep value"
      }
    }
  }
}
```

输出：
```json
{
  "level1": {
    "level2": {
      "level3": "<object truncated at depth 3>"
    }
  }
}
```

默认阈值 8 层。

### 示例 5 — 递归组合压缩（R1 + R4 + R5 同时生效）

输入（`truncate_strings_at = 10` 为例）：
```json
{
  "outer": {
    "inner": {
      "long_text": "This is a very long text that should be truncated",
      "null_field": null,
      "number": 42
    }
  }
}
```

输出：
```json
{
  "outer": {
    "inner": {
      "long_text": "This is a … (truncated)",
      "number": 42
    }
  }
}
```

### 示例 6 — 小型数组内对象的复合压缩（R3 + R4 + R5）

输入（`truncate_arrays_at = 2`、`array_tail_preserve = 0` 为例）：
```json
[
  {"id": 1, "debug": "remove me", "value": null},
  {"id": 2},
  {"id": 3},
  {"id": 4}
]
```

输出：
```json
[
  {"id": 1},
  {"id": 2},
  "<... 2 more items truncated, not stashed>"
]
```

第一个对象的 `debug`（R4）和 `value: null`（R5）被移除，数组在第 2 个元素后截断（R3）。少于 33 项，因此该数组不是 Record Array。

### 示例 7 — Record Reduction（R2）

一个包含 64 个同 Shape Object 的数组，在可恢复路径中最多保留 32 条普通记录；如果第 31 条
含有 `status: "failed"` 或超过 `2σ` 的数值，它即使位于集合中间也会被保留。数组末尾会追加：

```text
32 of 64 records omitted. If needed, run in shell: tokenless retrieve HASH
```

对应 Stash Payload 是缩减前 64 条记录组成的完整 Compact JSON 数组。

`HASH` 代表输出中的完整 24 位 Hash。AgentScope 使用实际静态 Tool 名称，例如
`If needed, call tool tokenless_retrieve with hash_or_marker=HASH`，不要求 Shell。
Schema、字符串、深度、普通数组和 BuildLog 的省略提示使用同一恢复指令格式；只有省略说明
不同。历史 `<<tokenless:HASH>>` 仍可读取，但不再生成。只有完整恢复指令或历史 Marker 能进入
可见性授权集合，孤立 Hash 不能授权。恢复仅在需要时执行，额外开销应独立计入。

## 六、默认配置汇总

| `JsonCompressionConfig` 字段 | 默认值 | 含义 |
|------|-------|-------------|
| `truncate_strings_at` | 4096 | 最大 Unicode 字符数 |
| `truncate_arrays_at` | 32 | 保留的 head 元素数 |
| `array_tail_preserve` | 8 | 保留的 tail 元素数；0 表示仅保留 head |
| `drop_nulls` | true | 移除 null 值 |
| `drop_empty_fields` | true | 移除空字符串、数组和对象 |
| `max_depth` | 8 | 最大递归深度 |
| `add_truncation_marker` | true | 为截断内容生成有界标记 |

## 七、Fail-Open 设计

所有集成路径均采用 fail-open 策略：

- **OpenClaw 插件**：Protocol v2 进程失败、非法响应或不能安全重建 Slot 时，Hook 不返回值 → 原始结果透传
- **copilot-shell hook**：任何失败点（依赖缺失、压缩失败、输出为空）均 `exit 0` 且不输出 stdout → 原始结果透传
- **CLI**：错误输出到 stderr，调用方可检查退出码决定是否回退

## 八、关键文件路径

| 用途 | 文件路径 |
|------|--------|
| JSON 领域压缩器（JsonCompressor） | `crates/tokenless-compressors/src/json.rs` |
| Build Log 领域压缩器（BuildLogCompressor） | `crates/tokenless-compressors/src/build_log.rs` |
| PostTool Pipeline 与最终仲裁 | `crates/tokenless-runtime/src/post_tool/` |
| Schema 压缩器（SchemaCompressor） | `crates/tokenless-schema/src/schema_compressor.rs` |
| 内容压缩公开 API | `crates/tokenless-compressors/src/lib.rs` |
| CLI 子命令 | `crates/tokenless-cli/src/main.rs` |
| 环境检查 | `crates/tokenless-cli/src/env_check.rs` |
| 统计记录器（SQLite WAL） | `crates/tokenless-stats/src/recorder.rs` |
| 统计记录类型及操作枚举 | `crates/tokenless-stats/src/record.rs` |
| OpenClaw 插件 | `adapters/tokenless/openclaw/index.ts` |
| OpenClaw 插件配置 | `adapters/tokenless/openclaw/openclaw.plugin.json` |
| copilot-shell hook（响应+TOON 流水线） | `adapters/tokenless/common/hooks/compress_response_hook.py` |
| Hermes 插件 | `adapters/tokenless/hermes/__init__.py` |
| Qoder 插件配置 | `adapters/tokenless/qoder/hooks/hooks.json` |
| Claude Code 插件 | `adapters/tokenless/claude-code/hooks/run-hook.sh` |
| Codex 响应诊断 Hook | `adapters/tokenless/codex/scripts/response-diagnostics` |
| TOON 编解码器（crates.io toon-format） | `toon-format` crate v0.4.6 |
| JSON 压缩测试 | `crates/tokenless-compressors/src/tests/json_tests.rs` |
| Build Log 压缩测试与 Fixture | `crates/tokenless-compressors/src/tests/build_log_tests.rs`、`crates/tokenless-compressors/tests/fixtures/build_logs/` |
| PostTool 集成测试 | `crates/tokenless-runtime/src/entry.rs` |
| TOON E2E 测试 | `tests/test-toon-full.sh` |
| 全量测试套件 | `tests/run-all-tests.sh` |

## 九、TOON 压缩与统计验证

### 9.1 TOON 压缩 CLI

短于 500 字符的负载默认原样透传（与 Hook 层阈值一致）；示例中使用
`--min-toon-chars 0` 对短负载强制编码。透传和无收益场景会在 stdout 上
逐字节原样复现输入（不添加、不去除末尾换行符），脚本可直接比较
stdout 与输入来判断是否发生了编码。

```bash
# TOON 编码（JSON → 紧凑二进制文本格式）
echo '{"users":[{"id":1,"name":"Alice"}]}' | tokenless compress-toon --min-toon-chars 0

# TOON 解码（往返验证）
echo '{"name":"test","value":42}' | tokenless compress-toon --min-toon-chars 0 | tokenless decompress-toon

# 附带统计追踪（自动记录到 SQLite 数据库）
tokenless compress-toon -f data.json --agent-id my-agent --session-id sess-001
```

`tests/test-toon-full.sh` 把上面这套契约做成手动 E2E 校验（不是 make 目标）。它驱动
PATH 上的 `tokenless`，因此要求该二进制与本 checkout 同版本（`TOKENLESS_ALLOW_VERSION_SKEW=1`
可放行已安装的旧版本）。场景 1/2 只需要仓库树；场景 3 需要一个启用了 tokenless 插件的
OpenClaw，且必须显式设置 `TOKENLESS_TOON_FULL_LIVE=1` 才会真实调用模型。可选前置条件
缺失记为 SKIP，不计入失败。

```bash
PATH="src/tokenless/target/debug:$PATH" bash src/tokenless/tests/test-toon-full.sh
```

### 9.2 通过统计数据库验证压缩效果

Tokenless 自动将每次压缩操作记录到 `~/.tokenless/stats.db`（SQLite WAL 模式）。四种操作类型均被追踪：`compress-schema`、`compress-response`、`rewrite-command`、`compress-toon`。

```bash
# 查看统计状态
tokenless stats status

# 列出最近 20 条记录
tokenless stats list

# 查看某条记录的压缩前后文本对比
tokenless stats show <id>

# 查看汇总统计（按操作类型分组）
tokenless stats summary
```

统计启用条件：`TOKENLESS_STATS_ENABLED` 环境变量未设为 `0`/`false`，或通过 `tokenless stats enable` 启用。

> **SLS 日志记录（JSONL）**：除 SQLite 统计外，tokenless 默认还会将每次压缩以 SLS JSONL 记录写入 `/var/log/anolisa/sls/ops/tokenless.jsonl`（默认开启）。该文件由 **anolisa SLS 组件统一管理**，tokenless 不创建/删除，仅在文件存在时追加，不存在则跳过。开关字段 `~/.tokenless/config.json` 的 `sls_enabled`（默认 `true`），环境变量 `TOKENLESS_SLS_ENABLED` 优先；输出路径可用 `TOKENLESS_SLS_PATH` 覆盖（须位于 `/var/log/` 或 `/tmp/` 下）。仅记录度量，不含原文/敏感数据。详见 [Tokenless 效果度量 · SLS JSONL](../../../docs/user-guide/zh/token-saving/tokenless/measuring-savings.md#sls-jsonl)。

### 9.3 压缩效果说明

| 数据类型 | 响应压缩 | 响应压缩+TOON | 说明 |
|---------|---------|--------------|------|
| 仓库 JSON 参考 Fixture | 36.3%（无损） | 50.6% | 无损清理超过 15% 门槛，因此保留全部 Record |
| 可恢复的 Record Array | 取决于记录数和重复度 | 取决于所选表示 | 未达到无损门槛时才缩减；关键记录可突破 32 条基础预算 |
| 简单扁平对象 | 可能无收益 | 取决于字段和值长度 | 两种表示都没有同时减少字符和估算 Token 时透传 |

Schema 压缩不经过本表的响应压缩或 TOON 流程。当前仓库参考 fixture 上的独立 Schema
压缩结果为 47.3%；该数字不是生产范围或任意 Schema
的保证值，实际结果取决于输入结构、description 长度和可移除字段。

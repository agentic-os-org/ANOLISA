# Response 压缩功能说明

## 一、功能概述

PostTool 压缩由 Runtime 内部的 `PostToolPipeline` 编排，并按内容类型与内容来源静态派发给六个领域之一：JSON、Build Log、Tabular、Search Results 路径共享与 HTML 页面转写默认启用；Diff 上下文裁剪需要通过 `TOKENLESS_DIFF_COMPRESSION_ENABLED` 显式开启。JSON 压缩器只解析一次输入，先生成保留全部字段、空值和数组元素的 Compact JSON/TOON 候选；该候选同时减少字符与估算 Token 且 Token 节省率达到 15% 时直接采用。否则再生成包含 Record Reduction 或既有截断规则的 Bounded 候选，并选择更小的合法表示。Build Log 压缩器只处理成功的 `command_output`，先清理终端控制输出，再对已识别构建与测试日志中的重复常规进度进行可恢复缩减。Tabular 压缩器生成保留单元格或可恢复选取整行的 CSV/TSV 视图；Search Results 压缩器在连续行之间共享重复路径，不丢弃任何命中；Diff 裁剪只删减未修改的上下文行，元信息与改动行原样保留；HTML 转写选取 `<main>`、`role=main` 或唯一的最外层 `<article>` 作为主内容根（都不存在时为 `<body>`），只渲染该根并移除其中可枚举的非内容元素，根外节点计数后省略。这两个领域都把原文写入 Stash，只有恢复路径可用时才会执行，结果可通过 Retrieve 找回。Tool Error 保留原始输出并只追加环境诊断；`file_content` 来源与其他内容类型透传。

## 二、压缩规则与保留约定

| # | 规则 | 判断条件 | 处理方式 | 默认阈值 |
|---|------|---------|---------|---------|
| R1 | **字符串截断** | Unicode 字符数 > 4096 | 在 Unicode 字符边界截断，追加截断标记 | 4096 字符 |
| R2 | **Record Reduction** | 数组至少有 33 项，且每项都是 JSON Object | 保留首尾、错误、结构异常和数值异常记录；普通记录去重后稳定等距采样；完整数组写入 Stash | 32 条普通预算；关键记录可突破预算 |
| R3 | **普通数组截断** | 非 Record Array 的元素数超过配置预算 | 保留前 32 个 + 末尾 8 个（`array_tail_preserve`），head 与 tail 之间插入截断标记；head+tail 覆盖全部元素时不截断 | 32 + 8 个 |
| R7 | **深度截断** | 嵌套深度 > 8 | 替换为 `<{type} truncated at depth {N}>` | 8 层 |
| R8 | **原始类型保留** | null / bool / number | 直接保留，不做处理 | — |

**保留约定**：所有字段名均按普通业务字段处理，包括 `debug`、`trace`、`traces`、`stack`、`stacktrace`、`logs`、`logging`。`null`、`""`、`[]`、`{}` 在对象和数组中均保留；数组空项保留原来的位置与顺序。只有下述显式 Record Reduction 和截断规则会省略内容，Compact JSON 与通过校验的 TOON 仅改变表示。规则编号沿用原编号，R4–R6 删除规则已移除。

**R8 数值字面量保真**：JSON 领域按扫描到的源字面量保留每个数字，Record Reduction、
深度截断、Compact 序列化与 Stash Payload 都不会重新序列化它。因此 `serde_json::Value`
无法精确表示的数字（`18446744073709551617`、`-9223372036854775809`、
`1.234567890123456789`、`1e-400`）同样参与压缩，输出保持原拼写与 JSON 数字类型，不会变成
字符串或四舍五入近似值。等价拼写不做归一化：`1.2500` 仍写作 `1.2500`，`-0` 保留负号。
Record 去重按数值而非拼写比较，`1.2500` 与 `1.25` 视为同一条记录，被保留的那条输出自己的
拼写；数值离群点只使用 `f64` 近似值排序，近似值不进入输出，没有有限近似值的数字所在的
记录直接保留。TOON 候选必须解码回与原文档一致的结构、字符串、数值类型和精确十进制值才会
被采用，允许等价拼写归一化（`1.2500` 可写作 `1.25`），不接受精度损失、整数与浮点类型互换
或负零丢符号；校验失败时保留已压缩的精确 JSON 候选。`serde_json` 无法读回的数字（例如超出
有限 `f64` 范围的 `1e400`）沿用既有失败保护：整个响应原样透传，不写入 Stash。

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
 │   │   └─ 追加截断标记
 │   └─ object → compress_object()
 │       └─ 保留所有字段，逐值递归 compress_value(val, depth+1)
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

### 路径 1：OpenClaw 插件（实时工具结果 middleware）

```
工具执行完成
   ↓
OpenClaw 在模型消费前调用已声明的 agentToolResultMiddleware
   ↓
Plugin 按 Tool Call ID 消费 PreTool 输出优化状态
   ↓
转换为 `operation: "post_tool"` 的 Protocol v2 Request
   ↓
execFileSync("tokenless", ["compress"], { input: Request, timeout: 8s })
   ↓
Core 执行状态路由、内容域 Pipeline、TOON 与最终仲裁
   ↓
Plugin 通过 { result } 替换单文本 Tool Result，保留其余元数据
```

OpenClaw 的 PreTool 同样调用一次 `tokenless compress`，并把 Core 返回的
`output_optimization: "rtk"` 按 Session 与 Tool Call ID 存入进程内、消费一次的 Ledger。
匹配的 PostTool Request 携带该状态，Core 负责直接透传 RTK 输出。Ledger 按 24 小时 TTL、
1024 条上限和 Session 结束事件清理。

OpenClaw 不提供 Marker 授权恢复路径，因此需要恢复的 Lossy Candidate 以
`recoverability_unavailable` 透传。本地 `tokenless retrieve` 是受信运维入口，不能替代 Agent
授权。插件要求 OpenClaw 2026.6.6 或更高版本，Manifest 必须声明
`contracts.agentToolResultMiddleware: ["openclaw"]`，且插件须显式启用。
`registerAgentToolResultMiddleware` 返回的结果直接进入同轮模型请求，并由宿主持久化；
插件不再注册 `tool_result_persist`，避免重复压缩与统计。仅单个 Text Block 可以替换，
其余 Tool Result 元数据保留；结构化、Media 和多个 Block 原样透传。
安装器拒绝旧版和无法识别的版本。运行时 API 缺失或注册失败时，PostTool 明确关闭，
PreTool 可以继续；Core 失败、超时或响应非法时，Adapter 不返回替换内容，保留宿主
交付的结果。2026.8.2 等旧版宿主会在 middleware 回调前裁剪到 100,000 个 UTF-16
Code Unit；Fail-open 无法恢复被宿主丢弃的内容或元数据。它们仍在此限制内受支持，
建议使用包含上游提交 `2f620b970a04` 的构建，详见
[OpenClaw 兼容说明](../../../docs/user-guide/zh/tokenless/framework-integration.md#openclaw)。

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
JSON → `JsonCompressor`；Build Log → `BuildLogCompressor`；Tabular → `TabularCompressor`；
Search Results → `SearchResultsCompressor`；Diff → Runtime Diff 裁剪（需开启）；
HTML → `HtmlExtractor`（默认启用，可关闭）；其他 → Passthrough
   ↓
Runtime 执行一次字符/Token 仲裁和一次 Stash Commit/Rollback
   ↓
Hook 校验版本与 Operation，并按宿主 Capability 应用 v2 Result
```

**流水线说明**：`PostToolPipeline` 位于 Runtime 内部。工具错误、`file_content` 来源和
低于 `min_input_chars` 的输入在入口直接透传。来源由 adapter 按工具分类，shell 工具随请求附带
`command` 字段；Core（`post_tool/file_read.rs`）把只打印本地文件的命令（`cat`、`head`、`tail`、
`nl`、`less`、`more`、`bat`、打印范围的 `sed -n`，可带 `cd … &&` 前缀，不含管道、重定向或其他命令）
改判为 `file_read`：其中的数据照常压缩，打印出的 HTML 页面视为可编辑源码而保持原样，
且跳过新增的纯路径列表策略。`Grep` 工具输出只允许进入无损的 Search Results 路径共享。
纯路径列表按内容识别，允许非文件来源的命令或 API 结果，不依赖 `find` 或特定工具名。
通过入口后按下表静态派发，每个领域各调用一次：

| ContentType | 前提 | 领域处理 | 默认 |
|------|------|------|------|
| `json`（含宿主强制 JSON 与包裹的结构化 JSON） | 无 | `JsonCompressor`：Record Reduction、截断、Compact JSON 与可选 TOON | 启用 |
| `build_log` | 来源为 `command_output` 或 `file_read` | `BuildLogCompressor`：Terminal Cleanup、方言分类、Stack Trace 保护、可恢复 Progress Reduction | 启用 |
| `tabular` | 宿主支持文本替换 | `TabularCompressor`：保留全部单元格的 CSV/TSV 压紧，无收益时原样返回，不写 Stash | 启用 |
| `search_results` | 宿主支持文本替换；带行号记录要求 `api_response`，纯路径列表允许 `api_response` 或 `command_output` | `SearchResultsCompressor`：连续记录共享完整路径，或全部路径共享目录前缀，无损且无需 Stash | 启用，`TOKENLESS_SEARCH_PATH_SHARING_ENABLED=false` 关闭 |
| `diff` | 来源为 `command_output` 或 `file_read`，宿主支持文本替换，Stash 与恢复路径可用 | Runtime `post_tool/diff.rs`：按代价裁剪未修改上下文，元信息与改动行原样保留 | 关闭，`TOKENLESS_DIFF_COMPRESSION_ENABLED=true` 开启 |
| `html` | 来源不是 `file_read`，宿主支持文本替换，Stash 与恢复路径可用 | `HtmlExtractor`：只渲染主内容根（`<main>`、`role=main`、唯一的最外层 `<article>` 或 body），移除可枚举非内容元素（含表单控件、媒体嵌入、dialog）后转写为 Markdown，根外节点计数省略，嵌套超过 512 层不解析 | 启用，`TOKENLESS_HTML_EXTRACTION_ENABLED=false` 关闭 |

Diff 与 HTML 领域要求至少节省 16 个 Token 才采用结果，其他领域至少 1 个。Diff 与 HTML
没有可用 Stash 或宿主未声明恢复能力（例如裸 `tokenless` 不在 Shell `PATH` 中）时，即使已开启
环境变量也原文透传。
纯路径列表至少包含三条 POSIX 路径，统一使用 LF 或 CRLF。固定英文说明后接紧凑 JSON，
字段为 `base`、`paths`、`line_ending`、`trailing_newline`；将 `base` 与每个解码后的路径项
拼接，按 `line_ending` 连接，仅当 `trailing_newline` 为 true 时追加该行尾，即可逐字节还原。
全部路径、顺序、重复项均保留。空行、混合输出、NUL 分隔或歧义格式透传；完整视图的字节数
和估算 Token 均减少才采用。检测器只采样完整行，压缩器仍验证全部输入。

PreTool 让包含已识别原生 `find` 调用的整条命令跳过 RTK，包括复合命令及 RTK 内置透明前缀：
`env`、`command`、`exec`、`noglob`、`nocorrect`、`builtin`、`timeout`、`time`、`nice`、`nohup`
和 `uv run`；支持嵌套及反斜杠换行续行，进程 Wrapper 的未知参数会停止识别。
不读取自定义 `hooks.transparent_prefixes`，这些前缀中的 `find` 仍可能被 RTK 改写。
关闭搜索路径共享只停止后置压缩，仍保留原生 `find` 的完整输出；混合或不支持的输出因此可能不压缩。
其余 ContentType（如 `stack_trace`、`plain_text`）透传。
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

以下 JSON 以便于阅读的格式展示。截断示例说明未配置 Stash 时的 Bounded 候选；
最终是否采用还取决于完整候选的收益和调用路径的恢复要求。

### 示例 1 — 格式压紧，保留全部字段与空值

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
{"status":"success","data":{"name":"test","count":42},"debug":{"request_id":"abc123","timing":0.05},"trace":"GET /api/data 200 OK","metadata":null,"tags":[],"extra":""}
```

`debug`、`trace`、`metadata`、`tags`、`extra` 均保留；省去的是 JSON 的缩进和分隔空白。

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

### 示例 5 — 递归字符串截断，保留相邻空值（R1）

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
      "long_text": "This is a ",
      "null_field": null,
      "number": 42
    }
  }
}
```

### 示例 6 — 小型数组保留全部字段（R3）

输入（`truncate_arrays_at = 2`、`array_tail_preserve = 0` 为例）：
```json
[
  {"id": 1, "debug": "remove me", "value": null},
  {"id": 2},
  {"id": 3},
  {"id": 4}
]
```

未配置 Stash 时生成的 Bounded 候选为：
```json
[
  {"id": 1, "debug": "remove me", "value": null},
  {"id": 2},
  "<... 2 more items truncated, not stashed>"
]
```

首个对象的 `debug` 和 `value: null` 保留。该候选比保留全部四项的 Compact JSON 更长，因此最终选择完整的 Compact JSON。少于 33 项，该数组不使用 Record Reduction。

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
| `max_depth` | 8 | 最大递归深度 |
| `add_truncation_marker` | true | 为截断内容生成有界标记 |

Rust 调用方应移除 `JsonCompressionConfig.drop_nulls`、`drop_empty_fields` 和
`JsonCompressionContext.preserve_top_level_shape` 的初始化项；这些字段已删除。
字段与空值保留适用于所有输出槽位，不再需要顶层恢复步骤。`json_cleanup` 操作标识继续保留，
表示紧凑序列化，也包括对封装对象或数组的 JSON 字符串信封解包一次。解包会改变顶层
类型，但保留内部 Payload 的值；该操作不代表删除字段。

## 七、Fail-Open 设计

所有集成路径均采用 fail-open 策略：

- **OpenClaw 插件**：Protocol v2 进程失败、非法响应或不能安全重建 Slot 时，Hook 不返回值 → 宿主交付的结果透传（不恢复宿主此前的截断）
- **copilot-shell hook**：任何失败点（依赖缺失、压缩失败、输出为空）均 `exit 0` 且不输出 stdout → 原始结果透传
- **CLI**：错误输出到 stderr，调用方可检查退出码决定是否回退

## 八、关键文件路径

| 用途 | 文件路径 |
|------|--------|
| JSON 领域压缩器（JsonCompressor） | `crates/tokenless-compressors/src/json.rs` |
| Build Log 领域压缩器（BuildLogCompressor） | `crates/tokenless-compressors/src/build_log.rs` |
| Tabular 领域压缩器（TabularCompressor） | `crates/tokenless-compressors/src/tabular.rs` |
| Search Results 领域压缩器（SearchResultsCompressor） | `crates/tokenless-compressors/src/search_results.rs` |
| HTML 领域转写器（HtmlExtractor） | `crates/tokenless-compressors/src/html.rs` |
| Diff 上下文裁剪 | `crates/tokenless-runtime/src/post_tool/diff.rs` |
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
OpenClaw、GNU `timeout`（coreutils，用于给每次模型调用限时），且必须显式设置
`TOKENLESS_TOON_FULL_LIVE=1` 才会真实调用模型。OpenClaw 的状态目录按
`OPENCLAW_STATE_DIR` → `OPENCLAW_HOME` → `~/.openclaw` 解析（与
`adapters/tokenless/openclaw/scripts/` 下的 install/detect 脚本一致），CLI 路径可用
`OPENCLAW_BIN` 覆盖。可选前置条件缺失记为 SKIP，不计入失败；但一旦设置了
`TOKENLESS_TOON_FULL_LIVE=1`，场景 3 的前置条件缺失会直接报错退出，不会以"全部跳过"
的姿态给出绿色结果。

`timeout` 约束的是**每一次** OpenClaw 调用，只读探测（`openclaw plugins list`）也不例外：
缺少 `timeout` 时脚本根本不会发起该调用，探测退化为只查磁盘上的
`<state-dir>/extensions/tokenless` 并在结论里注明"未查询 plugins list"；显式 live 运行则在
探测之前就报错退出，不会卡在探测里。live 模式下 session 查询失败、超时或返回不可解析的内容
一律计为 FAIL，只有"查询成功但没有可复用 session"才保留 SKIP——否则一次什么也没验证的 live
运行会显示 0 failures 并 exit 0。

场景 3 还有一条**既存限制**（早于本次改动，merge base 就存在）：3.1 断言的
`rtk-rewrite` / `schema-compression` / `response-compression` / `toon-compression`
并不是插件当前的输出，`adapters/tokenless/openclaw/index.ts` 打印的是 `pre-tool` /
`post-tool`，且仅在 verbose 开启时打印。重新推导这些断言需要一台启用了 tokenless 插件的
真实 OpenClaw 主机，因此暂按原样保留；脚本中对应位置已就地标注 KNOWN STALE。

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

> **SLS 日志记录（JSONL）**：除 SQLite 统计外，tokenless 默认还会将每次压缩以 SLS JSONL 记录写入 `/var/log/anolisa/sls/ops/tokenless.jsonl`（默认开启）。该文件由 **anolisa SLS 组件统一管理**，tokenless 不创建/删除，仅在文件存在时追加，不存在则跳过。开关字段 `~/.tokenless/config.json` 的 `sls_enabled`（默认 `true`），环境变量 `TOKENLESS_SLS_ENABLED` 优先；输出路径可用 `TOKENLESS_SLS_PATH` 覆盖（须位于 `/var/log/` 或 `/tmp/` 下）。仅记录度量，不含原文/敏感数据。详见 [Tokenless 效果度量 · SLS JSONL](../../../docs/user-guide/zh/tokenless/measuring-savings.md#sls-jsonl)。

### 9.3 压缩效果说明

| 数据类型 | 响应压缩 | 响应压缩+TOON | 说明 |
|---------|---------|--------------|------|
| 完整 JSON | 取决于格式空白 | 取决于所选表示 | 保留全部字段和空值；完整候选节省达到 15% 时直接采用 |
| 可恢复的 Record Array | 取决于记录数和重复度 | 取决于所选表示 | 未达到无损门槛时才缩减；关键记录可突破 32 条基础预算 |
| 简单扁平对象 | 可能无收益 | 取决于字段和值长度 | 两种表示都没有同时减少字符和估算 Token 时透传 |

Schema 压缩不经过本表的响应压缩或 TOON 流程。当前仓库参考 fixture 上的独立 Schema
压缩结果为 47.3%；该数字不是生产范围或任意 Schema
的保证值，实际结果取决于输入结构、description 长度和可移除字段。

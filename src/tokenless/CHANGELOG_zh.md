# 更新日志

[English](CHANGELOG.md)

Tokenless 的所有重要变更都会记录在此文件中。

从 0.7.2 版本开始，发布记录遵循
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式。

## [未发布]

## [0.8.4] - 2026-09-22

### 新增

- 在 npm 安装之外新增独立安装脚本及 `install-tokenless` OS Skill 入口。安装脚本在开始安装前选择 npm 或仅限 Linux 的源码构建，记录安装路径归属以供卸载，并在替换失败时恢复原有安装。源码构建仅安装 CLI；npm 还提供 RTK 和适配器（[#2322](https://github.com/alibaba/anolisa/pull/2322)）。

### 变更

- npm 安装现在要求 Node.js 16.7 或更新版本，并保留属于其他安装的共享适配器资源。用户可通过 `ANOLISA_TOKENLESS_FORCE_ADAPTERS=1` 显式替换；否则安装程序会报告包内适配器的位置（[#2322](https://github.com/alibaba/anolisa/pull/2322)）。
- 独立卸载仅移除安装记录中仍归本次安装所有的资源，除非指定 `--purge`，否则保留运行数据；无法确认框架注册已移除时保留适配器资源。Codex 插件显示为 `not installed` 时视为已成功移除，使重复卸载可以正常完成（[#2322](https://github.com/alibaba/anolisa/pull/2322)、[#3412](https://github.com/alibaba/anolisa/pull/3412)）。

### 修复

- HTML 提取现在将页面 `</html>` 结束标签后的命令输出（如构建日志和 HTTP 状态码）保留在渲染后的页面之后，避免内容丢失。模仿页面边界标记的文本会被转义，并继续支持恢复原文（[#3386](https://github.com/alibaba/anolisa/pull/3386)）。
- HTML 提取不再将包含大量自闭合 SVG 元素的浅层页面误判为嵌套过深。深度限制现在依据解析后的页面结构，异常标记和宽表格也不会再触发可能拖慢工具结果返回的重复扫描（[#3396](https://github.com/alibaba/anolisa/pull/3396)）。

## [0.8.3] - 2026-09-20

### 新增

- Git diff 上下文裁剪可在保留全部变更行和文件元数据的同时缩减周边上下文，并支持恢复收到的原始内容。通过 `TOKENLESS_DIFF_COMPRESSION_ENABLED=1` 或 SDK `diff_compression_enabled=True` 启用；默认关闭，要求宿主支持文本替换和原文恢复（[#3299](https://github.com/alibaba/anolisa/pull/3299)）。
- 命令或 API 返回的完整 HTML 页面可渲染为 Markdown，提供原文恢复及被移除页面元素的计数。通过 `TOKENLESS_HTML_EXTRACTION_ENABLED=1` 或 SDK `html_extraction_enabled=True` 启用；默认关闭，要求宿主支持文本替换和原文恢复，文件读取保持原样，包括通过 Shell 文件读取命令输出的 HTML（[#3306](https://github.com/alibaba/anolisa/pull/3306)）。

### 变更

- npm 安装在 Claude CLI 可用时自动启用 Claude Code 适配器，注册未完成时给出重试指引（[#2193](https://github.com/alibaba/anolisa/pull/2193)）。
- QwenPaw 安装在交付插件包前检查对应 SDK wheel，确认资源缺失时报告版本和 URL。离线或镜像环境可通过 `ANOLISA_SKIP_WHEEL_PREFLIGHT=1` 跳过探测（[#3289](https://github.com/alibaba/anolisa/pull/3289)）。

### 修复

- Claude Code 检测在有界等待窗口内退避重试已部署插件，减少宿主注册表尚未更新时误报“未安装”的情况（[#3272](https://github.com/alibaba/anolisa/pull/3272)）。
- Raw 包现在声明并包含 OpenCode 适配器和适配器清单，使 ANOLISA 能发现并管理随包提供的插件。启用或禁用后需重启 OpenCode（[#3324](https://github.com/alibaba/anolisa/pull/3324)、[#3346](https://github.com/alibaba/anolisa/pull/3346)）。

## [0.8.2] - 2026-09-15

### 新增

- 支持的搜索结果列表（包括不带上下文行的 Claude Code 原生 `Grep` 结果）现在会共享连续重复的文件路径，同时保留每条已接收的匹配、行号和行尾。这项无损优化默认开启，要求宿主支持文本替换；可通过 `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0` 或 SDK `search_path_sharing_enabled=False` 关闭（[#3173](https://github.com/alibaba/anolisa/pull/3173)）。

### 变更

- 内置 RTK 升级至 0.49.0，采用保守的管道改写规则，并保持 `sudo` 命令不变。直接使用 `rtk grep` 时，须改用 `--max-len` 和 `--max` 设置 RTK 显示限制：`-l` 和 `-m` 现在保留原生 grep 含义，文件类型过滤改用 `rtk rg -t`（[#3273](https://github.com/alibaba/anolisa/pull/3273)）。
- 支持的 RTK 过滤器现在会通过 `rtk recall HASH` 提示恢复已保留的失败或截断输出。恢复存储以宿主 OS 用户为范围，受容量和过期限制；共用该用户的会话可以访问同一存储。RTK recall 与 Tokenless Stash 检索仍相互独立（[#3273](https://github.com/alibaba/anolisa/pull/3273)）。

### 修复

- Cosh-NG 和 copilot-shell 在 Protocol v2 下现在能正确归因 JSON 编码的 shell 结果中的失败，保留原始失败输出，并将错误详情送交诊断（[#2238](https://github.com/alibaba/anolisa/pull/2238)）。
- 内置 RTK 保留 pytest 启动和回退诊断，即使关闭恢复存储也不会丢失这些信息，并在 stderr 旁保留未发现测试的摘要（[#3273](https://github.com/alibaba/anolisa/pull/3273)）。

## [0.8.1] - 2026-09-09

### 新增

- CSV/TSV 工具输出现在可以通过精简引号和行结束符保留全部单元格；具备恢复能力时，较大的表格可以保留首尾行、诊断行及代表性采样。缩减视图会标明省略情况，并提供逐字节恢复原文的入口；完整枚举或计算必须使用原始表格。文件读取和缺少文本替换能力的宿主保持原样 ([#3089](https://github.com/alibaba/anolisa/pull/3089))。
- SLS 记录现在可以从 `TOKENLESS_TRACEPARENT` 或 `TRACEPARENT` 携带 `tokenless.trace_id` 和 `tokenless.span_id`，让可观测后端将 Token 节省关联到宿主 trace。上下文必须由启动 Tokenless 的宿主或适配器注入；没有有效值时记录保持未关联状态，本地 `stats.db` 数据不变 ([#3094](https://github.com/alibaba/anolisa/pull/3094))。

### 修复

- Claude Code 检测现在会在首次成功查询注册表却未列出已准备好的插件时进行短暂重试，避免刚安装后误报“未安装” ([#3085](https://github.com/alibaba/anolisa/pull/3085))。
- Hermes 现在会跳过 API 或调用签名与适配器不兼容的可信共享 hook 模块，继续寻找兼容候选；全部失败时会列出拒绝原因，避免旧安装或缓存模块破坏生命周期 hook ([#2249](https://github.com/alibaba/anolisa/pull/2249))。
- OpenClaw 安装现在会在宿主支持相应选项时接受声明的能力，并且仅在宿主声明 unsafe-install 标志仍有效时传入该标志。将其标记为 no-op 的宿主不会收到绕过标志，安装被拒绝时会提示检查 `security.installPolicy` ([#3126](https://github.com/alibaba/anolisa/pull/3126), [#3152](https://github.com/alibaba/anolisa/pull/3152))。

## [0.8.0] - 2026-09-06

### 新增

- 大型 JSON 记录数组现在可以按 32 条记录的基础预算保留代表性记录、错误、异常结构和数值离群点，并通过 Stash 恢复完整集合。若紧凑 JSON 或 TOON 候选可节省至少 15%，则优先采用，避免进一步缩减记录 ([#3047](https://github.com/alibaba/anolisa/pull/3047))。
- 现在可以压缩识别出的 Cargo、pytest、npm/Jest、Go、Make/C 和通用构建/测试日志，省略重复的常规进度，同时保留诊断、摘要、阶段信息和堆栈。支持的构建/测试命令会保留原生日志供此路径压缩，具备恢复能力时可取回省略的区间 ([#3067](https://github.com/alibaba/anolisa/pull/3067))。
- Cosh-NG、Claude Code、Qoder CLI、OpenCode、Hermes 和 DeepSeek Harness 现在可以通过已有 Shell 工具恢复省略内容。成功执行的独立 `tokenless retrieve HASH` 命令结果不会再次压缩；恢复要求 CLI 可用，且宿主能够替换当前工具结果，其中 Claude Code 要求 2.1.121 或更新版本 ([#3052](https://github.com/alibaba/anolisa/pull/3052))。
- 新增 QwenPaw 插件，在进程内提供 Schema 压缩、RTK 命令改写、响应/TOON 压缩和静态 `tokenless_retrieve` 工具。安装时选择匹配版本的 SDK Wheel，支持的内置工具表之外的工具保持原样 ([#3075](https://github.com/alibaba/anolisa/pull/3075))。

### 变更

- **不兼容变更：** `tokenless compress` 现在仅接受 Protocol v2 的 `before_model`、`pre_tool`、`post_tool` 和 `retrieve` 请求，移除 Protocol v1 和 `tokenless mcp serve`。Core、适配器和自定义调用方必须同步升级，并提供必需的显式恢复能力声明 ([#2978](https://github.com/alibaba/anolisa/pull/2978), [#3068](https://github.com/alibaba/anolisa/pull/3068))。
- **不兼容变更：** Rust 调用方现在使用 Runtime 生命周期方法替代 `TokenlessRuntime::compress`。直接调用响应压缩的代码需要从已移除的 `tokenless-pipeline` Crate 和 `tokenless_schema::ResponseCompressor` 迁移至 Runtime 或 `tokenless-compressors` API ([#2974](https://github.com/alibaba/anolisa/pull/2974), [#2978](https://github.com/alibaba/anolisa/pull/2978))。
- **不兼容变更：** Python SDK 现在通过类型化生命周期请求调用 `before_model`、`pre_tool`、`post_tool` 和 `retrieve`，移除旧的 `ModelRequest`、`ToolCall`、`ToolResult` 和 `ToolResponseCompressor` API。AgentScope 集成要求为自定义工具提供 `ToolContract` 元数据，并暴露一个静态恢复工具 ([#2986](https://github.com/alibaba/anolisa/pull/2986), [#3029](https://github.com/alibaba/anolisa/pull/3029))。
- 恢复提示现在直接给出可用的 Shell 命令或配置的静态工具名，并使用裸 Hash，便于 Agent 选择正确的恢复动作。历史 `<<tokenless:HASH>>` 标记仍可读取；自定义 `retrieve_tool_name` 必须符合工具名规则 ([#3068](https://github.com/alibaba/anolisa/pull/3068))。
- 生命周期 Schema 压缩现在要求提供授权静态恢复工具，并具备可用的 Stash。缺少该能力的 Common BeforeModel Hook 会保留原始 Schema ([#2978](https://github.com/alibaba/anolisa/pull/2978), [#2995](https://github.com/alibaba/anolisa/pull/2995), [#3029](https://github.com/alibaba/anolisa/pull/3029))。
- 生命周期压缩现在保留声明为文件内容的结果，以及已经由 RTK 优化的结果。Common Hook 会传递每次调用的优化状态，缺少稳定工具调用 ID 时保持命令参数不变 ([#2974](https://github.com/alibaba/anolisa/pull/2974), [#2978](https://github.com/alibaba/anolisa/pull/2978), [#2995](https://github.com/alibaba/anolisa/pull/2995))。
- **不兼容变更：** OpenClaw 现在使用 `post_tool_enabled` 控制 PostTool 优化，移除原有的响应、TOON、跳过工具和 Shell 工具策略配置。DeepSeek Harness 也改用共享压缩策略，替代适配器专用阈值和工具列表。OpenClaw 仍仅支持持久化记录的无损更新，不支持当前轮结果替换或恢复 ([#3009](https://github.com/alibaba/anolisa/pull/3009), [#3036](https://github.com/alibaba/anolisa/pull/3036))。
- Hermes 现在将 PreTool 改写和 PostTool 结果处理交给 Core，使用共享响应与 TOON 策略替代适配器本地决策；旧版 Hermes 仍保留阻止原命令并建议改写命令的兼容方式 ([#3018](https://github.com/alibaba/anolisa/pull/3018))。
- 压缩统计现在通过 `content_origin`、`applied_operations` 和 `recoverability` 替代 `seam` 与 `compressor_chain`；授权恢复事件也会记录 Agent、Session 和工具调用归属 ([#2978](https://github.com/alibaba/anolisa/pull/2978))。

### 修复

- OpenClaw 现在读取插件自身配置，即使全局配置存在冲突值，显式启用和禁用设置也能生效 ([#3009](https://github.com/alibaba/anolisa/pull/3009))。
- npm 包现在将共享 Hook 分发脚本作为可执行普通文件打包，避免 `npm pack` 忽略源码符号链接后，已安装插件无法调用 Tokenless ([#3068](https://github.com/alibaba/anolisa/pull/3068))。

## [0.7.14] - 2026-08-26

### 新增

- 协议 v1 的 `tokenless compress` 命令与 `TokenlessRuntime::compress` 现在为 Schema、Response 和 TOON 压缩提供统一入口，使 Adapter 最多调用一次子进程、只执行一次最终大小判定，并且只记录胜出的操作（[#2844](https://github.com/alibaba/anolisa/pull/2844)）。
- `tokenless stats summary` 现在会从活跃汇总中排除 Dry-run Row 并报告排除数量，同时新增总节省、Retrieve Token、净节省、Retrieve 命中/未命中/错误计数和不可恢复截断归因；JSON 输出使用 Schema 版本 1.1，Python `TokenlessStats` API 也开放相同的类型化字段（[#2885](https://github.com/alibaba/anolisa/pull/2885)）。

### 变更

- 截断 Marker 现在包含可直接运行的 `tokenless retrieve` 命令，通过 CLI、MCP 和内嵌 Runtime 执行的 Retrieve 都会计入归因，并且 MCP Retrieve 与 CLI 一样接受大写 Hash（[#2885](https://github.com/alibaba/anolisa/pull/2885)）。

### 修复

- Codex 集成不再在 Tool 使用后追加第二份压缩副本；它会保留环境诊断与 RTK 源头压缩能力，同时避免增加 Model 可见 Prompt（[#2866](https://github.com/alibaba/anolisa/pull/2866)）。
- `tokenless compress-toon` 与 Runtime/Python SDK 现在会一致地保持少于 500 字符的有效 Payload 不变，同时可用 `--min-toon-chars 0` 强制编码；格式错误的 JSON 仍会失败，CLI 输出也会精确保留输入末尾换行符的原有形式（[#2869](https://github.com/alibaba/anolisa/pull/2869)）。

## [0.7.13] - 2026-08-25

### 新增

- Rust 调用方现在可以使用 `tokenless-protocol` 与 `tokenless-pipeline` Crate，获得带版本的压缩 Request/Response、受限成本的内容探测、Registry 路由、分阶段执行和 fail-open 仲裁能力（[#2783](https://github.com/alibaba/anolisa/pull/2783)、[#2788](https://github.com/alibaba/anolisa/pull/2788)、[#2799](https://github.com/alibaba/anolisa/pull/2799)）。

### 变更

- CLI `compress-response` 命令、`TokenlessRuntime::compress_response` 与 Python Binding 现在通过共享 Pipeline 处理 Record 结构的 JSON；标量 JSON 根节点会保持原样透传，超时或被拒绝的候选结果会返回原始内容并回滚其 Stash 写入（[#2816](https://github.com/alibaba/anolisa/pull/2816)）。
- Runtime 与 Python 的 `disposition` 值现在使用协议定义的 snake_case 形式（如 `dry_run` 和 `no_savings`），并可能返回 `passthrough`、`timeout` 或 `error`；当未发生截断时，纯清理节省现在无需 Stash 也可在 `require_reversible` 下生效（[#2816](https://github.com/alibaba/anolisa/pull/2816)）。

## [0.7.12] - 2026-08-22

### 变更

- Response 压缩现在会在保留数组头部后继续保留可配置的尾部窗口（默认 8 项，可通过 `--array-tail-preserve` 和 Runtime API 控制），使最终状态与错误细节继续内联，而 Stash 只存储被省略的中间段（[#2433](https://github.com/alibaba/anolisa/pull/2433)）。
- 当 `BeforeModel` Payload 格式错误或未携带工具声明时，Schema Hook 现在会每个 Session 警告一次，使 Hook 被跳过与正常执行但未产生节省可以区分；显式空工具数组仍会静默透传（[#2606](https://github.com/alibaba/anolisa/pull/2606)）。
- L2 Benchmark 的 JSON、Markdown 与 Semantic Gate Finding 现在会列出保真失败时缺失的 Ground Truth 项，不再只报告计数（[#2433](https://github.com/alibaba/anolisa/pull/2433)）。

### 修复

- `tokenless stats enable` 与 `stats disable` 现在只基于磁盘配置持久化 Stats 开关，因此临时的压缩与 SLS 环境变量覆盖不会被写入 `config.json`（[#2592](https://github.com/alibaba/anolisa/pull/2592)）。
- 当任一 Session 没有记录时，`tokenless stats summary --compare` 现在会失败，并且 `--limit 0` 会被拒绝，避免拼写错误或空样本显示为成功的 0% 对比（[#2674](https://github.com/alibaba/anolisa/pull/2674)）。
- Schema 压缩现在支持包含顶层 `tools` 数组的完整请求对象，在保留非 Function 工具与数组外字段的同时压缩 Function Calling 条目（[#2758](https://github.com/alibaba/anolisa/pull/2758)）。
- 无 Stash 的数组截断 Marker 现在可以完整通过 TOON 往返，极大的尾部保留值也会保留完整数组而不再溢出（[#2433](https://github.com/alibaba/anolisa/pull/2433)）。

## [0.7.11] - 2026-08-20

### 修复

- `tokenless compress-toon` 现在用与 `stats summary` 以及 Python/SDK 路径相同的 CJK 感知字符估算器计算 TOON 节省，因此 dry-run 的 stderr 预测计数与记录的 `before_tokens`/`after_tokens` 一致。JSON 解析、超限输入和 TOON 编码失败仍以退出码 2 结束（[#2681](https://github.com/alibaba/anolisa/pull/2681)）。

## [0.7.10] - 2026-08-19

### 新增

- Gemini 原生 `functionDeclarations` 工具 Schema 现在可在 copilot-shell 等 `BeforeModel` 集成中压缩，包括使用 `parametersJsonSchema` 的声明，同时保留无关的 Gemini Tool 字段（[#2663](https://github.com/alibaba/anolisa/pull/2663)）。
- `anolisa-tokenless` Python SDK 现在通过 `TokenlessStats` 开放类型化的只读 Status、Summary、List、Show、Diff 和 Comparison 查询，复用同一个 Runtime 数据目录，并且只在显式 Show 和 Diff 调用中返回已存储的 Tool 内容（[#2666](https://github.com/alibaba/anolisa/pull/2666)）。

### 变更

- Raw、RPM、npm 和源码安装不再构建或提供未使用的独立 `toon` 可执行文件；TOON 编码仍可通过 `tokenless compress-toon` 与 `tokenless decompress-toon` 使用，升级时只清理 Tokenless 所属的旧版残留文件（[#2657](https://github.com/alibaba/anolisa/pull/2657)）。

### 修复

- AgentScope 集成 Wheel 现在声明 `tqdm` 依赖，因此使用受支持 AgentScope 1.x 范围的全新安装在搭配 OpenAI 3.3.0 及更高版本时可以直接导入，无需手动补装依赖（[#2665](https://github.com/alibaba/anolisa/pull/2665)）。

## [0.7.9] - 2026-08-18

### 新增

- `anolisa-tokenless` Python Wheel 现在开放框架无关的 `before_model`、`before_tool_call`、`after_tool_call` 和 `retrieve` 生命周期，内置 RTK，并提供原生 Schema 与 Response 压缩、TOON、受 Marker 授权的 Retrieve 和逐调用归属（[#2627](https://github.com/alibaba/anolisa/pull/2627)）。

### 变更

- AgentScope 1.0.11 至 1.x 以及 AgentScope 2.0.x 集成现在挂载相同的完整 SDK 契约，在已有 Response 压缩与 Retrieve 支持上增加 Schema 压缩、命令改写、TOON、环境错误提示和逐调用归属（[#2627](https://github.com/alibaba/anolisa/pull/2627)）。

### 修复

- Cosh-NG Extension 的 RTK 重写 Hook 现在直接匹配小写 `shell` 工具名，因此无需依赖宿主侧工具名别名也能重写 Shell 命令（[#2611](https://github.com/alibaba/anolisa/pull/2611)）。

## [0.7.8] - 2026-08-18

### 变更

- 当载荷少于 500 字符时跳过 TOON 编码；低于该阈值时 token 节省几乎为零，而每次事件的编码开销保持不变（[#2613](https://github.com/alibaba/anolisa/pull/2613)）。

### 修复

- npm 平台包（`@anolisa/tokenless-*`）不再声明 `tokenless`/`rtk`/`toon` bin 入口。与根包的同名冲突会导致 npm 在安装时删除所有冲突的 `.bin` 链接，使安装后没有可用的 `tokenless` 可执行文件（[#2613](https://github.com/alibaba/anolisa/pull/2613)）。

## [0.7.7] - 2026-08-17

### 新增

- 现在可从源码构建 `anolisa-tokenless` ABI3 Wheel，为 CPython 3.11+ 提供有状态的进程内 JSON Response 压缩和基于 Marker 的 Stash Retrieve，且无需启动 CLI 子进程（[#2501](https://github.com/alibaba/anolisa/pull/2501)）。
- AgentScope 1.0.11 至 1.x 以及 AgentScope 2.0.x 应用现在可以安装独立的同版本集成 Wheel，压缩成功的最终 Tool Response，并且只允许 Retrieve 当前 Agent 可见 Marker 对应的内容（[#2507](https://github.com/alibaba/anolisa/pull/2507)、[#2528](https://github.com/alibaba/anolisa/pull/2528)、[#2553](https://github.com/alibaba/anolisa/pull/2553)）。
- DeepSeek Harness Profile 现在可以启用随包提供的原生 Plugin，在保持环境错误归因与 fail-open 行为的同时，压缩成功的单 Block JSON Tool Result（[#2581](https://github.com/alibaba/anolisa/pull/2581)）。

### 变更

- Claude Code Adapter 探测现在会重试首次运行时暂时性的二进制文件与 Plugin Registry 初始化失败，减少预置完成后立即出现的错误未就绪结果（[#2519](https://github.com/alibaba/anolisa/pull/2519)）。
- Tokenless RPM 现在提供虚拟能力 `anolisa-component(tokenless)`，使 ANOLISA 在仓库组件索引不可用时仍可解析该 Package（[#2576](https://github.com/alibaba/anolisa/pull/2576)）。

### 修复

- Cosh-NG Extension 执行现在会把硬关闭的 Tool Ready Hook 所返回的空结果视为成功 no-op，而不再 fail closed（[#2506](https://github.com/alibaba/anolisa/pull/2506)）。
- 无节省的 Response 压缩现在只删除已丢弃候选结果所创建的 Stash Row，既避免孤立数据，也不会删除被其他进程刷新过的条目（[#2480](https://github.com/alibaba/anolisa/pull/2480)）。

## [0.7.6] - 2026-08-13

### 变更

- `TOKENLESS_DATA_DIR` 现在接受真实用户 home 之外的绝对非根目录；显式目录无效时会停用 SQLite 状态，不再静默回退到 home（[#2434](https://github.com/alibaba/anolisa/pull/2434)）。
- 所有 Adapter 的 Tool Ready 调用前检查、修复和阻断现已硬关闭，避免错误的就绪结果阻止有效工作；工具执行后的失败归因和其他 Tokenless 功能保持启用（[#2487](https://github.com/alibaba/anolisa/pull/2487)）。

### 修复

- 直接 JSON Schema 的 Description 现在只会 Stash 一次，因此一次 Retrieve 即可返回不含嵌套 Marker 的原始内容（[#2399](https://github.com/alibaba/anolisa/pull/2399)）。
- 通过环境变量设置 Stats 与 SLS 开关时，`config.json` 中的 Dry-run 压缩配置现在仍会生效（[#2380](https://github.com/alibaba/anolisa/pull/2380)）。
- `tokenless retrieve` 现在会逐字节写出已存储的 Payload，不再追加换行符（[#2396](https://github.com/alibaba/anolisa/pull/2396)）。
- Stash Retrieve 现在会跳过格式错误的 Marker 以查找后续有效 Key，并在对抗性输入下保持线性扫描（[#2386](https://github.com/alibaba/anolisa/pull/2386)）。
- RPM 安装现在包含 Codex Adapter 安装脚本所需的共享生命周期 Helper（[#2425](https://github.com/alibaba/anolisa/pull/2425)）。

## [0.7.5] - 2026-08-10

### 新增

- OpenCode 用户现在可以通过无冲突的本地 Plugin 启用 Tokenless，并复用现有的就绪检查、命令重写、Schema 压缩和响应压缩 Hook（[1233cfcf](https://github.com/alibaba/anolisa/commit/1233cfcfd863de4bca7819b0a98615c569da2c9a)）。

### 变更

- Qoder Adapter 现在使用原生 Plugin 和 Hook 约定，在保持 fail-open 行为的同时原位替换压缩后的 Tool Output（[13817938](https://github.com/alibaba/anolisa/commit/13817938f0a8cf2b8df78d3e59f97302e4fb1947)）。

### 修复

- 重写后的 Shell 命令现在使用解析出的 `rtk` 绝对路径，因此在 `PATH` 受限的 Agent 环境中仍可正常执行（[ae83f7d3](https://github.com/alibaba/anolisa/commit/ae83f7d3ef9c85d5f42e7b1c0fd6884a0ffc4869)）。
- Qoder 和 OpenClaw Hook 现在会跨命令重写与 Proxy 边界保留 Agent、Session 和 Tool 归因信息（[#2158](https://github.com/alibaba/anolisa/issues/2158)、[2f330656](https://github.com/alibaba/anolisa/commit/2f330656fe94fc5936e1ebcaf586d7ebcd7df0d5)）。
- Adapter 安装现在可识别旧版 `/usr/local` 布局、推荐使用 RPM 升级模式，并在升级时删除过期的已打包用户手册文件（[f7ce3878](https://github.com/alibaba/anolisa/commit/f7ce38786cfba318614849a73a7f9acb693ea803)、[ec25d516](https://github.com/alibaba/anolisa/commit/ec25d516b8c05ebd8e88a703f57708749c8032ab)、[917f151e](https://github.com/alibaba/anolisa/commit/917f151ea4157850b51162668b3e1a441fb04262)）。

## [0.7.4] - 2026-07-31

### 新增

- Tokenless 现在可通过 npm 安装到 Linux 和 macOS x64/arm64，并包含 `tokenless`、`rtk`、`toon` 二进制文件以及 Framework Adapter（[#1929](https://github.com/alibaba/anolisa/pull/1929)）。
- `tokenless stats diff` 现在可通过文本或 JSON 报告以及有界 Unified Diff，说明 Record、Session 和 Tool Use 的预计节省量（[#1991](https://github.com/alibaba/anolisa/pull/1991)）。
- `TOKENLESS_DATA_DIR` 现在可为统计数据库和可逆压缩数据库设置一个可信目录，同时保留各数据库的独立覆盖项（[#2038](https://github.com/alibaba/anolisa/pull/2038)）。

### 修复

- Qwencode Adapter 现在声明其提供的 `compress-toon` Capability，使 Adapter 发现结果与其压缩行为保持一致（[#1945](https://github.com/alibaba/anolisa/pull/1945)）。
- Hermes 副本安装现在可从可信的系统、XDG 和用户数据路径解析共享 Hook 资源，并在找不到安全候选路径时提供可操作的诊断信息（[#2058](https://github.com/alibaba/anolisa/pull/2058)）。

## [0.7.3] - 2026-07-28

### 新增

- ANOLISA 现在可在 macOS 上安装 Tokenless，并将 Qwencode 作为独立 Adapter 启用（[#1964](https://github.com/alibaba/anolisa/pull/1964)）。

### 变更

- Adapter Hook 现在可在用户、`/usr/local`、RPM 和旧版安装布局中发现 `tokenless`、`rtk` 和 `toon`（[#1957](https://github.com/alibaba/anolisa/pull/1957)）。
- Hook Launcher 现在优先使用当前安装中的资源，避免多个 Tokenless 安装共存时混用不同版本（[#1964](https://github.com/alibaba/anolisa/pull/1964)）。

### 修复

- Tool Schema 压缩现在读取 Cosh 和 Cosh-NG 的规范请求字段，因此 Schema 会被压缩，而不再静默原样通过（[#1894](https://github.com/alibaba/anolisa/pull/1894)）。
- 存在 Hook 环境变量时，Cosh-NG 压缩统计现在归因到 `cosh-ng`（[#1894](https://github.com/alibaba/anolisa/pull/1894)）。
- Qoder Plugin 安装现在展开缓存的 Hook 路径，避免无效的 `/rewrite_hook.py` 命令阻塞 Tool Call；用户手册也包含受影响升级的恢复步骤（[#1924](https://github.com/alibaba/anolisa/pull/1924)）。
- ANOLISA Package 现在包含 Tokenless Adapter 所需的共享 Hook 资源（[#1964](https://github.com/alibaba/anolisa/pull/1964)）。

## [0.7.2] - 2026-07-27

### 新增

- Tokenless 现在通过替换原有 Model 可见内容来压缩 Cosh-NG Tool Response（[#1669](https://github.com/alibaba/anolisa/pull/1669)）。
- Tokenless 现在可重写受支持的 Cosh-NG Shell 命令，以生成更紧凑的输出（[#1669](https://github.com/alibaba/anolisa/pull/1669)）。

### 变更

- Shell 环境检查现在只报告当前命令引用的推荐工具（[#1598](https://github.com/alibaba/anolisa/pull/1598)）。
- `tokenless env-check --fix` 现在只安装必需依赖，不会修改可选推荐项（[#1598](https://github.com/alibaba/anolisa/pull/1598)）。
- 自动依赖修复现在会针对认证、网络或权限问题快速失败并提供可操作信息，而不再提示输入 sudo（[#1598](https://github.com/alibaba/anolisa/pull/1598)）。
- Cosh-NG 压缩统计现在记录在 `cosh-ng` Agent 下（[#1669](https://github.com/alibaba/anolisa/pull/1669)）。
- Cosh-NG 压缩现在从 Model Context 中排除仅供显示的内容（[#1669](https://github.com/alibaba/anolisa/pull/1669)）。
- 无法检测版本的 Cosh-NG 运行现在会保持原始 Tool Response 不变（[#1669](https://github.com/alibaba/anolisa/pull/1669)）。
- 压缩结果不够小时，Tokenless 现在会保持 Tool Result 不变（[#1674](https://github.com/alibaba/anolisa/pull/1674)）。
- Tokenless 用户手册现在位于 ANOLISA 中央指南中，而不再随 RPM Package 提供（[#1586](https://github.com/alibaba/anolisa/pull/1586)）。

### 修复

- Claude Code 2.1.121 及更高版本现在会用压缩结果替换原始 Tool Result，避免 Context 重复（[#1674](https://github.com/alibaba/anolisa/pull/1674)、[#1686](https://github.com/alibaba/anolisa/pull/1686)）。
- 较旧或无法检测版本的 Claude Code 现在会原样传递 Tool Result，不再复制压缩后的 Context（[#1674](https://github.com/alibaba/anolisa/pull/1674)、[#1686](https://github.com/alibaba/anolisa/pull/1686)）。
- Claude Code 替换现在会保留内置 Tool Result 格式，包括空字段（[#1674](https://github.com/alibaba/anolisa/pull/1674)、[#1686](https://github.com/alibaba/anolisa/pull/1686)）。
- ANOLISA 现在可以正确识别已打包的 Tokenless 版本（[#1587](https://github.com/alibaba/anolisa/pull/1587)）。

## 0.7.1

- 修复 RPM Tarball，使其排除生成的 `.anolisa/component.toml`，确保 rpmbuild 始终从权威 `.toml.in` Template 重新生成 Adapter Contract；此前签入的过期副本会缺少 claude-code、codex 和 cosh Adapter 声明（关闭 #1470）
- 同步 Adapter Contract：在 `component.toml.in` 中声明所有已交付 Driver（qoder、claude-code、codex、cosh、qwencode），并增加 CI 检查 `check-component-contract` 以保持同步
- 将测试覆盖率从 75% 提高到 90%：为四个 Crate 增加约 170 个单元测试，覆盖压缩边界情况、Stash 往返、Schema Migration、SLS Writer 和 CLI Dispatch
- 强化测试隔离：使用 RAII `TempDbGuard` / `EnvGuard` 替换不安全的环境变量修改，避免测试接触真实的 `~/.tokenless` 状态；在 Makefile 中强制使用 `--test-threads=1`（Rust 2024 的 `set_var` 是 unsafe）

## 0.7.0

- 增加 MCP `tokenless_retrieve` stdio Server（`tokenless mcp serve`），使连接 MCP 的 Agent 可按需恢复截断 Payload；这是 `tokenless retrieve` CLI 的 MCP 对应实现，补齐与 Headroom CCR `headroom_retrieve` 的 Stash MCP 差距
- 完成其余有损路径的可逆压缩（Stash / CCR）覆盖：`ResponseCompressor` 字符串截断、`ResponseCompressor` 深度截断和 `SchemaCompressor` 描述截断现在都由 Stash 支持并使用 `<<tokenless:KEY>>` Marker；写入 Stash 前进行容量检查以避免孤立条目，共享的 `stash_suffix()` Helper 保持 Marker 预算一致
- 为 `compress-schema` 增加 `--no-stash` / `--stash-db` Flag（与 `compress-response` 一致）；Dry Run（`compression_on=false`）会跳过 Stash，确保没有可检索条目时 Marker 不会进入 LLM
- 为 `SqliteStore` 增加惰性 TTL 清理：在 Retrieve 查询前物理删除过期行，避免 Stash DB 无限增长
- 增加实际节省率展示：`StatsSummary::actual_savings_percent(session_total_tokens)`；`format_summary()` / `format_summary_json()` 接受可选的 Session 总量，并输出“Overall Savings vs Total Consumption”区段及新的 JSON 字段（`session_total_tokens`、`actual_savings_tokens`、`actual_savings_percent`）；未提供时保持向后兼容
- 为压缩统计增加 Stash 写入与大小计数器（扩展 `record_compression_stats`）；Retrieve 端的命中/未命中统计延后到出现明确使用场景时实现
- 增加 Qoder Framework Driver（qodercli 安装、settings.json Merge/Prune、`AdapterOps::read_file`、Symlink-safe 原子 `write_file`）；仅允许 Qoder 使用 `adapter_type=plugin`；对伪造 Receipt fail closed，并要求所有受管 Hook 存在
- 将测试覆盖率从 59% 提高到 75%：四个 Crate 新增 100 多个单元测试和 18 个 CLI 集成测试；测试代码通过 `include!()` 移至 `src/tests/`，使代码结构更清晰
- 增加可逆压缩用户手册（`docs/stash-reversible-compression.md`）并更新 README：为 `tokenless-ccr` 增加架构树条目，增加说明 Hash/Marker 输入和 `--no-stash` / `--stash-db` 的 Retrieve 小节，并按场景映射重写“适用场景与预期效果”章节
- 将 Tokenless 文档从 `*_CN.md` 重命名为 `*_zh.md`，增加双向双语链接，并创建 `README.md` 与 `README_zh.md`
- 处理 Adapter Review 发现：信任 Codex Symlink Target 的已打包数据目录 Root；按组件限定 Claude Code Marketplace 并 fail closed；启用前拒绝 Framework/Adapter Type 不匹配
- 消除 rustc 1.94 stable 在现有测试中发现的 Clippy Warning（tokenless-cli 中的 `field_reassign_with_default`，tokenless-stats 中的 `bool_assert_comparison` 和 `default_constructed_unit_structs`）

## 0.6.1

- 将 tool_categories.json 打包进 dist，用于 npm 安装
- 使用 node: Prefix，并在 OpenClaw Plugin 中移除 Shell Subprocess

## 0.6.0

- 在 JSON 输出中增加绝对保存值和 Schema Version
- 在 OpenClaw Plugin 中使用 import.meta.dirname 替代 __dirname
- 为 Qwen Code Extension 增加 Qwencode Adapter
- 修复 RTK pytest 的“No tests collected”回归
- 为 Codex Script 中的 hook_utils Import 增加可信 FHS Fallback Path
- 增加带配置开关的 SLS JSONL 数据收集
- 增加 Tokenless RPM Component Contract（发布元数据）
- 增加带 Dry-run 比较模式的压缩开关（`TOKENLESS_COMPRESSION_ENABLED`、`stats summary --compare`）
- 默认启用 SLS 记录并补充使用文档
- 对齐压缩模式的 Serde/DB 形式并去重配置加载
- 扩展 RPM Component Contract（bundle.entry + hermes）
- 将 SLS Writer 改为仅追加，并在日志文件不存在时跳过
- Qwencode Hook 优先使用 tool_call_id，而不是内部 tool_use_id
- 将 Vendored RTK 升级至 v0.43.0；针对重构后的 Runner 重做 pytest stderr 输出 Patch；删除 grep-fallback-fix（上游已修复根因）和 preflight-skip-python（上游已撤销）
- 将 Makefile 和 Spec 中的 toon-format 同步到 0.5.0（此前仍为 0.4.6）

## 0.5.1

- 为 `stats summary` 增加 `--json` 输出
- 实现统一 Tool 分类和三层压缩策略
- 增加 RTK grep Fallback Pattern 修复 Patch
- 增加 RTK pytest Error Report Patch

## 0.5.0

- 增加 Hermes Adapter Runner
- 移除 TOON Wrapper Prefix 并精简诊断 Tag
- 在各 Adapter 中统一 RTK Rewrite Exit Code 3 的处理
- 保护 env-fix 和 Hook 中的 Shell 变量插值
- 增加 Subprocess Return Code 检查，并提取共享 Hook Utility
- 保护 resolveBinaryPath 并改进 Binary Cache 失效逻辑
- 在测试中使用 mktemp 和安全的 Home 展开
- 限制 `SchemaCompressor` 递归深度，防止 Stack Overflow
- 传播 env-fix Subprocess Failure，不再只返回 stdout
- 基于 getpwuid_r 进行 Home 查询，并对候选 Binary 执行 Trust Check
- 使用 UID Trust Check 强化 env-fix 安装路径，并将 stderr 转存到日志
- Stats Recorder 从 Poisoned Mutex 恢复，不再直接失败
- 增加输入大小限制并校验 DB Path
- 在 Response Compressor 中预留 Truncation Marker 长度
- 将 OpenClaw Plugin Name 重命名为 Tokenless，ID 重命名为 tokenless
- 增加 Qoder CLI Adapter
- 支持对 Array Input 执行 compress-schema
- 压缩被跳过时发出警告
- 增加 Stats Command Syntax
- 增加 Claude Code Adapter Plugin
- TTY stdin 输入时返回错误，不再挂起
- 增加 Codex Adapter Plugin
- 修复压缩 Pipeline 的输出膨胀、截断和 Hook Timeout
- 强化 env-fix、版本提取、文件信任、Schema 和权限
- 处理 Review 发现：Trailing Newline、chmod Guard、Rate-limited Log 和 Comment
- 使 skip-tools 条目可进行环境归因
- 增加 selective-claw Context Engine Plugin
- 处理 selective-claw Plugin Review 发现
- 从 selective-claw 中移除无效的 `2` 依赖
- 恢复 compress_response_hook.py 中的缩进
- 强化 Hook Exit Code 处理和 Trust Model 一致性
- 只对真正异常的 RTK Exit Code 发出警告
- 去重 rewrite_hook，并从 hook_utils 导入

## 0.4.1

- 修复 env_check.rs 中 version_ge 的三段版本截断问题（比较所有分段）
- 增加 Qoder、Claude Code、Codex Adapter Plugin 和文档
- 将 manifest.json 与 Template 同步，以包含全部六个 Agent
- 更新 README 和用户手册，记录新的 Agent Integration
- 将 __pycache__ 加入根 `.gitignore`
- 更新 response-compression.md，记录全部 Agent Integration Path
- 从 Cargo.toml 派生 Makefile 版本，并修复 Spec Changelog 的星期
- 将 Adapter 版本号统一为 0.4.0
- 从 Cargo.toml 派生 Adapter Plugin 版本，不再硬编码

## 0.4.0

- 修复 Stats、命名、SQL、路径和权限中的 5 个 Bug
- 对齐 FHS 路径、重构 Adapter 目录并移除 install.sh
- 处理 Schema、env-check、Hook 和 Plugin 中的 Code Review 发现
- 增加 Hermes Agent Plugin
- 强化安全性并修复关键算法正确性
- 修复行为正确性和逻辑问题
- 去重、删除 Dead Code 并进行表面清理
- 支持 Staged Install
- 支持 Debian/Ubuntu FHS 路径并强化 Binary Resolution
- 将 OpenClaw Plugin 构建到 dist/index.js

## 0.3.2

- 使用 libc::getuid() Syscall 替换可伪造的 Home Directory UID 推导，保证 Trust Chain 完整性
- 使用进程内 toon_format::encode_default() Library Call 替换 Subprocess `toon -e` 调用
- 使用 crates.io Dependency 和内联 toon-format Source 替换 RTK/TOON Git Submodule
- justfile 的 setup-rtk Recipe 在 RTK Stats Patch 失败时 Hard Fail
- 统一 compress-toon、compress-schema 和 compress-response 的 Error Exit Code（均为 2）
- 从 Makefile 的 toon 安装中移除 `2>/dev/null || true`（Binary 缺失时 Hard Fail）
- 删除已具有 `#[from]` 的 thiserror Variant 上多余的 `#[source]` Attribute
- 将 Python Hook 的 FHS Path Constant 去重到共享 hook_utils Module
- 将 libc 加入 Workspace Dependency，用于 UID Syscall
- 在 spec.in 中增加 rust >= 1.89 的详细注释，说明 CI Pin 的原因

## 0.3.0

- 增加与 Cosh Extension 集成的 Tool-ready 四阶段环境预检查
- 无 Token 节省时跳过压缩和统计
- 通过 `.rewrite-context` 文件向 RTK Stats 传递 Caller Context
- 从 install.sh 中移除多余的 Cosh Extension 安装/卸载
- 按 Cosh 开发指南将 Cosh Hook 转换为 Extension 格式
- 跳过零压缩和统计记录
- 在 OpenClaw Plugin 中使用 isExecutable() 和解析后的路径
- 为 RPM 安装的 Plugin 解析 RTK/TOON Binary Path
- 修正 RPM 安装路径，使其符合 install.sh 预期
- 在 TOON Encoding 中保留 Tool Result Message 结构
- 将安装路径与 FHS 对齐
- 使用 Hook Payload 中真实的 tool_use_id 自动记录 Stats
- 重构 RPM 目录并移除自动 Plugin/Hook 安装

## 0.2.0

- 增加基于真实数据自动记录的压缩统计
- 增加 TOON Context 压缩支持
- 对 Skill 和内容检索 Tool 跳过压缩

## 0.1.0

- 将 Tokenless 引入 ANOLISA（#199）

[0.7.6]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.5...tokenless/v0.7.6
[0.7.5]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.4...tokenless/v0.7.5
[0.7.4]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.3...tokenless/v0.7.4
[0.7.3]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.2...tokenless/v0.7.3
[0.7.2]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.1...tokenless/v0.7.2

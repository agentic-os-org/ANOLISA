# Tokenless 用户手册

[English](../../../en/token-saving/tokenless/user-manual.md)

Tokenless 面向工具调用密集的 AI Agent。它的 CLI 可以精简 Schema 和工具响应，Adapter 还可以改写 Shell 命令、检查工具依赖，并把压缩结果交给 Agent。最终效果取决于宿主框架：有的 Adapter 会替换原始结果，有的只会追加压缩上下文而保留原文。

第一次使用请从[快速开始](QUICKSTART.md)进入。

## 从源码构建独立 CLI

源码构建适合开发和调试。当前项目只在 Linux 上验证和支持源码构建：

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/src/tokenless
cargo build --release --locked -p tokenless-cli
./target/release/tokenless --version
```

这条路径只生成独立的 `tokenless` CLI，不会安装 `rtk` 或 Agent 接入资源。需要在 Agent 中使用完整能力时，请按照[快速开始](QUICKSTART.md)通过 anolisa CLI 安装。

## 从源码构建 Python SDK

CPython 应用可以在进程内使用 Tokenless，无需为每个生命周期操作启动 CLI：

```bash
make python-wheel
python3 -m venv /tmp/tokenless-python
/tmp/tokenless-python/bin/pip install target/wheels/anolisa_tokenless-*.whl
```

构建要求系统可发现 CPython 3.11+ 开发环境。Wheel 使用 CPython 3.11 stable ABI，但仍与
构建时的操作系统和 CPU 架构绑定。

Python SDK 分为两层。`anolisa-tokenless` 包开放通用 `TokenlessSdk`、可直接调用的
`TokenlessRuntime` 操作和 typed `TokenlessStats` 查询；相同版本的
`anolisa-tokenless-agentscope` 包把该通用生命周期映射到 AgentScope。两层结构、可运行示例与
配置见 [Python SDK 指南](sdk.md)。

## 能力与边界

| 能力 | 当前代码实际执行的行为 | 重要边界 |
|------|------------------------|----------|
| Schema 压缩 | 移除 `title` 和 `examples`，删除描述中的围栏代码和行内代码，合并空白并截断描述 | Common BeforeModel 在没有 Marker 授权恢复时透传有损变换；OpenCode 逐工具路径和直接 CLI 仍会压缩（Qwen Code 会跳过声明的事件） |
| Content-aware 响应压缩 | 成功的 PostTool JSON 路由给 `JsonCompressor`；已识别的成功构建/测试命令输出路由给 `BuildLogCompressor`；CSV/TSV 路由给 `TabularCompressor`；支持的搜索列表交给 `SearchResultsCompressor`；只接受端到端更小的结果 | 其他内容域与 Tool Error 透传；可恢复缩减需要受 Marker 授权的 Framework 恢复或受支持的 Marker 命令路径 |
| 搜索路径共享 | API 搜索记录（含 Claude 原生 Grep）的连续行共享完整路径，保留收到的全部文本与位置 | 默认开启；需要 API 响应来源、文本替换能力及无上下文记录；文件和命令输出不进入此域 |
| TOON 编码 | 编码 JSON；估算 Token 没有下降时保留 JSON 输入 | 宿主支持文本替换时替换原文；无替换能力的宿主透传 |
| 命令重写 | 有匹配规则时调用 `rtk rewrite`，再向框架提交改写后的 Shell 输入 | 已识别的构建/测试命令保持原生输出交给 Build Log；其他无规则或被拒绝的改写透传 |
| Tool Ready | 旧版调用前能力，用于检查声明的二进制、版本、配置、权限和可选依赖 | 已硬关闭；不会检查、修复或阻断工具调用 |
| Stash | 保存因字符串、数组、深度或 Schema 描述截断而省略的内容、Record Reduction 背后的完整原始数组，被省略的 Build Log 进度区间，以及行缩减背后的完整原始表格 | 默认 TTL 一小时、最多 10,000 个有效条目；其他被移除字段不会进入 Stash |

代码没有提供固定节省率保证。结果取决于 Payload、Adapter 交付语义，以及工具数据在模型上下文中的占比。请按[效果度量](measuring-savings.md)使用自己的工作负载测量。

## Tokenless 如何参与一次工具调用

启用对应 Adapter 后，一次工具调用可能经过以下阶段：

```text
工具调用前：已识别的构建/测试命令预留给 Build Log；其他命令 RTK 改写 → 传递输出优化状态
工具调用后：状态与优化旁路 → JSON/CSV/TSV/Search/Build Log PostTool Pipeline → 可选 Stash/TOON → 写入统计
模型调用前：Schema 压缩 → 提取可见 Marker → 条件式 Retrieve 声明
Retrieve：可见 Marker 授权 → 字节级一致的 Stash Read
```

这是能力示意，不是所有框架都会完整执行的固定流水线。例如 content-aware Protocol 路径
当前服务于 Cosh-NG、OpenClaw、Hermes、Qoder、受支持的 Claude Code 版本、OpenCode 和
DeepSeek Harness。Codex 和 Qwen Code 当前宿主契约不能替换工具后输出。具体见
[Agent 集成](framework-integration.md)。

## 需要特别理解的行为

### 安装不等于启用

`anolisa install tokenless` 安装组件和 Adapter 资源。要让某个 Agent 自动使用 Tokenless，还需要：

```bash
anolisa adapter enable tokenless <framework>
```

CLI-only 用法不需要 Adapter。

### “关闭压缩”只影响压缩操作

设置 `compression_enabled=false` 或 `TOKENLESS_COMPRESSION_ENABLED=0` 后，`compress`、
`compress-schema`、`compress-response` 和 `compress-toon` 仍会计算预测节省并可能写入统计，
但会返回原始输入。该模式不会写入 Stash 条目。

这个设置不会关闭 RTK 命令重写、Adapter 执行或内容取回。Tool Ready 已独立硬关闭。如需停止 Agent 中的所有 Tokenless 行为，应禁用 Adapter：

```bash
anolisa adapter disable tokenless <framework>
```

### 压缩的触发条件与阈值

Adapter 不会压缩每一次工具结果。以响应压缩为例，只有以下条件全部满足，才会实际产出压缩内容：

1. 压缩未被停用。`compression_enabled=false` 或 `TOKENLESS_COMPRESSION_ENABLED=0` 时进入 dry-run，仍计算统计但返回原文（见上一节）。
2. 工具不属于内容读取类。Read/Glob/Grep/LSP/NotebookRead 及别名会跳过响应压缩，保留完整内容。搜索路径共享引入了一个很窄的例外：Claude Code 原生 `Grep` 的无上下文 content 模式结果会改走该无损压缩器，同样保留全部已收到命中（见[控制搜索路径共享](#控制搜索路径共享)）。
3. 响应长度达到最小阈值。共享响应 Hook、OpenClaw 和 Hermes 跳过短于 200 字符的响应。长度按字符数而非字节数计算。
4. 内容是合法 JSON。按阈值截断的响应压缩只处理 JSON 对象和数组。
   - **4a. 共享响应 Hook 路径**：到达时不是 JSON 的纯文本会交给内容感知的文本压缩（构建/测试日志的终端输出清理与进度缩减、CSV/TSV 表格压紧、API 搜索路径共享），见[Adapter 处理规则](framework-integration.md#adapter-处理规则)；表格的具体规则见 [CSV/TSV 视图可能不完整](#csvtsv-视图可能不完整)，搜索的规则见[控制搜索路径共享](#控制搜索路径共享)。
   - **4b. OpenClaw 和 Hermes**：两者都会拆出 Shell 结果中的主文本字段（如 `{"stdout": ...}` 信封里的 `output`）并允许文本替换，因此 Core 也能把这部分纯文本交给构建日志和 CSV/TSV 压缩器处理；OpenClaw 的结构化槽位只接受 JSON。

   共享响应 Hook 还会在启动压缩子进程前跳过带 YAML frontmatter、形似 Skill 的文本（这类文本在 Core 侧本来也会原样透传）。
5. 压缩结果严格小于原文。响应压缩和 TOON 编码都没有让内容变小时，保留原文。

通过上述检查后，截断强度由工具类别决定。分类和阈值定义在 Adapter 目录下的 `tool_categories.json`（各 Adapter 共享的单一事实来源）；文件缺失或无效时使用内置的安全回退值：

| 类别 | 代表工具 | 字符串截断阈值 | 数组保留上限 | 最大嵌套深度 |
|------|----------|----------------|--------------|--------------|
| 内容读取类 | Read、Glob、Grep、LSP、NotebookRead 及别名 | 跳过压缩 | — | — |
| Shell/exec | Bash、Shell、exec、terminal 等 | 65,536 字符 | 128 项 | 8 |
| 其他结构化工具 | 未列入前两类的工具 | 1,048,576 字符 | 65,536 项 | 32 |

阈值含义：字符串超过阈值时从阈值处截断（启用 Stash 时可取回原文）；数组超过上限时只保留前面的项，尾部被截断（启用 Stash 时同样可取回）；嵌套超过深度上限的子树折叠为截断标记。

几点路径差异：

- 独立运行 `tokenless compress-response` 时使用 CLI 自身默认值（4,096 字符 / 32 项 / 深度 8），可用 `--truncate-strings-at`、`--truncate-arrays-at`、`--max-depth` 覆盖，详见 [CLI 参考](cli-reference.md)。
- Codex 和 Qwen Code 在当前宿主契约下不运行响应压缩：Codex 依靠 RTK 源头减量并附加环境失败诊断，Qwen Code 没有工具后替换槽位。各集成的实际能力详见下方适配器表格。
- OpenClaw Plugin 读取同一份 `tool_categories.json` 分类，把工具映射为内容来源（文件内容、命令输出或 API 响应），该文件缺失或无效时回退到内置列表，再由 Core 套用对应阈值；它原有的 `skip_tools`、`shell_tools` 覆盖项已删除，不再控制 Adapter。当前选项见[配置与数据隐私](configuration-and-privacy.md)。
- TOON 编码是独立的触发判断：只对至少 500 字符的负载、且宿主槽位接受文本时运行，并且只有编码结果比当前内容更小时才会采用。
- AgentScope 框架集成不使用上面的 Adapter 阈值，而是按 `conservative` / `balanced` / `aggressive` 模式选择阈值，见[框架集成](framework-integration.md)。

### 控制搜索路径共享

API 搜索路径共享默认开启。在 Agent 进程环境中设置 `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0`，
可通过 CLI 关闭该功能。未设置时保持开启，`1`、`true`、`yes`（不区分大小写）也表示开启；
空值和其他值均关闭。该设置独立于 `config.json`。Python SDK 可使用
`TokenlessConfig(search_path_sharing_enabled=False)` 关闭；Rust 将
`RuntimeConfig.search_path_sharing_enabled` 设为 `false`。所有入口均默认开启。

关闭此功能时搜索列表原样返回。其他工具名仍可使用 JSON、表格和日志压缩。精确名称 `Grep`
始终排除这些压缩器以保留已收到命中，即使路径共享关闭也不例外。因此，自定义 `Grep` 工具
无法通过此开关恢复此功能引入前的 JSON、表格和日志压缩。支持的无上下文 Claude Grep 结果
保留全部已收到命中；
文件读取和命令输出（包括没有 RTK 的 Bash）均不进入搜索路径共享。其他 API 工具也可使用同一 Core 能力。
整任务节省取决于工作负载；搜索结果变小并不保证总 Token 用量更低。

### CSV/TSV 视图可能不完整

宿主支持用文本替换输出时，成功的 CSV/TSV 工具结果可以被压缩。文件来源结果、失败工具、
已由 RTK 优化的输出以及 Retrieve 输出透传。支持的表格必须有表头和至少两条等宽数据行，
且逗号或制表符分隔格式没有歧义。此压缩器不处理引号格式错误、分隔符有歧义、单列文本、
Markdown 或定宽表格。

全量压紧保留所有单元格字符串，包括空单元格、重复表头、前导零和大数值字符串。
它移除非必要引号并规范化记录分隔符；单元格内部的换行保持不变。
这保证单元格等价，不保证原始字节一致。全量视图的估算 Token 节省达到 15% 时优先采用。

行筛选要求列名证据：每个非空表头以 Unicode 字母或 `_` 开头，后续只允许字母、数字、
`_`、`-` 和 `.`，且至少有一个非空列名。允许重复和空列名。含空格、表达式或句子标点的
表头保留全部行，避免对这些源码或散文形式采样。该保守启发式规则也会跳过部分真实表格的行筛选。

否则，超过 32 条数据行的表格可保留首尾各四行、含诊断关键词的行，并均匀选择普通行补足
32 行基础预算。受保护行可以超出该预算。表格外的提示说明保留行数和总行数、从 1 开始且
不含表头的原始数据行区间，以及恢复方法。完整原始 CSV/TSV 会存入 Stash，Retrieve 返回
原始字节。完整枚举或计算前应先恢复原文：选定行只是一个不完整视图。
缺少恢复能力或 Stash 写入失败时，只允许全量压紧或原文透传。
精确源行号范围列表超过 1 KiB 时也只保留全量候选或原文，不会只报告部分诊断行或源行号。

计入提示后，缩减候选的字符数和估算 Token 数必须同时小于原文及全量视图。
这些检查不保证在所有模型的 Tokenizer 下都有节省。

### 原生 Grep 保留收到的全部命中

Claude Code 2.1.121 及更新版本的原生 Grep 文本结果可以共享重复文件路径。
`File="..."` 头提供后续 `line:text` 记录的完整路径，直到下一个文件头。
收到的全部记录、源码正文、空白和换行均保留。仅采用更小的表示，不需要 Stash 条目或回取命令。

首版支持至少三条记录的无上下文 `path:line:text` 列表，路径不能包含冒号。
上下文查询、计数/文件列表模式、不支持的格式和文件读取保持现有行为；Bash 搜索继续经过 RTK。
Grep 可能在 Tokenless 收到结果前已经应用宿主限额，路径共享无法恢复此前未交付的命中。
首次结果变短不保证整个任务的总消耗下降。

### 可逆压缩是有条件的

启用压缩时，响应和 Schema 截断默认会把被移除的 Payload 写入
`~/.tokenless/stash.db`，并在输出中加入：

```text
<<tokenless:0123456789abcdef01234567>>
```

本地可以通过受信 `tokenless retrieve` 命令取回。受支持的 CLI Adapter 会把这条精确命令
写入 Marker，让模型通过已有 Shell Tool 执行；只有裸 `tokenless` 能从 Shell 的 `PATH`
解析时，Adapter 才启用可恢复压缩；DSH 还要求它解析到 Core 调用选中的同一个可执行文件。
AgentScope 则使用静态恢复 Tool，并对照模型当前的
`visible_markers` 集合授权。旧的无状态 MCP Server 无法获得可信模型可见性上下文，因此已经
删除。以下情况会失去可恢复性：

- 使用了 `--no-stash`。
- 压缩处于 dry-run 模式。
- Stash 数据库不可用或写入失败。
- 条目已经超过 TTL。
- 有效条目超过 10,000 个后，较早条目被容量策略淘汰。
- 调用方使用了不同的 Stash 数据库路径。
- 在 DSH 中，裸 `tokenless` 不存在于稳定的绝对 `PATH` 项中，或解析到与
  `tokenlessBin`/`TOKENLESS_BIN` 不同的可执行文件。

Stash 并不能让所有压缩都可逆。被移除的 `debug`/`trace` 字段、`null` 和空值、Schema `title`/`examples` 以及 Markdown 格式不会保存供取回。启用实际压缩前，应使用有代表性的数据验证关键 Payload。

### 普通处理错误通常 fail-open

缺少 `tokenless` 或 `rtk`、压缩无收益时，压缩和重写 Hook 通常不返回修改。Protocol v2
`compress` 的正常未应用结果使用退出码 `0`；Transport 格式错误退出 `2`；RTK Timeout、
未授权 Retrieve、Stash 或 Pipeline 失败退出 `1`，且不输出 Response JSON。Tool Ready 会在
旧版检查、修复和阻断逻辑之前硬退出；工具执行后的失败归因是独立能力，保持不变。

命令重写也会改变宿主提交的 Shell 命令。大多数 Adapter 会直接替换命令输入；Hermes 会先阻止第一次调用，再提示 Agent 使用改写命令重试。因此，除了压缩结果，还应验证重要命令工作流。

## 支持的 Agent Adapter

| Agent 产品 | 集成方式 | 当前代码路径 |
|------|----------|--------------|
| cosh | Extension | Tool Ready（已硬关闭）、命令重写、Schema；Cosh-NG 替换符合条件的 Pipeline 输出并支持 Marker 命令恢复，旧版 Copilot Shell 透传工具后输出 |
| OpenClaw | Plugin | Tool Ready（已硬关闭）、`exec` 命令重写、替换持久化结果、可选 TOON；无 Schema |
| Hermes | Plugin | Tool Ready（已硬关闭）、Core-owned 阻止后重试改写、用 Core 选择的 TOON 替换结果、Marker 命令恢复；无 Schema |
| Qoder | Plugin | Tool Ready（已硬关闭）、命令重写、通过 `updatedToolOutput` 交付响应 Pipeline 和 Marker 命令恢复；无 Schema |
| Claude Code | Marketplace Plugin | Tool Ready（已硬关闭）、Bash 命令重写；Claude Code 2.1.121 及以上可替换响应并支持 Marker 命令恢复；条件式 TOON；无 Schema |
| Codex | Plugin | Tool Ready（已硬关闭）、RTK 命令重写、环境失败诊断；不替换响应/TOON，无 Schema |
| OpenCode | Plugin | Tool Ready（已硬关闭）、Bash 命令重写、用响应压缩 + TOON 替换工具输出、Marker 命令恢复、Schema |
| Qwen Code | Extension | Tool Ready（已硬关闭）、命令重写；当前宿主缺少工具后替换能力，并跳过声明的 BeforeModel 事件 |
| DeepSeek Harness | 原生 Plugin | 单文本结果替换、Marker 命令恢复和环境错误归因；无 Schema 或命令重写 |

## 支持的 Agent 开发框架

| 框架 | 集成方式 | 当前代码路径 |
|------|----------|--------------|
| AgentScope | 进程内 Python Middleware | 通过独立 Python 包替换成功的最终工具响应，并提供受 marker 约束的恢复 Tool |

## 按任务查找文档

| 我想做什么 | 文档 |
|------------|------|
| 第一次安装并验证 | [快速开始](QUICKSTART.md) |
| 从源码构建独立 CLI | [本页 · 从源码构建独立 CLI](#从源码构建独立-cli) |
| 使用进程内 Python SDK | [Python SDK](sdk.md) |
| 集成 AgentScope | [AgentScope SDK 集成](sdk/agentscope.md) |
| 接入 Agent 产品 | [Agent 集成](framework-integration.md) |
| 手动压缩或取回 | [CLI 参考](cli-reference.md) |
| 了解压缩何时触发、阈值多大 | [本页 · 压缩的触发条件与阈值](#压缩的触发条件与阈值) |
| 查看节省或内容变化、做双跑对比 | [效果度量](measuring-savings.md) |
| 修改配置或了解本地数据 | [配置与数据隐私](configuration-and-privacy.md) |
| 解决无统计、Adapter 或 Stash 问题 | [故障排查](troubleshooting.md) |
| 排查 Schema 压缩没有记录 | [故障排查 · Schema 压缩没有统计记录](troubleshooting.md#schema-压缩没有统计记录) |
| 升级或卸载 | [故障排查 · 升级与卸载](troubleshooting.md#升级与卸载) |

## 推荐的上线顺序

1. 在非敏感测试任务中完成[快速开始](QUICKSTART.md)。
2. 使用 dry-run 记录同一任务的基线。
3. 开启真实压缩并比较结果质量与节省。
4. 确认本地数据和 SLS 策略符合要求。
5. 再为生产使用的 Agent 启用 Adapter。

Tokenless 的配置和 CLI 以当前安装版本的 `tokenless --help` 为最终依据。

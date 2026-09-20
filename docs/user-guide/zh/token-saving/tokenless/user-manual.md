# Tokenless 用户手册

[English](../../../en/token-saving/tokenless/user-manual.md)

Tokenless 面向工具调用密集的 AI Agent。它的 CLI 可以精简 Schema 和工具响应，Adapter 还可以改写 Shell 命令，并把压缩结果交给 Agent。最终效果取决于宿主框架：受支持的 Adapter 会替换原始结果，其余 Adapter 原样透传，至多为失败的命令追加环境诊断。

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
| Schema 压缩 | 移除 `title` 和 `examples`，删除描述中的代码，合并空白并截断描述 | 只在直接 `compress-schema` 命令，以及声明了静态恢复 Tool 的进程内集成（QwenPaw、AgentScope）上生效；cosh、Cosh-NG 和 OpenCode 逐工具路径共用的 BeforeModel Hook 没有 Marker 授权恢复，因此原样返回工具，Qwen Code 不运行该 Hook |
| Content-aware 响应压缩 | 成功的 JSON、已识别的构建/测试日志、CSV/TSV 表格、受支持的搜索列表，以及需显式开启的 Git Diff 和 HTML 域各有自己的压缩器；只接受端到端更小的结果 | 其他内容与 Tool Error 透传；可恢复缩减需要经 Framework 或 Shell 命令的 Marker 授权取回 |
| 搜索路径共享 | API 搜索记录（含 Claude 原生 Grep）的连续行共享完整路径，保留收到的全部文本与位置 | 默认开启；需要 API 响应来源、文本替换能力及无上下文记录；文件和命令输出不进入此域 |
| TOON 编码 | 编码 JSON；估算 Token 没有下降时保留 JSON 输入 | 宿主支持文本替换时替换原文；无替换能力的宿主透传 |
| 命令重写 | 有匹配规则时调用 `rtk rewrite`，再向框架提交改写后的 Shell 输入 | 已识别的构建/测试命令保持原生输出交给 Build Log；其他无规则或被拒绝的改写透传 |
| Tool Ready | 旧版调用前能力，用于检查声明的二进制、版本、配置、权限和可选依赖 | 已硬关闭；不会检查、修复或阻断工具调用 |
| Stash | 保存截断移除的内容、省略的日志段，以及缩减视图（记录缩减、表格行缩减、Diff 裁剪、HTML 转写）背后的完整原文 | 默认 TTL 一小时、最多 10,000 个有效条目；其他被移除字段不会进入 Stash |

代码没有提供固定节省率保证。结果取决于 Payload、Adapter 交付语义，以及工具数据在模型上下文中的占比。请按[效果度量](measuring-savings.md)使用自己的工作负载测量。

## Tokenless 如何参与一次工具调用

启用对应 Adapter 后，一次工具调用可能经过以下阶段：

```text
工具调用前：已识别的构建/测试命令保持原样；其他受支持命令由 RTK 改写
工具调用后：旁路检查 → 领域压缩器 → 可选 Stash/TOON → 写入统计
模型调用前：Schema 压缩 → 提取可见 Marker
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
3. 响应长度达到最小阈值。Core 在共享响应 Hook、OpenClaw 和 Hermes 路径上跳过短于 200 字符的响应。长度按字符数而非字节数计算。
4. 内容命中受支持的压缩域。JSON 对象和数组走按阈值截断的响应压缩；纯文本只有命中匹配的文本压缩器才会被压缩：构建/测试日志、CSV/TSV 表格、API 搜索列表，以及需显式开启的 Git Diff 和 HTML 域（见[Adapter 处理规则](framework-integration.md#adapter-处理规则)、[CSV/TSV 视图可能不完整](#csvtsv-视图可能不完整)和[控制搜索路径共享](#控制搜索路径共享)）。哪些结果能到达 Core 因宿主而异：共享 Hook 会在 Shell 信封中最大的 `stdout` 或 `stderr` 字段至少 2,000 字符时拆出该字段（以 `diff --git` 开头的 Bash `stdout` 不受该下限限制），Hermes 拆出 `output` 字段，两者压缩后都回填到同一信封；更小的信封整体作为 JSON 交给 Core。OpenClaw 压缩纯字符串和单文本块的工具结果，跳过其他工具结果且不调用 Core，并把结构化对象作为 JSON 整体交给 Core 且不做文本替换。带 YAML frontmatter 的 Skill 文件透传。
5. 压缩结果严格小于原文。响应压缩和 TOON 编码都没有让内容变小时，保留原文。

通过上述检查后，截断强度由 Adapter 为该工具上报的内容来源决定。阈值由 Core 持有、对所有 Adapter 相同；类别背后的工具名列表因 Adapter 而异，例如 DSH 和 QwenPaw 使用各自内置表，QwenPaw 把 MCP 服务器等未知工具视为文件内容并透传，而 AgentScope 对没有声明契约的工具直接报错：

| 类别 | 代表工具 | 字符串截断阈值 | 数组截断阈值 | 最大嵌套深度 |
|------|----------|----------------|--------------|--------------|
| 内容读取类 | Read、Glob、Grep、LSP、NotebookRead 及别名 | 跳过压缩 | — | — |
| Shell/exec | Bash、Shell、exec、terminal 等 | 65,536 字符 | 128 项 | 8 |
| 其他结构化工具 | 未列入前两类的工具 | 1,048,576 字符 | 65,536 项 | 32 |

阈值含义：字符串超过阈值时从阈值处截断；数组长度超过「阈值 + 8 项尾部窗口」时保留头尾并在中间插入标记；至少包含 33 个 JSON Object 的数组改走记录缩减，保留约 32 条记录的选集并把完整数组写入 Stash；超过深度上限的子树折叠为标记。截断和记录缩减只在启用 Stash 且宿主声明了恢复方式（可解析的 `tokenless retrieve` Shell 命令或静态恢复 Tool）时运行；没有恢复方式时 Core 保留全部元素和记录，只做无损清理。完整规则与参数见 [CLI 参考](cli-reference.md)。

几点路径差异：

- 独立运行 `tokenless compress-response` 时使用 CLI 自身默认值（字符串 4,096 字符、头部窗口 32 项 + 尾部窗口 8 项、深度 8），可用 `--truncate-strings-at`、`--truncate-arrays-at`、`--array-tail-preserve`、`--max-depth` 覆盖，详见 [CLI 参考](cli-reference.md)。
- Codex 和 Qwen Code 在当前 PostToolUse 契约下无法替换模型可见的原始输出，因此不运行响应压缩和 TOON：Codex 保留原文，只对被归类的环境失败附加上下文；Qwen Code 原样透传。各集成的实际能力详见下方适配器表格。
- OpenClaw Plugin 按同一套共享分类把工具映射为内容来源（文件内容、命令输出或 API 响应）。它不声明 Marker 恢复，因此启用压缩时 Core 会把任何截断或记录缩减判定为不可恢复并拒绝，OpenClaw 的结果只可能被无损 JSON 清理和 TOON 替换；它原有的 `skip_tools`、`shell_tools` 选项已不存在，当前选项见[配置与数据隐私](configuration-and-privacy.md)。
- TOON 编码是独立的触发判断：只对至少 500 字符的负载、且宿主槽位接受文本时运行，并且只有编码结果比当前内容更小时才会采用。
- Git Diff 上下文裁剪和 HTML 页面转写是独立的可选判断，默认关闭；开关与行为见[配置与数据隐私](configuration-and-privacy.md)。
- Python SDK 与 AgentScope 层不通过 Python 配置设置上述阈值：压缩阈值、内容检测和 TOON 选择都是 Core 行为；直接调用 `TokenlessRuntime.compress_response` 时仍可按次覆盖截断参数。详见 [Python SDK](sdk.md) 与 [AgentScope 集成](sdk/agentscope.md)文档。

### 控制搜索路径共享

API 搜索路径共享默认开启。在 Agent 进程环境中设置 `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0`，
可通过 CLI 关闭该功能。未设置时保持开启，`1`、`true`、`yes`（不区分大小写）也表示开启；
空值和其他值均关闭。该设置独立于 `config.json`。Python SDK 可使用
`TokenlessConfig(search_path_sharing_enabled=False)` 关闭；Rust 将
`RuntimeConfig.search_path_sharing_enabled` 设为 `false`。所有入口均默认开启。

关闭此功能时搜索列表原样返回。精确名称 `Grep` 无论路径共享是否开启都不进入 JSON、表格和
日志压缩器，以保留已收到命中。文件读取和命令输出（包括没有 RTK 的 Bash）均不进入搜索
路径共享。整任务节省取决于工作负载；搜索结果变小并不保证总 Token 用量更低。

### CSV/TSV 视图可能不完整

宿主支持用文本替换输出时，成功的 CSV/TSV 工具结果可以被压缩。文件来源结果、失败工具、
已由 RTK 优化的输出以及 Retrieve 输出透传。支持的表格必须有表头和至少两条等宽数据行，
且逗号或制表符分隔格式没有歧义。此压缩器不处理引号格式错误、分隔符有歧义、单列文本、
Markdown 或定宽表格。

全量压紧保留所有单元格字符串，包括空单元格、重复表头、前导零和大数值字符串。
它移除非必要引号并规范化记录分隔符；单元格内部的换行保持不变。
这保证单元格等价，不保证原始字节一致。全量视图的估算 Token 节省达到 15% 时优先采用。

行筛选只在表头形似列名（以字母或 `_` 开头、其余只含字母、数字、`_`、`-`、`.` 的单词；
允许空列名和重复列名，但至少一个非空）时进行；含空格或句子标点的表头保留全部行，避免对源码和逗号分隔的散文采样。该保守规则
也会跳过部分真实表格。

否则，超过 32 条数据行的表格可保留首尾各四行、含诊断关键词的行，并均匀选择普通行补足
32 行基础预算；受保护行可以超出该预算。表格外的提示说明保留行数和总行数、选中的数据行区间（从 1 开始、不含表头）
以及恢复方法；完整原始 CSV/TSV 存入 Stash。完整枚举或计算前应先恢复原文：选定行只是一个
不完整视图。缺少恢复能力、Stash 写入失败或行号区间列表超过 1 KiB 时，只允许全量压紧或原文透传。

计入提示后，缩减候选的字符数和估算 Token 数必须同时小于原文及全量视图。
这些检查不保证在所有模型的 Tokenizer 下都有节省。

### 原生 Grep 保留收到的全部命中

Claude Code 2.1.121 及更新版本的原生 Grep 文本结果可以共享重复文件路径。
`File="..."` 头提供后续 `line:text` 记录的完整路径，直到下一个文件头。
收到的全部记录、源码正文、空白和换行均保留。仅采用更小的表示，不需要 Stash 条目或回取命令。

首版支持至少三条记录、路径不含冒号的无上下文 `path:line:text` 列表。
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
解析时，Adapter 才启用可恢复压缩。AgentScope 则使用静态恢复 Tool，并对照当前模型调用
可见的 Marker 授权。以下情况会失去可恢复性：

- 使用了 `--no-stash`。
- 压缩处于 dry-run 模式。
- Stash 数据库不可用或写入失败。
- 条目已经超过 TTL。
- 有效条目超过 10,000 个后，较早条目被容量策略淘汰。
- 调用方使用了不同的 Stash 数据库路径。
- 在 DSH 中，裸 `tokenless` 不在绝对路径的 `PATH` 项中（`PATH` 中靠前的相对路径项同样会关闭恢复），或解析到与 Plugin 使用的可执行文件不同的文件（见
  [Agent 集成](framework-integration.md#deepseek-harness-原生处理路径)）。

Stash 并不能让所有压缩都可逆。被移除的 `debug`/`trace` 字段、`null` 和空值、Schema `title`/`examples` 以及 Markdown 格式不会保存供取回。启用实际压缩前，应使用有代表性的数据验证关键 Payload。

### 普通处理错误通常 fail-open

缺少 `tokenless` 或 `rtk`、压缩无收益时，压缩和重写 Hook 通常不返回修改；操作失败时
以非零退出码结束且不输出 Response JSON，宿主保留原文（退出码见 [CLI 参考](cli-reference.md#compress)）。
Tool Ready 已硬关闭；工具执行后的失败归因是独立能力，保持不变。

命令重写也会改变宿主提交的 Shell 命令。大多数 Adapter 会直接替换命令输入；Hermes 会先阻止第一次调用，再提示 Agent 使用改写命令重试。因此，除了压缩结果，还应验证重要命令工作流。

## 支持的 Agent Adapter

| Agent 产品 | 集成方式 | 当前代码路径 |
|------|----------|--------------|
| cosh | Extension | 命令重写；Cosh-NG 替换符合条件的 Pipeline 输出并支持 Marker 命令恢复，旧版 Copilot Shell 透传工具后输出；Schema Hook 会运行但原样返回工具 |
| OpenClaw | Plugin | `exec` 命令重写、替换持久化结果、可选 TOON；无 Schema |
| Hermes | Plugin | 阻止后重试改写、用 Core 选择的 TOON 替换结果、Marker 命令恢复；无 Schema |
| Qoder | Plugin | 命令重写、通过 `updatedToolOutput` 替换响应并支持 Marker 命令恢复；无 Schema |
| Claude Code | Marketplace Plugin | Bash 命令重写；Claude Code 2.1.121 及以上可替换响应并支持 Marker 命令恢复；条件式 TOON；无 Schema |
| Codex | Plugin | RTK 命令重写、环境失败诊断；不替换响应/TOON，无 Schema |
| OpenCode | Plugin | Bash 命令重写、用响应压缩 + TOON 替换工具输出、Marker 命令恢复；逐工具 Schema Hook 会运行但原样返回工具 |
| Qwen Code | Extension | 命令重写；宿主没有工具后替换能力，也不运行 BeforeModel 事件 |
| DeepSeek Harness | 原生 Plugin | 单文本结果替换、Marker 命令恢复和环境错误归因；无 Schema 或命令重写 |

凡是注册了 Tool Ready 的 Adapter，该能力都已硬关闭。

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

# Tokenless Agent 集成

[English](../../en/tokenless/framework-integration.md)

Tokenless 通过 Plugin、Hook 和 Extension 接入具体 Agent 产品。本文只说明产品 Adapter。
Python SDK 及其 AgentScope 专用子文档放在 [Python SDK 指南](../../../developer-guide/zh/sdk/tokenless/index.md) 下。

当解析器无法保留某个 JSON 数字的值时，PostTool 会保持整个响应原样。其他响应仍可进行清理和 Record Reduction；Core 仅在 TOON 解码后保留相同的值与类型时采用它。

## Agent Adapter 支持矩阵

| Agent 产品 | 值 | Tool Ready | 命令重写行为 | 响应交付方式 | TOON | Schema |
|------|----|------------|--------------|--------------|------|--------|
| cosh-ng | `cosh` | 已硬关闭 | 替换受支持的 Shell 输入 | Cosh-NG 替换受支持的 JSON 结果；旧版 Copilot Shell 透传 | 对可替换文本由 Pipeline 选择 | Common Hook 仅接受无损结果 |
| OpenClaw | `openclaw` | 已硬关闭 | 替换 `exec` 命令输入 | 在模型消费前替换单文本工具结果 | Core 为可替换文本选择 | — |
| Hermes | `hermes` | 已硬关闭 | 阻止第一次调用并建议使用 Core 返回的改写命令 | 替换已接受的结果或追加错误指引；支持 Marker 命令恢复 | 对可替换文本由 Core 选择 | — |
| Qoder | `qoder` | 已硬关闭 | 输出改写后的 Shell 输入 | 通过 `updatedToolOutput` 替换输出 | 对可替换文本由 Pipeline 选择 | — |
| 千问办公（macOS） | `qwenwork` | — | 替换原生 Bash 和工作区 Shell 输入 | 替换符合条件的完整结果；仅提供遥测的 MCP 包装工具透传 | 对可替换文本由 Pipeline 选择 | — |
| Claude Code | `claude-code` | 已硬关闭 | 替换 Bash 输入 | 2.1.121 及以上替换输出；否则透传 | 对可替换文本由 Pipeline 选择 | — |
| Codex | `codex` | 已硬关闭 | 替换受支持的 Shell 输入 | 保留原文；仅对识别出的环境失败追加上下文 | — | — |
| DeepSeek Harness | `dsh` | 未注册 | 未注册 | 把已接受的单文本结果委托给 Core；支持 Marker 命令恢复 | 对可替换文本由 Core 选择 | 未注册 |
| OpenCode | `opencode` | 已硬关闭 | 替换 Bash 输入 | 替换工具输出 | 对可替换文本由 Pipeline 选择 | ✅ |
| Qwen Code | `qwencode` | 已硬关闭 | 输出改写后的 Shell 输入 | 宿主没有替换字段，因此透传 | — | — |
| QwenPaw | `qwenpaw` | — | 替换 `execute_shell_command` 的输入 | 在 AgentScope 中间件链中替换工具结果的文本块 | 对可替换文本由 Core 选择 | ✅ |

“—”表示该能力不可用：当前 Adapter 没有注册，或当前宿主版本不会运行；对应的 Tokenless CLI 命令仍可能可用。

Cosh-NG 使用的 Adapter 值为 `cosh`，扩展仍使用现有的 `.copilot-shell` 目录。

Schema 压缩到达模型路径的方式因宿主而异：cosh 与 Cosh-NG 触发 `BeforeModel` Hook；OpenCode 通过其 `tool.definition` 插件 Hook 逐个压缩工具定义（MCP 工具不经过该 Hook）；Qwen Code 的清单声明了 `BeforeModel` Hook，但当前 Qwen Code 版本在注册时会跳过这一未知事件名，Schema Hook 实际不会运行，因此矩阵标记为不可用。该条目保留注册，未来 Qwen Code 版本实现该事件后会自动生效。

这些 Adapter 仍会注册 Tool Ready，但会在检查、修复或阻断之前无条件硬退出，任何运行时设置都无法重新启用。工具执行后的失败归因不受影响。

`additionalContext` 是追加型 Hook 字段。共享 Hook 不会把压缩副本放入其中，否则原文
仍然可见，总 Context 反而增加；该字段只用于追加环境错误指引。统计记录只能证明压缩候选
内容变小了，不能单独证明宿主已经从模型请求中移除原文。

## Adapter 处理规则

共享 Cosh-NG、Qoder、千问办公、Claude Code 和 OpenCode PostTool Hook 会向 `tokenless compress`
发送一个 `post_tool` 请求。当宿主可以替换结果，且裸 `tokenless` 能从 Shell 的 `PATH`
解析时，Marker 可以提示模型通过已有 Shell Tool 恢复省略内容；否则 Core 只接受无损候选。
所有非 `applied` Disposition 都保留原文。当前路由如下：

| 内容 | 当前共享 Hook 行为 |
|------|--------------------|
| JSON | 无损结构清理；文本替换槽还会考虑 TOON |
| 需要 Record Reduction 或字符串、数组、深度截断的 JSON | 仅在 Marker 命令恢复可用时应用；否则以 `recoverability_unavailable` 拒绝候选 |
| 来自命令输出的构建/测试/包管理日志 | 终端输出清理与常规进度缩减；每段省略都带有就地取回标记 |
| 宿主支持文本替换时的 CSV/TSV 表格 | 仅在字符数和估算 Token 数同时减少时做保留全部单元格的整表压紧，否则透传；不写 Stash，无需恢复能力 |
| 路径共享开启且宿主支持文本替换时，带行号的 API 搜索记录，或非文件来源命令/API 结果中的纯路径列表 | 无损共享连续路径或目录前缀；保留全部已收到记录，无需 Stash 或恢复调用 |
| 显式开启 `TOKENLESS_DIFF_COMPRESSION_ENABLED`（默认关闭）且宿主支持文本替换时，来自命令输出的 Git Diff | 按 Hunk 选择裁剪未变更上下文；保留全部变更行，完整原文经 Stash 可取回，收益过小的候选会被拒绝 |
| 宿主支持文本替换且 Stash 恢复可用时，来自命令输出或 API 响应（不含文件读取）的完整 HTML 文档；默认开启，`TOKENLESS_HTML_EXTRACTION_ENABLED=0` 关闭 | 主内容转写为 Markdown，视图头部写明移除计数；完整原文经 Stash 可取回，收益过小的候选会被拒绝 |
| 长纯文本、Stack Trace、源码、Unknown | 对应领域 Compressor 接入前原样透传 |

内容检测、PostTool 200 字符门禁、基于工具来源的阈值、诊断、TOON 选择和最终接受均属于
Core 策略。Hook 只把宿主对象映射为 v2 字段；它可以跳过明显不是 JSON 的 Skill 文件，避免
无意义地启动子进程。

Common BeforeModel Hook 同样没有 Marker 授权恢复路径。当前 Schema 变换均为有损，因此 Core
原样返回 Tools。OpenCode 独立的逐工具定义路径和直接 `compress-schema` 命令不受影响。

OpenClaw、Hermes 与 DeepSeek Harness 已把 PostTool 决策委托给 Core。独立
`compress-response` 命令继续作为显式 JSON 清理入口。

对于 JSON 响应清理，Adapter 按以下方式把宿主工具映射为 Core 的内容 Origin：

| 类别 | Adapter 默认行为 |
|------|------------------|
| 内容读取类，包括 Read/Glob/Grep/LSP/NotebookRead 别名 | 跳过响应压缩 |
| Shell/exec | 字符串 65,536 字符、数组保留 128 项、深度 8 |
| 其他结构化工具 | 字符串 1,048,576 字符、数组保留 65,536 项、深度 32 |

Common Hook、OpenClaw 与 Hermes 的 PostTool 大小门禁、基于工具来源的阈值和 TOON 选择均归
Core。只有宿主槽支持文本且 Core 找到更小的合法表示时才会使用 TOON。独立
`compress-toon` CLI 和 SDK TOON 路径继续使用文档规定的默认门槛，CLI 可通过
`--min-toon-chars` 为单次调用降低阈值。Codex 和 Qwen Code 当前的 PostToolUse 契约不能
替换原始模型可见输出，因此不运行响应压缩或 TOON。

Common Hook 与 OpenClaw 会把 RTK 所有权传给匹配的 PostTool 调用。Hermes 为兼容旧宿主版本
采用阻止后建议重试；最终结果 Hook 会从 Hermes 实际执行的命令中识别带 Attribution 的 RTK
Wrapper（含可选的执行批次参数）。只有完整回执确认成功调用的输出字节未变，Core 才恢复 PostTool，并用保存的原命令
检查文件来源。宿主必须在 PostTool 的 `command` 中传回注册的完整执行命令；命令缺失或被修改，
以及输出已改变或无法确认时，均保持旁路。三种 Adapter 在回执查询前均为 RTK 输出声明
`recovery: none`，因此回退仅接受无损候选，不会为这些调用启用 Stash/retrieve 恢复。

Claude Code 需要 2.1.121 或更高版本才能使用 `updatedToolOutput`。版本更旧或无法确定时，响应压缩会关闭，以免重复注入原文。结构化工具输出会保留宿主 Schema，不会转换成文本 TOON；以字符串承载的 JSON 在 TOON 更小时可以使用 TOON。

### 千问办公（Windows x64 本地模式）

Tokenless 可在千问办公中自动精简受支持的命令和工具输出，保留文件读取原文。
Windows 支持范围为 x64 上的千问办公 1.3.2 本地会话，需要安装 Node.js/npm 和
Python 3.10+。不支持 Windows ARM64 和安全工作区／VM 会话，其他 Windows Agent
尚未验证。

Windows 安装包尚未发布，目前需按
[Windows 构建说明](../../../developer-guide/zh/tokenless/windows.md#在-windows-上构建和测试)构建并安装本地 npm 包。

安装 npm 包后，退出千问办公，在 PowerShell 中执行：

```powershell
$adapter = Join-Path (npm root -g) 'anolisa-tokenless/adapters/tokenless/qwenwork/scripts/lifecycle.ps1'
& $adapter -Action detect
& $adapter -Action install
```

重启千问办公，在应用内按需启用 Tokenless 插件，再开启新的本地会话。
之后照常使用千问办公，受支持的输出会自动精简。

如果找不到 Python，可在上述命令后添加 `-Python 'C:/path/to/python.exe'`。
千问办公安装在自定义位置时，先将 `TOKENLESS_QWENWORK_APP` 设置为包含
`QwenWorkCN.exe` 的目录，再执行安装命令。

卸载时先退出千问办公，执行以下命令后再重启应用：

```powershell
& $adapter -Action uninstall
```

### 千问办公（macOS）

`qwenwork` Adapter 面向千问办公 QwenWorkCN，与 Qoder CLI、Qwen Code、QwenPaw 独立。
已在 QwenWorkCN 1.2.5 的 macOS 本地会话中实测 Bash 改写、完整 MCP 响应替换、
JSON 转 TOON 输出及 Read 透传。适配器也接受 QwenWorkCN 1.3.2 中观察到的
macOS 工作区 Shell 契约。Linux VM 执行仍不受支持，Hook 在非 macOS 环境直接透传。

PreToolUse 匹配 `Bash`、`mcp__workspace__bash` 和
`mcp__qw-builtin__mcp_workspace_bash`，使用 RTK 改写受支持的命令。PostToolUse **不设置工具 matcher**：
符合条件的完整 Bash 及非 Bash 结果进入公共策略。`mcp_list`、`mcp_get`、`mcp_call`
仅包含遥测信息的回调保持透传，避免用遥测替换真实内容；完整的 `mcp_call` 响应仍可正常压缩。
被接受的结果通过 `updatedToolOutput` 替换原文；对象和数组保留类型，文本槽可以使用 TOON。
文件内容保护、不支持内容透传及 RTK 去重规则保持不变。统计使用独立 Agent ID `qwenwork`，
不注册 Tool Ready 或 Schema Hook。

工作区 Shell 的结果恰好包含一个普通 MCP 文本块时，内容进入文本槽，支持 JSON 转 TOON。
多个块、混合媒体或带注解的块数组保持透传，其他 MCP 工具保留已有结构化结果处理。
工作区 Shell 的统计及 Pre/Post 去重状态保存在 `<cwd>/.tokenless`；原生 Bash 保留原有数据位置。
独立安装在插件内提供 `bin/` 时，适配器使用这些二进制。模型 Shell 不会继承 Hook 的环境，
因此工作区 Shell 或插件私有二进制不声明裸 `tokenless retrieve` 恢复能力，PostTool 只应用无损压缩。

先通过 `anolisa install tokenless` 安装 Tokenless。本 Adapter 需要 Bash、Python 3.9+
和 Tokenless 二进制。命令改写需要 RTK，响应压缩不依赖 RTK；缺少 RTK 会显示警告。
确保桌面会话能够找到这些二进制，而不只是交互式终端能够找到。

安装或升级前退出千问办公，然后执行：

```bash
anolisa adapter scan
anolisa --dry-run adapter enable tokenless qwenwork
anolisa adapter enable tokenless qwenwork
anolisa adapter status tokenless
```

CLI 将自包含插件复制到 `~/.qwenworkcn/plugins-custom/tokenless` 并记录文件归属。
完成后重启千问办公、开启新会话，并在应用内检查插件激活状态。
`status` 验证应用存在及受管理文件完整性，加载和激活状态保持 `unknown`。
旁加载不修改 settings、权限、数据库或禁用／阻止列表。兼容性基线为 1.2.5 原生 Bash 和 1.3.2 工作区 Shell；
其他版本会显示兼容性未验证的警告。

`TOKENLESS_QWENWORK_APP` 指定非默认应用 Bundle；`TOKENLESS_QWENWORK_HOME` 指定数据目录。
默认在 `/Applications` 和 `~/Applications` 查找应用，数据目录为 `~/.qwenworkcn`。
使用绝对路径（支持开头的 `~`）。status、disable 和重新 enable 时保持同一数据目录；
需要迁移时，先使用原配置 disable。不要指向 Qoder CLI 或 Qwen Code 的配置目录。

CLI 不接管没有自身 receipt 的已有安装。若此前使用随包脚本或 `make qwenwork-install`，
先用原脚本卸载，再通过 CLI 启用（按实际安装位置调整资源路径）：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/qwenwork/scripts/uninstall.sh
anolisa adapter enable tokenless qwenwork
```

CLI 管理的副本使用以下命令卸载：

```bash
anolisa --dry-run adapter disable tokenless qwenwork
anolisa adapter disable tokenless qwenwork
```

卸载不依赖应用、源 Bundle 或 Hook 运行依赖仍然存在。只移除未修改的 receipt 归属文件及
空插件目录，无关文件保留。修改过的文件会被保留，同时保留清理失败的 receipt：
恢复或移除报告的冲突后重试。重新 enable 拒绝覆盖用户修改；复制中断会保留 receipt，
供重试或清理。卸载后同样需要重启应用。

随包脚本及源码 Checkout 的 `make qwenwork-install` / `make qwenwork-uninstall`
仍是独立的备选方式。CLI 副本不包含这些生命周期脚本及其归属标记。两种方式均不调用
内嵌 Qoder CLI 或未公开的桌面 API。需要恢复独立脚本管理时，先 disable CLI 管理的
Adapter，再使用独立安装器。

### DeepSeek Harness 原生处理路径

DSH Bundle 要求 Node.js 22 或更高版本，并需要兼容的 DSH profile。应在同一条
enable 命令中列出全部目标 profile，随后使用其中一个名称启动 DSH。

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
dsh --profile web
```

`--profile` 是必填且可重复的参数。每次 enable 或 re-enable 都会把本次参数视为
完整目标集合。旧 receipt 中已有但新命令没有列出的 profile 会卸载 Bundle，因此
每次都要列出需要继续使用 Tokenless 的全部 profile。ANOLISA 会把选择的 profile
和解析后的 DSH home 写入 adapter receipt。后续 status、disable 和 re-enable 会
继续操作同一棵 profile 目录树。

Plugin 在 DSH 的 `tools/post-execute` Waterfall 上运行，并把包含一个文本块、可替换的
根调用结果发送给 `tokenless compress`。内容检测、JSON 与 Build Log 压缩、TOON 选择、
大小门禁、基于工具来源的阈值和最终接受均由 Core 负责。不受支持的内容域与文件内容结果会
透传。当裸
`tokenless` 能从 DSH Shell 的 `PATH` 解析到 Core 调用选中的同一个可执行文件时，Marker
可以提示模型执行一条独立的 `tokenless retrieve` 命令；成功的恢复输出会绕过压缩。多文本块、图片、Code Mode 子调用的
成功结果，以及后续 Waterfall Listener 已替换的 Canonical Value 均保持不变。CLI 缺失、
失败或超时也会保留原始内容。

DSH 会从模型 Shell 命令中移除继承的 `TOKENLESS_*` 环境变量。Adapter 会发布选定数据目录
以及可选统计库和 Stash 库路径的受控别名，让 Core 与 Shell 使用同一份恢复状态。默认状态
位于会话工作区下的 `.tokenless`。Adapter 会创建内容为 `*` 的
`.tokenless/.gitignore`，避免完整工具文本与 Stash Payload 被 `git add -A` 纳入提交。
如需自定义，应在启动 DSH 前设置 `TOKENLESS_DATA_DIR`、`TOKENLESS_STATS_DB` 或
`TOKENLESS_STASH_DB`，路径必须是其 Shell 沙箱可访问的绝对路径；自定义路径需要按仓库策略
自行保护并排除。

在 `$DSH_HOME/profiles/<profile>/cordis.patch.yml` 中覆盖安装后的 row，然后重启
对应的 DSH profile。

```yaml
- id: anolisa-tokenless
  config:
    responseCompressionEnabled: true
    timeoutMs: 5000
    maxBuffer: 4194304
```

后续 DSH patch layer 会替换该 row 的完整 `config` 值。Plugin 会为省略的 key 提供
默认值，因此只需写出准备修改的 key。

| 配置项 | 默认值 | 行为 |
|--------|--------|------|
| `responseCompressionEnabled` | `true` | 控制响应压缩。设为 `false` 后，环境错误归因仍保持启用。 |
| `tokenlessBin` | `$TOKENLESS_BIN`，随后使用 `tokenless` | 选择 Tokenless CLI 可执行文件。非空 Plugin 配置优先于环境变量。Marker 恢复还要求 Shell `PATH` 中的裸 `tokenless` 解析到同一个文件。 |
| `timeoutMs` | `3000` | 限制一次 Tokenless 子进程的运行时间，单位为毫秒。只接受正整数。 |
| `maxBuffer` | `2097152` | 限制捕获的子进程输出，单位为 byte。只接受正整数。 |
| `agentId` | `dsh` | 设置 Tokenless 统计记录中的 Agent Attribution。 |

Plugin 把 DSH 内置的读取/搜索工具映射为 `file_content`，命令工具映射为
`command_output`，未知工具映射为 `api_response`。这些映射只描述宿主事实，后续策略由
Core 决定。即使压缩关闭，DSH 原始失败和结构化命令失败仍会交给 Core 做环境诊断。
后续 Waterfall Listener 替换 Canonical `value` 后，Tokenless 只检查该替换值，且不会对其
应用内容压缩。

完整触发条件（压缩开关、最小响应长度、受支持的压缩域、严格变小保护）与阈值含义见[用户手册 · 压缩的触发条件与阈值](index.md#压缩的触发条件与阈值)。

## 通过 anolisa 管理（推荐）

这些命令需要 ANOLISA 组件记录。如果 Tokenless 是通过 YUM 直接安装的，
继续操作前先记录该 RPM。

```bash
sudo yum install anolisa
sudo anolisa --install-mode system adopt tokenless
```

YUM 安装的 CLI 位于 `sudo` 可见的系统路径。`get.agentic-os.sh` 安装在用户
目录中的 CLI 可能会被 `sudo` 的 `secure_path` 隐藏。

后续 adapter 命令请用拥有目标 Agent 配置的用户执行。user scope 的 adapter
操作可以读取已采纳的 system 软件包，同时把框架改动留在当前用户的配置中。

### 1. 扫描 Agent 产品

```bash
anolisa adapter scan
```

如果目标框架未出现，先确认其 CLI 或应用已经安装，再重新扫描。

### 2. 启用一个 Adapter

```bash
anolisa adapter enable tokenless <framework>
```

例如：

```bash
anolisa adapter enable tokenless cosh
anolisa adapter enable tokenless openclaw
anolisa adapter enable tokenless hermes
anolisa adapter enable tokenless qoder
anolisa adapter enable tokenless claude-code
anolisa adapter enable tokenless codex
anolisa adapter enable tokenless opencode
anolisa adapter enable tokenless qwencode
anolisa adapter enable tokenless qwenpaw
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

只需启用实际使用的 Agent 产品。多个产品应分别执行并验证各自的命令。DSH 的全部
目标 profile 应写在同一条 enable 命令中。

DeepSeek Harness 按 profile 管理，因此必须至少提供一个 `--profile`。每个名称应与
`dsh --profile <profile>` 使用的名称一致，不带 profile 的通用命令会被拒绝。
后续 enable 或 re-enable 必须再次列出需要保留的全部 profile。

执行 OpenClaw adapter enable 或 tokenless 的 OpenClaw `install.sh` 即同意
插件声明的能力。两个入口仅在 `plugins install --help` 列出完整的
`--accept-capabilities` 参数时传递它，以兼容旧版宿主。独立的 `install.sh`
也只在安装器仍声明该参数有效时传递 `--dangerously-force-unsafe-install`；
将其列为 deprecated no-op 的宿主（OpenClaw 2026.6.5+）不再收到该参数，
安全扫描改由 `security.installPolicy` 决定。

对于 OpenClaw，anolisa 会先尝试普通安装，默认不会加入 unsafe-install 覆盖参数。如果 OpenClaw 的安全扫描拒绝此 Plugin，应先阅读其报告；确认接受风险后，才显式重试：

```bash
anolisa adapter enable tokenless openclaw \
  --allow-unsafe-plugin-install
```

如果当前 OpenClaw 不支持底层覆盖参数，或已把它标记为无效的废弃选项，anolisa 会拒绝上述参数；此时应按照错误中的 `security.installPolicy` 指引处理。

组件软件包可以安装在 system scope，adapter receipt 仍由当前用户管理。只有
目标框架配置和 receipt 都明确归 root 所有时，才需要使用 `sudo`。

### 3. 检查状态

```bash
anolisa adapter status tokenless
anolisa doctor tokenless
```

完成后重启目标 Agent CLI 或 IDE。已经运行的会话通常不会动态载入刚安装的 Hook/Plugin。

### 4. 禁用

```bash
anolisa adapter disable tokenless <framework>
```

请用启用 adapter 的同一用户执行禁用操作。只有 root 管理的 receipt 需要在
两个操作中都使用 `sudo`。

禁用后重启目标 Agent。卸载 Tokenless 前必须先释放所有已启用的 Adapter。

## npm 安装后的手动接入

npm 的 postinstall 脚本会尝试把 Adapter 资源复制到：

```text
~/.local/share/anolisa/adapters/tokenless/
```

应确认该目录确实存在。Adapter 复制属于补充步骤，失败时只输出警告，不会让二进制安装失败；因此可能出现命令可用但这里没有资源副本的情况。目录缺失时应检查 npm postinstall 警告，并优先改用 anolisa 管理的安装。

npm 安装不会创建 anolisa 组件安装记录，因此不要假设 `anolisa adapter enable` 能管理这次安装。OpenClaw、Hermes、Qoder、Claude Code、Codex、OpenCode、Qwen Code 和 QwenPaw 可以运行各自的安装脚本：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/install.sh
```

例如：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh
bash ~/.local/share/anolisa/adapters/tokenless/opencode/scripts/install.sh
```

卸载相同 Adapter：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/uninstall.sh
```

脚本会调用框架自身的 Plugin/Extension 机制；按照脚本输出完成重启。安装脚本缺失、失败或框架版本不兼容时，优先改用 anolisa 管理的安装方式。

在安装器仍执行安全扫描的宿主上，OpenClaw 安装脚本会带 `--dangerously-force-unsafe-install` 调用 `plugins install`，因为 Plugin 通过 Node.js 子进程 API 启动 `tokenless` 和 `rtk` 二进制；将该参数列为 deprecated no-op 的宿主不再收到该参数，安全扫描由 `security.installPolicy` 决定。运行前应审查已安装的 Adapter 源码和 OpenClaw 安全策略。如果策略不允许该覆盖参数，就不要安装此 Plugin。

### npm + Cosh-NG

Cosh-NG 使用 Extension 目录，不提供单独的 `scripts/install.sh`。将 npm 安装的共享资源复制到用户 Extension 目录：

```bash
mkdir -p ~/.copilot-shell/extensions/tokenless
cp -R ~/.local/share/anolisa/adapters/tokenless/common/hooks \
  ~/.local/share/anolisa/adapters/tokenless/common/commands \
  ~/.local/share/anolisa/adapters/tokenless/common/cosh-extension.json \
  ~/.copilot-shell/extensions/tokenless/
```

完成后重启 Cosh-NG。移除前先退出 Cosh-NG，并确认目标目录确实是本次 npm 安装创建的 Tokenless Extension。

## Agent Adapter 生效提示

### Cosh-NG

Extension 在启动时发现。启用后重启 Cosh-NG，并运行一个 Shell 工具任务，再使用 `tokenless stats list` 检查记录。

### OpenClaw

要求 OpenClaw 2026.6.6 或更高版本，并显式启用 Tokenless 插件。插件通过声明的工具结果
middleware，在同轮模型请求之前替换支持的文本结果；不另外压缩持久化副本。

安装脚本会在旧版宿主上使用上文说明的 OpenClaw unsafe-install 覆盖参数。确认风险并安装后，重启 Gateway。Plugin 代码默认启用响应压缩和 RTK 重写；Core 会选择符合条件的无损 TOON 候选。由于底层检查已硬关闭，Plugin 的 Tool Ready 选项当前不会生效。

Tokenless 只处理 OpenClaw 交付的结果。2026.8.2 等旧版宿主只要注册了适用的结果
middleware，就会在回调前将每个文本块裁剪到 100,000 个 UTF-16 Code Unit，即使
middleware 不返回替换内容也是如此。过大的 `details` 也可能被截断，丢失退出状态等
字段。Fail-open 保留的是收到的结果，无法恢复宿主已经移除的文本或元数据。因此，被
截断的 JSON 可能直接透传而不压缩。这些宿主仍在该限制内受支持。

如需在此边界接收完整的原生文本结果，建议使用包含
[上游提交 `2f620b970a04`](https://github.com/openclaw/openclaw/commit/2f620b970a0458531588d6c8067540164197580e)
的 OpenClaw 构建。已验证构建和此前发布版均报告 `2026.10.2`，仅凭版本字符串不能
确认修复是否存在。旧式嵌套 `toolResult` 归一化、工具自身和下游模型上下文预算仍可能
限制输出。Tokenless 不会自动读取保存完整输出的文件，也不能恢复已丢弃的尾部。
设置 `post_tool_enabled: false` 可停止 Tokenless 注册该 middleware；其他插件仍可能
注册自己的 middleware。

可选的 `selective-claw` 上下文引擎在 `plugins.entries.selective-claw.config` 中配置，
与响应压缩适配器的配置相互独立：

```json
{
  "plugins": {
    "entries": {
      "selective-claw": {
        "config": { "enabled": true, "freshTailTurns": 3, "dbPath": "~/.openclaw/selective-claw.db" }
      }
    }
  }
}
```

`~/` 会展开为当前用户的主目录。未设置 `dbPath` 时，归档为 `OPENCLAW_STATE_DIR` 下的
`selective-claw.db`；该环境变量未设置时使用 `~/.openclaw`。将插件的 `enabled` 选项设为
`false` 时，不打开数据库，也不注册上下文引擎或召回工具。

### Hermes

Plugin 在 Hermes 新会话中生效。重启 Hermes 后先执行 Shell 工具任务验证阻止后重试改写，
再执行返回 JSON 的工具验证结果替换。当裸 `tokenless` 能从 Shell 的 `PATH` 解析时，Marker
可以提示 Hermes 执行 `tokenless retrieve`；成功的恢复结果不会再次进入压缩。

### Qoder

Qoder IDE 和 qodercli 可能缓存 Plugin 配置。启用或升级后应完全重启 IDE。若出现旧 Hook 路径错误，参阅[Qoder Plugin 缓存问题](troubleshooting.md#qoder-plugin-缓存问题)。

### Claude Code

Marketplace Plugin 在 Claude Code 重启后生效，也可以按照安装脚本提示刷新 Plugin。

### Codex

Plugin 在新的 Codex 会话中加载。关闭旧会话并重新启动后验证行为。Codex 的 PostToolUse Hook 不能替换或抑制原始输出，因此 Plugin 不追加压缩内容，也不记录响应压缩候选，只对识别出的环境失败追加上下文。真正的首轮节省来自 RTK 在执行前重写受支持的 Shell 命令。

### DeepSeek Harness

原生 Bundle 会在选定的 DSH profile 启动时加载。启用 Bundle 或修改 profile patch
后，重启 `dsh --profile <profile>`，运行一个返回可压缩 JSON 的工具，再检查
`tokenless stats list`。禁用命令是 `anolisa adapter disable tokenless dsh`。
receipt 已经记录 profile 名称，因此 disable 不再接受 `--profile`。

### OpenCode

Schema 压缩使用宿主提供给模型的 JSON Schema，并保留运行时参数校验器。
仅提供运行时校验器的宿主会跳过 Schema 压缩；响应压缩和命令重写仍然可用。

OpenCode 启动时会自动加载配置目录下的 Plugin。通过 ANOLISA 管理安装时，使用：

```bash
anolisa adapter enable tokenless opencode
anolisa adapter status tokenless
anolisa adapter disable tokenless opencode
```

内置 driver 按 `OPENCODE_CONFIG_DIR`、`XDG_CONFIG_HOME/opencode`、`~/.config/opencode`
的顺序解析配置目录。它不读取 `TOKENLESS_OPENCODE_CONFIG_DIR`；如需与独立脚本共用自定义目录，
请设置 `OPENCODE_CONFIG_DIR`，禁用时也保持相同的目录设置。

上述 Bundle 生命周期脚本仍可用于 npm 和手工安装，源码构建可使用 `make opencode-install`。
这些脚本额外支持 `TOKENLESS_OPENCODE_CONFIG_DIR`，其优先级最高。两种方式都会创建
`plugins/tokenless.js`，并拒绝覆盖冲突的文件或链接。ANOLISA enable 会将已存在且指向同一
插件源文件的链接纳入 receipt 管理，之后 disable 会删除该链接。链接按原目录展开相对路径后，
必须与记录的源路径一致；通过目录别名或不同安装前缀指向源文件的链接会作为冲突保留。
这样可以确保源目录删除后仍能清理。

通过 ANOLISA 启用前，请使用原安装 profile 以及原有的 `PREFIX` 或 `SHARE_DIR` 覆盖值
卸载冲突的独立链接。例如，在 Tokenless 源码目录中移除使用默认 system 前缀安装的链接：

```bash
make opencode-uninstall INSTALL_PROFILE=system PREFIX=/usr
anolisa adapter enable tokenless opencode
```

两条命令都应保持原配置目录设置。如果使用 Bundle 中的 `scripts/uninstall.sh`，请运行
原 adapter Bundle 内的脚本；若设置了 `ANOLISA_ADAPTER_DIR`，它也必须与原目录匹配。
从其他前缀卸载会保留链接，输出 warning 并以 0 退出；启用前请确认链接已移除。如果原 Bundle
已不可用，请用 `readlink` 检查 `plugins/tokenless.js`，仅手动移除确认过的旧符号链接，
保留无关文件和目录。

若要恢复独立脚本管理，请先完成 `anolisa adapter disable tokenless opencode`，包括所有
待恢复的操作。清理成功前应保留错误中报告的恢复目录；独立脚本不会恢复 ANOLISA 的 receipt
或 journal。之后再运行 `make opencode-install` 或 Bundle 中的 `scripts/install.sh`。

启用或禁用后请重启 OpenCode：已有进程会保留加载的插件及其工具输出替换行为，直到重启。
启用并重启后，执行一次工具调用，再运行 `tokenless stats list` 检查统计记录。

### Qwen Code

Extension 在新的 Qwen Code 会话中加载。重启后执行一次工具调用验证。

### QwenPaw

该 Adapter 是一个 QwenPaw Plugin：`anolisa adapter enable tokenless qwenpaw` 和自带的安装脚本都会执行
`qwenpaw plugin install <bundle> --force`，由 QwenPaw 把插件复制到 `<工作目录>/plugins/tokenless/`，
并把其 `requirements.txt` 安装进 QwenPaw 自己的 Python 环境。该依赖是对应 GitHub Release 中的
`anolisa_tokenless` wheel，因此首次安装需要联网。QwenPaw 只在其解释器的包元数据里找不到 `anolisa_tokenless`
时才运行 pip，所以离线主机可先用 `pip install` 把 wheel 装进 QwenPaw 的 Python 环境；同样的规则意味着已装过的旧版
wheel 不会被 `plugin install` 升级。因此安装脚本会通过 `qwenpaw` 命令背后的解释器确认 `anolisa_tokenless`
可导入且具备插件需要的 SDK 接口，没有 wheel 匹配当前平台（`requirements.txt` 列出 Linux x86_64、Linux aarch64
和 macOS arm64）时安装失败。插件本身也会拒绝在旧版 wheel 上注册并在日志中给出所需的 release，而不是在第一次模型
调用时报错。插件需要 Tokenless 0.8.0 引入的恢复入口。请在 QwenPaw 的 Python 环境中安装与插件 Release 匹配的
SDK Wheel；0.7.14 Wheel 不提供这些 API。工作目录与 QwenPaw 本身的解析一致：`QWENPAW_WORKING_DIR`，否则 `COPAW_WORKING_DIR`，否则已存在的
`~/.copaw`，否则 `~/.qwenpaw`。没有 `qwenpaw` 命令时安装脚本打印提示并以 0 退出，`make setup` 在未安装 QwenPaw
的主机上可以完整跑完。

正在运行的 QwenPaw 会热加载插件；否则启动 QwenPaw 即可。Schema 压缩和 `tokenless_retrieve` 工具从下一次
模型调用开始生效；命令重写发生在 QwenPaw 的审批步骤之后，因此已批准的 `execute_shell_command` 会执行改写后的
命令。只有 QwenPaw 内置工具会被分类：`execute_shell_command` 为命令输出，`read_file`、`recall_history`、`view_image`、
`view_video` 为文件内容，其余内置工具为 API 响应；Skill、MCP 工具以及后续 QwenPaw 版本新增的工具原样
透传。QwenPaw 自己的工具结果裁剪在 Tokenless 之后运行，且保留结果头部（最近两条工具结果 50000 字节，更早的 3000
字节，溢出部分写入 `tool_results/`），因此压缩结果末尾的恢复指令只在结果未超出该预算时可见；被省略的内容仍可用
`tokenless retrieve` 从 Stash 取回。统计记录默认按 QwenPaw 工作区写入 `<workspace>/.tokenless`。可在 QwenPaw 环境中设置绝对路径 `TOKENLESS_DATA_DIR` 覆盖此位置；相对路径会被拒绝。运行 `tokenless stats list --data-dir` 时指向实际使用的目录。

## AgentScope 框架集成

AgentScope 是 Python SDK 的第二层，不是产品 Adapter。完整的构建、版本、挂载、配置与验证说明
现已放在 [AgentScope SDK 集成](../../../developer-guide/zh/sdk/tokenless/agentscope.md) 子文档。本标题继续保留，作为已有链接的兼容入口。

## 验证 Agent Adapter

对于 Agent Adapter，不要只以“安装命令退出码为 0”作为成功标准。至少完成：

```bash
tokenless --version
anolisa adapter status tokenless
tokenless stats list --limit 5
```

然后在目标 Agent 中执行一次有明显输出的工具任务。如果 `stats list` 仍为空，请按照[启用后没有产生统计记录](troubleshooting.md#启用后没有产生统计记录)排查。

## 相关文档

- [快速开始](index.md)
- [Python SDK](../../../developer-guide/zh/sdk/tokenless/index.md)
- [AgentScope SDK 集成](../../../developer-guide/zh/sdk/tokenless/agentscope.md)
- [效果度量](measuring-savings.md)
- [配置与数据隐私](configuration-and-privacy.md)
- [故障排查](troubleshooting.md)

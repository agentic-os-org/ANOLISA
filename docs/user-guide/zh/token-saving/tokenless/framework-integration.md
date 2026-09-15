# Tokenless Agent 集成

[English](../../../en/token-saving/tokenless/framework-integration.md)

Tokenless 通过 Plugin、Hook 和 Extension 接入具体 Agent 产品。本文只说明产品 Adapter。
Python SDK 及其 AgentScope 专用子文档放在 [Python SDK 指南](sdk.md) 下。

## Agent Adapter 支持矩阵

| Agent 产品 | 值 | Tool Ready | 命令重写行为 | 响应交付方式 | TOON | Schema |
|------|----|------------|--------------|--------------|------|--------|
| cosh | `cosh` | 已硬关闭 | 替换受支持的 Shell 输入 | Cosh-NG 替换受支持的 JSON 结果；旧版 Copilot Shell 透传 | 对可替换文本由 Pipeline 选择 | Common Hook 仅接受无损结果 |
| OpenClaw | `openclaw` | 已硬关闭 | 替换 `exec` 命令输入 | 替换持久化工具结果消息 | 默认关闭，需主动启用 | — |
| Hermes | `hermes` | 已硬关闭 | 阻止第一次调用并建议使用 Core 返回的改写命令 | 替换已接受的结果或追加错误指引；支持 Marker 命令恢复 | 对可替换文本由 Core 选择 | — |
| Qoder | `qoder` | 已硬关闭 | 输出改写后的 Shell 输入 | 通过 `updatedToolOutput` 替换输出 | 对可替换文本由 Pipeline 选择 | — |
| Claude Code | `claude-code` | 已硬关闭 | 替换 Bash 输入 | 2.1.121 及以上替换输出；否则透传 | 对可替换文本由 Pipeline 选择 | — |
| Codex | `codex` | 已硬关闭 | 替换受支持的 Shell 输入 | 保留原文；仅对识别出的环境失败追加上下文 | — | — |
| DeepSeek Harness | `dsh` | 未注册 | 未注册 | 把已接受的单文本结果委托给 Core；支持 Marker 命令恢复 | 对可替换文本由 Core 选择 | 未注册 |
| OpenCode | `opencode` | 已硬关闭 | 替换 Bash 输入 | 替换工具输出 | 对可替换文本由 Pipeline 选择 | ✅ |
| Qwen Code | `qwencode` | 已硬关闭 | 输出改写后的 Shell 输入 | 宿主没有替换字段，因此透传 | — | — |
| QwenPaw | `qwenpaw` | — | 替换 `execute_shell_command` 的输入 | 在 AgentScope 中间件链中替换工具结果的文本块 | 对可替换文本由 Core 选择 | ✅ |
| WorkBuddy | `workbuddy` | 已硬关闭 | 默认透传 Bash 输入；设置 `TOKENLESS_WORKBUDDY_AUTO_ALLOW=1` 后才通过 `modifiedInput` 替换 | 在 CodeBuddy Code CLI 宿主上替换输出；其他 WorkBuddy 宿主原样透传 | 在响应压缩后尝试 | — |

“—”表示该能力不可用：当前 Adapter 没有注册，或当前宿主版本不会运行；对应的 Tokenless CLI 命令仍可能可用。

Schema 压缩到达模型路径的方式因宿主而异：cosh 与 Cosh-NG 触发 `BeforeModel` Hook；OpenCode 通过其 `tool.definition` 插件 Hook 逐个压缩工具定义（MCP 工具不经过该 Hook）；Qwen Code 的清单声明了 `BeforeModel` Hook，但当前 Qwen Code 版本在注册时会跳过这一未知事件名，Schema Hook 实际不会运行，因此矩阵标记为不可用。该条目保留注册，未来 Qwen Code 版本实现该事件后会自动生效。

这些 Adapter 仍会注册 Tool Ready，但会在检查、修复或阻断之前无条件硬退出，任何运行时设置都无法重新启用。工具执行后的失败归因不受影响。

`additionalContext` 是追加型 Hook 字段。共享 Hook 不会把压缩副本放入其中，否则原文
仍然可见，总 Context 反而增加；该字段只用于追加环境错误指引。统计记录只能证明压缩候选
内容变小了，不能单独证明宿主已经从模型请求中移除原文。

WorkBuddy 当前使用下文说明的随附生命周期脚本，本版本尚未把它注册到
`anolisa adapter enable` 的驱动集合。

## Adapter 处理规则

共享 Cosh-NG、Qoder、Claude Code 和 OpenCode PostTool Hook 会向 `tokenless compress`
发送一个 `post_tool` 请求。当宿主可以替换结果，且裸 `tokenless` 能从 Shell 的 `PATH`
解析时，Marker 可以提示模型通过已有 Shell Tool 恢复省略内容；否则 Core 只接受无损候选。
所有非 `applied` Disposition 都保留原文。当前路由如下：

| 内容 | 当前共享 Hook 行为 |
|------|--------------------|
| JSON | 无损结构清理；文本替换槽还会考虑 TOON |
| 需要 Record Reduction 或字符串、数组、深度截断的 JSON | 仅在 Marker 命令恢复可用时应用；否则以 `recoverability_unavailable` 拒绝候选 |
| 构建/测试/包管理日志、长纯文本、Diff、Stack Trace、HTML、搜索结果、表格、源码、Unknown | 对应领域 Compressor 接入前原样透传 |

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
Wrapper。因此三者都会让 RTK 输出绕过第二次压缩。

Claude Code 需要 2.1.121 或更高版本才能使用 `updatedToolOutput`。版本更旧或无法确定时，响应压缩会关闭，以免重复注入原文。结构化工具输出会保留宿主 Schema，不会转换成文本 TOON；以字符串承载的 JSON 在 TOON 更小时可以使用 TOON。

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

### npm + cosh

cosh 使用 Extension 目录，不提供单独的 `scripts/install.sh`。将 npm 安装的共享资源复制到 cosh 的用户 Extension 目录：

```bash
mkdir -p ~/.copilot-shell/extensions/tokenless
cp -R ~/.local/share/anolisa/adapters/tokenless/common/hooks \
  ~/.local/share/anolisa/adapters/tokenless/common/commands \
  ~/.local/share/anolisa/adapters/tokenless/common/cosh-extension.json \
  ~/.copilot-shell/extensions/tokenless/
```

完成后重启 cosh。移除前先退出 cosh，并确认目标目录确实是本次 npm 安装创建的 Tokenless Extension。

## Agent Adapter 生效提示

### cosh

Extension 在启动时发现。启用后重启 cosh，并运行一个 Shell 工具任务，再使用 `tokenless stats list` 检查记录。

### OpenClaw

安装脚本会在旧版宿主上使用上文说明的 OpenClaw unsafe-install 覆盖参数。确认风险并安装后，重启 Gateway。Plugin 代码默认启用响应压缩和 RTK 重写，默认关闭 TOON。由于底层检查已硬关闭，Plugin 的 Tool Ready 选项当前不会生效。

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

OpenCode 启动时会自动加载配置目录下的 Plugin。使用上述 Tokenless 生命周期脚本
完成安装或卸载后，请重启 OpenCode。重启后执行一次工具调用，再运行
`tokenless stats list`，确认已生成统计记录。

脚本会优先使用 `TOKENLESS_OPENCODE_CONFIG_DIR`，其次使用
`OPENCODE_CONFIG_DIR`。如果两者均未设置，则使用
`${XDG_CONFIG_HOME}/opencode`；如果 `XDG_CONFIG_HOME` 也未设置，则回退到
`~/.config/opencode`。

安装过程中，脚本只会创建由 Tokenless 管理的 `plugins/tokenless.js` 符号链接。
如果目标路径已经存在但不由 Tokenless 管理，安装会停止，原有内容不会被覆盖。

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
`tokenless retrieve` 从 Stash 取回。统计记录按 QwenPaw 工作区写入 `<workspace>/.tokenless`，运行 `tokenless stats list --data-dir` 时指向该目录。

### WorkBuddy

WorkBuddy（腾讯 CodeBuddy）的各产品形态共用同一套 Hook 协议：CodeBuddy Code CLI、WorkBuddy 桌面版（IDE）与 WorkBuddy 企业版都读取用户级 `~/.codebuddy/settings.json` 中的 `hooks` 键，其结构沿用 Claude Code 的 matcher 组形态。较新的 CodeBuddy CLI 版本虽然也提供了插件系统，但 `settings.json` Hook 仍是唯一同时覆盖三种宿主的集成面，因此随附的生命周期脚本把 Tokenless Hook 组合并进该文件：

```bash
# 先通过 `make -C src/tokenless install`（或 RPM）安装 Adapter 资源，然后：
make -C src/tokenless workbuddy-install
# 或直接运行脚本：
bash ~/.local/share/anolisa/adapters/tokenless/workbuddy/scripts/install.sh
# 移除：
bash ~/.local/share/anolisa/adapters/tokenless/workbuddy/scripts/uninstall.sh
```

用户已有的 Hook 配置和其他所有设置键都会被保留；卸载脚本只移除 Tokenless 自己的条目。两个脚本都通过临时文件改写 `settings.json`，且从不放宽已有文件的权限位——`.codebuddy` 目录可能存放凭据（官方支持在 `settings.json.env` 中保存 `CODEBUDDY_API_KEY` 与认证 token）；新建的 `settings.json` 默认使用 `0600`。

命令重写 Hook 输出 WorkBuddy 的 `modifiedInput` 部分字段覆盖，并同时返回官方 PreToolUse 契约要求参数修改生效所必须的 `permissionDecision: "allow"`（契约的故障排查明确：其他任何决策下工具都按原参数执行）。由于 `allow` 会绕过宿主的权限确认，Hook 只在重写获得背书时才输出它。Protocol v2 将 rtk 执行移入 Tokenless Core，而 Core 向 Hook 上报时刻意不区分 Allow 判定（rtk 权限规则批准）与 Ask/Default 判定（未背书），Hook 因此无法证明重写已获背书。WorkBuddy 契约无法把 `modifiedInput` 与确认弹窗组合使用（输出 `ask` 时参数修改会被静默丢弃），因此默认情况下，Hook 原样透传原始命令，保留宿主自身的权限流程。愿意跳过宿主确认运行重写的用户可以设置 `TOKENLESS_WORKBUDDY_AUTO_ALLOW=1`，此时 Hook 输出重写及 `allow`，决策原因中会明确记录该绕过。响应压缩在 CodeBuddy Code CLI 宿主下通过 `updatedToolOutput` 整体替换工具结果；CLI 宿主由多信号组合识别，且每个信号都向非 CLI 方向保守失败：`CODEBUDDY_FORCE_HEADLESS_BUNDLE`（自 CLI 2.136.0 起由 WorkBuddy 宿主在启动 headless bundle 前设置）是宿主持有的正向证据；`CODEBUDDY_SESSION_KIND=daemon` 标记常驻 daemon 工作进程（官方 Daemon Mode 参考将该变量记载为 worker 类型，`daemon start` fork 的常驻子进程会在参数前补 `--serve`）；进程祖先中的 CLI 二进制（`codebuddy` / `codebuddy-code` / `cbc`）若携带托管 sidecar 标志（`--prewarm` / `--prewarm-force` / `--teammate-mode`），即使在早于该 marker 的产物中也属于被托管的 headless 进程；不携带这些托管信号的 CLI 二进制祖先即为独立 CLI。此处刻意不要求控制终端——受支持的 headless 形态（用于 CI/CD 与 stdin 管道的 `-p` / `--print`，以及 `--acp`、`--bg` 和用户自启的 `--serve` Web UI）本来就没有 TTY，CLI Hooks 契约在这些场景下同样支持 `updatedToolOutput`。`--serve` 刻意不作为宿主证据：官方 Web UI 参考记载用户直接以 `codebuddy --serve` 启动 Web UI，该形态是独立 CLI 会话；参数形态完全相同的常驻 daemon 子进程改由上述 daemon 会话类型区分。除 `daemon` 之外的会话类型（`interactive` / `bg` / 未声明）绝不作为宿主证据——独立 CLI 自己的会话也会声明它们（如 `codebuddy --bg` 声明 `bg`）。marker 缺失绝不会被当作独立 CLI 的证明：它自 CLI 2.136.0 才存在，而声明的支持范围从 1.16.0 开始，更早的产物已经携带托管模式；早于 marker 且不携带上述任何托管信号的 sidecar 形态会按 CLI 处理——这是良性的，因为这类 bundle 执行的是同一份 CLI Hooks 契约。`CODEBUDDY_PROJECT_DIR` 同样无法区分，IDE Hooks 参考也为 IDE Hook 脚本列出了该变量（CLI 的 Hook 能力要求 CodeBuddy Code v1.16.0 及以上版本）。宿主分类在任何压缩动作之前完成，非 CLI 宿主不会承担压缩子进程延迟，也不会产生压缩统计或 stash 记录。IDE 与企业版宿主的 PostToolUse 契约只定义了追加式的 `additionalContext`，原始工具结果会保留，经由它压缩反而会让上下文变长，因此在这些宿主上压缩被禁用，只交付真正追加式的环境归因诊断。安装或移除后请重启 WorkBuddy/CodeBuddy；CodeBuddy CLI 的 `/hooks` 面板可能会要求你先审核外部添加的 Hook 才会生效。

生命周期脚本由 `make -C src/tokenless install`、RPM、npm 的 postinstall 复制，以及 raw 方式的 `anolisa install` 组件契约安装——契约中的 `workbuddy` `[[adapters]]` 条目会把本 Adapter 目录与其他 Adapter 一起铺设。WorkBuddy 仍没有内置 anolisa 驱动，本版本也未注册进 `anolisa adapter enable`；请通过生命周期脚本安装或移除 Hook。

`scripts/detect.sh` 是只读脚本，采用与其他生命周期 Adapter 一致的三态退出码汇报状态：`0` = 已检测到 WorkBuddy/CodeBuddy 且 Tokenless Hook 已安装；`1` = 已检测到但 Hook 尚未安装；`2` = 缺少前置条件。缺少 `~/.codebuddy` 视为未安装 WorkBuddy/CodeBuddy，`detect.sh` 返回 `2`；而 `install.sh` 对同样情况做优雅跳过（输出提示并以 `0` 退出），因此在没有 WorkBuddy 的主机上运行生命周期脚本不会失败。本版本中，缺少 `tokenless` 或 `rtk` 二进制同样按前置条件缺失（退出码 `2`）上报。

RPM 卸载钩子（`%preun`）以 root 身份运行，因此只会清理 root 用户的 `~/.codebuddy/settings.json`。在多用户主机上，其他用户需要自行运行 `uninstall.sh`。

## AgentScope 框架集成

AgentScope 是 Python SDK 的第二层，不是产品 Adapter。完整的构建、版本、挂载、配置与验证说明
现已放在 [AgentScope SDK 集成](sdk/agentscope.md) 子文档。本标题继续保留，作为已有链接的兼容入口。

## 验证 Agent Adapter

对于 Agent Adapter，不要只以“安装命令退出码为 0”作为成功标准。至少完成：

```bash
tokenless --version
anolisa adapter status tokenless
tokenless stats list --limit 5
```

然后在目标 Agent 中执行一次有明显输出的工具任务。如果 `stats list` 仍为空，请按照[启用后没有产生统计记录](troubleshooting.md#启用后没有产生统计记录)排查。

## 相关文档

- [快速开始](QUICKSTART.md)
- [Python SDK](sdk.md)
- [AgentScope SDK 集成](sdk/agentscope.md)
- [效果度量](measuring-savings.md)
- [配置与数据隐私](configuration-and-privacy.md)
- [故障排查](troubleshooting.md)

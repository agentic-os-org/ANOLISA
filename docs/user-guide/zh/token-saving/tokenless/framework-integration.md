# Tokenless Agent 集成

[English](../../../en/token-saving/tokenless/framework-integration.md)

Tokenless 通过 Plugin、Hook 和 Extension 接入具体 Agent 产品。本文只说明产品 Adapter。
Python SDK 及其 AgentScope 专用子文档放在 [Python SDK 指南](sdk.md) 下。

## Agent Adapter 支持矩阵

| Agent 产品 | 值 | Tool Ready | 命令重写行为 | 响应交付方式 | TOON | Schema |
|------|----|------------|--------------|--------------|------|--------|
| cosh | `cosh` | 已硬关闭 | 替换受支持的 Shell 输入 | Cosh-NG 替换受支持的 JSON 结果；旧版 Copilot Shell 透传 | 对可替换文本由 Pipeline 选择 | —（Hook 会运行，工具原样返回） |
| OpenClaw | `openclaw` | 已硬关闭 | 替换 `exec` 命令输入 | 替换持久化工具结果消息 | 默认关闭，需主动启用 | — |
| Hermes | `hermes` | 已硬关闭 | 阻止第一次调用并建议使用 Core 返回的改写命令 | 替换已接受的结果或追加错误指引；支持 Marker 命令恢复 | 对可替换文本由 Core 选择 | — |
| Qoder | `qoder` | 已硬关闭 | 输出改写后的 Shell 输入 | 通过 `updatedToolOutput` 替换输出 | 对可替换文本由 Pipeline 选择 | — |
| Claude Code | `claude-code` | 已硬关闭 | 替换 Bash 输入 | 2.1.121 及以上替换输出；否则透传 | 对可替换文本由 Pipeline 选择 | — |
| Codex | `codex` | 已硬关闭 | 替换受支持的 Shell 输入 | 保留原文；仅对识别出的环境失败追加上下文 | — | — |
| DeepSeek Harness | `dsh` | — | — | 把已接受的单文本结果委托给 Core；支持 Marker 命令恢复 | 对可替换文本由 Core 选择 | — |
| OpenCode | `opencode` | 已硬关闭 | 替换 Bash 输入 | 替换工具输出 | 对可替换文本由 Pipeline 选择 | —（Hook 会运行，工具原样返回） |
| Qwen Code | `qwencode` | 已硬关闭 | 输出改写后的 Shell 输入 | 宿主没有替换字段，因此透传 | — | — |
| QwenPaw | `qwenpaw` | — | 替换 `execute_shell_command` 的输入 | 在 AgentScope 中间件链中替换工具结果的文本块 | 对可替换文本由 Core 选择 | ✅ |

“—”表示该能力不可用：当前 Adapter 没有注册、当前宿主版本不会运行，或虽然运行但无法生效（见单元格说明）；对应的 Tokenless CLI 命令仍可能可用。

Schema 压缩到达模型路径的方式因宿主而异：cosh 与 Cosh-NG 触发 `BeforeModel` Hook；OpenCode 通过其 `tool.definition` 插件 Hook 对每个工具定义运行同一个 Hook（MCP 工具不经过该 Hook）；当前 Qwen Code 版本不运行声明的 `BeforeModel` 事件。这些宿主都不会应用结果：共享 Hook 没有 Marker 授权恢复（见[Adapter 处理规则](#adapter-处理规则)），Qwen Code 则根本不运行该事件。只有声明了静态恢复 Tool 的 QwenPaw 和 AgentScope 会替换工具定义。

这些 Adapter 仍会注册 Tool Ready，但会在检查、修复或阻断之前无条件硬退出，任何运行时设置都无法重新启用。工具执行后的失败归因不受影响。

`additionalContext` 是追加型 Hook 字段。共享 Hook 不会把压缩副本放入其中，否则原文
仍然可见，总 Context 反而增加；该字段只用于追加环境错误指引。统计记录只能证明压缩候选
内容变小了，不能单独证明宿主已经从模型请求中移除原文。

## Adapter 处理规则

共享 Cosh-NG、Qoder、Claude Code 和 OpenCode PostTool Hook 会向 `tokenless compress`
发送一个 `post_tool` 请求。当宿主可以替换结果，且裸 `tokenless` 能从 Shell 的 `PATH`
解析时，Marker 可以提示模型通过已有 Shell Tool 恢复省略内容；否则 Core 只接受无损候选。
所有非 `applied` Disposition 都保留原文。当前路由如下：

| 内容 | 当前共享 Hook 行为 |
|------|--------------------|
| JSON | 无损结构清理；文本替换槽还会考虑 TOON |
| 需要 Record Reduction 或字符串、数组、深度截断的 JSON | 仅在 Marker 命令恢复可用时应用；否则以 `recoverability_unavailable` 拒绝候选 |
| 来自命令输出的构建/测试/包管理日志 | 终端输出清理与常规进度缩减；每段省略都带有就地取回标记 |
| 宿主支持文本替换时的 CSV/TSV 表格 | 整表压紧；数据行超过 32 行的表格在 Stash 支持的恢复可用时可做行缩减 |
| 路径共享开启且宿主支持文本替换时的 API 搜索结果列表 | 无损搜索路径共享；保留全部已收到命中 |
| 显式开启 `TOKENLESS_DIFF_COMPRESSION_ENABLED`（默认关闭）且宿主支持文本替换时，来自命令输出的 Git Diff | 按 Hunk 选择裁剪未变更上下文；保留全部变更行，完整原文经 Stash 可取回，收益过小的候选会被拒绝 |
| 显式开启 `TOKENLESS_HTML_EXTRACTION_ENABLED`（默认关闭）、宿主支持文本替换且 Stash 恢复可用时，来自命令输出或 API 响应（不含 Shell 文件读取）的 HTML 页面 | 把页面正文根转写为 Markdown；收到的页面经 Stash 可取回；否则原样透传 |
| 长纯文本、Stack Trace、源码、Unknown | 原样透传 |

内容检测、PostTool 200 字符门禁、基于工具来源的阈值、诊断、TOON 选择和最终接受均属于
Core 策略；Hook 负责把宿主对象映射为协议字段并把结果回填成宿主的形状。

cosh、Cosh-NG 和 OpenCode 逐工具定义路径共用的 BeforeModel Hook 没有 Marker 授权恢复路径，
因此 Schema 压缩在该路径原样返回工具。只有直接 `compress-schema` 命令，以及声明了静态恢复
Tool 的进程内集成（QwenPaw、AgentScope）会应用它。

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

RTK 输出不会被二次压缩：共享 Hook 与 OpenClaw 会把 RTK 所有权传给匹配的 PostTool 调用，
Hermes 则从实际执行的命令中识别 RTK Wrapper。

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

Plugin 把包含一个文本块、可替换的根调用结果发送给 `tokenless compress`；内容检测、
压缩、TOON 选择、大小门禁、基于工具来源的阈值和最终接受均由 Core 负责。不受支持的
内容域与文件内容结果会透传。当裸 `tokenless` 能从 DSH Shell 的 `PATH` 解析到 Core 调用
选中的同一个可执行文件时，Marker 可以提示模型执行一条独立的 `tokenless retrieve` 命令；
成功的恢复输出会绕过压缩。多文本块、图片和 Code Mode 子调用的成功结果保持不变，已被
其他 DSH 策略替换过值的结果不会被压缩（替换值仍是结构化命令失败时只追加诊断），CLI
缺失、失败或超时也会保留原始内容。

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
`command_output`，未知工具映射为 `api_response`；后续策略由 Core 决定。即使压缩关闭，
DSH 标记的失败以及结构化结果里报非零退出码、信号或超时的命令结果仍会交给 Core 做环境诊断。

完整触发条件（压缩开关、最小响应长度、受支持的压缩域、严格变小保护）与阈值含义见[用户手册 · 压缩的触发条件与阈值](user-manual.md#压缩的触发条件与阈值)。

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

通过 anolisa 或自带的 `install.sh` 启用 OpenClaw Adapter，即同意插件声明的能力；两者都只在宿主
`plugins install --help` 列出 `--accept-capabilities` 时传递它，旧版宿主仍可安装。
独立的 `install.sh` 只在安装器仍认为该参数有效的宿主上附加
`--dangerously-force-unsafe-install`；OpenClaw 2026.6.5 及以上由
`security.installPolicy` 决定安全扫描。

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

在安装器仍执行安全扫描的宿主上，OpenClaw 安装脚本会附加 `--dangerously-force-unsafe-install`，因为 Plugin 会启动 `tokenless` 和 `rtk` 二进制；较新的宿主由 `security.installPolicy` 决定安全扫描。运行前应审查已安装的 Adapter 源码和 OpenClaw 安全策略；策略不允许该覆盖参数时，就不要安装此 Plugin。

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
并把对应 GitHub Release 中的 `anolisa_tokenless` SDK wheel 安装进 QwenPaw 自己的 Python 环境，因此首次安装需要联网。
QwenPaw 只在找不到该包时才运行 pip，所以离线主机应先自行安装 wheel；已装过的旧版 wheel 也不会被 `plugin install`
升级。wheel 只提供 Linux x86_64、Linux aarch64 和 macOS arm64，其它平台上安装脚本会在确认 QwenPaw 的 Python
无法导入 `anolisa_tokenless` 后失败；`PATH` 上没有 `qwenpaw` 命令时，安装脚本只打印提示并以 0 退出，不安装任何内容。
wheel 缺少插件导入的 SDK 入口（Tokenless 0.8.0 引入）时插件拒绝注册并在日志中给出应安装的 release。工作目录与
QwenPaw 本身的解析一致：`QWENPAW_WORKING_DIR`，否则 `COPAW_WORKING_DIR`，否则已存在的 `~/.copaw`，否则 `~/.qwenpaw`。

正在运行的 QwenPaw 会热加载插件；否则启动 QwenPaw 即可。Schema 压缩和 `tokenless_retrieve` 工具从下一次模型调用
开始生效，已批准的 `execute_shell_command` 会执行改写后的命令。只有 QwenPaw 内置工具会被分类：`execute_shell_command`
为命令输出，`read_file`、`recall_history`、`view_image`、`view_video` 为文件内容，其余内置工具为 API 响应；Skill、MCP
工具以及后续 QwenPaw 版本新增的工具原样透传。QwenPaw 自己的工具结果裁剪在 Tokenless 之后运行，只保留每条结果的头部（最近两条
预算更大），因此压缩结果末尾的恢复指令可能被截掉；被省略的内容仍可通过 `tokenless_retrieve` 工具，或 `tokenless retrieve --stash-db <workspace>/.tokenless/stash.db` 取回。统计记录按
QwenPaw 工作区写入 `<workspace>/.tokenless`，运行 `tokenless stats list --data-dir` 时指向该目录。

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

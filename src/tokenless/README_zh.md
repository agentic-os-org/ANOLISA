# Token-Less

[English](README.md)

LLM Token 优化工具包——content-aware 压缩 + 命令重写 + 环境失败诊断。Token-Less 是 [ANOLISA](../../README_zh.md) 的 Token 节省组件，通过多种互补策略最小化 LLM Token 消耗。

随包提供的 RTK 0.49.0 保留原生 `grep -l` / `-m` 语义，保守处理 Pipeline 重写，
并让 `sudo` 命令保持原样。RTK 恢复提示使用 `rtk recall`，保留的输出以宿主 OS 用户为作用域，
不按 Tokenless 租户或 Session 隔离。
Flag 迁移、Pipeline 行为和输出恢复详见
[随包提供的 RTK 命令](../../docs/user-guide/zh/token-saving/tokenless/cli-reference.md#随包提供的-rtk-命令)。

## 核心能力

| 能力 | 节省率示例 | 说明 |
|------|-----------|------|
| Schema 压缩 | 参考 fixture 47.3% | 压缩 OpenAI Function Calling 工具定义 |
| Content-aware 响应压缩 | JSON 参考 fixture 无损节省 36.3% | 把成功 JSON 路由给 `JsonCompressor`；达到 15% 的无损候选优先，可恢复的 Record Array 使用 32 条基础预算 |
| Build Log 压缩 | 取决于具体负载 | 清理终端控制输出，并缩减已识别 Cargo、pytest、npm/Jest、Go、Make/C 和通用命令日志中的重复常规进度，同时保留诊断、摘要、阶段和 Stack Trace |
| 搜索路径共享 | 取决于工作负载 | API 搜索列表（含 Claude 原生 Grep）可共享连续记录的文件路径并保留全部已收到命中；默认开启，通过 `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0` 或 SDK `search_path_sharing_enabled=False` 关闭；命令输出保持原路由 |
| Git Diff 上下文裁剪 | 取决于工作负载 | 通过 `TOKENLESS_DIFF_COMPRESSION_ENABLED=1` 或 SDK `diff_compression_enabled=True` 启用；保留全部增删行，按 hunk 裁剪上下文并提供原文恢复。默认关闭，尚未证实稳定的 Agent 整轮 token 收益 |
| HTML 页面转写 | 取决于工作负载 | 默认开启：把命令或 API 返回的完整 HTML 文档转写为 Markdown，只移除可枚举的非内容元素（脚本、样式、导航、页眉、页脚、侧栏、表单控件、媒体嵌入）并在视图头部计数，提供原文恢复；通过 `TOKENLESS_HTML_EXTRACTION_ENABLED=0` 或 SDK `html_extraction_enabled=False` 关闭；文件读取透传 |
| CSV/TSV 表格压缩 | 取决于具体负载 | 压紧引号和记录分隔符时保留全部单元格；较大的表格可保留选定行，明确提示表格不完整，并支持取回字节一致的原文。需要文本替换能力；文件读取透传 |
| TOON 上下文压缩 | 参考响应 17.0% | 将 JSON 编码为 TOON 格式 |
| 命令重写 | 60–90% | 通过 RTK 过滤 CLI 输出（支持 70+ 命令） |
| Tool Ready | 减少重试浪费 | 旧版调用前预检、自动修复与阻断；当前硬关闭 |

表中 Schema、响应和 TOON 数字是当前仓库内置参考 fixture 的独立测试
结果，既不是生产范围，也不能相加。实际压缩率取决于 Payload 的大小和结构、可移除字段、
配置阈值，以及工具数据在会话中的占比。短小或已经紧凑的 Payload 可能只节省几个百分
点，也可能直接原样透传。精确输入、命令、完整结果和限制见
[Tokenless 效果度量](../../docs/user-guide/zh/token-saving/tokenless/measuring-savings.md#运行仓库参考负载)。

Tool Ready 当前在所有 Adapter 中无条件硬旁路，不会读取依赖规范、执行调用前检查、自动修复环境或阻止工具调用。任何环境变量都无法恢复旧行为；重新启用必须修改源码并重新发布。

工具执行后的失败归因、响应压缩、RTK 命令重写、TOON、Stash 和统计是独立能力，仍保持原有行为。

## 适用场景与预期效果

tokenless 优化进入 LLM 上下文前、由它实际处理的工具相关内容，包括工具 Schema、
工具/API 响应和受支持的 Shell 输出；它不触及模型推理与对话历史。收益高度取决于
这些内容在会话中的占比与形态。

### 哪些场景收益高

| 工作负载 | 主要受益策略 | 原因 |
|----------|-------------|------|
| Shell 密集（编译/测试/排查） | Build Log 压缩 + RTK | 已识别的构建/测试命令保留原生输出交给 PostTool 压缩，其他受支持 Shell 命令使用 RTK |
| API/抓取密集（REST、web_fetch） | 响应压缩 + TOON | JSON 可能含可移除的 debug/null/空值；足够大且结构规则的数据也有可削减的语法开销 |
| 工具数量多的 Agent | Schema 压缩 | 大量 Function Calling 定义可能含冗长描述和可移除元数据 |
| 长响应需保真 | 可逆压缩（Stash） | 截断后可 `retrieve` 原文，端到端无损，可放心收紧阈值 |

### 哪些场景收益低或不适用

- **纯对话/少工具调用**：工具响应占比极低，整体节省接近 0。
- **没有固定最小 Payload**：`compress-schema` 和 `compress-response` 会为每个通过输入
  规则的合法 JSON 生成候选结果。在 Active 模式下，只有候选结果的估算 Token 数严格少于原文时
  才输出它。包含可移除内容的小输入仍可能被压缩，而已经紧凑的较大输入也可能原样透传；CLI 会把
  原因写入 stderr，且不记录统计。在 Dry-run 模式下，CLI 始终输出原文，并可能把较小的候选结果
  记为预测节省。[CLI 参考](../../docs/user-guide/zh/token-saving/tokenless/cli-reference.md)
  中的描述、字符串、数组和深度阈值只决定单项转换何时触发，并不是整个 Payload 的最小大小；
  Agent Adapter 还可能应用独立的
  [预检门槛](../../docs/user-guide/zh/token-saving/tokenless/framework-integration.md#adapter-处理规则)。
- **模型推理 token / 计费 token**：不在 tokenless 经手范围。

### 预期效果估算

> 下表比例为**示意性经验估值**，随任务差异很大，非实测常数。

| 会话组成 | 典型占比 | tokenless 能否优化 |
|----------|---------|-------------------|
| LLM 推理输出（文本生成） | ~35% | ❌ 不涉及 |
| LLM 输入（system prompt + 对话历史） | ~40% | ❌ 不涉及 |
| 工具调用参数 | ~5% | ❌ 不涉及 |
| **工具响应（API 返回 + 命令输出）** | **~20%** | **✅ 优化范围** |

**实际节省率 = 面板节省率 × 工具响应占比**

例如：面板显示压缩率 60%，若工具响应占总消耗 20%，实际节省率为 60% × 20% = **12%**。这也是为何在总消耗 1500 万 Token 的实验中节省量观感偏小——tokenless 只作用于其中约 300 万 Token 的工具响应部分。

> 可恢复省略会附带按需操作：`If needed, run in shell: tokenless retrieve HASH`。AgentScope 则看到 `If needed, call tool tokenless_retrieve with hash_or_marker=HASH`，其中 Tool 名称采用实际静态配置。数据仍在 Stash 时可以取回；恢复会增加 Token 开销，并非每次省略都要恢复。历史 `<<tokenless:HASH>>` Marker 仍可读取，但不再生成。建议用 `TOKENLESS_COMPRESSION_ENABLED=0/1` 双跑对照真实节省。至少 33 项且全为 Object 的数组使用 Record Reduction：完整数组进入 Stash，默认保留首尾、错误、结构/数值异常并稳定采样至 32 条基础预算；其他数组截断默认保留头部 32 项与尾部 8 项。完整参数见用户手册 CLI 参考。
> 对支持实时结果替换的 Agent，恢复提示通过已有 Shell Tool 执行裸 Hash 命令；成功的恢复结果原样返回，不会再次压缩。
> 各策略触发条件与阈值见 [用户手册](../../docs/user-guide/zh/token-saving/tokenless/user-manual.md)。

## 集成路径

### Agent Adapter

- **OpenClaw 插件** — 通过 Core 执行 PreTool RTK 改写和无损 transcript PostTool 优化；宿主不支持 BeforeModel Schema 与授权 Retrieve
- **copilot-shell 钩子** — Tool Ready（已硬关闭）+ 命令重写；Cosh-NG 支持响应压缩和 Marker 命令恢复，旧 copilot-shell 保持无损 PostTool；Common BeforeModel 在授权 Retrieve 接入前透传 Schema
- **Hermes Agent 插件** — 把阻止后建议式命令重写和模型可见结果优化委托给 Core，并通过已有 Shell Tool 支持 Marker 命令恢复；无 Schema 压缩
- **Qoder CLI 插件** — Tool Ready（已硬关闭）+ 命令重写 + 通过 `updatedToolOutput` 交付响应 Pipeline + Marker 命令恢复
- **Claude Code 插件** — Tool Ready（已硬关闭）+ 命令重写 + 响应压缩 + TOON；2.1.121 及以上版本支持 Marker 命令恢复
- **Codex 插件** — Tool Ready（已硬关闭）+ RTK 命令重写 + 环境失败诊断；Codex
  协议不支持替换原始输出，因此不追加压缩副本
- **OpenCode 插件** — Tool Ready（已硬关闭）+ 命令重写 + Schema/响应压缩 + TOON + Marker 命令恢复
- **DeepSeek Harness 插件** — 通过 DSH 原生 `tools/post-execute` 接入响应压缩、Marker 命令恢复和环境错误归因
- **Qwen Code Extension** — Tool Ready（已硬关闭）+ 命令重写；当前宿主不支持工具后输出替换，并跳过声明的 Schema 事件
- **QwenPaw 插件** — 通过 QwenPaw 插件系统注册 AgentScope 中间件，进程内调用 `anolisa_tokenless` wheel，提供 Schema 压缩、RTK 命令重写、响应/TOON 压缩和 `tokenless_retrieve` 静态工具恢复

OpenClaw Plugin 只保留宿主事件转换与逐调用状态：`before_tool_call` 把 `exec` 参数交给
`tokenless compress`，`tool_result_persist` 把 OpenClaw 自己持久化的 Tool Result 交给同一
Protocol v2 入口。Core 持有 RTK、JSON 检测、清理、TOON、阈值、诊断与最终仲裁；Plugin
把 PreTool 返回的 `output_optimization` 传到匹配的 PostTool，因此不会二次压缩 RTK 输出。
本地 CLI 恢复命令是受信运维入口，不等价于 Agent 的 Marker 授权，因此 OpenClaw 只应用无损
候选。该 PostTool Hook 只改写持久化 transcript，不会改变同一轮中模型已经看到的实时结果；
Media 与多个 Content Block 也会透传。

该 Adapter 要求 OpenClaw Plugin API `2026.4.22` 或更高版本；支持兼容性检查的宿主会在
安装阶段根据 Package Metadata 强制执行该下限。OpenClaw 配置只包含
`rtk_enabled`、`post_tool_enabled`、`tool_ready_enabled` 和 `verbose`；
默认值依次为 `true`、`true`、`true` 和 `false`。旧的 Response、TOON 与工具分类开关已经
删除，所有压缩策略由 Core 统一决定。

### Agent 开发框架集成

- **通用 Python SDK** — 面向任意 Agent 框架的生命周期 API、单项 Runtime 操作与统计查询。
- **AgentScope 专用层** — 基于通用 SDK，完整开放 Schema 压缩、RTK 改写、响应压缩、
  TOON、受 marker 约束的恢复和归属统计。

## 快速开始

首选 ANOLISA CLI 安装已发布的组件。

安装脚本会把 `anolisa` 放到 `~/.local/bin`。user mode 安装的 `tokenless`
和 `rtk` 也在这个目录。如果当前 Shell 还找不到命令，先把该目录加入
`PATH`。

```bash
curl -fsSL https://get.agentic-os.sh | bash

# 让默认安装目录在当前 Shell 中生效
export PATH="$HOME/.local/bin:$PATH"
anolisa --version
anolisa install tokenless
tokenless --version
```

已配置 YUM 源的 Alinux 用户也可以安装 RPM 包。

```bash
sudo yum install anolisa tokenless
sudo anolisa --install-mode system adopt tokenless
```

从同一 YUM 源安装 CLI 后，`sudo` 可以从系统路径找到 `anolisa`。`adopt` 会把
直接安装的 RPM 写入 system 状态，adapter 命令随后才能读取组件契约。

另外两条公开安装路径可以独立安装 CLI，但不会写入 anolisa 组件记录。npm 路径
提供预编译的 `tokenless` 和 `rtk` 二进制以及随包分发的 Agent adapter，需要
Node.js 16.7+（`fs.cpSync` 自该版本起提供，也是包的 postinstall 所需版本）。
curl 路径是独立安装脚本，优先使用 npm，失败时回退到源码构建：

```bash
npm install -g anolisa-tokenless

curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

由于两者都不注册组件，`anolisa adapter enable` 对它们不适用；请改用
`~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/install.sh`
下对应框架的脚本启用。curl 安装脚本会把它创建的内容记录到
`~/.local/share/tokenless/install-receipt`，`scripts/uninstall.sh` 依据该
回执只删除这些路径；其源码构建路径是 CLI-only（没有 `rtk`，也没有
adapter）。Agent 框架可以通过 `install-tokenless` OS Skill 执行同样的步骤。
完整的方式对照见 `docs/user-guide/zh/token-saving/tokenless/QUICKSTART.md`。

`~/.local/share/anolisa/adapters/tokenless` 与 anolisa 管理的安装共享，因此两条
公开路径都不会盲目接管它。所有权必须被证明：npm 的 postinstall 只刷新带有它
自己或 curl 安装脚本写下的标记的目录，其余一律保留——受管组件安装、旧版本留
下的目录、手工拷贝的目录都一样——并提示包内资源的位置。
`ANOLISA_TOKENLESS_FORCE_ADAPTERS=1` 可强制接管。curl 安装
脚本会把其他所有者放置的目录恢复回去、不为它记录 Adapter 目录，并且用「每次安装
专属的标记」（`.tokenless-owner`）而不只是内容哈希来证明自己记录的内容归自己所有
——anolisa 或直接 npm
安装同一版本会留下逐字相同的字节，此时它的文件、Adapter 资源、框架注册与 npm
全局包都会被保留。中途失败的替换（tag 缺失、构建报错）会把原安装放回，而不是让
机器上没有可用 CLI。

当前公开软件包支持 Linux x86_64、aarch64 和 macOS Apple Silicon。Intel Mac
仍暂无已发布的软件包：源码中保留的 `@anolisa/tokenless-darwin-x64`
optional dependency 只是发布构建目标，不代表 registry 中已有可安装的软件包，
因此 npm 路径无法在 Intel Mac 上提供二进制。独立安装脚本在 macOS 上也不会
回退到源码构建——它会直接报错退出而不执行 `cargo`——所以 Intel Mac 目前没有
受支持的安装路径。在该软件包发布之前，请使用 Linux 或 Apple Silicon macOS。

通过 ANOLISA 管理的安装或已执行 `adopt` 的 RPM 会放置可用 adapter，但不会
直接改动 Agent 产品的用户配置。请用拥有该配置的用户执行以下命令，并且只启用
准备使用的 adapter。

```bash
anolisa adapter scan
anolisa adapter enable tokenless openclaw
anolisa adapter status tokenless
```

DeepSeek Harness 必须指定至少一个 profile。需要启用多个 profile 时，应在同一条
命令中列出全部名称，完整集合语义见下文。启动 DSH 时请使用已经启用的名称。

```bash
anolisa adapter enable tokenless dsh --profile <profile>
dsh --profile <profile>
```

### `compress` 压缩入口

共享 Agent Hook 会向 `tokenless compress` 发送生命周期请求；只有成功且未旁路的
PostTool JSON、CSV/TSV 表格和符合条件的命令输出 Build Log 会进入 Runtime 内部 Pipeline。Tool Error
旁路压缩、保留原始输出，再由 Core 追加环境诊断信息。

PreTool 会保持已识别的 Cargo、pytest、npm/Jest、Go 和 Make 构建/测试命令不变，使其原生
输出只由 PostTool 处理。其他受支持命令仍可由 RTK 改写，其结果继续旁路 PostTool 压缩。

Claude Code 2.1.121 及以上版本、Qoder CLI、
OpenCode 和 Cosh-NG 能替换实时结果；同时裸 `tokenless` 可从 Shell `PATH` 解析时，其
PostTool 请求才启用恢复能力。压缩 Marker 会提示模型通过已有 Shell Tool 执行精确的
`tokenless retrieve` 命令。Hook 只识别成功执行、参数为
有效 Hash 或 Marker 的单条恢复命令，并让其结果绕过压缩，避免二次处理。旧 copilot-shell 和
不能替换结果的宿主保持无损模式。BeforeModel Schema 压缩仍需要独立的授权恢复能力。
请求/响应契约和可执行示例见
[CLI 参考](../../docs/user-guide/zh/token-saving/tokenless/cli-reference.md#compress)。

### Schema 压缩 CLI

`compress-schema` 支持单个工具定义、工具定义 JSON 数组，以及包含顶层
`tools` 数组的完整请求对象。处理完整请求对象时不传 `--batch`；其中的 OpenAI
Wrapper、Gemini `functionDeclarations` 工具对象及裸 Function Calling 定义会被压缩，
非函数工具及 `tools` 之外的字段会原样保留。

```bash
# 单个工具定义
tokenless compress-schema -f tool.json

# 工具定义数组
tokenless compress-schema -f tools.json --batch

# 包含顶层 tools 数组的请求对象
tokenless compress-schema -f request.json
```

### TOON 压缩 CLI

`compress-toon` 将 JSON 编码为 TOON 格式（`decompress-toon` 解码回
JSON）。短于 500 字符的负载默认原样透传（与 Adapter Hook 应用的最小
长度一致）；传入 `--min-toon-chars 0` 可强制编码：

```bash
# TOON 编码（短负载，本次调用关闭最小长度门槛）
echo '{"name":"Alice","age":30}' | tokenless compress-toon --min-toon-chars 0
# name: Alice
# age: 30

# TOON 解码回 JSON
printf 'name: Alice\nage: 30\n' | tokenless decompress-toon
# {"name":"Alice","age":30}
```

从源码构建适合开发者。

```bash
git clone <repo-url>
cd Token-Less

# 完整安装，构建并安装二进制，随后部署所有 adapter
make setup
```

源码安装会把 `tokenless` 放在 `~/.local/bin`，`rtk` 辅助
二进制也位于同一个目录，并部署开发所需的全部 adapter。

### 构建 Python SDK

框架开发者可以从源码构建进程内 Python API：

```bash
make python-wheel
python3 -m venv /tmp/tokenless-python
/tmp/tokenless-python/bin/pip install target/wheels/anolisa_tokenless-*.whl
```

该目标要求系统可发现 CPython 3.11+ 开发环境，并默认通过 `uvx` 提供
Maturin。请先安装 [`uv`](https://docs.astral.sh/uv/)，或者在 `PATH` 中已有
兼容 Maturin 时执行 `make python-wheel MATURIN=maturin`。执行
`cargo test --workspace` 同样需要该 Python 环境；普通 Cargo workspace 默认
命令不包含 Python Extension。

`anolisa_tokenless` 模块支持 CPython 3.11 及更高版本，但只能在构建该原生
Wheel 的对应平台使用。它开放四个 Tokenless 生命周期接口并内置对应平台的 RTK；
TOON 已链接进原生 Runtime，不依赖 Tokenless CLI 或系统 helper。仓库会构建并测试该包，
但目前尚未发布到 PyPI。可运行生命周期与 Stats 示例见
[Python SDK 指南](../../docs/user-guide/zh/token-saving/tokenless/sdk.md)，AgentScope 挂载见
[AgentScope SDK 集成](../../docs/user-guide/zh/token-saving/tokenless/sdk/agentscope.md)，产品 Adapter 见
[Agent 集成指南](../../docs/user-guide/zh/token-saving/tokenless/framework-integration.md)，内部契约见
[Runtime 设计](docs/design/runtime-library_zh.md)。

同一个 Wheel 还提供不依赖 CLI 的只读 typed Stats 查询。可以让 `TokenlessStats` 指向
Runtime 使用的状态目录，或使用延迟创建的 `sdk.stats`：

```python
from anolisa_tokenless import TokenlessStats

stats = TokenlessStats("/absolute/path/to/tokenless-data")
summary = stats.summary()
print(summary.total.tokens_saved, summary.total.tokens_saved_percent)
```

Token 数量是估算值，并且只有产生正向节省的操作才会记录。`show()` 和详细 `diff()`
结果可能包含 `stats.db` 中保存的敏感工具输入与输出。这里的只读是指 API 能力；客户端
打开时遵循 CLI 初始化流程，可能创建或迁移 `stats.db`，因此数据目录必须可写。
`summary(limit=None)` 和 `compare(..., limit=None)` 最多查询最近 10,000 条记录；Session
或 Tool-use Diff 最多读取最近 10,000 条匹配记录。要获得有意义的对比，应先传入 dry-run
Session，再传入启用 Tokenless 的 Session。

### OpenCode 安装

OpenCode 适配器通过 `tool.execute.before/after` 原生插件事件注册已硬关闭的 Tool Ready、
RTK 命令重写和响应/TOON 压缩，并通过 `tool.definition` 压缩工具 Schema。
压缩后的响应会替换原始模型可见输出，避免重复占用上下文。
响应中包含 Retrieve Marker 时，模型可以通过已有 Shell Tool 执行其中的
`tokenless retrieve` 命令；成功的恢复结果会绕过压缩并原样返回。

通过 ANOLISA 启用已安装的插件，然后重启 OpenCode：

```bash
anolisa adapter enable tokenless opencode
```

driver 按 `OPENCODE_CONFIG_DIR`、`XDG_CONFIG_HOME/opencode`、`~/.config/opencode`
的顺序选择目录并创建 `plugins/tokenless.js`。它不读取 `TOKENLESS_OPENCODE_CONFIG_DIR`；
如需共用自定义目录，请设置 `OPENCODE_CONFIG_DIR`。enable 会接管指向同一源路径且不经过目录别名的现有链接，
`anolisa adapter disable tokenless opencode` 会删除该链接。两种操作后都应重启 OpenCode。

独立源码安装可使用：

```bash
make opencode-install
```

Bundle 生命周期脚本额外支持最高优先级的 `TOKENLESS_OPENCODE_CONFIG_DIR` 覆盖。
两种方式都会拒绝冲突的文件或链接。在 ANOLISA disable 后若要恢复独立管理，重新运行
`make opencode-install` 或 Bundle 中的 `scripts/install.sh`。
完整生命周期参阅[框架集成](../../docs/user-guide/zh/token-saving/tokenless/framework-integration.md#opencode)。

### QwenPaw 安装

QwenPaw 适配器是一个原生 QwenPaw 插件：`plugin.py` 通过 `api.register_middleware` 注册
AgentScope 中间件，通过 `api.register_tool` 注册 `tokenless_retrieve` 工具，并直接调用进程内的
`anolisa_tokenless.TokenlessSdk`。`on_model_call` 压缩工具 Schema，`on_acting` 在 QwenPaw 审批之后
用 RTK 改写 `execute_shell_command` 的输入，并替换 QwenPaw 内置工具结果中的文本块（文件读取类工具和内置表之外的工具原样透传）。

```bash
make qwenpaw-install
```

安装器执行 `qwenpaw plugin install <bundle> --force`，由 QwenPaw 把 Bundle 复制到
`<工作目录>/plugins/tokenless/`（`QWENPAW_WORKING_DIR`，否则 `COPAW_WORKING_DIR`，否则已存在的
`~/.copaw`，否则 `~/.qwenpaw`），并按 `requirements.txt` 从对应 GitHub Release
安装 `anolisa_tokenless` wheel。统计记录写入 `<workspace>/.tokenless`。

在把 Bundle 交给 QwenPaw 之前，安装器会先探测这个 wheel URL，资产返回 `404` 时给出说明性报错并停止。
离线或镜像网络可用 `ANOLISA_SKIP_WHEEL_PREFLIGHT=1` 跳过探测；
`ANOLISA_TOKENLESS_PROBE_TIMEOUT` 用于设置单次探测的超时秒数（默认 15）。完整参考见
[故障排查](../../docs/user-guide/zh/token-saving/tokenless/troubleshooting.md#qwenpaw-安装提示-sdk-wheel-不可用)。

### DeepSeek Harness 插件

DSH 原生 Bundle 通过 `tools/post-execute` 把可替换的单文本工具结果交给 Tokenless
PostTool Core。内容检测、JSON 清理、TOON 选择、最终接受和环境错误诊断均由 Core
负责。缩减结果包含 Retrieve Marker 时，模型可以通过已有 Shell Tool 执行其中的
`tokenless retrieve` 命令。Adapter 从 `exec.arguments.command` 检查成功执行的单条恢复命令，
并让其结果绕过 Core 压缩、原样返回。只有裸 `tokenless` 能从 Shell 的 `PATH` 解析到
`tokenlessBin` 或 `TOKENLESS_BIN` 为 Core 选中的同一个可执行文件时，Adapter 才启用可恢复压缩；
仅给 Plugin 配置绝对路径，或让裸命令指向另一个二进制，都不足以执行 Marker 恢复。
DSH 会从模型 Shell 命令中清除继承的 `TOKENLESS_*` 环境变量，因此 Adapter 会把选定的状态目录
以及可选统计库和 Stash 库覆盖作为受控 DSH Shell 信息发布，并让 Core 使用相同路径。默认目录
是会话工作区中的 `.tokenless`，其中包含自忽略的 `.gitignore`；如需覆盖，应在启动 DSH 前设置
`TOKENLESS_DATA_DIR`、`TOKENLESS_STATS_DB` 或 `TOKENLESS_STASH_DB`，且路径必须可从 DSH Shell
沙箱访问。关闭响应压缩后，错误指引仍会工作。

需要启用多个 DSH profile 时，应在同一条命令中重复传入 `--profile`。

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

每次 enable 或 re-enable 都会把本次传入的 profile 视为完整目标集合。旧 receipt
中已有但本次没有列出的 profile 会卸载 Bundle，因此每次都要列出需要继续使用
Tokenless 的全部 profile。每个名称必须与 `dsh --profile <profile>` 使用的名称
一致。配置写在对应 profile 的 `cordis.patch.yml` 中。全部配置项和默认值见
[DeepSeek Harness 集成参考](../../docs/user-guide/zh/token-saving/tokenless/framework-integration.md#deepseek-harness-原生处理路径)。

### AgentScope 框架集成

AgentScope 1.0.11 至 1.0.x 及 AgentScope 2.0.x 应用需要显式安装两个相同版本的 Python
Wheel。框架集成直接调用 `anolisa-tokenless` Runtime，不会启动 CLI 子进程。两个 Python
包当前都尚未发布到包索引。当前应从源码 checkout 构建并同时安装两个 Wheel：

```bash
make python-wheel agentscope-wheel
python -m pip install \
  target/wheels/anolisa_tokenless-*.whl \
  target/wheels/anolisa_tokenless_agentscope-*.whl
```

两个大版本使用相同的公开入口和配置对象；由于 AgentScope 1.x 与 2.x 提供的生命周期
扩展点不同，仅最后的挂载方式不同。

AgentScope 1.x 使用 Tokenless Toolkit，因此在 Agent 构造前后动态注册的普通工具和
MCP 工具都会获得相同的生命周期处理。安装时必须显式提供 Session 标识。

```python
from agentscope.agent import ReActAgent
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = integration.create_toolkit()
toolkit.register_tool_function(application_tool)
agent = ReActAgent(..., toolkit=toolkit)
integration.install(agent, session_id="conversation-id")
```

AgentScope 2.x 在构造阶段接收恢复 Tool 和中间件；该方式从 2.0.0 即可使用，不依赖后续
补丁版本才新增的 Toolkit 动态修改接口。

```python
from agentscope.agent import Agent
from agentscope.tool import Toolkit
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
        # retrieve_tool_name="tenant_tokenless_retrieve",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = Toolkit(tools=[*application_tools, *integration.tools])

agent = Agent(
    ...,
    toolkit=toolkit,
    middlewares=integration.middlewares,
)
```

AgentScope App 从 2.0.3 开始支持。它会在配置的绝对基础目录下，为每个
user/agent/session 派生独立的 Tokenless 数据目录：

```python
from agentscope.app import create_app

app = create_app(..., **integration.app_options())
```

`app_options()` 只提供一个 Middleware Factory。AgentScope 通过该 Middleware 实例的
`list_tools()` 发布静态 Retrieve Tool，并在 `AgentState.middle_context` 中持久化 Marker 授权。

如果应用已经定义 `tokenless_retrieve`，应在 `TokenlessConfig` 中设置唯一的
`retrieve_tool_name`；App 组装阶段不会把其他工具暴露给该 factory，无法预先检查重名。

AgentScope 2.0.0 至 2.0.2 只支持直接构造 Agent；这些版本的 App API 尚未同时提供由
Middleware 发布 Tool 和持久化 Middleware 状态的能力。原有 `TokenlessMiddleware` 2.x API
继续保留兼容；新代码应使用 `TokenlessAgentScope`。

AgentScope 为已知的 Shell、文件和 API 工具提供显式契约。每个自定义工具都必须注册
`ToolContract`：从 `COMMAND_OUTPUT`、`FILE_CONTENT` 或 `API_RESPONSE` 中选择来源，
并且只为可能由 RTK 改写的命令设置 `command_field`。未知自定义工具会在注册阶段或
Model 边界快速失败，不会根据输出文本猜测来源。压缩阈值、TOON 选择、诊断和 Retrieve
授权都保留在 Rust Core。

只读 Retrieve Tool 的声明保持静态，并在模型调用之间留在工具列表中；它只接受本次模型
调用保留的精确 Marker 集合中的 Hash。直接构造 Agent 时，每个用户或租户必须显式传入
不同的绝对 `data_dir`；省略 `data_dir` 时，`TOKENLESS_DATA_DIR` 只作为进程级回退。除非
应用有明确生命周期策略，否则保留默认一小时 Stash TTL，且不要依赖跨节点恢复。

两个 AgentScope Adapter 都启用 Schema 压缩、RTK 命令改写、响应压缩、TOON、恢复、
环境错误提示和逐调用归属。原生 Wheel 内置 RTK 并直接链接 TOON，不搜索系统可执行文件。
宿主对象和流式 chunk 保持不变，只转换复制后的调用参数和最终模型可见文本。Tool Ready
仍保持硬关闭。

## Raw 打包

Raw 打包接收同一目录中已经构建好的 `tokenless` 和 `rtk`，并按照
组件维护的稳定目录结构生成制品：

```bash
make package-raw \
  BIN_DIR="$PWD/target/release-bins" \
  TARGET_OS=linux \
  TARGET_ARCH=aarch64 \
  OUTPUT_DIR="$PWD/dist"
```

Raw 支持矩阵为 `linux-x86_64`、`linux-aarch64` 和 `macos-aarch64`。
输入可使用 `darwin`/`arm64`、`amd64`/`x64` 别名，产物名始终采用 ANOLISA
规范名称。脚本不会执行跨平台二进制，而是直接检查 ELF 或 Mach-O 架构，
并负责嵌入组件自维护的 `.anolisa/component.toml`、展开适配器 Hook 符号链接、
统一权限以及生成可复现的
`tokenless-<version>-<os>-<arch>.tar.gz`。需要固定其他时间戳时可传入
`SOURCE_DATE_EPOCH`。

npm 打包同样从 `target/npm-prebuilt` 下读取预构建的 `linux-x64`、
`linux-arm64`、`darwin-x64`、`darwin-arm64` 四个二进制目录，并负责校验和组装：

```bash
node npm/scripts/package-npm.js --all
```

固定目录结构和单目标接口见 [npm/README.md](npm/README.md#packaging-for-npm)。

## 查看 Token 节省明细

`stats summary` 用于查看合计；`show` 用于原样打印完整的压缩前后内容；
`diff` 用于解释估算 Token 节省，并只突出发生变化的行：

```bash
tokenless stats summary
tokenless stats summary --limit 1000
tokenless stats summary --compare <baseline-session> <active-session>
tokenless stats show 42
tokenless stats diff 42
tokenless stats diff --session <session-id>
tokenless stats diff --session <session-id> --tool-use-id <tool-use-id>
tokenless stats diff 42 --json
```

`stats summary --limit`、`stats list --limit` 与 `stats diff --limit` 必须为正整数；
`--limit 0` 会在解析阶段被拒绝。`--compare` 在任一 Session 没有记录时失败，而不是
报告 0% 节省。Session 总览只包含指标；单记录和 tool-use 报告包含 unified content diff。只有相邻
active 阶段的输出与输入内容完全一致时才会串成一条链，从而避免重复计算中间
阶段的 Token。完整选项和度量限制见
[Tokenless 效果度量](../../docs/user-guide/zh/token-saving/tokenless/measuring-savings.md)。

## Trace 关联

导出的 SLS 记录会带上产生它的宿主 span 的 trace 标识，AgentLoop
这类可观测后端因此可以把 Token 节省量归因到具体 trace。两个可选
环境变量负责传入该标识：

- `TOKENLESS_TRACEPARENT` —— 面向 Adapter 的覆盖项，优先读取。
- `TRACEPARENT` —— 标准 W3C 变量，覆盖项缺失、为空或无法解析时使用。

注入是启动方的责任：OpenTelemetry 只在进程内 carrier 中保存 active
span，不会导出到子进程，因此需要关联能力的宿主或 Adapter 必须
在启动 Tokenless 前写入其中一个。没有可用上下文时记录结构不变，
且该标识只写入 SLS JSONL，不会写入本地 `stats.db`。详见
[Tokenless 效果度量](../../docs/user-guide/zh/token-saving/tokenless/measuring-savings.md)
与
[配置与数据隐私](../../docs/user-guide/zh/token-saving/tokenless/configuration-and-privacy.md)。

## 数据库位置

Tokenless 默认将统计数据和可逆压缩数据分别存储在
`~/.tokenless/stats.db` 与 `~/.tokenless/stash.db`。可为两个数据库统一
指定目录：

```bash
export TOKENLESS_DATA_DIR="$HOME/path/to/tokenless-data"
```

该目录可以是当前用户有权访问的任意绝对路径，包括 `/var/lib` 下由服务管理
的目录；文件系统根目录、相对路径和父目录遍历会被拒绝。若只需自定义一个
数据库，现有的 `TOKENLESS_STATS_DB`、`TOKENLESS_STASH_DB` 和 `--stash-db`
覆盖项优先级更高，但必须位于真实用户 home 或选定的数据目录下。配置文件
仍位于 `~/.tokenless/config.json`。

## Tool Ready

旧版 Tool Ready 会在工具调用前预检 `tool-ready-spec.json` 中声明的环境依赖，
缺失时报告 `NOT_READY` 并提示跳过重试。当前已无条件硬关闭，Hook 会在读取规范、
检查、修复或阻断之前返回；工具执行后的失败归因保持独立。

```bash
# 报告单个工具对应的硬关闭状态
tokenless env-check --tool Shell

# 报告全部工具模式的硬关闭状态
tokenless env-check --all

# 报告清单模式的硬关闭状态
tokenless env-check --checklist

# 机器可读的硬关闭状态；不会输出 tools/summary 清单
tokenless env-check --checklist --json

# 为兼容性保留；不会检查或修复环境
tokenless env-check --tool Shell --fix
```

这些命令当前只报告 Tool Ready 已硬关闭，不会检查或修改环境。
所有 JSON 模式都只返回相同的三个字段：

```json
{"tool":"checklist","status":"UNKNOWN","enabled":false}
```

`tool` 表示指定的工具或 `all`/`checklist` 范围。硬旁路生效期间绝不会输出
休眠旧版实现的 `tools` 与 `summary` 清单字段。

## 架构

安装产物检查与可选的真实 Agent 任务见
[发布前回归测试](tests/release_regression/README_zh.md)，分别报告工具输出节省和恢复开销。

- `crates/tokenless-schema/` — BeforeModel 工具 Schema 压缩：`SchemaCompressor`
- `crates/tokenless-ccr/` — 可逆压缩缓存（Compress-Cache-Retrieve）
- `crates/tokenless-runtime/` — 生命周期 API 与 Runtime 内部的 `PostToolPipeline`
- `crates/tokenless-protocol/` — 版本化 Adapter 契约与共享 `heuristic-v1` Token Estimator
- `crates/tokenless-compressors/` — 已接入 PostTool 的 `JsonCompressor`、`TabularCompressor` 与 `BuildLogCompressor`
- `crates/tokenless-cli/` — CLI 二进制
- `python/tokenless/` — 面向 CPython 3.11+ 的 PyO3 `anolisa_tokenless` 包
- `python/agentscope/` — 独立的 AgentScope 框架集成与 Wheel 元数据
- `adapters/tokenless/` — 面向具体 Agent/CLI 的 Plugin、Hook 与 Extension 适配器包
- `adapters/tokenless/dsh/`。DeepSeek Harness 原生 Bundle
- `third_party/rtk/` — RTK 命令重写引擎（vendored）
- `packaging/raw/` — Tokenless 自维护的 ANOLISA Raw 打包与目标校验

## 前置依赖

- **Rust** toolchain >= 1.89 — RTK（edition 2024）及 toon-format 所需
- **just** — 用于下载并应用 RTK Patch
- **Git** — 用于通过 justfile 下载 RTK 源码
- **CPython 3.11+ 开发环境与 uv** — 仅构建 Python Wheel 或显式包含全部
  workspace member 的命令需要

## 许可证

Apache License 2.0 — 详见 [LICENSE](../../LICENSE)。

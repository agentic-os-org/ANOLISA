# Tokenless 用户手册

[English](../../en/tokenless/index.md)

Tokenless 面向工具调用密集的 AI Agent。它的 CLI 可以精简 Schema 和工具响应，Adapter 还可以改写 Shell 命令、检查工具依赖，并把压缩结果交给 Agent。最终效果取决于宿主框架：有的 Adapter 会替换原始结果，有的只会追加压缩上下文而保留原文。

第一次使用请从[快速开始](index.md)进入。

## 快速上手

大约三分钟内完成 Tokenless 安装、接入 Claude Code、运行一次真实任务，并确认一条
压缩前后的 Token 记录。Tokenless 在后台工作，不需要改变 Prompt 或日常使用
Agent 的方式。

实际节省效果取决于工作负载。工具调用密集型任务通常最明显；较短或以对话为主的
任务可能变化不大。

### 1. 安装 Tokenless 并接入 Claude Code {#安装-tokenless}

根据使用场景选择安装方式：

| 方式 | 适用场景 | 说明 |
|------|----------|------|
| [anolisa CLI](#方式-aanolisa-cli推荐) | 完整 ANOLISA 组件管理 | 统一管理所有组件和 Adapter |
| [RPM](#rpm-alinux) | 已配置 YUM 仓库的 Alinux | 通过 `yum` 安装受管软件包，再 adopt 进 anolisa 系统状态 |
| [npm](#方式-bnpm) | 独立安装 CLI 和 Adapter | 面向开发者，预编译二进制 + Adapter 资源 |
| [curl](#方式-ccurl-独立安装) | Linux 或 macOS 上的一键安装 | 有 npm 时走 npm（需要 Node.js 16.7+），否则源码构建（需要 Rust 工具链） |
| [Skill](#方式-dskill面向-agent) | Agent 自动安装 | 面向 Agent 框架的 Skill 安装方式 |

#### 方式 A：anolisa CLI（推荐）

下面以 Claude Code 作为示例 Agent：

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
anolisa install tokenless
anolisa adapter enable tokenless claude-code
```

如果已经安装 anolisa CLI，可以直接从 `anolisa install tokenless` 开始。只有首次
安装提示当前 Shell 找不到 `~/.local/bin` 时，才需要执行 PATH 设置。

使用其他 Agent？按照[使用其他 Agent](#使用其他-agent)中的对应接入方式操作，后续
步骤保持不变。下方列出的 Agent（包括 OpenCode）都支持 `anolisa adapter enable`。
OpenCode 还提供自带的生命周期脚本，供 npm 或手工安装使用。

#### RPM（Alinux） {#rpm-alinux}

已配置 YUM 仓库的 Alinux 用户可以直接安装受管 RPM，而不必执行 CLI 安装脚本：

```bash
sudo yum install anolisa tokenless
sudo anolisa --install-mode system adopt tokenless
```

`adopt` 会把直接安装的 RPM 记录进系统状态，使 Adapter 命令能使用它的组件契约。
升级与卸载都通过 `yum` 进行，详见[故障排查 · YUM/RPM 安装](troubleshooting.md#yumrpm-安装)。

下面几种方式会自行安装 CLI，且不生成 anolisa 组件记录，因此不适用
`anolisa adapter enable`。

#### 方式 B：npm

需要 Node.js 16.7+——`fs.cpSync` 从该版本起提供，而包的 postinstall 用它放置 Adapter 资源。自动安装适合当前平台的预编译二进制（`tokenless`、`rtk`）和框架 Adapter 资源：

```bash
npm install -g anolisa-tokenless
tokenless --version
```

安装完成后，Adapter 资源位于 `~/.local/share/anolisa/adapters/tokenless/`。npm 安装不会生成 anolisa 组件记录，因此 `anolisa adapter enable` 不适用于这条路径，请按[按安装方式启用 Adapter](#按安装方式启用-adapter)启用。

该目录与 anolisa CLI 共享。当它已属于受管组件安装时，包的 postinstall 会保持原样并给出提示，而不是替换组件记录仍然指向的资源；确需接管时设置 `ANOLISA_TOKENLESS_FORCE_ADAPTERS=1`。该保护随本页所述改动之后的第一个 npm 发布版本一起提供：截至 `0.8.2`（含）的已发布包尚不具备它，会无条件替换该目录，因此安装这些版本前请先备份受管的 adapter 目录（可用 `npm view anolisa-tokenless version` 确认当前发布版本）。

postinstall 还会启用 `claude-code` adapter：它会执行该 adapter 自己的 `install.sh`，在能找到 Claude CLI 时注册插件（用 `CLAUDE_BIN` 指定）。找不到 Claude CLI 时它跳过注册并给出提示，而不是让安装失败。该注册位于 Claude 自己的配置中——既不在 npm prefix 内，也不在 adapter 目录内——因此仅删除该包并不会移除它。

**卸载直接 npm 安装（方式 B）**有它自己的入口，**不是** `scripts/uninstall.sh`。先反注册，再删除包：

```bash
# 1. 你启用过的每个 adapter，例如 claude-code
bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/uninstall.sh
# 2. 然后才是包本身
npm uninstall -g anolisa-tokenless
```

`scripts/uninstall.sh` 是 **curl 安装器（方式 C）**的卸载脚本。它依赖 `~/.local/share/tokenless/install-receipt` 回执，而该回执只由 curl 安装器写入；因此在直接 npm 安装上运行会立即以 `No install receipt found` 退出且不做任何修改。请仅在 curl 安装下使用它。

支持的平台：

| 平台 | 架构 | npm 包 |
|------|------|--------|
| Linux (glibc) | x86_64 | `@anolisa/tokenless-linux-x64` |
| Linux (glibc) | aarch64 | `@anolisa/tokenless-linux-arm64` |
| macOS | x86_64 (Intel) | `@anolisa/tokenless-darwin-x64` —— 仅为发布构建目标，**尚未发布** |
| macOS | aarch64 (Apple Silicon) | `@anolisa/tokenless-darwin-arm64` |

`@anolisa/tokenless-darwin-x64` 只是发布构建目标：registry 中并没有该包，因此 npm 路径无法在 Intel Mac 上提供二进制。方式 C 同样不行——它的源码构建回退只支持 Linux，`scripts/install.sh` 在 macOS 上会直接报错退出而不执行 `cargo`。在该软件包发布之前，Intel Mac 没有受支持的安装路径，详见[平台适配性](#平台适配性)。

#### 方式 C：curl 独立安装

一键安装脚本，优先使用 npm，否则源码构建。两者的选择发生在**执行 npm 之前**——无 npm、平台为 musl Linux，或显式设置 `TOKENLESS_FORCE_BUILD=1`。一旦调用了 `npm install`，安装器**不再自动切换**方式：npm 的退出码无法证明其 postinstall 没有留下框架注册，因此该阶段的失败会被报告为「安装未完成」——保留并指名无法回滚的内容——而不是改用源码构建后宣称安装成功。请重新执行，或用 `TOKENLESS_FORCE_BUILD=1` 显式选择源码构建：

```bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/install.sh | bash
```

前置依赖取决于脚本实际走哪条路径：

| 路径 | 触发条件 | 依赖 | 安装内容 |
|------|----------|------|----------|
| npm | 存在 npm，且平台为 glibc Linux 或 macOS | `curl`、`tar`、Node.js 16.7+（含 `npm`） | `tokenless`、`rtk` 以及 Adapter 资源 |
| 源码构建（仅 Linux） | 在**执行 npm 之前**选定：无 npm、平台为 musl Linux，或设置了 `TOKENLESS_FORCE_BUILD=1`。调用 `npm install` 之后不再自动切换——见上文失败处理说明 | `curl`、`tar`、Rust 工具链（`cargo`） | 仅 `tokenless` CLI —— 不含 `rtk`，也不含 Adapter |

脚本只支持 Linux 和 macOS；在 Windows 上会直接报错退出，请改用 WSL2。源码构建路径同样只支持 Linux：在 macOS 上安装脚本要么走 npm 路径，要么直接报错退出，绝不会调用 `cargo`。

可指定版本或安装目录。变量要传给 `bash`，不要传给 `curl`：

```bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/install.sh | TOKENLESS_VERSION=0.7.4 bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/install.sh | TOKENLESS_INSTALL_DIR=/usr/local/bin bash
```

指定版本是强约束：源码构建只下载对应的 `tokenless/v<VERSION>` tag。该 tag 不存在时安装直接失败，不会静默改用 `main` 分支构建。

安装脚本会把本次创建的文件记录到 `~/.local/share/tokenless/install-receipt`。之后可以只删除这些记录的路径：

```bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/uninstall.sh | bash
```

#### 方式 D：Skill（面向 Agent）

当 Agent 框架（如 cosh-ng、OpenClaw、Hermes 等）需要自行安装和管理 Tokenless 时，可以使用 Skill 方式。

Skill 文件位于仓库的 `src/os-skills/ai/install-tokenless/SKILL.md`。加载此文件的 Agent 可自动完成安装和配置。

它已声明在 `os-skills` 组件清单（`src/os-skills/component.toml`）中，因此下一个 `os-skills` 版本会把它安装到 `/usr/share/anolisa/skills/install-tokenless/`，`anolisa adapter enable os-skills openclaw`（或 `hermes` adapter）会把它部署到对应框架的 Skill 目录。**当前已发布**的 `os-skills` 产物对应的分发契约里还没有它，这是刻意的：该契约被 `index.toml` 固定到某一个带 sha256 的不可变产物，若声明了产物中并不存在的 Skill，启用时复制 source 就会失败。它会随下一次版本号提升一起加入。在此之前，请直接从仓库路径加载该 Skill。

使用时，将 Skill 文件路径指向 Agent 框架，或将其内容直接传给 Agent。Skill 包含完整的安装、验证和框架集成指引。

安装 Tokenless 后，还需要为对应的 Agent 框架启用 Adapter。Skill 会自动引导此步骤，并按它实际采用的安装方式执行，因此对照[按安装方式启用 Adapter](#按安装方式启用-adapter)中对应的一行即可。

#### 按安装方式启用 Adapter {#按安装方式启用-adapter}

安装只是把文件放到磁盘上，并不会把 Tokenless 注册给 Agent。启用方式取决于安装方式：

| 安装方式 | Adapter 资源 | 启用方式 |
|----------|--------------|----------|
| anolisa CLI（方式 A） | 随组件一起安装 | `anolisa adapter scan`，再执行 `anolisa adapter enable tokenless <framework>` |
| npm（方式 B），或 curl（方式 C）走 npm 路径 | 由包的 postinstall 复制到 `~/.local/share/anolisa/adapters/tokenless/` | 运行对应框架自带的脚本，例如 `bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh`。这条路径不能用 `anolisa adapter enable`，因为 npm 安装不会生成 anolisa 组件记录 |
| curl（方式 C）走源码构建路径 | 无 | 不适用 —— 这是 CLI-only 安装。请直接使用 `tokenless` 子命令，或改用方式 A／方式 B 安装以获得 Agent 接入能力 |
| Skill（方式 D） | 取决于 Skill 实际采用的方式 | 按对应方式的行处理 |

启用后请重启 Agent CLI、IDE 或 Gateway。

### 2. 运行一次真实任务

重启 Claude Code，使其加载 Adapter，然后启动新的 Session，并运行一次工具密集型
任务。例如：

> 运行当前仓库的完整测试，只总结失败项。

Prompt 中不需要提到 Tokenless。

### 3. 查看节省效果

Claude Code 使用一次 Shell、API 或其他受支持的工具后，运行：

```bash
tokenless stats list --limit 5
tokenless stats summary
```

输出示例（实际数值因任务而异）：

```text
Showing 1 record(s):
================================================================================
[ID:42] 2026-08-12 10:20:30 | claude-code | Session:- | Tool:- | Chars:5120→2880(-2240) | Tokens:1280→720(-44%)

Tokenless Statistics Summary
============================================================
Total Records: 1

Character Savings:
  Before: 5120 chars
  After:  2880 chars
  Saved:  2240 chars (43.8%)

Token Savings:
  Before: 1280 tokens
  After:  720 tokens
  Saved:  560 tokens (43.8%)

Breakdown by Operation:
----------------------------------------
  compress-response: 1 records
    Chars: 5120 -> 2880 (-43.8%)
    Tokens: 1280 -> 720 (-43.8%)
```

当 `stats list` 中出现 Token 估算值从压缩前到压缩后下降的记录时，首次体验即完成。
如需检查某条记录具体改变了什么，复制其 ID 后运行：

```bash
tokenless stats diff <record-id>
```

需要查看一段时间内的可视化节省趋势时，前往
[AgentSight 用户指南](../agentsight/integrations.md#tokenlesstoken-节省)。
Tokenless 与 AgentSight 由同一用户运行时，Dashboard 可以直接读取本地统计，不需要
配置 SLS。

如果没有记录，可能是内容没有经过 Tokenless，或处理后没有变短。先检查 Adapter
和组件健康状态：

```bash
anolisa adapter status tokenless
anolisa doctor tokenless
```

再参阅[开启后没有产生统计记录](troubleshooting.md#启用后没有产生统计记录)。

Token 数只是在 Tokenless 已处理内容范围内的估算值，不等于模型账单的直接变化。
统计和 diff 可能包含原始工具内容；涉及敏感数据时不要分享输出。完整说明见
[效果度量](measuring-savings.md)和
[配置与数据隐私](configuration-and-privacy.md)。

### 使用其他 Agent

扫描当前机器，然后只启用正在使用的 Agent：

```bash
anolisa adapter scan
```

| Agent | 接入方式 |
|-------|----------|
| cosh-ng | `anolisa adapter enable tokenless cosh` |
| OpenClaw | `anolisa adapter enable tokenless openclaw` |
| Hermes | `anolisa adapter enable tokenless hermes` |
| Qoder | `anolisa adapter enable tokenless qoder` |
| Claude Code | `anolisa adapter enable tokenless claude-code` |
| Codex | `anolisa adapter enable tokenless codex` |
| DeepSeek Harness（dsh） | `anolisa adapter enable tokenless dsh --profile <profile>` |
| OpenCode | `anolisa adapter enable tokenless opencode` |
| Qwen Code | `anolisa adapter enable tokenless qwencode` |
| QwenPaw | `anolisa adapter enable tokenless qwenpaw` |

接入后重启对应的 Agent CLI 或 IDE。OpenClaw 还需要运行
`openclaw gateway restart`；如果安全检查拒绝 Plugin，请按照
[OpenClaw 接入说明](framework-integration.md#2-启用一个-adapter)处理。
DeepSeek Harness 必须提供 `<profile>`，并与 `dsh --profile <profile>` 使用的名称
保持一致。启用 Bundle 后应重启这个 profile。需要启用多个 profile 时，应在同一条
命令中重复传入 `--profile`。

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

后续每次 enable 或 re-enable 都会替换 receipt 记录的完整 profile 集合。每次都要
列出需要继续使用 Tokenless 的全部 profile。

OpenCode 可使用相同的 Adapter 命令。对于没有 ANOLISA 组件记录的 npm 安装，随附生命周期
脚本仍可作为替代方式。

Command Code（`cmd`）当前还没有内置的 ANOLISA Driver，因此 `anolisa adapter scan` 不会列出它，
上面的命令也会报告未知 Framework。请改用随包生命周期脚本注册它的 Mod，详见
[Command Code（cmd）](framework-integration.md#command-code-cmd)。

### 可选：不接入 Agent 测试压缩

需要在启用 Adapter 前单独确认 CLI 时，运行下面这组结果确定的检查：

```bash
tokenless compress-response <<'JSON'
{
  "status": "ok",
  "data": {
    "name": "demo",
    "items": [1, 2, 3]
  },
  "debug": {
    "trace": "verbose"
  },
  "metadata": null
}
JSON

tokenless stats list --limit 1
```

这个示例只压紧格式空白，保留全部字段和值，包括 `debug`、`trace` 和
`metadata: null`。已经紧凑且无法进一步缩小的输入会原样返回，不写入统计记录。

### 平台适配性

| 平台 | anolisa CLI | npm | curl | Skill |
|------|-------------|-----|------|-------|
| Linux x86_64/aarch64（glibc） | 支持 | 支持 | 支持（npm 路径） | 支持（跟随 curl） |
| 使用 musl 的 Linux（例如 Alpine） | 暂不支持 | 暂不支持 | 仅源码构建，需要 Rust 工具链 | 仅源码构建，需要 Rust 工具链 |
| macOS Apple Silicon | 支持 | 支持 | 支持（npm 路径） | 支持（跟随 curl） |
| macOS x86_64 | 暂不支持 | 暂不支持 | 暂不支持 | 暂不支持 |
| Windows | 暂不支持 | 暂不支持 | 暂不支持，请使用 WSL2 | 暂不支持，请使用 WSL2 |

上述边界的补充说明：

- macOS x86_64 在当前版本没有受支持的安装路径。`@anolisa/tokenless-darwin-x64` 只是发布构建目标，registry 中并没有该包，因此 npm 与 curl 的 npm 路径都无法在该平台提供二进制；curl 的源码构建回退在 macOS 上会被拒绝——`scripts/install.sh` 会直接报错退出而不执行 `cargo`。在该软件包发布之前，请使用 Linux 或 Apple Silicon macOS。
- macOS 上的 curl 依赖 npm 路径。其源码构建回退只在 Linux 上验证过，并且安装脚本在 macOS 上拒绝执行该回退，因此没有 npm 的 macOS 机器没有受支持的 curl 路径。
- npm 包声明了 `os: linux, darwin`，所以本页所有方式都不支持 Windows。在 WSL2 内按 Linux 各行处理。
- Skill 方式会委托给 anolisa CLI、npm 或 curl，其支持范围跟随实际选中的方式。

需要从源码构建独立 CLI 时，请参阅[用户手册 · 从源码构建独立 CLI](index.md#从源码构建独立-cli)。

### 下一步

- [Python SDK](../../../developer-guide/zh/sdk/tokenless/index.md)：通用与 AgentScope 两层及可运行示例
- [AgentScope SDK 集成](../../../developer-guide/zh/sdk/tokenless/agentscope.md)：AgentScope 1.x、2.x 与 App 挂载
- [Agent 集成](framework-integration.md)：产品 Adapter 激活
- [能力与边界](#能力与边界)：运行行为和适用范围
- [CLI 参考](cli-reference.md)：全部子命令和参数
- [效果度量](measuring-savings.md)：统计、双跑对比和 AgentSight/SLS
- [配置与数据隐私](configuration-and-privacy.md)：开关、存储和敏感数据
- [故障排查](troubleshooting.md)：常见错误、升级和卸载

## 从源码构建独立 CLI

源码构建适合开发和调试。当前项目只在 Linux 上验证和支持源码构建：

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa/src/tokenless
cargo build --release --locked -p tokenless-cli
./target/release/tokenless --version
```

这条路径只生成独立的 `tokenless` CLI，不会安装 `rtk` 或 Agent 接入资源。需要在 Agent 中使用完整能力时，请按照[快速开始](index.md)通过 anolisa CLI、npm、curl 或 Skill 安装。

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
配置见 [Python SDK 指南](../../../developer-guide/zh/sdk/tokenless/index.md)。

## 能力与边界

| 能力 | 当前代码实际执行的行为 | 重要边界 |
|------|------------------------|----------|
| Schema 压缩 | 移除 `title` 和 `examples`，删除描述中的围栏代码和行内代码，合并空白并截断描述 | Common BeforeModel 在没有 Marker 授权恢复时透传有损变换；OpenCode 逐工具路径和直接 CLI 仍会压缩（Qwen Code 会跳过声明的事件） |
| Content-aware 响应压缩 | 成功的 PostTool JSON 路由给 `JsonCompressor`；已识别的成功构建/测试命令输出路由给 `BuildLogCompressor`；CSV/TSV 路由给 `TabularCompressor`；支持的搜索列表交给 `SearchResultsCompressor`；只接受端到端更小的结果 | 其他内容域与 Tool Error 透传；可恢复缩减需要受 Marker 授权的 Framework 恢复或受支持的 Marker 命令路径 |
| 搜索路径共享 | 在带行号的 API 搜索记录（含 Claude 原生 Grep）中共享连续路径，或在命令与 API 的纯路径列表中共享目录前缀；保留全部已收到记录 | 默认开启；要求文本替换和更小的字节可逆视图；文件读取跳过路径列表策略 |
| TOON 编码 | 编码 JSON；估算 Token 没有下降时保留 JSON 输入 | 宿主支持文本替换时替换原文；无替换能力的宿主透传 |
| 命令重写 | 有匹配规则时调用 `rtk rewrite`，再向框架提交改写后的 Shell 输入 | 已识别的构建/测试命令，以及直接或经内置 Wrapper 调用的 `find`，使整条命令保持原生形式；关闭搜索路径共享不会恢复这些调用的改写；RTK 自定义 Wrapper 前缀不在此保护范围 |
| Tool Ready | 旧版调用前能力，用于检查声明的二进制、版本、配置、权限和可选依赖 | 已硬关闭；不会检查、修复或阻断工具调用 |
| Stash | 保存因字符串、数组、深度或 Schema 描述截断而省略的内容、Record Reduction 背后的完整原始数组，被省略的 Build Log 进度区间 | 默认 TTL 一小时、最多 10,000 个有效条目；JSON 字段名称和空值本身不会触发 Stash 写入 |

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

无论通过 anolisa CLI、npm、curl 还是 Skill 安装 Tokenless，安装都只是把组件和 Adapter 资源放到磁盘上，不会把 Tokenless 注册给任何 Agent。三条生产路径留下的内容不同，因此启用步骤也不同：

| 安装方式 | Adapter 资源 | 启用步骤 |
|----------|--------------|----------|
| anolisa CLI | 随组件一起安装，并生成 anolisa 组件记录 | `anolisa adapter enable tokenless <framework>` |
| npm，或 curl 走 npm 路径 | 由包的 postinstall 复制到 `~/.local/share/anolisa/adapters/tokenless/`；不存在 anolisa 组件记录 | 运行对应框架自带的脚本，例如 `bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh`。这条路径无法使用 `anolisa adapter enable` |
| curl 走源码构建路径 | 无 —— 源码构建只安装 `tokenless` CLI | 不适用。这是 CLI-only 安装，请直接使用 `tokenless` 子命令；需要 Agent 接入能力时改用 anolisa CLI 或 npm 安装 |
| Skill | 取决于 Skill 实际采用的方式 | 按对应方式的行处理 |

CLI-only 用法不需要 Adapter。

关闭也遵循同样的划分：`anolisa adapter disable tokenless <framework>` 只适用于 anolisa CLI 安装；npm 安装需要通过对应框架自带的卸载脚本，或移除安装脚本写入的 Hook 注册来关闭。

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
2. Read/Glob/LSP/NotebookRead 等文件内容工具及别名保留原文。Claude Code 原生 `Grep` 的无上下文 content 模式结果可以使用无损搜索路径共享。非文件来源的命令和 API 结果也可按内容共享纯路径列表，不扩大文件读取的适用范围（见[控制搜索路径共享](#控制搜索路径共享)）。
3. 响应长度达到最小阈值。Core 在共享响应 Hook、OpenClaw 和 Hermes 路径上跳过短于 200 字符的响应。长度按字符数而非字节数计算。
4. 内容命中受支持的压缩域。按阈值截断的响应压缩只处理 JSON 对象和数组；纯文本只有命中匹配的文本压缩器才会被压缩，且各路径可触发的压缩器不同：
   - **4a. 共享响应 Hook 路径**：到达时不是 JSON 的纯文本会交给内容感知的文本压缩（构建/测试日志的终端输出清理与进度缩减、CSV/TSV 表格压紧、API 记录与非文件来源命令/API 路径列表的搜索路径共享、HTML 页面转写，以及需显式开启的 Git Diff 上下文裁剪），见[Adapter 处理规则](framework-integration.md#adapter-处理规则)；表格的具体规则见 [CSV/TSV 压紧保留完整表格](#csvtsv-压紧保留完整表格)，搜索的规则见[控制搜索路径共享](#控制搜索路径共享)。对 Shell 工具，Hook 会先拆出信封中的主文本字段（`stdout` 或 `stderr`，至少 2,000 字符；以 `diff --git` 开头的 Bash `stdout` 不受该下限限制）送入文本槽位，压缩后再回填到同形状的信封中。
   - **4b. OpenClaw**：纯字符串、或内容恰好是单个合法文本块的 `toolResult` 消息走可替换的文本路径。其余 `toolResult`（多个文本块、图片块、空或无效 content）原样跳过——Plugin 直接返回、不调用 Core，这类结果既不压缩也不产生统计。非 `toolResult` 的对象和数组——包括 `{"stdout": ...}` 这类 Shell 信封——整体作为结构化 JSON 传给 Core 且禁用文本替换，信封保持顶层结构，只适用 JSON 域压缩。
   - **4c. Hermes**：对 Shell 工具，Hermes 会拆出信封中的 `output` 字段，把该文本送入 Core 并允许替换，压缩后再回填到同一信封；其他工具的结果直接传递。

   共享响应 Hook 还会在启动压缩子进程前跳过带 YAML frontmatter、形似 Skill 的文本（这类文本在 Core 侧本来也会原样透传）。
5. 压缩结果严格小于原文。响应压缩和 TOON 编码都没有让内容变小时，保留原文。

通过上述检查后，截断强度由工具类别决定。分类和阈值定义在 Adapter 目录下的 `tool_categories.json`（各 Adapter 共享的单一事实来源）；文件缺失或无效时使用内置的安全回退值：

| 类别 | 代表工具 | 字符串截断阈值 | 数组截断阈值 | 最大嵌套深度 |
|------|----------|----------------|--------------|--------------|
| 内容读取类 | Read、Glob、Grep、LSP、NotebookRead 及别名 | 跳过压缩 | — | — |
| Shell/exec | Bash、Shell、exec、terminal 等 | 65,536 字符 | 128 项 | 8 |
| 其他结构化工具 | 未列入前两类的工具 | 1,048,576 字符 | 65,536 项 | 32 |

阈值含义：字符串超过阈值时从阈值处截断（启用 Stash 时可取回原文）。数组只有在长度超过「类别阈值 + 尾部窗口」时才会截断：前部保留至多阈值个元素，尾部默认保留 8 个元素（尾部窗口），被丢弃的中间段在启用 Stash 时可取回，两个窗口之间插入截断标记。至少包含 33 个 JSON Object 的数组不受这些阈值控制，改走记录缩减（Record Reduction）：按 32 条记录的基础预算选取（前 4 条、后 4 条、携带错误或异常信号的记录、数值异常记录，以及其余记录的稳定采样），并追加取回标记，完整原始数组写入 Stash；记录缩减依赖 Stash，没有 Stash 时保留全部记录。嵌套超过深度上限的子树折叠为截断标记。完整规则与参数见 [CLI 参考](cli-reference.md)。

几点路径差异：

- 独立运行 `tokenless compress-response` 时使用 CLI 自身默认值（字符串 4,096 字符、头部窗口 32 项 + 尾部窗口 8 项、深度 8），可用 `--truncate-strings-at`、`--truncate-arrays-at`、`--array-tail-preserve`、`--max-depth` 覆盖，详见 [CLI 参考](cli-reference.md)。
- Codex 和 Qwen Code 在当前 PostToolUse 契约下无法替换模型可见的原始输出，因此不运行响应压缩和 TOON：Codex 保留原文，只对被归类的环境失败附加上下文；Qwen Code 原样透传。各集成的实际能力详见下方适配器表格。
- OpenClaw Plugin 读取同一份 `tool_categories.json` 分类，把工具映射为内容来源（文件内容、命令输出或 API 响应），该文件缺失或无效时回退到内置列表，再由 Core 套用对应阈值；它原有的 `skip_tools`、`shell_tools` 覆盖项已删除，不再控制 Adapter。当前选项见[配置与数据隐私](configuration-and-privacy.md)。
- TOON 编码是独立的触发判断：只对至少 500 字符的负载、且宿主槽位接受文本时运行，并且只有编码结果比当前内容更小时才会采用。
- Git Diff 上下文裁剪是独立的可选判断，默认关闭：在 Agent 进程环境设置 `TOKENLESS_DIFF_COMPRESSION_ENABLED=1`（或 SDK 的 `diff_compression_enabled` 选项）后，槽位接受文本时 Core 才会裁剪命令输出中 Git Diff 的未变更上下文；每条变更行都保留，完整原始输出写入 Stash 并附取回提示，计入该包装文本后净节省不足 16 个估算 Token 的候选会被拒绝。
- Python SDK 与 AgentScope 层不通过 Python 配置设置上述阈值：压缩阈值、内容检测和 TOON 选择都是 Core 行为；直接调用 `TokenlessRuntime.compress_response` 时仍可按次覆盖截断参数。详见 [Python SDK](../../../developer-guide/zh/sdk/tokenless/index.md) 与 [AgentScope 集成](../../../developer-guide/zh/sdk/tokenless/agentscope.md)文档。

### 控制搜索路径共享

带行号的 API 搜索记录，以及命令或 API 的纯路径列表默认开启搜索路径共享。在 Agent 进程环境中设置 `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0`，
可通过 CLI 关闭该功能。未设置或为空时保持开启，`1`、`true`、`yes`（不区分大小写）也表示开启；
其他值均关闭。该设置独立于 `config.json`。Python SDK 可使用
`TokenlessConfig(search_path_sharing_enabled=False)` 关闭；Rust 将
`RuntimeConfig.search_path_sharing_enabled` 设为 `false`。所有入口均默认开启。

关闭此功能时搜索列表原样返回。其他工具名仍可使用 JSON、表格和日志压缩。精确名称 `Grep`
始终排除这些压缩器以保留已收到命中，即使路径共享关闭也不例外。因此，自定义 `Grep` 工具
无法通过此开关恢复此功能引入前的 JSON、表格和日志压缩。支持的无上下文 Claude Grep 结果
保留全部已收到命中。带行号的搜索记录要求 API 来源；纯路径列表也允许命令输出，包括没有 RTK 的 Bash。
文件内容和文件读取跳过路径列表策略；既有文件读取的 JSON、表格和日志处理仍可用。
其他非文件来源工具结果无论工具名称如何，都可使用同一 Core 能力。

纯列表至少需要三条 POSIX 路径，并使用统一的 LF 或 CRLF。更小的视图以固定英文说明开头，
后接包含公共目录 `base`、全部 `paths`、`line_ending` 和 `trailing_newline` 的紧凑 JSON。
将 `base` 与每个解码后的路径项拼接，保留顺序和重复项，以 `line_ending` 连接；仅当
`trailing_newline` 为 true 时追加该行尾。无需 Stash 或恢复调用。混合输出、空行、NUL 分隔、
歧义格式，以及扣除完整格式开销后字节数或估算 Token 无收益的候选均透传。

直接调用或经 [RTK 内置前缀](cli-reference.md#shell-重写) 调用的原生 `find`，使整条命令（含复合命令）
跳过 RTK 改写。支持嵌套和 Shell 反斜杠换行续行；不会读取自定义 `hooks.transparent_prefixes`，
此类前缀下的 `find` 仍可能被改写。关闭路径共享会停止压缩，但保留内置前缀保护，使后续读取
仍可使用完整路径。混合或无收益的原生输出因此可能不压缩。
整任务节省取决于工作负载；搜索结果变小并不保证总 Token 用量更低。

### CSV/TSV 压紧保留完整表格

宿主支持用文本替换输出时，成功的 CSV/TSV 工具结果可以被压紧。文件来源结果、失败工具、
已由 RTK 优化的输出以及 Retrieve 输出透传。支持的表格必须有表头和至少两条等宽数据行，
且逗号或制表符分隔格式没有歧义。此压缩器不处理引号格式错误、分隔符有歧义、单列文本、
Markdown 或定宽表格。

压紧保留全部单元格字符串及行列顺序，包括重复行、空单元格、重复表头、前导零和大数值
字符串。它移除非必要引号，将记录分隔符规范为 LF，但保留单元格内部的换行。不补末尾
记录分隔符，因此已有的末尾分隔符可能被移除，即使这是唯一的格式变化。这保证单元格
等价，不保证原始字节一致，也不保证模型读取准确率不变。

只有完整候选的字符数和估算 Token 数同时减少时才采用，否则原样返回。不设置百分比
收益门槛；估算结果不保证在所有模型的 Tokenizer 下都有节省。

无论表格大小和恢复能力如何，都不采样或缩减行。表格压紧不写 Stash，不产生不完整表格
提示，也无需为完整枚举或计算而先调用 Retrieve。生效压紧报告 `lossless` 和
`tabular_compaction`。此前已存入 Stash 的表格原文仍按现有过期与授权规则取回；新的表格
压紧不保存原始排版。

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

JSON 响应压缩保留诊断类名称的字段、`null` 和空值。Stash 并不能让所有压缩域都可逆：被移除的 Schema `title`/`examples` 以及 Markdown 格式不会保存供取回。启用实际压缩前，应使用有代表性的数据验证关键 Payload。

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
| 第一次安装并验证 | [快速开始](index.md) |
| 使用 npm、curl 或 Skill 安装 | [快速开始 · 安装 Tokenless](index.md#安装-tokenless) |
| 从源码构建独立 CLI | [本页 · 从源码构建独立 CLI](#从源码构建独立-cli) |
| 使用进程内 Python SDK | [Python SDK](../../../developer-guide/zh/sdk/tokenless/index.md) |
| 集成 AgentScope | [AgentScope SDK 集成](../../../developer-guide/zh/sdk/tokenless/agentscope.md) |
| 接入 Agent 产品 | [Agent 集成](framework-integration.md) |
| 手动压缩或取回 | [CLI 参考](cli-reference.md) |
| 了解压缩何时触发、阈值多大 | [本页 · 压缩的触发条件与阈值](#压缩的触发条件与阈值) |
| 查看节省或内容变化、做双跑对比 | [效果度量](measuring-savings.md) |
| 修改配置或了解本地数据 | [配置与数据隐私](configuration-and-privacy.md) |
| 解决无统计、Adapter 或 Stash 问题 | [故障排查](troubleshooting.md) |
| 排查 Schema 压缩没有记录 | [故障排查 · Schema 压缩没有统计记录](troubleshooting.md#schema-压缩没有统计记录) |
| 升级或卸载 | [故障排查 · 升级与卸载](troubleshooting.md#升级与卸载) |

## 推荐的上线顺序

1. 在非敏感测试任务中完成[快速开始](index.md)。
2. 使用 dry-run 记录同一任务的基线。
3. 开启真实压缩并比较结果质量与节省。
4. 确认本地数据和 SLS 策略符合要求。
5. 再为生产使用的 Agent 启用 Adapter。

Tokenless 的配置和 CLI 以当前安装版本的 `tokenless --help` 为最终依据。

# Tokenless 快速开始

[English](../../../en/token-saving/tokenless/QUICKSTART.md)

大约三分钟内完成 Tokenless 安装、接入 Claude Code、运行一次真实任务，并确认一条
压缩前后的 Token 记录。Tokenless 在后台工作，不需要改变 Prompt 或日常使用
Agent 的方式。

实际节省效果取决于工作负载。工具调用密集型任务通常最明显；较短或以对话为主的
任务可能变化不大。

## 1. 安装 Tokenless 并接入 Claude Code {#安装-tokenless}

根据使用场景选择安装方式：

| 方式 | 适用场景 | 说明 |
|------|----------|------|
| [anolisa CLI](#方式-aanolisa-cli推荐) | 完整 ANOLISA 组件管理 | 统一管理所有组件和 Adapter |
| [RPM](#rpm-alinux) | 已配置 YUM 仓库的 Alinux | 通过 `yum` 安装受管软件包，再 adopt 进 anolisa 系统状态 |
| [npm](#方式-bnpm) | 独立安装 CLI 和 Adapter | 面向开发者，预编译二进制 + Adapter 资源 |
| [curl](#方式-ccurl-独立安装) | Linux 或 macOS 上的一键安装 | 有 npm 时走 npm（需要 Node.js 16.7+），否则源码构建（需要 Rust 工具链） |
| [Skill](#方式-dskill面向-agent) | Agent 自动安装 | 面向 Agent 框架的 Skill 安装方式 |

### 方式 A：anolisa CLI（推荐）

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
步骤保持不变。大多数 Agent 使用 `anolisa adapter enable` 命令，OpenCode 则使用
链接指向的生命周期脚本。

### RPM（Alinux） {#rpm-alinux}

已配置 YUM 仓库的 Alinux 用户可以直接安装受管 RPM，而不必执行 CLI 安装脚本：

```bash
sudo yum install anolisa tokenless
sudo anolisa --install-mode system adopt tokenless
```

`adopt` 会把直接安装的 RPM 记录进系统状态，使 Adapter 命令能使用它的组件契约。
升级与卸载都通过 `yum` 进行，详见[故障排查 · YUM/RPM 安装](troubleshooting.md#yumrpm-安装)。

下面几种方式会自行安装 CLI，且不生成 anolisa 组件记录，因此不适用
`anolisa adapter enable`。

### 方式 B：npm

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

### 方式 C：curl 独立安装

一键安装脚本，优先使用 npm，否则源码构建。两者的选择发生在**执行 npm 之前**——无 npm、平台为 musl Linux，或显式设置 `TOKENLESS_FORCE_BUILD=1`。一旦调用了 `npm install`，安装器**不再自动切换**方式：npm 的退出码无法证明其 postinstall 没有留下框架注册，因此该阶段的失败会被报告为「安装未完成」——保留并指名无法回滚的内容——而不是改用源码构建后宣称安装成功。请重新执行，或用 `TOKENLESS_FORCE_BUILD=1` 显式选择源码构建：

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

前置依赖取决于脚本实际走哪条路径：

| 路径 | 触发条件 | 依赖 | 安装内容 |
|------|----------|------|----------|
| npm | 存在 npm，且平台为 glibc Linux 或 macOS | `curl`、`tar`、Node.js 16.7+（含 `npm`） | `tokenless`、`rtk` 以及 Adapter 资源 |
| 源码构建（仅 Linux） | 在**执行 npm 之前**选定：无 npm、平台为 musl Linux，或设置了 `TOKENLESS_FORCE_BUILD=1`。调用 `npm install` 之后不再自动切换——见上文失败处理说明 | `curl`、`tar`、Rust 工具链（`cargo`） | 仅 `tokenless` CLI —— 不含 `rtk`，也不含 Adapter |

脚本只支持 Linux 和 macOS；在 Windows 上会直接报错退出，请改用 WSL2。源码构建路径同样只支持 Linux：在 macOS 上安装脚本要么走 npm 路径，要么直接报错退出，绝不会调用 `cargo`。

可指定版本或安装目录。变量要传给 `bash`，不要传给 `curl`：

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | TOKENLESS_VERSION=0.7.4 bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | TOKENLESS_INSTALL_DIR=/usr/local/bin bash
```

指定版本是强约束：源码构建只下载对应的 `tokenless/v<VERSION>` tag。该 tag 不存在时安装直接失败，不会静默改用 `main` 分支构建。

安装脚本会把本次创建的文件记录到 `~/.local/share/tokenless/install-receipt`。之后可以只删除这些记录的路径：

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/uninstall.sh | bash
```

### 方式 D：Skill（面向 Agent）

当 Agent 框架（如 cosh、OpenClaw、Hermes 等）需要自行安装和管理 Tokenless 时，可以使用 Skill 方式。

Skill 文件位于仓库的 `src/os-skills/ai/install-tokenless/SKILL.md`。加载此文件的 Agent 可自动完成安装和配置。

它已声明在 `os-skills` 组件清单（`src/os-skills/component.toml`）中，因此下一个 `os-skills` 版本会把它安装到 `/usr/share/anolisa/skills/install-tokenless/`，`anolisa adapter enable os-skills openclaw`（或 `hermes` adapter）会把它部署到对应框架的 Skill 目录。**当前已发布**的 `os-skills` 产物对应的分发契约里还没有它，这是刻意的：该契约被 `index.toml` 固定到某一个带 sha256 的不可变产物，若声明了产物中并不存在的 Skill，启用时复制 source 就会失败。它会随下一次版本号提升一起加入。在此之前，请直接从仓库路径加载该 Skill。

使用时，将 Skill 文件路径指向 Agent 框架，或将其内容直接传给 Agent。Skill 包含完整的安装、验证和框架集成指引。

安装 Tokenless 后，还需要为对应的 Agent 框架启用 Adapter。Skill 会自动引导此步骤，并按它实际采用的安装方式执行，因此对照[按安装方式启用 Adapter](#按安装方式启用-adapter)中对应的一行即可。

### 按安装方式启用 Adapter {#按安装方式启用-adapter}

安装只是把文件放到磁盘上，并不会把 Tokenless 注册给 Agent。启用方式取决于安装方式：

| 安装方式 | Adapter 资源 | 启用方式 |
|----------|--------------|----------|
| anolisa CLI（方式 A） | 随组件一起安装 | `anolisa adapter scan`，再执行 `anolisa adapter enable tokenless <framework>` |
| npm（方式 B），或 curl（方式 C）走 npm 路径 | 由包的 postinstall 复制到 `~/.local/share/anolisa/adapters/tokenless/` | 运行对应框架自带的脚本，例如 `bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh`。这条路径不能用 `anolisa adapter enable`，因为 npm 安装不会生成 anolisa 组件记录 |
| curl（方式 C）走源码构建路径 | 无 | 不适用 —— 这是 CLI-only 安装。请直接使用 `tokenless` 子命令，或改用方式 A／方式 B 安装以获得 Agent 接入能力 |
| Skill（方式 D） | 取决于 Skill 实际采用的方式 | 按对应方式的行处理 |

启用后请重启 Agent CLI、IDE 或 Gateway。

## 2. 运行一次真实任务

重启 Claude Code，使其加载 Adapter，然后启动新的 Session，并运行一次工具密集型
任务。例如：

> 运行当前仓库的完整测试，只总结失败项。

Prompt 中不需要提到 Tokenless。

## 3. 查看节省效果

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
[AgentSight 用户指南](../../agent-observability/agentsight/integrations.md#tokenlesstoken-节省)。
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

## 使用其他 Agent

扫描当前机器，然后只启用正在使用的 Agent：

```bash
anolisa adapter scan
```

| Agent | 接入方式 |
|-------|----------|
| cosh / Copilot Shell | `anolisa adapter enable tokenless cosh` |
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

## 可选：不接入 Agent 测试压缩

需要在启用 Adapter 前单独确认 CLI 时，运行下面这组结果确定的检查：

```bash
printf '%s\n' \
  '{"status":"ok","data":{"name":"demo","items":[1,2,3]},"debug":{"trace":"verbose"},"metadata":null}' \
  | tokenless compress-response

tokenless stats list --limit 1
```

命令返回的仍是合法 JSON，其中 `debug` 和 `metadata` 会被省略。不包含可移除字段的
内容会原样返回且不记录。

## 平台适配性

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

需要从源码构建独立 CLI 时，请参阅[用户手册 · 从源码构建独立 CLI](user-manual.md#从源码构建独立-cli)。

## 下一步

- [Python SDK](sdk.md)：通用与 AgentScope 两层及可运行示例
- [AgentScope SDK 集成](sdk/agentscope.md)：AgentScope 1.x、2.x 与 App 挂载
- [Agent 集成](framework-integration.md)：产品 Adapter 激活
- [用户手册](user-manual.md)：能力边界和文档导航
- [CLI 参考](cli-reference.md)：全部子命令和参数
- [效果度量](measuring-savings.md)：统计、双跑对比和 AgentSight/SLS
- [配置与数据隐私](configuration-and-privacy.md)：开关、存储和敏感数据
- [故障排查](troubleshooting.md)：常见错误、升级和卸载

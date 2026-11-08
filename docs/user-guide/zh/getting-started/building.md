# 从源码构建 ANOLISA

[English](../../en/getting-started/building.md)

本指南说明如何从源码构建和安装 ANOLISA 组件，包括统一构建脚本和各组件的构建
入口。组件特有的依赖和运行方式仍以组件 README 为准。验证命令见[测试](testing.md)。

## 1. 准备源码目录

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa
```

通用前置条件包括 Git、Bash 4.4 或更高版本、`make`、用于编译 Rust 原生依赖的
C 编译器、运行构建辅助脚本的系统 `python3`，以及可下载依赖的网络环境。组件矩阵会列出平台特有的要求。
仓库没有统一的 Rust 版本，构建某个组件时请遵循该组件声明的
`rust-toolchain.toml` 或 `rust-version`。

## 2. 仓库结构

下表列出组件在仓库中的位置。

| 组件 | 目录 | 平台和职责 |
|------|------|------------|
| cosh-ng | [`src/cosh-ng`](../../../../src/cosh-ng/README_zh.md) | Rust Agent OS CLI 和 Shell，Linux 提供完整构建，macOS 提供受限源码构建 |
| agent-sec-core | [`src/agent-sec-core`](../../../../src/agent-sec-core/README_zh.md) | Rust sandbox 与 V2 安全 CLI/daemon，以及框架适配器，Linux |
| agentsight | [`src/agentsight`](../../../../src/agentsight/README_zh.md) | Rust/eBPF 可观测组件，Linux 提供完整 tracing，macOS 提供 `trace` 和 `serve` |
| tokenless | [`src/tokenless`](../../../../src/tokenless/README_zh.md) | Rust Token 与命令输出优化，支持 Linux 和 Windows x64 源码构建，macOS 使用从 Linux 交叉编译的 npm 制品 |
| agent-memory（`memory`） | [`src/agent-memory`](../../../../src/agent-memory/README_zh.md) | Rust MCP memory server，Linux |
| os-skills（`skills`） | [`src/os-skills`](../../../../src/os-skills/README_zh.md) | 静态 Skill 定义和脚本，具体平台取决于各 Skill 的声明 |
| anolisa | [`distribution/anolisa`](../../../../distribution/anolisa/README_zh.md) | Rust 组件生命周期 CLI，支持 Linux 和 macOS arm64 |
| SkillFS（`skillfs`） | [`src/skillfs`](../../../../src/skillfs/README_zh.md) | Rust FUSE Skill 文件系统，Linux |
| ws-ckpt | [`src/ws-ckpt`](../../../../src/ws-ckpt/README_zh.md) | Rust workspace checkpoint daemon 和 TypeScript adapter，作为 Linux system service 运行 |
| ktuner | [`src/ktuner`](../../../../src/ktuner/README.md) | Rust kernel tuning engine，Linux |
| AW（`aw`） | [`src/aw`](../../../../src/aw/README_zh.md) | 统一 Agent 入口，Linux |
| `aw-provider-sec-core` | [`providers/agent-sec-core`](../../../../providers/agent-sec-core/README_zh.md) | AW 到 agent-sec-core 的桥接程序，Linux |
| `aw-provider-tokenless` | [`providers/tokenless`](../../../../providers/tokenless/README_zh.md) | AW 到 Tokenless 的桥接程序，Linux |

仓库级规则位于 [`AGENTS.md`](../../../../AGENTS.md)。修改组件前先阅读对应的 `AGENTS.md`，
架构和运行细节请查看组件 README。

## 3. 工具链和系统依赖

构建脚本可以在受支持的 Linux 发行版上安装常用依赖，但无法让不支持的系统构建
Linux-only 组件。

| 需求 | 依据 |
|------|------|
| Node.js | 脚本使用 Node.js 22 或更高版本和 npm 构建 AgentSight 的内嵌 Dashboard，以及独立的框架插件。 |
| Python | 系统 `python3` 用于构建辅助脚本和安装检查。agent-sec-core 默认的 V2 构建不需要 uv、虚拟环境或固定版本 Python runtime。Python SDK wheel 和 Python 专属测试有各自的要求。 |
| Rust | AW 和两个 Provider 固定 `1.97.1`；agent-sec-core 的 linux-sandbox 固定 `1.93.0`；cosh-ng 跟随 `stable`。遵循组件的 `rust-toolchain.toml` 和最低 `rust-version`；最低版本不代表需要单独安装该版本工具链。 |
| cosh-ng | Linux 源码构建需要 `pkg-config` 和 OpenSSL 开发文件。 |
| agent-sec-core | Linux sandbox 运行和集成检查可能需要 bubblewrap、GnuPG 以及 `jq`。 |
| agentsight | Linux 构建需要 clang、LLVM、libbpf 和 ELF 开发文件。运行 eBPF tracing 需要包含 BTF 在内的内核支持。`make build-mac` 构建不含 eBPF 的 macOS local viewer。 |
| tokenless | `just` 用于获取并修补 RTK，OpenClaw plugin 构建需要 npm。 |
| agent-memory | Linux 构建需要 CMake 和 libsystemd 开发头文件；内置的 OpenClaw adapter 由 npm 构建。 |
| SkillFS | Linux 构建需要 FUSE 开发文件。FUSE smoke test 需要 FUSE 3 和 `/dev/fuse`，普通 Cargo 测试不会挂载 FUSE。 |
| ws-ckpt | daemon 运行时使用 Linux systemd 和 btrfs。文件安装使用 system profile；`DESTDIR` 可以暂存文件而不启动服务。 |

组件有固定工具链时，从该组件目录执行命令，rustup 会自动选择对应版本。没有固定
版本的组件应先查看自己的 `Cargo.toml` 和当前 stable 工具链，再开始构建。

### 下载源

需要安装 Node.js 或 rustup 时，脚本默认使用官方 HTTPS 下载源。无法访问这些
地址时，可以显式配置可信的替代源：

| 环境变量 | 默认值 | 含义 |
|----------|--------|------|
| `NODE_DIST_MIRROR` | `https://nodejs.org/dist` | Node 发行目录的基础 URL；脚本追加 `latest-v22.x/`，并使用同一来源的 `SHASUMS256.txt` 校验安装包。 |
| `RUSTUP_INIT_SCRIPT_URL` | `https://sh.rustup.rs` | rustup 引导脚本的 URL。 |

这两个覆盖配置都要求使用不含内嵌凭据的 HTTPS URL。例如：

```bash
NODE_DIST_MIRROR=https://npmmirror.com/mirrors/node \
  ./scripts/build-all.sh --all --deps-only
```

Rust 工具链下载仍遵循 `RUSTUP_DIST_SERVER` 和 `RUSTUP_UPDATE_ROOT`。
组件构建继续使用用户的 npm registry 配置（包括 `npm_config_registry`）、Cargo
配置和 Git 配置。旧脚本自动改写全局镜像配置的行为已移除，请按自己的环境显式
配置这些下载源。

## 4. 统一构建脚本

### 仓库根 Makefile

先准备所选组件的依赖，并使用其支持的构建平台。在仓库根目录中执行：

```bash
make list-components
make build COMPONENTS="cosh-ng tokenless"
# Optionally install the build outputs
make install COMPONENTS="cosh-ng tokenless"
```

### 依赖准备与安装脚本

`scripts/build-all.sh` 准备所选组件在 Linux 上的构建依赖，通过组件 Makefile
构建交付文件，再安装组件文件。未指定组件或使用 `--all` 时，选择以下后端和 AW：

`cosh-ng`、`skillfs`、`ws-ckpt`、`tokenless`、`agentsight`、`agent-sec-core`、
`aw`。

所选后端存在对应的 AW Provider 时，脚本会自动选中该 Provider。发现条件是
`providers/<规范后端名>/` 下同时存在 `Makefile` 和 `Cargo.toml`，且后者的
`[package].name` 以 `aw-provider-` 开头。Provider 名称取自 manifest，而非根据
后端名称拼接：`agent-sec-core` 会带上 `aw-provider-sec-core`，`tokenless` 会带上
`aw-provider-tokenless`。当前源码目录的默认选集共有九个目标。以后增加匹配的
Provider 时会随对应后端自动加入；未选中的后端及其 Provider 不会被额外加入。

选中 `aw` 时（包括 `--all`），脚本构建并暂存所选组件的核心文件、随附技能和匹配的 AW Provider，
跳过 Tokenless、sec-core、ws-ckpt、AgentSight 和 agent-memory 的独立 `-plugins` 产物。
同时选中 `cosh-ng` 时，脚本也会交付所选 sec-core 和 Tokenless 后端的原生 Cosh Extension，
提供 Cosh 安全 Hook 及 Shell 输入和输出优化。AgentSight 仍构建内嵌 Dashboard。
需要某个组件的完整独立适配器包时，选择该组件即可，无需选择 `aw`。

```bash
# Prepare dependencies, build the default targets and install system files
./scripts/build-all.sh --all

# Compile all targets without installing component files
./scripts/build-all.sh --all --no-install

# Install the supported user components, reporting skipped system components
./scripts/build-all.sh --all --user --supported-only

# Check the user subset without changing the host
./scripts/build-all.sh --all --user --supported-only --check-deps

# Compile all default targets in user mode without installing their files
./scripts/build-all.sh --all --user --no-install

# Explicitly allow system build dependencies while keeping component files in user paths
./scripts/build-all.sh --component tokenless --user --install-system-deps

# Build Tokenless and its matching AW Provider
./scripts/build-all.sh --component tokenless --no-install

# Build selected backends and their matching AW Providers
./scripts/build-all.sh --component agent-sec-core --component tokenless --no-install

# Build and stage a package root using distribution package paths
./scripts/build-all.sh --all --prefix /usr --destdir "$PWD/target/pkgroot"

# Prepare dependencies only, or check them without installing anything
./scripts/build-all.sh --all --deps-only
./scripts/build-all.sh --all --check-deps

# Reuse a provisioned host, or inspect the planned commands
./scripts/build-all.sh --component tokenless --ignore-deps --no-install
./scripts/build-all.sh --all --dry-run
./scripts/build-all.sh --help
```

默认源码安装使用 system profile、`PREFIX=/usr/local` 和 `AWPRJ=anolisa`。
system 模式安装缺失系统包和系统组件文件时可能请求 `sudo`；请用普通用户运行构建脚本，
以便后续继续使用该用户的编译缓存。组件安装会复制文件并渲染配置和 service 文件。
启动服务、连接 Agent adapter 由后续配置步骤完成。

统一构建入口不再执行组件卸载。请根据已安装的文件布局使用组件专属的移除流程，
并处理相应的历史路径和运行时数据。

重复的 `--component` 参数用于选择目标，脚本会去重。别名 `sec-core`、`sight`、
`skills`、`memory` 分别对应 `agent-sec-core`、`agentsight`、`os-skills`、
`agent-memory`。后两者是默认列表以外的可选目标。`anolisa` 和 `ktuner` 仍按
第 5 节的组件命令单独构建。发现匹配的 Provider 时使用规范后端名，因此
`--component sec-core` 也会选中 `aw-provider-sec-core`。仍支持显式单独选择
Provider，此时不会选中它的后端。

使用 `--system`（默认）或 `--user` 选择安装 profile，也可以写为
`--install-mode system` 或 `--install-mode user`。user 模式默认使用
`PREFIX="$HOME/.local"`。文件安装支持情况如下：

| 可用组件 | 文件安装 profile |
|------------------|------------------|
| `cosh-ng`、`tokenless`、`aw`、`aw-provider-sec-core`、`aw-provider-tokenless` | system 和 user |
| `skillfs`、`ws-ckpt`、`agentsight`、`agent-sec-core` | 仅 system |

`--all --user` 会一次列出四个不支持的组件，在准备依赖前退出。加上
`--supported-only` 后，脚本会报告并跳过它们，再为保留的后端附加 Provider。
当前默认 user 子集为 `cosh-ng`、`tokenless`、`aw` 和 `aw-provider-tokenless`；
跳过 `agent-sec-core` 时也会省略它的 Provider。仍可显式选择 Provider，单独
构建和安装。该参数要求 user 模式，也会过滤显式的 `--component` 选集；过滤后
没有组件时报错。它还可以搭配 `--no-install`、`--deps-only` 和 `--check-deps`，
对同一组 user 组件进行预编译、依赖准备或检查。不加该参数时，
`--all --user --no-install` 会编译全部默认后端、AW 和匹配的 Provider。

user 模式默认不安装系统包，也不调用 `sudo`。缺失的系统构建包会汇总报告并使
依赖准备停止。请单独准备这些包，或明确添加 `--install-system-deps` 允许脚本
安装它们；组件文件仍按 user profile 安装。系统前置条件满足后，
`--deps-only --user` 可以准备用户自己的 Rust、Node 工具链和缓存。
`--check-deps` 始终只读，即使同时指定 `--install-system-deps` 也不安装依赖。

`--destdir` 要求绝对的打包根目录，按所选 profile 暂存文件，不检查运行时能力，
也不激活服务。发行包使用上例中的 `--prefix /usr`，按发行包路径暂存。
显式单独选择 Provider 只提供 AW 桥接程序，不会选中对应后端，也不会赋予后端
所需的权限。

每次打包都使用空的 `--destdir`。复用打包根目录会保留上次暂存的独立适配器文件。

依赖准备只处理所选构建目标的需求。脚本保留组件编译结果和缓存供后续构建复用，
不准备 uv 或固定版本 Python 环境。RTK、ActPlane 源码准备仍由各组件的构建流程
维护。AW 和 Provider 由脚本直接调用各自的 Makefile；上方仓库根 Makefile 仍有
自己的组件列表。

AW 和 Provider 支持用 `CARGO_TARGET_DIR` 指定构建输出，统一入口以对应组件目录
为基准解析相对值。Agent-sec-core 使用 `BUILD_DIR` 汇集待安装制品；该值原样转发，
以保留原生相对路径语义。两个变量都会作为显式参数传给构建与安装，包括通过
`sudo` 执行的系统安装，两个阶段使用相同的组件目录。其他后端沿用各自的输出布局；
这两个变量不是整个组件栈通用的输出根目录选项。

安装到宿主前，脚本单独检查声明的运行时依赖。system 模式可以集中安装缺失的
原生 runtime package；user 模式只报告这些缺包，不安装系统包。缺少 language
runtime 或宿主能力时，会在安装组件文件前给出提示。
只编译和打包暂存不要求构建机能够运行所选服务。`--ignore-deps` 同时跳过构建环境
准备和运行时检查，适用于已准备好环境的主机。

### 将已安装的主机迁移到 AW

覆盖安装会保留已有适配器文件和框架注册。切换前，使用实际运行框架的服务账户或
Agent 用户，并沿用其 profile、配置路径和环境。记录已启用的插件或 Hook 及其加载来源，
备份生效的配置，并保留原适配器包。由 ANOLISA 管理的注册可用
[`anolisa adapter status`](anolisa-cli.md#adapter) 查看来源。需要停用整项注册时，运行
`anolisa adapter disable <component> <framework>`。独立注册请使用框架自己的插件或
Hook 设置。

1. 准备并校验 [AW 配置](../aw/configuration.md)。按已启用的 Provider、事件和工具选择器，
   核对原生 Hook 的覆盖范围。保留 AW 尚未在当前 profile 接管的原生 Hook。Cosh 继续使用
   sec-core 和 Tokenless Extension，通过 [`/extensions list`](../cosh-ng/core/extensions.md)
   核对来源。依赖 OpenClaw 或 Hermes 的自动快照或原生工具时，保留
   [ws-ckpt 插件](../ws-ckpt/index.md#插件安装)。卸载脚本也会删除框架目录里的 Skill 副本。
   如仍需使用该 Skill，请将它恢复到框架的 Skill 目录。
2. 停止原 Agent 进程或 Gateway，关闭重叠的原生注册，再用 AW 启动。例如，在
   [AW 使用的 OpenClaw profile](../aw/configuration.md#openclaw) 中，用
   `openclaw plugins inspect <id> --json` 分别查看 `agent-sec` 和 `tokenless`。
   如果 AW 已接管 `exec` 扫描，并且可以停用原生扫描器的插件自保护，在选定的
   原生 JSON 中将 `plugins.entries.agent-sec.config.capabilities.scan-code.enabled`
   设为 `false`。同时调整服务环境中已有的 `CODE_SCANNER_HOOK_ENABLED` 覆盖值。
   `agent-sec` 的其他能力仍可使用。如果 AW Tokenless 步骤已覆盖当前 profile
   所需能力，将 `plugins.entries.tokenless.enabled` 设为 `false`。需要原生 Tokenless
   恢复工具时，保留原生插件，并在该 profile 中关闭重叠的 AW 步骤。
3. 通过 AW 启动新会话或 Gateway，按接管的能力执行
   [安全检查](../aw/security.md#验证检查)和 [Tokenless 验证](../aw/tokenless.md#查看效果)。
   对照 AW 记录和框架日志，确认测试调用只触发一次预期的检查或压缩。启动所需服务后，
   在同一环境中运行相应的 core 命令，例如 `agent-sec-cli skillsec status`、`tokenless --version`
   或 `ws-ckpt status`。确认 Agent 能发现并使用所需的 Skill。若已安装 checkpoint
   Skill，可在 [Cosh](../cosh-ng/core/skills.md) 中运行 `/skills detail ws-ckpt`。

回滚时退出 AW 管理的 Agent 并停止其 AW 服务。恢复备份的原生配置，通过原安装工具
或框架设置重新启用之前关闭的注册。使用原 profile 启动原生 Agent 或 Gateway，
再次检查插件、CLI 和 Skill。

## 5. 组件构建入口

除非命令中带有 `cd`，否则都从仓库根目录执行。组件特有的构建细节请阅读对应的
README 和开发指南。

| 组件 | 构建 |
| ------ | ------ |
| [AW](../../../../contribute/zh/aw/building.md) | `make -C src/aw build` |
| [AW sec-core Provider](../../../../providers/agent-sec-core/README_zh.md) | `make -C providers/agent-sec-core build` |
| [AW Tokenless Provider](../../../../providers/tokenless/README_zh.md) | `make -C providers/tokenless build` |
| [os-skills](../../../../src/os-skills/README_zh.md) | `cd src/os-skills && make build` |
| [agent-sec-core](../../../../src/agent-sec-core/README_zh.md) | `make -C src/agent-sec-core build` |
| [agentsight](../../../../src/agentsight/README_zh.md) | Linux 使用 `make -C src/agentsight build`；macOS local viewer 使用 `cd src/agentsight && make build-mac` |
| [tokenless](../../../../src/tokenless/README_zh.md) | Linux：`cd src/tokenless && make build`；[Windows x64](../../../developer-guide/zh/tokenless/windows.md#在-windows-上构建和测试) |
| [agent-memory](../../../../src/agent-memory/README_zh.md) | `cd src/agent-memory && make build` |
| [ws-ckpt](../../../../src/ws-ckpt/README_zh.md) | `cd src/ws-ckpt && make build` |
| [cosh-ng](../../../../src/cosh-ng/README_zh.md) | `cd src/cosh-ng && cargo build --workspace` |
| [anolisa](../../../../distribution/anolisa/README_zh.md) | `cd distribution/anolisa && cargo build --release --locked` |
| [SkillFS](../../../../src/skillfs/README_zh.md) | `cd src/skillfs && cargo build --workspace --release` |
| [ktuner](../../../../src/ktuner/README.md) | `cd src/ktuner && cargo build --release` |

完成构建后，按[测试](testing.md)中的组件门禁验证改动。

## 6. 延伸阅读

- [测试](testing.md)
- [用户安装指南](installation.md)
- [开发指南索引](../../../developer-guide/zh/index.md)
- [组件接入规范](../../../../specs/component-onboarding.md)
- [文档规范](../../../../specs/documentation-standard.md)
- [Commit 规范](../../../../specs/commit-message-standard.md)

组件特有的构建细节、生成制品和运行配置应放在组件 README 或对应开发指南中。本页
只维护仓库级入口，组件清单或脚本接口发生变化时同步更新这里。

## 选择安装文件组

组件提供分组安装目标时，只需后端程序可使用 `install-core`；需要框架集成或技能时，
再运行组件提供的 `install-plugins` 或 `install-skills`：

```bash
make -C src/tokenless build
make -C src/tokenless install-core
```

通过 AW 使用 Tokenless 时，安装 Tokenless core 和独立 Provider 即可；原生
Tokenless 插件按需选择。

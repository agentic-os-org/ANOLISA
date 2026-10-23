# 从源码构建 ANOLISA

[English](../../en/getting-started/building.md)

本指南说明如何从源码构建和安装 ANOLISA 组件，包括统一构建脚本和各组件的构建
入口。组件特有的依赖和运行方式仍以组件 README 为准。验证命令见[测试](testing.md)。

## 1. 准备源码目录

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa
```

通用前置条件包括 Git、Bash 4.3 或更高版本、`make`、用于编译 Rust 或 Python
原生扩展的 C 编译器，以及可下载依赖的网络环境。组件矩阵会列出平台特有的要求。
仓库没有统一的 Rust 版本，构建某个组件时请遵循该组件声明的
`rust-toolchain.toml` 或 `rust-version`。

## 2. 仓库结构

下表列出组件在仓库中的位置。

| 组件 | 目录 | 平台和职责 |
|------|------|------------|
| cosh-ng | [`src/cosh-ng`](../../../../src/cosh-ng/README_zh.md) | Rust Agent OS CLI 和 Shell，Linux 提供完整构建，macOS 提供受限源码构建 |
| agent-sec-core | [`src/agent-sec-core`](../../../../src/agent-sec-core/README_zh.md) | Rust sandbox 与 Python 安全 CLI，Linux |
| agentsight | [`src/agentsight`](../../../../src/agentsight/README_zh.md) | Rust/eBPF 可观测组件，Linux 提供完整 tracing，macOS 提供 `trace` 和 `serve` |
| tokenless | [`src/tokenless`](../../../../src/tokenless/README_zh.md) | Rust Token 与命令输出优化，源码构建面向 Linux，macOS 使用从 Linux 交叉编译的 npm 制品 |
| agent-memory（`memory`） | [`src/agent-memory`](../../../../src/agent-memory/README_zh.md) | Rust MCP memory server，Linux |
| os-skills（`skills`） | [`src/os-skills`](../../../../src/os-skills/README_zh.md) | 静态 Skill 定义和脚本，具体平台取决于各 Skill 的声明 |
| anolisa | [`distribution/anolisa`](../../../../distribution/anolisa/README_zh.md) | Rust 组件生命周期 CLI，支持 Linux 和 macOS arm64 |
| SkillFS（`skillfs`） | [`src/skillfs`](../../../../src/skillfs/README_zh.md) | Rust FUSE Skill 文件系统，Linux |
| ws-ckpt | [`src/ws-ckpt`](../../../../src/ws-ckpt/README_zh.md) | Rust workspace checkpoint daemon 和 TypeScript adapter，作为 Linux system service 运行 |
| ktuner | [`src/ktuner`](../../../../src/ktuner/README.md) | Rust kernel tuning engine，Linux |

仓库级规则位于 [`AGENTS.md`](../../../../AGENTS.md)。修改组件前先阅读对应的 `AGENTS.md`，
架构和运行细节请查看组件 README。

## 3. 工具链和系统依赖

构建脚本可以在受支持的 Linux 发行版上安装常用依赖，但无法让不支持的系统构建
Linux-only 组件。

| 需求 | 依据 |
|------|------|
| Node.js | agent-memory、agentsight、agent-sec-core、tokenless 和 ws-ckpt 的插件构建需要 npm。Node.js 版本要求以各组件的 `package.json` 和构建说明为准。 |
| Python 和 uv | `src/agent-sec-core/agent-sec-cli/pyproject.toml` 要求 Python `==3.11.6`，该项目使用 `uv`。不要把它扩展成仓库级 Python 最低版本。 |
| Rust | `src/agent-sec-core/linux-sandbox/rust-toolchain.toml` 固定 `1.93.0`；`distribution/anolisa/rust-toolchain.toml` 固定 `1.93.1`；`src/cosh-ng/rust-toolchain.toml` 跟随 `stable`。其他组件有 `rust-version` 时以各自 `Cargo.toml` 为准。 |
| cosh-ng | Linux 源码构建需要 `pkg-config` 和 OpenSSL 开发文件。 |
| agent-sec-core | Linux sandbox 运行和集成检查可能需要 bubblewrap、GnuPG 以及 `jq`。 |
| agentsight | Linux eBPF 构建需要 clang、LLVM、libbpf 和 ELF 开发头文件、内核头文件，以及启用 BTF 的内核。`make build-mac` 构建不含 eBPF 的 macOS local viewer。 |
| tokenless | `just` 用于获取并修补 RTK，OpenClaw plugin 构建需要 npm。 |
| agent-memory | Linux 构建需要 CMake 和 libsystemd 开发头文件；内置的 OpenClaw adapter 由 npm 构建。 |
| SkillFS | FUSE smoke test 需要 FUSE 3 和 `/dev/fuse`，普通 Cargo 测试不会挂载 FUSE。 |
| ws-ckpt | 安装 daemon 需要 Linux systemd 和 root 权限。组件的 user-mode Makefile 目标会有意跳过 service 安装。 |

组件有固定工具链时，从该组件目录执行命令，rustup 会自动选择对应版本。没有固定
版本的组件应先查看自己的 `Cargo.toml` 和当前 stable 工具链，再开始构建。

由 uv 管理的 Python runtime 默认从 GitHub 官方
`astral-sh/python-build-standalone` 下载。若当前网络无法访问该地址，请在构建前将
`UV_PYTHON_INSTALL_MIRROR` 设置为兼容镜像的 base URL。

```bash
export UV_PYTHON_INSTALL_MIRROR="https://your-mirror.example/python-build-standalone"
```

## 4. 统一构建脚本

`scripts/build-all.sh` 是 Linux 上的便捷入口，并不负责构建整个 monorepo。请通过
重复的 `--component` 参数选择需要的维护中组件：`skills`、`sec-core`、`tokenless`、
`ws-ckpt`、`memory`、`cosh-ng` 和 `sight`。

`anolisa`、`skillfs` 和 `ktuner` 不由该脚本构建，请按第 5 节的组件命令单独构建。

默认安装模式是 user mode，各组件按自己的安装规则选择用户目录中的路径。
首次安装系统依赖仍可能请求 `sudo`。使用 `--system`（或
`--install-mode system`）切换到系统路径，脚本会暂存文件并可能调用 `sudo`。
`--no-install` 只构建并暂存制品，不执行安装。

安装组件文件前，脚本会集中收集并检查所有已选组件的 runtime contract。user mode
若缺少系统 runtime package，会一次性列出缺失项和 package manager 安装命令，然后
在安装任何组件文件前退出。system mode 会用一次事务安装可自动处理的原生 runtime
package；若缺少 language runtime 或 platform capability，则在修改 package 前退出。
需要 Node.js 的 system install 必须能从标准 system PATH 找到 Node.js 20 或更高版本。

```bash
# Build and install selected components
./scripts/build-all.sh --component sec-core --component tokenless

# Build and stage without installing component files
./scripts/build-all.sh --component sec-core --no-install

# Use system paths instead of the default user profile
./scripts/build-all.sh --component ws-ckpt --system

# Select cosh-ng and agentsight
./scripts/build-all.sh --component cosh-ng --component sight

# Reuse already-installed dependencies
./scripts/build-all.sh --component sec-core --ignore-deps

# Install dependencies only, or print the plan without changing the system
./scripts/build-all.sh --component sec-core --deps-only
./scripts/build-all.sh --component sec-core --dry-run

# Explicit non-interactive mode and help
./scripts/build-all.sh --component sec-core --non-interactive
./scripts/build-all.sh --help
```

`--ignore-deps` 会同时跳过 dependency setup 和 runtime dependency verification。
它只适用于已经准备好全部依赖的主机，调用者需要自行保证安装后的组件具备所需 runtime。

脚本可能先在 `target/` 中生成构建结果，再执行安装，
最终行为仍由组件自己的安装规则决定。例如 `ws-ckpt` 只有在 `--system` 下才会安装
daemon，默认 user profile 不会创建 user service。

## 5. 组件构建入口

除非命令中带有 `cd`，否则都从仓库根目录执行。组件特有的构建细节请阅读对应的
README 和开发指南。

| 组件 | 构建 |
| ------ | ------ |
| [AW](../../../developer-guide/zh/aw/building.md) | `cd src/aw && cargo build --locked -p aw-cli -p aw-service -p aw-package -p aw-provider -p aw-host --bins --examples` |
| [os-skills](../../../../src/os-skills/README_zh.md) | `cd src/os-skills && make build` |
| [agent-sec-core](../../../../src/agent-sec-core/README_zh.md) | `cd src/agent-sec-core && make build-all` |
| [agentsight](../../../../src/agentsight/README_zh.md) | Linux 使用 `cd src/agentsight && make build-all`；macOS local viewer 使用 `cd src/agentsight && make build-mac` |
| [tokenless](../../../../src/tokenless/README_zh.md) | `cd src/tokenless && make build` |
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
- [开发指南索引](../../../developer-guide/zh/README.md)
- [组件接入规范](../../../../specs/component-onboarding.md)
- [文档规范](../../../../specs/documentation-standard.md)
- [Commit 规范](../../../../specs/commit-message-standard.md)

组件特有的构建细节、生成制品和运行配置应放在组件 README 或对应开发指南中。本页
只维护仓库级入口，组件清单或脚本接口发生变化时同步更新这里。

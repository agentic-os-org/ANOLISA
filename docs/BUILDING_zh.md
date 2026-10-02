# 从源码构建 ANOLISA

[English](BUILDING.md)

本指南面向从源码参与 ANOLISA 开发的贡献者，说明仓库级构建入口、聚合测试脚本
的覆盖范围，以及 12 个组件各自的构建和测试命令。组件特有的依赖和运行方式仍以
组件 README 为准。

## 1. 准备源码目录

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa
```

通用前置条件包括 Git、Bash 4.3 或更高版本、`make`、用于编译 Rust 或 Python
原生扩展的 C 编译器，以及可下载依赖的网络环境。组件矩阵会列出平台特有的要求。
仓库没有统一的 Rust 版本，构建某个组件时请遵循该组件声明的
`rust-toolchain.toml` 或 `rust-version`。

## 2. 仓库结构

当前 `src/` 下包含 12 个组件。

| 组件 | 目录 | 平台和职责 |
|------|------|------------|
| copilot-shell（`cosh`） | [`deprecated/copilot-shell`](../deprecated/copilot-shell/README_zh.md) | TypeScript 终端助手，支持 Linux、macOS 和 Windows |
| cosh-ng | [`src/cosh-ng`](../src/cosh-ng/README_zh.md) | Rust Agent OS CLI 和 Shell，Linux 提供完整构建，macOS 提供受限源码构建 |
| agent-sec-core | [`src/agent-sec-core`](../src/agent-sec-core/README_zh.md) | Rust sandbox 与 Python 安全 CLI，Linux |
| agentsight | [`src/agentsight`](../src/agentsight/README_zh.md) | Rust/eBPF 可观测组件，Linux 提供完整 tracing，macOS 提供 `trace` 和 `serve` |
| tokenless | [`src/tokenless`](../src/tokenless/README_zh.md) | Rust Token 与命令输出优化，源码构建面向 Linux，macOS 使用从 Linux 交叉编译的 npm 制品 |
| agent-memory（`memory`） | [`src/agent-memory`](../src/agent-memory/README_zh.md) | Rust MCP memory server，Linux |
| os-skills（`skills`） | [`src/os-skills`](../src/os-skills/README_zh.md) | 静态 Skill 定义和脚本，具体平台取决于各 Skill 的声明 |
| anolisa | [`distribution/anolisa`](../distribution/anolisa/README_zh.md) | Rust 组件生命周期 CLI，支持 Linux 和 macOS arm64 |
| SkillFS（`skillfs`） | [`src/skillfs`](../src/skillfs/README_zh.md) | Rust FUSE Skill 文件系统，Linux |
| ws-ckpt | [`src/ws-ckpt`](../src/ws-ckpt/README_zh.md) | Rust workspace checkpoint daemon 和 TypeScript adapter，作为 Linux system service 运行 |
| ktuner | [`src/ktuner`](../src/ktuner/README.md) | Rust kernel tuning engine，Linux |
| blaze | [`src/blaze`](../src/blaze/README_zh.md) | Rust 单机 sandbox 编排 daemon，Linux |

仓库级规则位于 [`AGENTS.md`](../AGENTS.md)。修改组件前先阅读对应的 `AGENTS.md`，
架构和运行细节请查看组件 README。

## 3. 工具链和系统依赖

构建脚本可以在受支持的 Linux 发行版上安装常用依赖，但无法让不支持的系统构建
Linux-only 组件。

| 需求 | 依据 |
|------|------|
| Node.js | `deprecated/copilot-shell/package.json` 要求 Node.js `>=20.0.0`。agent-memory、agentsight、agent-sec-core、tokenless 和 ws-ckpt 的插件构建也会使用 npm。 |
| Python 和 uv | `src/agent-sec-core/agent-sec-cli/pyproject.toml` 要求 Python `==3.11.6`，该项目使用 `uv`。不要把它扩展成仓库级 Python 最低版本。 |
| Rust | `src/agent-sec-core/linux-sandbox/rust-toolchain.toml` 固定 `1.93.0`；`distribution/anolisa/rust-toolchain.toml` 固定 `1.93.1`；`src/blaze/rust-toolchain.toml` 固定 `1.88.0`；`src/cosh-ng/rust-toolchain.toml` 跟随 `stable`。其他组件有 `rust-version` 时以各自 `Cargo.toml` 为准。 |
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

`scripts/build-all.sh` 是便捷入口，并不负责构建整个 monorepo。当前脚本支持 8 个
组件。

- 默认 6 个组件包括 `cosh`、`skills`、`sec-core`、`tokenless`、`ws-ckpt` 和
  `memory`。
- 可选 2 个组件包括 `cosh-ng` 和 `sight`。可以用 `--all` 或 `--component` 显式加入。

脚本之外的 4 个组件是 `anolisa`、`skillfs`、`ktuner` 和 `blaze`，请按第 5 节的
组件命令单独构建。

默认安装模式是 user mode，组件文件安装到 `~/.local` 和 Copilot Shell 的用户目录
时无需 `sudo`，首次安装系统依赖仍可能请求 `sudo`。使用 `--system`（或
`--install-mode system`）切换到系统路径，脚本会暂存文件并可能调用 `sudo`。
`--no-install` 只构建并暂存制品，不执行安装。

安装组件文件前，脚本会集中收集并检查所有已选组件的 runtime contract。user mode
若缺少系统 runtime package，会一次性列出缺失项和 package manager 安装命令，然后
在安装任何组件文件前退出。system mode 会用一次事务安装可自动处理的原生 runtime
package；若缺少 language runtime 或 platform capability，则在修改 package 前退出。
需要 Node.js 的 system install 必须能从标准 system PATH 找到 Node.js 20 或更高版本。

```bash
# Default six components, user install
./scripts/build-all.sh

# Build and stage without installing
./scripts/build-all.sh --no-install

# Use system paths instead of the default user profile
./scripts/build-all.sh --system

# Include cosh-ng and agentsight as well
./scripts/build-all.sh --all

# Select one or more of the eight supported names
./scripts/build-all.sh --component cosh --component sec-core
./scripts/build-all.sh --component cosh-ng --component sight

# Reuse already-installed dependencies
./scripts/build-all.sh --ignore-deps

# Install dependencies only, or print the plan without changing the system
./scripts/build-all.sh --deps-only
./scripts/build-all.sh --dry-run

# Explicit non-interactive mode and help
./scripts/build-all.sh --non-interactive
./scripts/build-all.sh --help
```

`--ignore-deps` 会同时跳过 dependency setup 和 runtime dependency verification。
它只适用于已经准备好全部依赖的主机，调用者需要自行保证安装后的组件具备所需 runtime。

`--component` 的合法名称为 `cosh`、`skills`、`sec-core`、`tokenless`、`ws-ckpt`、
`memory`、`cosh-ng` 和 `sight`。脚本可能先在 `target/` 中生成构建结果，再执行安装，
最终行为仍由组件自己的安装规则决定。例如 `ws-ckpt` 只有在 `--system` 下才会安装
daemon，默认 user profile 不会创建 user service。

## 5. 组件构建和测试入口

除非命令中带有 `cd`，否则都从仓库根目录执行。以下命令是最小的本地门禁；修改
组件内部实现前，请阅读对应 README 和开发指南。

| 组件 | 构建 | 测试和质量门禁 |
|------|------|----------------|
| [copilot-shell](../deprecated/copilot-shell/README_zh.md) | `cd deprecated/copilot-shell && make deps && make build` | `cd deprecated/copilot-shell && make lint && make test` |
| [os-skills](../src/os-skills/README_zh.md) | `cd src/os-skills && make build` | 没有编译目标。检查变更过的 `SKILL.md`，并按文件说明的解释器运行变更脚本。 |
| [agent-sec-core](../src/agent-sec-core/README_zh.md) | `cd src/agent-sec-core && make build-all` | `cd src/agent-sec-core && make test` 会运行 Python、Rust sandbox 和 OpenClaw plugin 测试。Python 使用 uv 与 Python 3.11.6。 |
| [agentsight](../src/agentsight/README_zh.md) | Linux 使用 `cd src/agentsight && make build-all`；macOS local viewer 使用 `cd src/agentsight && make build-mac` | Linux 使用 `cd src/agentsight && make lint && make test`；macOS 运行 local viewer 和 trajectory collector 相关测试。 |
| [tokenless](../src/tokenless/README_zh.md) | `cd src/tokenless && make build` | `cd src/tokenless && make lint && make test` |
| [agent-memory](../src/agent-memory/README_zh.md) | `cd src/agent-memory && make build` | `cd src/agent-memory && make fmt-check && make lint && make test`；`cd src/agent-memory && make smoke` 覆盖 MCP stdio 路径。仅 Linux。 |
| [ws-ckpt](../src/ws-ckpt/README_zh.md) | `cd src/ws-ckpt && make build` | `cd src/ws-ckpt && make test`；安装和 service 检查需要 Linux system mode。 |
| [cosh-ng](../src/cosh-ng/README_zh.md) | `cd src/cosh-ng && cargo build --workspace` | `cd src/cosh-ng && cargo fmt --all -- --check`，随后按[贡献指南](../src/cosh-ng/CONTRIBUTING_zh.md)选择最接近改动的测试。只有明确要求对大型或跨模块改动执行完整验证时，才运行全量本地门禁。 |
| [anolisa](../distribution/anolisa/README_zh.md) | `cd distribution/anolisa && cargo build --release --locked` | `cd distribution/anolisa && cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked` |
| [SkillFS](../src/skillfs/README_zh.md) | `cd src/skillfs && cargo build --workspace --release` | `cd src/skillfs && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`；Linux 上再运行 `cd src/skillfs && scripts/test.sh` 执行 FUSE smoke test。 |
| [ktuner](../src/ktuner/README.md) | `cd src/ktuner && cargo build --release` | `cd src/ktuner && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test` |
| [blaze](../src/blaze/README_zh.md) | `cd src/blaze && cargo build --workspace --release` | `cd src/blaze && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` |

修改公共 API 或 rustdoc 时，在受影响的 Rust 组件门禁中追加
`cargo doc --workspace --no-deps`。`ktuner tune`、Blaze Firecracker 路径、FUSE 挂载、
eBPF tracing 和系统 daemon 需要普通单元测试之外的主机权限或内核能力。

[CI workflow](https://github.com/alibaba/anolisa/blob/main/.github/workflows/ci.yaml) 还会为部分组件执行 coverage、打包、前端、
adapter 和集成检查。这些任务比矩阵中的最小本地命令更严格，涉及生成制品或框架
adapter 时应再查看 workflow。

## 6. 聚合测试与 PR 门禁

`tests/run-all-tests.sh` 只是部分组件的便捷测试脚本。不带过滤条件时，它只会调用
copilot-shell、agent-sec-core、agentsight、tokenless 和 agent-memory 这 5 个组件，
不会测试 cosh-ng、os-skills、ws-ckpt、anolisa、SkillFS、ktuner 或 blaze。

```bash
./tests/run-all-tests.sh
./tests/run-all-tests.sh --filter shell
./tests/run-all-tests.sh --filter sec
./tests/run-all-tests.sh --filter sight
./tests/run-all-tests.sh --filter tokenless
./tests/run-all-tests.sh --filter memory
```

当前脚本在前置条件缺失时会跳过测试。没有 `uv` 时会跳过 agent-sec-core 的 Python
测试，没有 `/usr/local/bin/linux-sandbox` 时会跳过其 sandbox e2e，没有 `cargo` 时
会跳过 AgentSight。只有 `make` 和 `cargo` 都不存在时才会跳过 tokenless。在非 Linux
系统或没有 `cargo` 时会跳过 agent-memory。即使出现这些跳过，脚本仍会打印成功
信息，因此退出码为 0 不能证明
所有测试都已运行。agent-sec-core 的 e2e 调用还依赖当前工作目录布局，可靠的本地
门禁应使用组件 Makefile。

PR 应根据变更文件选择组件矩阵中的对应行，运行相应的构建、lint 和测试命令。变更
涉及的平台、集成、smoke、前端或文档时，还要补充相应检查。聚合脚本适合作为快速
信号，不应作为 PR 的唯一验收依据。

## 7. 延伸阅读

- [用户安装指南](user-guide/zh/installation.md)
- [开发指南索引](developer-guide/zh/README.md)
- [组件接入规范](../specs/component-onboarding.md)
- [文档规范](../specs/documentation-standard.md)

组件特有的构建细节、生成制品和运行配置应放在组件 README 或对应开发指南中。本页
只维护仓库级入口，组件清单或脚本接口发生变化时同步更新这里。

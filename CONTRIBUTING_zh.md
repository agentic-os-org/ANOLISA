# ANOLISA 贡献指南

[English](CONTRIBUTING.md)

本文档说明仓库级贡献流程，覆盖各组件的开发入口和最低本地检查要求。架构说明及组件专属规则继续放在组件自己的 `AGENTS.md`、`README.md` 或 `CONTRIBUTING.md` 中。

> **完成修改后？** 可以请 AI assistant 阅读 `AGENTS.md`，并协助起草 commit
> message 和 PR 描述。

修改文档前请先阅读
[`specs/documentation-standard.md`](specs/documentation-standard.md)。该文件是文件位置、双语命名和文档同步要求的唯一规范来源。

## 仓库概览

ANOLISA 是一个 monorepo。下表列出十二个组件及其支持的开发平台。构建名称供
`build-all.sh` 使用，scope 用于 commit subject 和 PR title。

| 组件 | 路径 | 平台 | 构建名称 | Scope | 开发入口和最低本地门禁 |
| --- | --- | --- | --- | --- | --- |
| copilot-shell | `deprecated/copilot-shell/` | 所有平台 | `cosh` | `cosh` | `cd deprecated/copilot-shell`；`make deps`、`make build`、`make lint`、`make test` |
| cosh-ng | `src/cosh-ng/` | Linux 完整功能；macOS 功能受限 | `cosh-ng` | `cosh-ng` | `cd src/cosh-ng`; `cargo build --workspace`、`cargo fmt --all -- --check`，随后按 `src/cosh-ng/CONTRIBUTING_zh.md` 选择最接近改动的测试 |
| agent-sec-core | `src/agent-sec-core/` | 仅 Linux | `sec-core` | `sec-core` | Python 3.11.6 和 `uv`; `make build-all`、`make test` |
| agentsight | `src/agentsight/` | Linux 完整 eBPF；macOS 仅 `trace`/`serve` | `sight` | `sight` | Linux 使用 `make build-all`；macOS 使用 `make build-mac`；Linux 运行 `make lint`、`make test` |
| tokenless | `src/tokenless/` | 完整开发在 Linux；发布的 CLI 二进制和 npm adapter 支持 macOS x64/arm64 | `tokenless` | `tokenless` | `make build`、`make lint`、`make test` |
| agent-memory | `src/agent-memory/` | 仅 Linux | `memory` | `memory` | `make build`、`make fmt-check`、`make lint`、`make test`；MCP 改动追加 `make smoke` |
| os-skills | `src/os-skills/` | 资源跨平台；单个脚本自行声明限制 | `skills` | `skill` | 静态 Markdown skill 定义和 shell 资源；`make build` 用于确认没有编译步骤 |
| anolisa | `distribution/anolisa/` | Linux 和 macOS arm64 | 不适用 | `anolisa` | `cargo fmt --all --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked` |
| SkillFS | `src/skillfs/` | 仅 Linux | 不适用 | `skillfs` | `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`；修改 FUSE 时运行 `scripts/test.sh` |
| ws-ckpt | `src/ws-ckpt/` | 仅 Linux | `ws-ckpt` | `ckpt` | `make build`、`make test` |
| ktuner | `src/ktuner/` | 仅 Linux | 不适用 | `ktuner` | `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` |
| blaze | `src/blaze/` | 仅 Linux | 不适用 | `blaze` | `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` |

`tokenless` 可以发布 macOS 产物，但其二进制需要从 Linux 交叉编译。不要把 macOS checkout 当作 tokenless 的构建环境。标为 Linux 的组件都必须在 Linux 上构建和测试。`cosh-ng` 和 `agentsight` 的 macOS 受限路径见上表。

### 工具链前置条件

- `copilot-shell` 和 TypeScript adapter 需要 Node.js 20 或更高版本。
- Rust 组件需要 stable Rust 工具链。若组件固定了工具链版本或增加了原生库，请遵循对应的 `AGENTS.md`。
- `agent-sec-core` 使用 Python **3.11.6** 和 [uv](https://docs.astral.sh/uv/)。`os-skills` 主要由静态 Markdown 和 shell 资源组成，不负责仓库的 Python 环境。
- 修改 `agentsight` eBPF probe 时需要 `clang` 和 `libbpf` 头文件。

## Fork 并创建分支

先在 GitHub 上 fork 本仓库，再克隆自己的 fork。开始修改前，从官方仓库更新
本地 `main`，然后创建一个工作分支。

```bash
git clone https://github.com/<your-account>/anolisa.git
cd anolisa
git remote add upstream https://github.com/alibaba/anolisa.git
git fetch upstream
git switch main
git merge --ff-only upstream/main
git switch -c feature/<scope>/<short-desc>
```

`upstream` 只需添加一次。后续修改时，换一个新的分支名并重复最后四条命令。
每个分支只包含一项逻辑变更。

内部贡献建议使用以下分支名。

```text
feature/<scope>/<short-desc>
fix/<scope>/<short-desc>
hotfix/<scope>/<short-desc>
release/<scope>/vX.Y
```

Fork 贡献者可以使用其他分支名，分支检查只会给出非阻塞警告。

## Issue 和贡献范围

实现非简单功能、行为变更或缺陷修复前，请先创建或找到对应 Issue。安全漏洞应按照 [`SECURITY.md`](SECURITY.md) 的流程处理，不要公开创建 Issue。纯拼写修正等确实简单的改动，可以在 PR 描述中写 `no-issue: <brief reason>`。

每个 PR 聚焦一项逻辑变更。若修改跨越多个组件，请在 PR 中说明契约变化和测试影响。

## 构建和测试入口

### 统一构建

`build-all.sh` 目前只支持八个组件。

- 默认集合为 `cosh`、`skills`、`sec-core`、`tokenless`、`ws-ckpt`、`memory`。
- 可选集合为 `cosh-ng` 和 `sight`。使用 `--all` 或通过 `--component` 指定它们。

它不会构建 `anolisa`、`skillfs`、`ktuner` 或 `blaze`。这四个组件请使用上方矩阵中的组件门禁。

不指定安装模式时，组件文件会安装到用户目录 `$HOME/.local`，这一步不需要提权。
依赖引导仍可能为了安装系统包请求 `sudo`。使用 `--system` 或 `--usr` 会采用系统
安装模式，通常需要 `sudo`。开发时建议指定组件并关闭安装。

```bash
./scripts/build-all.sh --help
./scripts/build-all.sh --no-install --component cosh
./scripts/build-all.sh --no-install --component sec-core
./scripts/build-all.sh --no-install --all
```

主要选项如下。

| 选项 | 作用 |
| --- | --- |
| `--no-install` | 安装依赖并构建，随后跳过安装。 |
| `--install-mode <mode>` | 选择 `user` 或 `system`，默认是 `user`。 |
| `--usr`、`--system` | 选择系统安装模式。 |
| `--ignore-deps` | 跳过依赖安装。 |
| `--deps-only` | 只安装依赖，不构建。 |
| `--uninstall` | 删除已安装文件；与 `--component` 一起使用可限定组件。 |
| `--dry-run` | 只打印动作，不修改文件或 systemd 状态。 |
| `--interactive`、`--non-interactive` | 打开引导流程，或明确选择自动化模式。 |
| `--all` | 纳入可选组件 `cosh-ng` 和 `sight`。 |
| `--component <name>` | 构建或卸载一个受支持的组件，可以重复指定。 |

不要把 `build-all.sh --all` 理解成构建每个 `src/<component>/` 目录。它的范围只包括 `./scripts/build-all.sh --help` 打印的八个名称。

`sight` 选项使用 Linux eBPF 构建。macOS 应在组件目录运行 `make build-mac`。

更完整的依赖和打包说明请阅读 [`docs/BUILDING.md`](docs/BUILDING.md) 与 [`docs/BUILDING_zh.md`](docs/BUILDING_zh.md)。

### 便捷测试脚本

`tests/run-all-tests.sh` 是五个组件的便捷聚合入口，覆盖 `copilot-shell`、`agent-sec-core`、`agentsight`、`tokenless` 和 `agent-memory`。它支持以下过滤器。

```bash
./tests/run-all-tests.sh
./tests/run-all-tests.sh --filter shell
./tests/run-all-tests.sh --filter sec
./tests/run-all-tests.sh --filter sight
./tests/run-all-tests.sh --filter tokenless
./tests/run-all-tests.sh --filter memory
```

当缺少 `uv`、`cargo`、Linux 环境或已安装的 `linux-sandbox` 二进制时，脚本可能跳过组件，并仍然打印成功汇总。因此脚本成功并不能证明整个仓库，甚至不能证明每个选中的测试套件都通过。PR 验收请使用上方矩阵中的受影响组件门禁。

## 各组件本地门禁

PR 涉及的每个组件都要运行适用的格式检查、lint、测试以及组件专属 smoke test。
根指南只保留简短矩阵，修改组件架构、安全行为、FUSE 代码或协议契约前请阅读
对应范围的 `AGENTS.md`。

Rust 代码变更的通用基线如下。

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

修改 Rust 公共 API 或 rustdoc 时，再运行 `cargo doc --workspace --no-deps`。纯文档
变更使用仓库文档检查，组件指南没有额外要求时无需运行 Cargo 验证。

请按仓库矩阵执行组件专属命令。`agent-sec-core` 的 Python 测试使用 `uv` 和 Python 3.11.6，版本命令必须报告 3.11.6。修改 `agentsight` 前端或 eBPF 时，补充对应的构建或 smoke 检查。`agent-memory` 的 MCP 改动需要 `make smoke`；`SkillFS` 的文件系统层改动在 FUSE 可用时需要 `scripts/test.sh`。

CI 会根据变更路径追加 coverage、打包、前端、adapter 和集成任务。修改生成包或框架
adapter 时请查看 [`.github/workflows/ci.yaml`](.github/workflows/ci.yaml)。

## Commit 规范

使用 [Conventional Commits](https://www.conventionalcommits.org/)，subject 使用英文祈使句，格式如下。

```text
type(scope): imperative description
```

scope 必填。Commitlint 会拒绝空 scope。下表列出仓库推荐使用的 scope。使用表外
scope 会产生警告，发起 review 前应说明理由或改用推荐值。

| Scope | 路径或用途 |
| --- | --- |
| `cosh` | `deprecated/copilot-shell/` |
| `cosh-ng` | `src/cosh-ng/` |
| `sec-core` | `src/agent-sec-core/` |
| `skill` | `src/os-skills/` |
| `sight` | `src/agentsight/` |
| `tokenless` | `src/tokenless/` |
| `ckpt` | `src/ws-ckpt/` |
| `memory` | `src/agent-memory/` |
| `anolisa` | `distribution/anolisa/` |
| `skillfs` | `src/skillfs/` |
| `ktuner` | `src/ktuner/` |
| `blaze` | `src/blaze/` |
| `deps` | 依赖变更，包括 lockfile。 |
| `ci` | `.github/workflows/` 和 CI 配置。 |
| `docs` | 纯文档变更。 |
| `chore` | 根目录脚本、工具和其他维护工作。 |

仓库规范要求完整 subject 不超过 50 个字符，commitlint 当前的硬限制是 120 个字符。
description 以小写开头，不要以句号结尾。测试和格式修正应尽量合并到对应的逻辑
commit。每个 commit 都必须使用贡献者自己的 Git identity 添加 `Signed-off-by`
trailer。

```bash
git commit -s -m 'docs(docs): refresh contribution guide'
```

如果 **Commit Message Lint** 拒绝最新的 commit，请修改 message，并安全推送改写
后的分支。

```bash
git commit --amend
git push --force-with-lease
```

如果需要修改更早的 commit，请在交互式 rebase 中选择 `reword`。

```bash
git rebase -i HEAD~N
git push --force-with-lease
```

不要使用普通的 `git push --force`。lease 会防止覆盖本地 checkout 中不存在的
远端改动。

若缺陷由同一 PR 中较早的 commit 引入，请使用 `git commit --fixup=<commit>`，随后
执行 autosquash rebase。修复已经进入 `main` 的代码时，应创建独立 commit，并用
`Fixes:` 指向引入缺陷的 commit。补充已经进入 `main` 的功能时使用 `Supplements:`。
组件版本升级应放在功能分支的最后一个 commit，并在同一 commit 中原子更新所有带
版本号的文件。

## Pull Request 和 CI

请以 [`.github/pull_request_template.md`](.github/pull_request_template.md) 为 PR 描述起点，并保留其中的章节。描述需要说明变更原因、具体内容、关联 Issue 或 `no-issue` 原因、用户或 Agent 影响、风险与兼容性、验证命令和环境，以及文档或回滚说明。

有关联 Issue 时使用 `closes #<number>`、`fixes #<number>` 或 `resolves #<number>`。
PR title 使用 `type(scope): description`。prelint workflow 会把标题、分支、Issue
链接和表外 scope 作为警告。空 scope、不符合 Conventional Commits 语法或超过
120 个字符等 commitlint 错误会阻止 prelint job。

发起 review 前确认以下事项。

- 受影响组件在支持的平台上通过了组件门禁。
- 新行为或变更行为有测试，或者 PR 解释了无法测试的原因。
- PR 模板记录了风险、兼容性和回滚考虑。
- 需要同步的文档和双语对应文件都已更新。

## 文档同步

文档变更必须遵循 [`specs/documentation-standard.md`](specs/documentation-standard.md)，并保持中英文页面语义等价。两种语言中的命令示例必须完全相同。根指南只做跨组件总览，完整用法和架构内容放在以下规范位置。

| 变更 | 同一 PR 中需要更新 |
| --- | --- |
| CLI 命令或 flag | 组件 `README.md` 和对应的 `docs/user-guide/` 页面。 |
| 配置选项 | 组件 `README.md` 和对应的 `docs/user-guide/` 页面。 |
| 安装方式 | `docs/QUICKSTART*.md` 和组件 README。 |
| 架构或协议 | `src/<component>/docs/design/`。 |
| 新增组件 | 根 README，必要时更新 `NOTICE`。 |

日常功能和修复 PR 更新 README 与 user-guide 页面。发布版本 bump PR 再把面向用户的变更汇总到 `CHANGELOG*.md`。

## 许可证

参与贡献即表示你同意贡献内容采用 Apache License 2.0 许可。

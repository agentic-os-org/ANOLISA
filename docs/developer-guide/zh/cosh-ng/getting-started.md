# 开发 cosh-ng

[English](../../en/cosh-ng/getting-started.md)

本指南帮助新贡献者完成环境准备、找到代码入口并验证改动。开始编辑前，请先阅读仓库
`AGENTS.md`、`src/cosh-ng/AGENTS.md` 和本页。其余页面不再重复这些约束。

## 1. 准备工作空间

cosh-ng 是以 Linux 为主要运行环境、同时可在 macOS 构建的 Rust 工作空间。
最低 Rust 版本为 1.74，`rust-toolchain.toml` 会选择 stable Rust，并安装
rustfmt 和 Clippy。

```bash
cd src/cosh-ng
rustup show
cargo build --workspace
```

不要在开发主机上安装软件包、修改服务或运行会产生修改的 `cosh-cli` 命令。请使用单元
测试、模拟实现、`--dry-run` 或明确隔离的环境。

## 2. 先看清运行边界

工作空间包含五个 crate，其中三个会生成面向用户的进程。

| 区域 | 阅读入口 | 边界 |
|---|---|---|
| 结构化系统操作 | `crates/cosh-cli/src/main.rs` | Clap → `cosh-platform` → JSON 信封 |
| Agent 运行时 | `crates/cosh-core/src/main.rs` | JSONL 和注册表输入，模型服务、工具与会话状态 |
| 交互式终端 | `crates/cosh-shell/src/main.rs` | 终端输入、PTY 事件、卡片和 cosh-core 子进程 |
| 共享平台代码 | `crates/cosh-platform/src/lib.rs` | 发行版、软件包、服务、审计和快照适配器 |
| 协议与输出类型 | `crates/cosh-types/src/lib.rs` | 无副作用的数据契约 |

`cosh-shell` 不链接工作空间中的其他 crate。它会启动 `cosh-core`，并通过带版本约束的
JSONL 控制协议通信。两端必须共同维护这个兼容性边界。

所有权和数据流详见[架构](architecture.md)。

## 3. 修改前先找到负责模块

`cosh-shell` 的新功能必须放入已有的负责目录，不要在 `src/` 根目录新增实现文件。

| 改动 | 负责目录 | 常用测试目标 |
|---|---|---|
| PTY、OSC、bash/zsh 集成 | `shell_host/` | `shell_host` |
| 输入路由和多行输入 | `raw_input/`、`input/`、`slash/` | `raw_cli` 或 `logic` |
| Agent 生命周期和事件策略 | `agent/` | `logic` |
| Core 适配和控制消息 | `adapter/` | `protocol` |
| 审批和问题卡片 | `approval/`、`question/`、`ui/` | `raw_cli` |
| Hooks | `hooks/` | 库测试或 `logic` |
| 运行时编排和状态修改 | `runtime/` | 库测试，再运行相关集成目标 |
| Agent 工具和风险规则 | `tools/` | 库测试和对抗性回归 |

移动或新增 Shell 代码后运行布局审计。

```bash
crates/cosh-shell/scripts/check-layout.sh
```

## 4. 使用最窄反馈循环

```bash
# Shared types/platform/CLI
cargo test --locked -p cosh-types
cargo test --locked -p cosh-platform
cargo test --locked -p cosh-cli --test cli_integration

# Core
cargo test --locked -p cosh-core --lib
cargo test --locked -p cosh-core --test jsonl_protocol

# Shell：先跑快速逻辑测试，再跑进程密集型测试
cargo test --locked -p cosh-shell --lib
cargo test --locked -p cosh-shell --test logic
cargo test --locked -p cosh-shell --test protocol
```

改动会启动 `cosh-shell`、显示卡片或经过模型服务交接时，选择 `raw_cli`。涉及 PTY、
OSC、termios、前台程序或原生 bash/zsh 行为时，选择 `shell_host`。

## 5. 验证最终改动

验证范围要与改动匹配。

- 纯文档改动检查链接、Markdown 格式、命令和中英文一致性，无需运行 Rust 测试或构建。
- 普通代码改动运行格式化和最接近改动 crate 或行为的测试。只在能捕获相关问题时
  增加针对性 Clippy 或 integration 检查。
- 较大或跨模块代码改动只在当前任务明确要求时运行全量本地门禁、持久 ECS 或手工级
  验证，其余广泛回归覆盖交给 CI。

公共 API 或 rustdoc 发生变化时，还需运行以下命令。

```bash
cargo doc --workspace --no-deps
```

测试目标和可选门禁组合见[测试](testing.md)。

## 6. 明确维护契约

- 每个 `cosh-cli` 结果都使用 `CoshResponse<T>` 和稳定退出状态。
- 未与守护进程协调时，禁止调整 ws-ckpt 协议枚举的变体顺序。
- 修改 cosh-core 协议时，必须同步更新协议类型、生产端、消费端、测试数据和协议测试。
- 安全允许规则必须先切分参数，拒绝 Shell 元字符，并在无法判断时拒绝执行。测试要覆盖
  制表符、换行和紧邻参数的元字符。
- 测试不能依赖真实 LLM 服务，也不能修改宿主系统状态。
- 不得为了通过检查而削弱断言、测试数量下限或已登记的布局债务。

## 下一步

- [测试策略](testing.md)
- [添加 CLI 命令](adding-commands.md)
- [添加发行版](adding-distros.md)
- [IPC 协议](ipc-protocol.md)
- [安全启发式](security-heuristics.md)
- [组件贡献规则](../../../../src/cosh-ng/CONTRIBUTING_zh.md)

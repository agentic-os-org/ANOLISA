# 测试 cosh-ng

[English](../../en/cosh-ng/testing.md)

cosh-ng 使用分层的确定性测试。先运行足以证明改动的最小测试，再根据进程、PTY、协议
或安全风险扩大范围。不要在文档中记录精确测试数量，代码增加后，测试清单也会变化。

## 快速反馈

在 `src/cosh-ng` 中运行。

```bash
cargo test --locked -p cosh-types
cargo test --locked -p cosh-platform
cargo test --locked -p cosh-cli --test cli_integration
cargo test --locked -p cosh-core --lib
cargo test --locked -p cosh-shell --lib
```

迭代时可使用测试名称过滤。

```bash
cargo test --locked -p cosh-core session_recovery
cargo test --locked -p cosh-shell --test logic slash_registry
```

## Shell 集成测试分层

| 目标 | 适合证明的行为 | 通常开销 |
|---|---|---|
| `--lib` | 私有纯逻辑或轻量组件 | 最低 |
| `--test logic` | 不经过进程通信的公开多模块行为 | 低 |
| `--test protocol` | 适配器、控制消息序列化和状态变化 | 低到中 |
| `--test raw_cli` | 启动 Shell 二进制、卡片、模型服务交接或脚本化原始输入 | 中 |
| `--test shell_host` | PTY、OSC、termios、原生 Shell 或前台程序行为 | 默认层中最高 |

以下是各层的运行示例。

```bash
cargo test --locked -p cosh-shell --test logic
cargo test --locked -p cosh-shell --test protocol -- --test-threads=4
cargo test --locked -p cosh-shell --test raw_cli <test-name> -- --exact
cargo test --locked -p cosh-shell --test shell_host -- --test-threads=4
```

不要把真实模型服务、视觉或手工终端检查加入默认 Cargo 门禁。只有用户明确要求时才
运行这些验证，并与确定性测试分开报告。

## Core 集成测试

Core 测试按契约拆分。

| 目标 | 契约 |
|---|---|
| `jsonl_protocol` | Headless 消息和流式行为 |
| `registry_protocol` | Skills、Extensions、认证和注册表操作 |
| `tool_approval` | 工具审批协议 |
| `session_recovery` | 持久化对话生命周期 |
| `compaction_lifecycle` | 手工和自动 compaction |
| `oauth_mcp` | MCP OAuth control flow |
| `sls_integration` | 使用固定测试数据的导出集成 |
| `sigint` | 进程中断行为 |

先运行最接近改动的目标。改动影响共享运行状态时，再运行完整的 core package。

## 标准门禁

仓库脚本会避免重复运行库和二进制测试，并检查测试清单与代码布局。

```bash
scripts/run-test-gates.sh fast         # 本地迭代和聚焦交付
scripts/run-test-gates.sh integration  # 所有进程/protocol integration targets
scripts/run-test-gates.sh all          # canonical deterministic suite
scripts/run-test-gates.sh heavy        # 选定的 ignored manual-grade cases
```

`scripts/check-test-inventory.sh` 检查回归测试下限和忽略测试上限。
`scripts/check-test-necessity.sh` 检查需要测试的改动是否提供测试；
`crates/cosh-shell/scripts/check-layout.sh` 检查源码和测试放置。不要只为通过 CI 而在
功能或修复改动中降低这些基线。

## 更广的本地门禁

普通代码改动只需运行 formatter 和最接近改动行为的测试。只有较大或横跨多模块的
代码改动，且当前任务明确要求时，才运行完整本地门禁；其余广泛回归覆盖交给 CI。

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
scripts/run-test-gates.sh all
cargo build --workspace --release
```

公共 API 或 rustdoc 改动还需运行 `cargo doc --workspace --no-deps`。
纯文档改动检查链接、格式、命令和中英文一致性，无需运行 Rust 测试。

## 测试设计规则

- 使用临时目录和仅供测试的路径覆盖，不要依赖开发者真实的主目录、配置、密钥环或
  会话存储。
- 模拟模型服务和传输层，网络凭据不能充当测试数据。
- 验证公开边界，包括 JSON 信封、JSONL 消息、终端输出、文件权限、退出状态或协议字节。
- 安全修复需要同时覆盖正常输入和此前能绕过门禁的对抗输入。
- PTY 测试必须限制等待时间，并等待可观察状态，避免任意休眠。
- 未说明行为原因时，不得删除断言、忽略测试或放宽超时。

可选的 `e2e/run.py` 会按指定组合验证已安装的启动器和真实 PTY 路径。这属于更晚阶段的
系统门禁，不能代替前面的针对性 Cargo 测试。

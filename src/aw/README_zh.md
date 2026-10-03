# AW

[English](README.md)

AW 为 Agent 策略提供统一配置和本地服务。Linux 服务独立于 Agent 或 shell，负责
准备外部 Provider、执行工具事件步骤，并持久保存审计元数据。Adapter 提供原生
能力信息、调度回调并采用返回效果。当前接口仍处于实验阶段，QwenPaw、Qoder CLI、
OpenClaw 和 Hermes 接入仍在开发中。

## 当前可用范围

| 能力 | 可用状态 |
| --- | --- |
| 校验一份包含命名 Provider 和全部 16 个事件名的 `aw.yaml` | ✅ |
| 运行独立 Linux 服务并查询本地执行记录 | ✅ 源码构建 |
| 执行 `tool.before`（`observe`/`block`）和 `tool.after`（`observe`）Provider 步骤 | ✅ 本地客户端 API 与合成事件示例 |
| 启动 Agent、安装其 Hook 或验证原生效果采用 | ❌ |
| 安装已发布的 AW 包或按需启动服务 | ❌ |
| 请求审批、替换工具结果或在原生 Hook 之外强制执行策略 | ❌ |

服务状态和 Provider 准入不能证明 Agent 已受到保护。服务返回候选效果，后续
Adapter 需要验证 Agent 确实采用这些效果。

## 从源码运行

AW 尚未通过 `anolisa install` 或 RPM 发布。在 Linux 上安装 rustup 后，从仓库
根目录构建：

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
target/debug/aw validate --config crates/aw-config/examples/aw.minimal.yaml
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw serve --config crates/aw-config/examples/aw.minimal.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

`serve` 在前台运行，起步配置没有 Provider。在另一个终端进入 `src/aw`，将
`AW_DEMO_SOCKET` 替换为上面打印的绝对路径，再查看或停止服务：

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

前台命令会在清理完成后退出，审计记录保留在状态目录中。退出后在原终端执行
`rm -r -- "$AW_DEMO_ROOT"`，仅删除本次演示目录及其审计历史。
[使用指南](../../docs/user-guide/zh/user-entrypoint/aw.md)提供可运行的 Provider
演示、命令参考和重启说明。

## 接入与开发

可复用的 `aw-service::Client` 绑定一次服务启动及其配置版本。Adapter 打开一个
事件，按原生语义串行或并行调用步骤，再关闭事件。所有步骤共享事件截止时间，
每个步骤只能尝试一次。服务先持久记录执行元数据，再返回结果；结果不确定的调用
不会自动重试。

`aw-host` 仍可直接嵌入应用。`aw-core` 提供独立的固定计划执行 API，以及被服务
复用的持久 `FileJournal` 存储。两者都不授予原生权限，也不认证效果采用。

- [使用指南](../../docs/user-guide/zh/user-entrypoint/aw.md)与
  [配置参考](../../docs/developer-guide/zh/aw/configuration.md)
- [本地服务与客户端合同](docs/design/local-service_zh.md)
- [Provider 协议](docs/design/provider-protocol_zh.md)、
  [Provider Host](docs/design/provider-host_zh.md)与
  [有界命令执行](docs/design/bounded-execution_zh.md)
- [Core 执行与存储](docs/design/core-execution_zh.md)
- [开发环境、Crate 职责与测试](CONTRIBUTING_zh.md)

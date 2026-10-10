# AW

[English](README.md)

AW 为 Agent 策略提供统一配置和本地服务。在 Linux 上，它可以通过已验证的原生
入口启动 Qoder CLI 和 OpenClaw，运行外部 Provider，并独立于 Agent 会话保存
执行元数据。原生调度与权限仍由框架负责，当前接口处于实验阶段。

AW Preview提供核心包和Provider包，通过`aw-package`安装。当前包含sec-core Provider，
并提供Qoder和OpenClaw共用一份配置的示例，详见[Preview安装与演示](../../docs/user-guide/zh/user-entrypoint/aw-preview.md)。

AW core 可独立构建与初始化，无需 sec-core。`aw-build` 默认只构建 core；
`--component all` 保留组合分发路径。`aw-package configure` 默认允许单 Agent、
无 Provider；通过 `--provider sec-core` 或 `--provider command` 显式启用。
布尔判断脚本只返回 true/false，effect 放在 YAML 中，AW 负责 Provider 握手
和响应关联。见[布尔判断脚本](../../docs/user-guide/zh/user-entrypoint/aw.md#布尔判断脚本)。

## 当前可用范围

| 能力 | 可用状态 |
| --- | --- |
| 校验一份包含命名 Provider 和全部 16 个事件名的 `aw.yaml` | ✅ |
| 启动 Qoder CLI 1.1.64 并接通工具前后 Hook | ✅ Linux 源码构建 |
| 工具前执行结构化 Provider 的 `observe`/`block`，工具成功后执行 `observe` | ✅ |
| 执行回调输入保持不变的原生 Hook 命令 | ✅ 字节输出和退出状态交回 Qoder；不含重写链与审批流程 |
| 启动或复用独立服务，查询执行元数据 | ✅ |
| 通过 AW 启动 OpenClaw | ✅ 新 Gateway 中的 Agent 工具 Hook |
| 启动其余首批框架 | ❌ 相应 Adapter 独立交付 |
| 安装已发布的 AW 包、跨框架请求审批或在原生 Hook 之外强制执行策略 | ❌ |

## 启动 Qoder

AW 尚未通过 `anolisa install` 或 RPM 发布。在 Linux 上安装 rustup 和 Qoder CLI
1.1.64 后，从仓库根目录构建。如果该版本不在 `PATH` 中，先修改示例的
`spec.agents.qoder.argv`。

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

示例在工具执行前和成功后运行一条中性命令，用于演示 Hook 接线，没有安装安全
规则。AW 启动或复用配置指定的服务，再打开 Qoder 的原有界面。退出 Qoder 后
回到原终端并释放本次会话，共享服务和审计历史继续保留。

```bash
target/debug/aw status --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.qoder.yaml
```

[使用指南](../../docs/user-guide/zh/user-entrypoint/aw.md)说明原生配置共存、Hook
串并行、Provider 配置、显式服务启动和记录查询。原生 Hook 仍受框架自身能力约束，
仅凭服务状态不能证明 Qoder 已采用策略。

`aw --help` 列出启动命令及 Adapter 专用参数。Qoder 的 `aw run` 支持
`--native-settings`，拒绝 `--native-profile`、`--native-state-dir` 和 `aw install`。
`install` 用于分派持久化原生 Hook 安装，当前版本没有支持它的 Adapter；它不安装
AW 或 Agent 软件。完整命令及参数支持表见使用指南。
升级前须用旧版 AW 停止旧 daemon；当前 CLI 会拒绝旧本地协议。升级步骤见使用指南。

## sec-core Provider

Linux 二进制 `aw-provider-sec-core` 将配置中选定的工具输入传给公开的
`agent-sec-cli scan-code` 命令，通过 AW Provider 协议返回工具前 observe/block
候选效果和工具后观察。它依赖已有 sec-core CLI 与 daemon，无需向 sec-core
安装 AW 代码。源码构建、配置和本地 Host 示例见
[sec-core Provider 指南](../../docs/user-guide/zh/user-entrypoint/aw-sec-core.md)。

## 启动 OpenClaw

使用 OpenClaw 2026.9.6，保留其原生模型配置。
[使用指南](../../docs/user-guide/zh/user-entrypoint/aw.md)说明必需的 profile 参数和
工具生命周期范围。从 `src/aw` 执行：

```bash
target/debug/aw run --config crates/aw-service/examples/aw.openclaw.yaml --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

## 接入与开发

可复用的 `aw-service::Client` 绑定一次服务启动及其配置版本。Adapter 归一化
回调并保留原生调度。相关回调共享同一个事件截止时间，每个步骤只能尝试一次。
服务先记录执行元数据，再返回结果；结果不确定的调用不会自动重试。

`aw-host` 同时支持结构化 Provider 消息和显式选择的原生 Hook 字节传输。
`aw-core` 提供独立的固定计划执行 API，以及被服务复用的持久 `FileJournal`
存储。这些边界让 cosh、桌面客户端和 Herdr 保持独立于服务实现。

- [使用指南](../../docs/user-guide/zh/user-entrypoint/aw.md)与
  [配置参考](../../docs/developer-guide/zh/aw/configuration.md)
- [本地服务与客户端合同](docs/design/local-service_zh.md)
- [Provider 协议](docs/design/provider-protocol_zh.md)、
  [Provider Host](docs/design/provider-host_zh.md)与
  [有界命令执行](docs/design/bounded-execution_zh.md)
- [Core 执行与存储](docs/design/core-execution_zh.md)
- [开发环境、Crate 职责与测试](CONTRIBUTING_zh.md)

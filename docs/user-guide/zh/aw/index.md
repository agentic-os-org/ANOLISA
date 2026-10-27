# AW 用户手册

[English](../../en/aw/index.md)

AW 让你在已有 Agent 中使用工具安全检查、输出压缩和自定义规则。
用一份 `aw.yaml` 选择 Agent 和所需能力，再通过 `aw run` 启动。
Agent 的界面、模型账号和权限设置继续由原框架管理。

## 从这里开始

| 要做什么 | 阅读哪一页 |
| --- | --- |
| 对比 Hook、MCP 与策略更新的支持范围 | [能力支持矩阵](capabilities.md) |
| 第一次使用，安装后启动 Qoder | [安装与首次运行](installation.md) |
| 选择 Agent、增加规则或修改配置 | [配置与 Agent 启动](configuration.md) |
| 减少工具结果占用的上下文 | [使用 Tokenless](tokenless.md) |
| 在工具执行前检查风险 | [使用 sec-core](security.md) |
| 关联 AgentSight 运行记录 | [接入 AgentSight](observability.md) |
| 查看状态、停止服务、升级或排错 | [运行维护](operations.md) |

当前 AW 为 Linux Preview，使用独立安装包，尚未提供 `anolisa install aw`
或 RPM 安装入口。先安装并登录所选 Agent；AW 不代为安装框架或配置模型账号。

## 一次运行

安装并配置后，从工作目录启动。

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

`qoder` 是配置中的目标名称。其他 Agent 见[配置指南](configuration.md)。

退出 Agent 后，AW 本地服务仍可被后续会话复用。不再使用时停止它。

```bash
aw status --config "$HOME/aw/aw.yaml"
aw stop --config "$HOME/aw/aw.yaml"
```

## 选择需要的能力

core 可独立启动 Agent。安全检查和压缩需安装对应 Provider，或直接安装 all-in-one，
再通过配置启用。

工具执行前可以检查或阻断调用，执行后可以压缩受支持的文本结果。
原生权限判断始终保留；AW 当前的 Hook 检查不提供 OS 沙箱隔离。
自有组件和 SDK 应用的接入方法见[开发指南](../../../developer-guide/zh/aw/index.md)。

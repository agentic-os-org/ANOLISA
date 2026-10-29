# AW 设计索引

[English](../en/README.md)

设计文档解释实现责任、协议不变量和取舍。操作步骤见[开发者指南](../../../../../docs/developer-guide/zh/aw/index.md)。先读[架构](architecture.md)与[边界](architecture-boundaries.md)，再按修改对象查阅。

`aw-package` 通过私有文件事务存储负责本地 Provider 管理与配置，`aw-exec`
负责运行时文件读锁。发行 Make 目标与脚本构建交付件；独立安装的 Provider
通过进程协议接入，不要求 AW 包记录。

## 请求执行

| 主题 | 合同 |
| --- | --- |
| 配置 | [配置](configuration/configuration.md) · [编辑与原生来源](configuration/configuration-editing.md) |
| 原生接线 | [Adapter 分派](adapters/adapter-dispatch.md) |
| 运行时 | [本地服务](runtime/local-service.md) · [策略更新](runtime/policy-reload.md) · [Provider Host](runtime/provider-host.md) · [有界执行](runtime/bounded-execution.md) |
| 能力计划 | [Core 执行](core-execution.md) |
| Provider 扩展 | [本地 Provider 管理](packaging/package-management.md) |

## Adapter 与扩展

[AgentScope](adapters/agentscope-adapter.md) · [Claude SDK](adapters/claude-sdk-adapter.md) · [Hermes](adapters/hermes-adapter.md) · [OpenClaw](adapters/openclaw-adapter.md) · [Pi](adapters/pi-adapter.md) · [QwenPaw](adapters/qwenpaw-adapter.md)

| 主题 | 合同 |
| --- | --- |
| 常驻任务与 Qoder SDK | [常驻 SDK](adapters/resident-sdk.md) |
| MCP | [结构化 MCP](adapters/structured-mcp.md) · [Claude MCP](adapters/claude-mcp.md) |
| 私有执行 | [执行租约](adapters/private-execution.md) |
| sec-core | [Provider](providers/sec-core/sec-core-provider.md) · [健康诊断](providers/sec-core/sec-core-health.md) |

工具选择与格式错误事件的失败语义见 [Provider Host 合同](runtime/provider-host.md)；
随附模板的名称与输入映射见 [sec-core 配置参考](../../../../../docs/developer-guide/zh/aw/providers/sec-core.md#配置-provider)。

## 证据与观测

[来源连接器](integrations/agentsight/agentsight-readonly.md) · [证据上下文](runtime/evidence-context.md) · [集成规划](integrations/agentsight/agentsight-integration.md)

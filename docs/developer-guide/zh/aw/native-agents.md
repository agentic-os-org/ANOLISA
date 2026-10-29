# Agent 与 SDK 接入

[English](../../en/aw/native-agents.md)

按应用使用的框架选择一页。每页列出依赖、启动步骤与接入限制。SDK 应用需显式使用 AW 工厂。

运行准入依据所需原生接口与协议行为。下表版本标明已验证组合。
`aw config validate` 检查 AW 配置结构与字段关系；`aw run` 定位已安装框架，
记录实际版本，检查原生注册。插件与 SDK bridge 需要就绪回执；Qoder CLI 使用原生命令 Hook。
缺失接口或不兼容回调会在对应启动或执行入口报错。
应用依赖变更后，按[原生验收](validation.md)检查实际行为。

| 接入 | 已验证基线 | 入口 |
| --- | --- | --- |
| [Qoder CLI](agents/qoder.md) | 1.1.64 | CLI |
| [Qoder SDK](agents/qoder-sdk.md) | 1.0.56 / Worker 1.1.78 | Node.js worker |
| [OpenClaw](agents/openclaw.md) | 2026.9.6 | Gateway |
| [QwenPaw](agents/qwenpaw.md) | 2.2.2.post1 / AgentScope 2.0.10 | App/API |
| [Hermes](agents/hermes.md) | 952c941e | chat |
| [AgentScope SDK](agents/agentscope.md) | 2.0.6 | Python worker |
| [Pi SDK](agents/pi.md) | 0.84.2 | Node.js worker |
| [Claude SDK](agents/claude-sdk.md) | 0.2.88 / CLI 2.1.161 | Python worker |

通用能力见[Hook 支持矩阵](../../../user-guide/zh/aw/capabilities.md)。多框架共用策略按[接入教程](integration.md)准备，启动后做[原生效果验收](validation.md)。长期服务继续阅读[常驻部署](deployment/resident.md)和[生命周期](deployment/lifecycle.md)。

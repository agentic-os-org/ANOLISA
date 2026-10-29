# Agent and SDK integration

[中文版](../../zh/aw/native-agents.md)

Choose the framework used by your application. Each page lists dependencies, startup and integration limits. SDK applications must explicitly use AW factories.

| Integration | Version | Entrypoint |
| --- | --- | --- |
| [Qoder CLI](agents/qoder.md) | ≥ 1.1.64 | CLI |
| [Qoder SDK](agents/qoder-sdk.md) | 1.0.56 / Worker 1.1.78 | Node.js worker |
| [OpenClaw](agents/openclaw.md) | 2026.9.6 | Gateway |
| [QwenPaw](agents/qwenpaw.md) | 2.2.2.post1 / AgentScope 2.0.10 | App/API |
| [Hermes](agents/hermes.md) | 952c941e | chat |
| [AgentScope SDK](agents/agentscope.md) | 2.0.6 | Python worker |
| [Pi SDK](agents/pi.md) | 0.84.2 | Node.js worker |
| [Claude SDK](agents/claude-sdk.md) | 0.2.88 / CLI 2.1.161 | Python worker |

See the [Hook matrix](../../../user-guide/en/aw/capabilities.md) for shared capabilities. Prepare shared policies with the [tutorial](integration.md), then [validate native behavior](validation.md). For services, continue with [resident deployment](deployment/resident.md) and [lifecycle](deployment/lifecycle.md).

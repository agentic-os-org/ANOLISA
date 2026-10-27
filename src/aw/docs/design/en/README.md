# AW design index

[中文版](../zh/README.md)

Design documents explain implementation ownership, protocol invariants and trade-offs. Operational steps live in the [developer guide](../../../../../docs/developer-guide/en/aw/index.md). Start with [architecture](architecture.md) and [boundaries](architecture-boundaries.md), then follow the area you are changing.

## Request execution

| Area | Contract |
| --- | --- |
| Configuration | [Configuration](configuration/configuration.md) · [Editing](configuration/configuration-editing.md) |
| Native wiring | [Adapter dispatch](adapters/adapter-dispatch.md) |
| Runtime | [Service](runtime/local-service.md) · [Policy updates](runtime/policy-reload.md) · [Host](runtime/provider-host.md) · [Bounded execution](runtime/bounded-execution.md) |
| Capability plans | [Core execution](core-execution.md) |
| Packages | [Package management](packaging/package-management.md) |

## Adapters and extensions

[AgentScope](adapters/agentscope-adapter.md) · [Claude SDK](adapters/claude-sdk-adapter.md) · [Hermes](adapters/hermes-adapter.md) · [OpenClaw](adapters/openclaw-adapter.md) · [Pi](adapters/pi-adapter.md) · [QwenPaw](adapters/qwenpaw-adapter.md)

| Area | Contract |
| --- | --- |
| Resident tasks and Qoder SDK | [Resident SDK](adapters/resident-sdk.md) |
| MCP | [Structured MCP](adapters/structured-mcp.md) · [Claude MCP](adapters/claude-mcp.md) |
| Private execution | [Execution leases](adapters/private-execution.md) |
| sec-core | [Provider](providers/sec-core/sec-core-provider.md) · [Health](providers/sec-core/sec-core-health.md) |

## Evidence and observation

[Connector](integrations/agentsight/agentsight-readonly.md) · [Evidence context](runtime/evidence-context.md) · [Integration roadmap](integrations/agentsight/agentsight-integration.md)

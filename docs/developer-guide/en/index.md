# Developer Guide

[中文版](../zh/index.md)

Use this guide when you need to understand, extend, test, or contribute to ANOLISA.

## Semantic Arbiter

- [Component overview](../../../src/semantic-arbiter/README.md)
- [Protocol and public library contracts](../../../src/semantic-arbiter/docs/design/protocol.md)
- [CLI and integration guide](../../user-guide/en/semantic-arbiter/index.md)

Run the developer examples from `src/semantic-arbiter` to inspect closed-scenario unit replacement, the in-memory executor fixture and EvidenceSource collection:

```bash
cargo run --example judgment_unit
cargo run --example executor_fixture
cargo run --example evidence_source
```

## SDK

- [Tokenless Python SDK](sdk/tokenless/index.md)
- [Tokenless AgentScope integration](sdk/tokenless/agentscope.md)

## AW

- [Development and integration index](aw/index.md)
- [Build and package](../../../contribute/en/aw/building.md)
- [Component integration tutorial](aw/integration.md)
- [Configuration reference](aw/configuration.md)
- [User guide](../../user-guide/en/aw/index.md)

## Cosh-ng

- [Getting started](../../../contribute/en/cosh-ng/getting-started.md)
- [Architecture](../../../src/cosh-ng/docs/design/en/architecture.md)
- [Adding commands](../../../contribute/en/cosh-ng/adding-commands.md)
- [Adding distributions](../../../contribute/en/cosh-ng/adding-distros.md)
- [IPC protocol](../../../src/cosh-ng/docs/design/en/ipc-protocol.md)
- [Security heuristics](../../../src/cosh-ng/docs/design/en/security-heuristics.md)
- [Testing](../../../contribute/en/cosh-ng/testing.md)

Start with **Getting started**. It maps common changes to code owners and the
smallest relevant test target; the remaining pages are focused references.

## Tokenless

- [Windows integration](tokenless/windows.md)

For repository-wide build and contribution requirements, also read [Build from source](../../user-guide/en/getting-started/building.md) and [CONTRIBUTING.md](../../../CONTRIBUTING.md).

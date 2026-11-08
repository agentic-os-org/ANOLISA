# ADR 0001: ANOLISA Architecture and Component Boundaries

[中文参考译文](0001-anolisa-architecture_zh.md)

> The English original is authoritative. All translations are for reference only.

- **Status:** Proposed
- **Date:** 2026-10-23
- **Scope:** ANOLISA system architecture and component placement.
- **Implementation baseline:** `5351c892f2e89d6be799ebdbeea948c173d3d7c2`.

## Context

ANOLISA combines Agent entrypoints, framework integration, Agent capabilities,
and host tooling. A shared architecture defines their responsibilities and source
locations while preserving independent component use.

## Decision

### Layers

Use four layers, numbered from the host upward.

![ANOLISA four-layer architecture](assets/0001/architecture.svg)

| Layer | Responsibility |
| --- | --- |
| L4 — User Entrypoint Layer | Terminal interaction and operational knowledge for users and Agents. |
| L3 — AW Unified Stack | AW Hook, AW Core coordination, and Provider Host connect Agent frameworks to capability Providers. |
| L2 — Agent Semantic Layer | Observation, security, context, Skills, and workspace capabilities for Agent workloads. |
| L1 — Host Foundation | Host facilities and the tools that manage them. |

L1 contains host facilities and ktuner side by side. ktuner belongs here because
its [decisions](../../src/ktuner/src/profile/mod.rs) use host state and generic
workload profiles. L2's six components form one horizontal row of peers.
Architectural placement is independent of manual navigation.

### AW integration

L3 places **AW Hook → AW Core → Provider Host** from left to right. Hooks adapt
native callbacks and return effects to Agent entrypoints. Provider Host invokes
Providers that bridge to capability backends, including external implementations.
AW Core denotes logical coordination here; the current Hook runtime uses
`aw-service`, while the `aw-core` crate exposes a separate plan API.

sec-core and Tokenless bridges exist today. Memory, SkillFS, checkpoint, and
cosh-ng Hook integrations are proposed and use dashed connectors. AgentSight
has a separate optional, read-only observation connection to AW coordination.
External Providers sit beside L2 as extension peers and use the existing process
protocol. Each connection terminates at its capability or Provider card.

### Component placement

| Component | Layer | Role | Source |
| --- | --- | --- | --- |
| cosh-ng | L4 | Terminal and Agent execution | [`src/cosh-ng/`](../../src/cosh-ng/) |
| os-skills | L4 | OS and DevOps instructions | [`src/os-skills/`](../../src/os-skills/) |
| AW | L3 | Agent integration and policy coordination | [`src/aw/`](../../src/aw/) |
| AW Providers | L3 extension interface | Bridges to L2 backends or external capabilities | [`providers/`](../../providers/) |
| AgentSight | L2 | Execution and Token observability | [`src/agentsight/`](../../src/agentsight/) |
| agent-sec-core | L2 | Security checks and sandbox execution | [`src/agent-sec-core/`](../../src/agent-sec-core/) |
| tokenless | L2 | Tool schema and output compression | [`src/tokenless/`](../../src/tokenless/) |
| agent-memory | L2 | Persistent context and retrieval | [`src/agent-memory/`](../../src/agent-memory/) |
| SkillFS | L2 | Skill discovery and filesystem views | [`src/skillfs/`](../../src/skillfs/) |
| ws-ckpt | L2 | Workspace filesystem checkpoint and rollback | [`src/ws-ckpt/`](../../src/ws-ckpt/) |
| ktuner | L1 | Host diagnosis and kernel tuning | [`src/ktuner/`](../../src/ktuner/) |
| anolisa CLI | Cross-layer | Distribution and operations | [`distribution/anolisa/`](../../distribution/anolisa/) |
| copilot-shell | Legacy | Deprecated terminal implementation | [`deprecated/copilot-shell/`](../../deprecated/copilot-shell/) |

The host OS, kernel, processes, filesystems, isolation mechanisms, networking,
and service facilities form the rest of L1. Linux is the full-capability baseline;
macOS support varies by component.

### Boundaries

- Components remain independently usable. Layering does not require every call
  to pass through AW.
- Native Agents retain scheduling and permission authority. Security components
  and host mechanisms enforce their respective policies.
- AW framework code stays in `src/aw/`; concrete Providers stay in
  `providers/<provider>/` or independent repositories. Framework crates do not
  depend on concrete Providers. See the [AW architecture](../../src/aw/docs/design/en/architecture.md).
- Source directories remain component-based. Layer labels do not change package
  names, installed commands, release tags, or build and test entrypoints.
- All ADRs, including component-specific decisions, live in `docs/adr/` with one
  numbering sequence. Referenced assets live in `docs/adr/assets/<number>/`.
  Detailed implementation designs remain with their components and link to the
  central ADR. See the [documentation standard](../../specs/documentation-standard.md).

## Alternatives considered

| Alternative | Reason for rejection |
| --- | --- |
| One mandatory runtime for all components | Couples otherwise independent capabilities. |
| Require all integration through AW | Removes valid direct CLI, plugin, and protocol paths. |
| Mirror layers in source directories | Adds migration work without improving component ownership. |

## Consequences

Components have explicit responsibilities and can be adopted independently.
Cross-component combinations still need compatibility validation. This decision
requires no source moves or runtime migration.

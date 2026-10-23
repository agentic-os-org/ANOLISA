# ANOLISA User Guide

[中文版](../zh/index.md)

Use this manual to install, configure, and operate ANOLISA components for Agent
workloads. Begin with the [Quickstart](getting-started/quickstart.md) for a first
result, or open a component's manual for its setup and reference chapters.
Shared installation, build, test, and troubleshooting instructions are grouped
under Getting started.

<!-- navigation:start -->

## Getting started

- [Quickstart](getting-started/quickstart.md)
- [Installation](getting-started/installation.md)
- [ANOLISA CLI](getting-started/anolisa-cli.md)
- [Building and deployment](getting-started/building.md)
- [Testing](getting-started/testing.md)
- [Troubleshooting](getting-started/troubleshooting.md)

## User Entrypoint Layer

- [cosh (cosh-ng)](cosh-ng/index.md): An AI-native terminal that preserves your existing shell experience, with natural-language tasks, tools, and Skills.
- [OS Skills](os-skills/index.md): An OS handbook for AI Agents, covering system administration, monitoring, security, DevOps automation, and more.

## AW Unified Stack

- [AW](aw/index.md): A unified architecture connecting Agent frameworks with extensible AI-native system software.

## Agent Semantic Layer

- [AgentSight](agentsight/index.md): Observe Agent execution through trajectories, LLM calls, and Token usage.
- [Agent Security Core](agent-sec-core/index.md): OS-level security kernel for AI Agents.
- [Tokenless](tokenless/index.md): Reduce AI Agent Token usage through tool output compression, tool schema simplification, command rewriting, and other techniques.
- [Agent Memory](agent-memory/index.md): Give Agents searchable, persistent memory to retain and reuse information across sessions.
- [SkillFS](skillfs/index.md): Organize Skills into stable filesystem views for Agents to discover and read on demand.
- [Workspace Checkpoint](ws-ckpt/index.md): Create Agent workspace snapshots, compare file differences, and roll back changes when needed.
- [ktuner](ktuner/index.md): Inspect Linux kernel parameters, get tuning recommendations, and apply or roll back changes.

<!-- navigation:end -->

## Terminology

| Term | Meaning |
|------|---------|
| Component | A software unit implementing a specific capability (e.g. `tokenless`) |
| Adapter | A bridge package connecting a component to an Agent framework |
| system mode | Installation requiring root privileges (`sudo anolisa install`) |
| user mode | Installation into user-local paths (no sudo required) |

<div align="center">

<picture>
  <source
    media="(prefers-color-scheme: dark)"
    srcset="docs/images/brand/anolisa-lockup-dark.svg"
  >
  <source
    media="(prefers-color-scheme: light)"
    srcset="docs/images/brand/anolisa-lockup-light.svg"
  >
  <img
    src="docs/images/brand/anolisa-lockup-light.svg"
    alt="ANOLISA"
    width="320"
  >
</picture>

<sub>**A**gentic **N**exus **O**perating **L**ayer & **I**nterface **S**ystem **A**rchitecture</sub>

**The operating system layer for Agent workloads.**

Let Agents drive the system straight from your terminal, and strip the tool
responses that reach the model before they cost you — while keeping the Shell,
Agent framework, and sandbox you already run.

[中文版](README_zh.md) · [Website](https://agentic-os.sh/) ·
[Quick Start](https://agentic-os.sh/docs/quickstart/) ·
[User Guide](https://agentic-os.sh/docs/user-guide/) ·
[Contributing](https://github.com/alibaba/anolisa/blob/main/CONTRIBUTING.md)

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://github.com/alibaba/anolisa/blob/main/LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-lightgrey.svg)](https://agentic-os.sh/docs/user-guide/installation/)
[![AARM Aligned](https://img.shields.io/badge/AARM-Aligned-blue.svg)](https://aarm.dev/builders/agentseccore-anolisa)
[![OWASP Agentic Top 10: 7 Full, 3 Partial](https://img.shields.io/badge/OWASP_Agentic_Top_10-7_Full,_3_Partial-blue)](docs/user-guide/en/agent-security/owasp-agentic-top10.md)

</div>

---

ANOLISA is a server-side operating layer for AI Agent workloads. It addresses
three practical constraints of Agent execution: terminal entry, Token cost, and
execution environments. Keep the Shell, Agent framework, and sandbox you
already use. ANOLISA CLI provides a single installation entry point, while each
capability can be enabled independently.

**New to ANOLISA?**
[Choose your first outcome in the Quick Start →](https://agentic-os.sh/docs/quickstart/)

## Components

<table width="100%">
  <thead>
    <tr>
      <th width="340">Agent entry</th>
      <th width="340">Context efficiency</th>
      <th width="340">Runtime &amp; security</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/user-entrypoint/cosh-ng/quickstart/">cosh-ng</a></strong><br><sub>Shell copilot</sub></td>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/token-saving/tokenless/quickstart/">Token-less</a></strong><br><sub>Tool-output compression</sub></td>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/runtime/ws-ckpt/">ws-ckpt</a></strong><br><sub>Checkpoint and rollback</sub></td>
    </tr>
    <tr>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/user-entrypoint/os-skills/">OS Skills</a></strong><br><sub>System and DevOps expertise</sub></td>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/agent-observability/agentsight/">AgentSight</a></strong><br><sub>Trace and Token visibility</sub></td>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/runtime/skillfs/">SkillFS</a></strong><br><sub>Focused Skill views</sub></td>
    </tr>
    <tr>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/user-entrypoint/ktuner/">ktuner</a></strong><br><sub>Kernel tuning</sub></td>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/token-saving/agent-memory/">Agent Memory</a></strong><br><sub>Cross-session memory</sub></td>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/agent-security/agent-sec-core/quickstart/">Agent Sec Core</a></strong><br><sub>Sandbox and verification</sub></td>
    </tr>
    <tr>
      <td></td>
      <td></td>
      <td><strong><a href="https://agentic-os.sh/docs/user-guide/runtime/blaze/">Blaze</a></strong><br><sub>Sandbox lifecycle</sub></td>
    </tr>
  </tbody>
</table>

## What it solves

<p align="center"><strong>01 · AGENT INTERFACE</strong></p>

<h3 align="center">Let the Agent work directly in the terminal</h3>

cosh-ng is an AI-native Linux terminal: it keeps familiar Bash/Zsh behavior,
then adds an Agent that can understand intent, use tools and Skills, and ask
for approval before risky work. Shell commands and natural language share one
terminal instead of forcing users into a separate chat application.

[Get started with cosh-ng →](https://agentic-os.sh/docs/user-guide/user-entrypoint/cosh-ng/quickstart/)

<p align="center"><strong>02 · CONTEXT EFFICIENCY</strong></p>

<h3 align="center">See where Tokens go and cut waste before it reaches the model</h3>

[Token-less](https://agentic-os.sh/docs/user-guide/token-saving/tokenless/quickstart/)
removes redundancy from tool schemas and responses before they reach the model.
[Agent Memory](https://agentic-os.sh/docs/user-guide/token-saving/agent-memory/)
reuses useful context across sessions.
[SkillFS](https://agentic-os.sh/docs/user-guide/runtime/skillfs/) keeps the
current Skill view focused and makes other Skills discoverable when needed.
[AgentSight](https://agentic-os.sh/docs/user-guide/agent-observability/agentsight/)
shows where Tokens are spent.

### See an Agent run from the kernel up

On Linux, AgentSight uses eBPF to observe an Agent without changing its code.
Follow user input through model and tool calls, with Token use and sub-agent
branches in the same view.

[Open the AgentSight guide →](https://agentic-os.sh/docs/user-guide/agent-observability/agentsight/)

<table align="center" cellpadding="0" cellspacing="0">
  <tr>
    <td>
      <video
        controls
        muted
        preload="metadata"
        src="https://github.com/user-attachments/assets/ed6952c6-19da-44c7-ae35-85d753bdccf9"
      ></video>
    </td>
  </tr>
</table>

### Try Token-less with Claude Code in 3 minutes

Install Token-less and connect it to Claude Code:

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
anolisa install tokenless
anolisa adapter enable tokenless claude-code
```

Restart Claude Code, run one tool-heavy task, then inspect the result:

```bash
tokenless stats summary
tokenless stats list --limit 5
```

[Open the full Token-less Quick Start →](https://agentic-os.sh/docs/user-guide/token-saving/tokenless/quickstart/)
· [Read the user manual](https://agentic-os.sh/docs/user-guide/token-saving/tokenless/user-manual/)

<table align="center" cellpadding="0" cellspacing="0">
  <tr>
    <td>
      <video
        controls
        muted
        src="https://github.com/user-attachments/assets/b372ae72-44fa-492f-9feb-e6cd137b631a"
      ></video>
    </td>
  </tr>
</table>

<p align="center">
  <sub>
    In one observed coding task, Token-less saved 317K Tokens (40.5%), based
    on AgentSight measurements.
    Results vary by workload.
  </sub>
</p>

`debug` and `trace` are dropped by the field blacklist, `metadata` as null, and
`tags` / `extra` as empty values. Compression runs between the Agent and the
model, so no Agent framework code changes. Dropped array items stay retrievable
through a `<<tokenless:KEY>>` marker, which keeps the compression reversible.

| Tool responses | Tool schemas | Full pipeline |
|----------------|--------------|---------------|
| **65.8% fewer Tokens** | **47.3% fewer Tokens** | **62.9% fewer Tokens** |
| ResponseCompressor · 46.85 µs | SchemaCompressor · 11.44 µs | 198.91 µs |

Savings apply to the tool responses entering the context, not to the whole
session bill. The [Token-less user manual](https://agentic-os.sh/docs/user-guide/token-saving/tokenless/user-manual/)
explains how to estimate the effect for a given workload.

<p align="center"><strong>03 · EXECUTION RUNTIME</strong></p>

<h3 align="center">Give every Agent execution a boundary and a way back</h3>

ANOLISA is building out the Agent execution environment:
[Agent Sec Core](https://agentic-os.sh/docs/user-guide/agent-security/agent-sec-core/quickstart/)
isolates risky operations, and
[ws-ckpt](https://agentic-os.sh/docs/user-guide/runtime/ws-ckpt/) keeps recovery
points for workspace changes.

### Catch a changed Skill before it runs

When a signed Skill changes, the Agent reports `drifted` before using it again.
A rescan records blocking findings as `deny`.

[Try the Agent demo →](https://agentic-os.sh/docs/user-guide/agent-security/agent-sec-core/qoder-skill-ledger-demo/)
· [Skill Ledger guide](https://agentic-os.sh/docs/user-guide/agent-security/agent-sec-core/skill-ledger/)

<table align="center" cellpadding="0" cellspacing="0">
  <tr>
    <td>
      <video
        controls
        muted
        preload="metadata"
        src="https://github.com/user-attachments/assets/aad6e296-7c5a-4a81-be2e-ea4f49e43637"
      ></video>
    </td>
  </tr>
</table>

[Choose a runtime or security starting point →](https://agentic-os.sh/docs/quickstart/)
· [Start with ANOLISA CLI](https://agentic-os.sh/docs/user-guide/user-entrypoint/anolisa-cli/)

## Install

ANOLISA CLI is the common installation entry point. cosh-ng is installed in
system mode; Token-less and other capabilities can be added independently.

```bash
curl -fsSL https://get.agentic-os.sh | bash

sudo anolisa --install-mode system install cosh-ng
anolisa install tokenless
```

Run `cosh` to enter the AI-native terminal. Token-less can also optimize tool
calls from an existing Agent without changing its framework.

[Read the Quick Start →](https://agentic-os.sh/docs/quickstart/)

## Standards & Compliance

| Standard / Framework | Status and details |
|---|---|
| [AARM](https://aarm.dev/builders/agentseccore-anolisa) | **Aligned** · Registry entry: AgentSecCore (ANOLISA) |
| [OWASP Agentic Top 10 (2026)](docs/user-guide/en/agent-security/owasp-agentic-top10.md) | All 10 ASI risk categories mapped to security controls |

## Documentation

[Quick Start](https://agentic-os.sh/docs/quickstart/) ·
[Installation](https://agentic-os.sh/docs/user-guide/installation/) ·
[User Guide](https://agentic-os.sh/docs/user-guide/) ·
[Troubleshooting](https://agentic-os.sh/docs/user-guide/troubleshooting/) ·
[Build from Source](https://agentic-os.sh/docs/building/) ·
[Changelog](https://agentic-os.sh/changelog/)

## Repository layout

Components remain independently buildable in one repository. Directory placement
separates runtime capabilities, distribution tooling, evaluation, and retained
legacy code; it does not rename installed commands, packages, or release tags.

| Directory | Contents |
|---|---|
| [`src/`](src/) | Runtime components, including AW, cosh-ng, observability, security, memory, and sandbox capabilities. Each component keeps its own build and test entry points. |
| [`distribution/`](distribution/) | Distribution tooling. `anolisa/` owns the installation CLI, component manifests, and packaging resources. |
| [`benchmark/`](benchmark/) | Cross-component evaluation runners: ClawEval, SWE, and Terminal Bench. Component-specific benchmarks stay with their components. |
| [`deprecated/`](deprecated/) | Retained legacy implementations. `copilot-shell/` is deprecated for new development; its existing build, install, CI, and release paths remain available until a separate retirement. |
| [`scripts/`](scripts/) | Repository-wide build, packaging, and validation scripts. |
| [`tests/`](tests/) | Repository-wide regression checks and test orchestration. Component tests remain local to each component. |
| [`docker/`](docker/) | Container image definitions and build resources. |
| [`docs/`](docs/) | User and developer guides, source-build instructions, and shared documentation assets. |
| [`specs/`](specs/) | Contribution standards, component onboarding, and documentation rules. |
| [`website/`](website/) | Public website source and generators. Generated output is not a second documentation source. |
| [`.github/`](.github/) | CI, release workflows, component routing metadata, and ownership. |

Before changing files, start with [AGENTS.md](AGENTS.md) for repository rules,
component locations, and build/test commands, then read the component's scoped
`AGENTS.md` when present. Use [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution
workflow. Source-path changes must be reflected in `.github/components.json` and
its build, release, ownership, and documentation consumers without changing the
component's public identity.

## Community

<div align="center">

<img src="docs/images/readme/dingtalk-qr.png" alt="ANOLISA DingTalk community QR code" width="180"/>

Scan with DingTalk to join the ANOLISA community.

</div>

- [Open an issue](https://github.com/alibaba/anolisa/issues) for bugs and
  feature requests.
- Read [CONTRIBUTING.md](https://github.com/alibaba/anolisa/blob/main/CONTRIBUTING.md)
  before submitting a pull request.
- Report vulnerabilities through the
  [Security Policy](https://github.com/alibaba/anolisa/blob/main/SECURITY.md).

## License

ANOLISA is released under the
[Apache License 2.0](https://github.com/alibaba/anolisa/blob/main/LICENSE).

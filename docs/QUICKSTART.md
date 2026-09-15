# ANOLISA Quick Start

[中文版](QUICKSTART_zh.md)

ANOLISA is a server-side operating layer for AI Agent workloads. Install the
CLI once, then enable only the capability that delivers the first result you
want.

## Install the CLI

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
```

If the anolisa CLI is already installed, skip these commands. Alinux 4 users
can alternatively install the CLI with `sudo yum install anolisa`.

## Choose your first outcome

### Reduce Agent Token usage

Install Tokenless, connect it to Codex, Claude Code, Qoder, OpenClaw, Hermes,
Qwen Code, or cosh, then verify a before/after Token record.

[Start the three-minute Tokenless Quick Start →](user-guide/en/token-saving/tokenless/QUICKSTART.md)

### Work with an Agent-native terminal

Choose the terminal that matches your environment:

- [Start with cosh-ng](user-guide/en/user-entrypoint/cosh-ng/QUICKSTART.md) — an AI-native terminal
- [Start with Copilot Shell](user-guide/en/user-entrypoint/copilot-shell/QUICKSTART.md) — an extensible Agent Shell

On Alibaba Cloud Linux 4, use the RPM backend so dnf selects the matching
system libraries. Only the component action uses `sudo`:

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

`--cosh-ng` remains available as shorthand for `--component cosh-ng`. On
macOS arm64, use `--backend raw --install-mode user` instead.

### Add observability, security, or runtime controls

| Goal | Start here |
|------|------------|
| Observe Agent activity and Token usage | [AgentSight](user-guide/en/agent-observability/agentsight/README.md) |
| Add security enforcement | [Agent Sec Core](user-guide/en/agent-security/agent-sec-core/QUICKSTART.md) |
| Create workspace recovery points | [ws-ckpt](user-guide/en/runtime/ws-ckpt.md) |
| Mount Skills on demand | [SkillFS](user-guide/en/runtime/skillfs.md) |
| Reuse context across sessions | [Agent Memory](user-guide/en/token-saving/agent-memory.md) |
| Tune kernel parameters deterministically | [ktuner](user-guide/en/user-entrypoint/ktuner.md) |

Each component page starts with its supported platforms and preferred
installation path. Linux-only components must be installed and run on Linux.

## Explore the installation

Use the CLI to inspect the current machine and installed components:

```bash
anolisa env
anolisa list
anolisa status
```

Adapters connect an installed component to an Agent framework. Scan the
available integrations after installing a component:

```bash
anolisa adapter scan
```

## Next steps

- [Installation guide](user-guide/en/installation.md) — platform support, system mode, RPM, and all component install commands
- [Full user guide](user-guide/en/README.md) — configure, operate, and troubleshoot each capability
- [anolisa CLI reference](user-guide/en/user-entrypoint/anolisa-cli.md) — lifecycle and adapter commands
- [Troubleshooting](user-guide/en/troubleshooting.md) — common installation and runtime failures
- [Build from source](BUILDING.md) — developer builds only

Qoder plugin publishers can also use the [standalone bundle builder](../src/tokenless/packaging/qoder/README.md); read its runtime requirements and recovery limitations before distribution.

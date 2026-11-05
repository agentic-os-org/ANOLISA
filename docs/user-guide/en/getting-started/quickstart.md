# ANOLISA Quick Start

[中文版](../../zh/getting-started/quickstart.md)

ANOLISA is a server-side operating layer for AI Agent workloads. Install the
CLI once, then enable only the capability that delivers the first result you
want.

For AW Preview packages, see [Install AW](../aw/installation.md).

## Install the CLI

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
```

If the anolisa CLI is already installed, skip these commands. Alinux 4 users
can alternatively install the CLI with `sudo yum install anolisa`.

## Choose your first outcome

### Reduce Agent Token usage

Windows x64 users can build the native Tokenless npm packages and connect
[QwenWorkCN local mode](../tokenless/framework-integration.md#qwenwork-windows-x64-local-mode).
See [Windows build instructions](../../../developer-guide/en/tokenless/windows.md#build-and-test-on-windows); these packages
are not yet published. The anolisa CLI installation above is for Linux/macOS.

Install Tokenless, connect it to Codex, Claude Code, Qoder, OpenClaw, Hermes,
Qwen Code, or cosh-ng, then verify a before/after Token record.

[Install and try Tokenless →](../tokenless/index.md)

### Work with an Agent-native terminal

Start with [cosh-ng](../cosh-ng/index.md), an AI-native terminal.

On Alibaba Cloud Linux 4, use the RPM backend so dnf selects the matching
system libraries. Only the component action uses `sudo`:

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

`--cosh-ng` remains available as shorthand for `--component cosh-ng`. On
macOS 11+ (arm64 / x86_64), use `--backend raw --install-mode user` instead.

### Add observability, security, or runtime controls

| Goal | Start here |
|------|------------|
| Observe Agent activity and Token usage | [AgentSight](../agentsight/index.md) |
| Add security enforcement | [Agent Sec Core](../agent-sec-core/index.md) |
| Evaluate the Rust SkillSec core | [SkillSec V2](../agent-sec-core/skillsec-v2.md) |
| Create workspace recovery points | [ws-ckpt](../ws-ckpt/index.md) |
| Mount Skills on demand | [SkillFS](../skillfs/index.md) |
| Reuse context across sessions | [Agent Memory](../agent-memory/index.md) |
| Tune kernel parameters deterministically | [ktuner](../ktuner/index.md) |

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

- [Images and cloud services](images.md) — system images, container base images, cloud services, and community submissions
- [Installation guide](installation.md) — platform support, system mode, RPM, and all component install commands
- [Full user guide](../index.md) — configure, operate, and troubleshoot each capability
- [anolisa CLI reference](anolisa-cli.md) — lifecycle and adapter commands
- [Troubleshooting](troubleshooting.md) — common installation and runtime failures
- [Build from source](building.md) — prepare toolchains and build components
- [Testing](testing.md) — validate source changes and understand test coverage

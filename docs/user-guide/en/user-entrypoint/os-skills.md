# OS Skills

OS Skills is a system management and DevOps skill library for AI Agents. It provides pre-built skills that enable Agents to perform common system administration and automation tasks.

---

## Overview

OS Skills covers three main areas:

- **System Administration** — user management, service control, package operations, filesystem tasks
- **Cloud Integration** — cloud resource queries, instance management, network configuration
- **DevOps Automation** — CI/CD pipeline management, container operations, deployment workflows

---

## Installation

```bash
anolisa install os-skills
```

---

## Quick Start

Once installed, OS Skills are available to any ANOLISA-compatible Agent runtime. The Agent can invoke skills via natural language:

```
> "Check disk usage on all mounted filesystems"
> "Restart the nginx service"
> "Show running containers and their resource usage"
```

---

## Skill Categories

### System Administration

| Skill | Description |
|-------|-------------|
| `disk-usage` | Check filesystem disk usage |
| `service-ctl` | Start/stop/restart system services |
| `process-mgmt` | List and manage processes |
| `user-mgmt` | User and group management |
| `package-ops` | Package install/remove/query |

### DevOps Automation

| Skill | Description |
|-------|-------------|
| `container-ops` | Docker/Podman container management |
| `log-analysis` | Search and analyze system logs |
| `network-diag` | Network diagnostics (ping, traceroute, port check) |
| `cron-mgmt` | Cron job management |

---

## Usage with Agent Runtimes

OS Skills integrates with cosh and other ANOLISA-compatible runtimes automatically. Skills are discovered at startup and made available to the Agent's tool inventory.

```bash
# Verify skills are loaded
anolisa status os-skills
```

---

## Configuration

Configuration file: `~/.config/os-skills/config.toml`

```toml
[skills]
# Enabled skill categories
enabled = ["system", "devops"]

[safety]
# Require confirmation for destructive operations
confirm_destructive = true
```

---

## OpenAI-compatible OpenClaw preflight

The OpenClaw installer validates both `anthropic-messages` (the default) and
`openai-completions` before writing configuration or starting Gateway:

```bash
python3 SKILL_DIR/scripts/install_openclaw.py --billing payg \
  --api-key-env BAILIAN_API_KEY --provider-api openai-completions \
  --base-url https://dashscope.aliyuncs.com/compatible-mode/v1 --model-id "$OPENAI_MODEL"
```

Replace `SKILL_DIR` with the installed `ai/install-openclaw` directory and set
`OPENAI_MODEL` to a model accepted by your configured endpoint. Supply the
complete compatible API base with `--base-url`; this protocol selection does
not replace the billing plan's default Base URL. The check appends
`/chat/completions` to the base path, preserving query parameters, and accepts
an already complete endpoint without duplicating the path. It sends a Bearer
API key and a user `ping` message with `max_tokens: 1`, using
`--preflight-timeout` (default 20 seconds). HTTP/authentication/network failures
stop before config writes and reuse the existing provider diagnostics.

`--skip-preflight` explicitly defers endpoint validation; unsupported provider
protocols retain the existing skip notice. `--dry-run` still performs the
preflight unless it is skipped, while `--precheck-only` performs local checks
without a model request. Endpoints must accept the standard chat request;
models requiring additional provider-specific parameters need validation
through their own supported configuration.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

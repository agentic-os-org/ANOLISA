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

## OpenClaw runtime option bounds

The OpenClaw installer validates its port and timeout flags during argument
parsing, before dependency checks, configuration writes or model calls:

```bash
python3 SKILL_DIR/scripts/install_openclaw.py --precheck-only --gateway-port 18789 --preflight-timeout 20
```

Replace `SKILL_DIR` with the installed `ai/install-openclaw` directory.
`--gateway-port` must be an integer from 1 to 65535. All five timeout options
must be positive integer seconds; zero and negative values are rejected.
Defaults remain unchanged:

| Option | Default |
| --- | --- |
| `--gateway-port` | 18789 |
| `--gateway-command-timeout` | 30 seconds |
| `--gateway-ready-timeout` | 30 seconds |
| `--gateway-status-timeout` | 8 seconds |
| `--gateway-write-check-timeout` | 60 seconds |
| `--preflight-timeout` | 20 seconds |

Validation also applies to supplied flags in `--precheck-only` or skip modes.
Use `--skip-preflight` to skip the model check; setting its timeout to zero
is not a substitute for that flag.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

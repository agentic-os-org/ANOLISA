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

## CSV and TSV separators

Use the `xlsx` skill reader for delimited exports that use a separator other
than the default comma (`.csv`) or tab (`.tsv`):

```bash
python3 SKILL_DIR/scripts/xlsx_reader.py regional.csv --delimiter ';' --json
```

Replace `SKILL_DIR` with the installed `others/xlsx` directory. `--delimiter`
accepts one literal character, including a tab or `|`, and overrides the file
extension's default separator. CSV quoting still protects separators inside
quoted cell text. Empty values, multiple characters, newline, carriage return,
and NUL are rejected. The option applies only to CSV/TSV; Excel input rejects
it. Encoding fallback, reports, data quality checks, and statistics continue to
use the parsed columns. The reader does not modify the source file.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

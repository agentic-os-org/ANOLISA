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

## Password-protected Excel analysis

Analyze an OOXML workbook with its known password without creating an unprotected file. The readonly reader supports `.xlsx` and `.xlsm` through the optional `msoffcrypto-tool` backend:

```bash
pip install pandas openpyxl msoffcrypto-tool
python3 SKILL_DIR/scripts/xlsx_reader.py protected.xlsx --password-env WORKBOOK_PASSWORD --json
python3 SKILL_DIR/scripts/xlsx_reader.py protected.xlsx --password-env WORKBOOK_PASSWORD --sheet Sales --quality
```

Set `WORKBOOK_PASSWORD` in the command environment before running these examples; the flag names the environment variable and never takes the password value itself. An unset or empty variable is a usage error; the backend requires a non-empty password. The reader does not prompt, discover or store passwords, and reports do not include the credential.

Known-password decryption occurs in memory, verifies the password and supported payload integrity, and keeps the original file unchanged. All-sheet and named-sheet analysis use the existing structure, quality and statistics schema. Plain Excel inputs do not require the optional dependency, including when a password variable was supplied. Missing credentials/backends, incorrect credentials and damaged encrypted inputs produce errors without plaintext output files.

This option applies only to OOXML `.xlsx`/`.xlsm`; CSV, TSV and legacy XLS are rejected when the flag is used. It reads stored table values without executing macros. Worksheet protection is separate from file encryption and does not need a file password for reading.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

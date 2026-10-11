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

## PDF navigation links

Use `--links` with JSON output to inspect navigation targets embedded on selected pages. This reads metadata without following URIs or opening destination files.

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f handbook.pdf --format json --links --pages 1-3 --metadata
```

Each selected page gains a `links` array; pages without links use `[]`. Records retain the fields returned by PyMuPDF. `kind` identifies the action, `from` becomes a four-number hotspot rectangle, and a point-valued `to` becomes `[x, y]`. Nonnegative destination `page` indices become one-based numbers, consistent with page reports; negative indirect-destination sentinels remain unchanged.

URI actions provide `uri`; remote or launch actions can provide `file`. Optional fields such as `xref`, `id` and `zoom` are retained. Symbolic targets remain text. Depending on the engine version, a remote indirect target can appear as a symbolic `to` with `page: -1`, or as a filename/URI anchor. The returned representation and engine coordinates are preserved.

Page selection controls both text and links, while metadata remains independently selectable. Omit the flag to retain existing output. Text-mode use is rejected before engine loading or file access, and the PDF stays unchanged. This covers page-link actions separately from TOC bookmarks and review annotations.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

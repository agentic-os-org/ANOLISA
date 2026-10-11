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

## PDF form field analysis

Extract stored values from a filled PDF form with `--form-fields --format json`. The optional per-page `form_fields` list contains each selected page's AcroForm widget appearances:

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f application.pdf --format json --form-fields
python3 SKILL_DIR/scripts/read_pdf.py -f application.pdf --format json --form-fields -p "1-2" --metadata
```

Each record includes `name`, `label`, `type`, `type_id`, stored `value`, `flags`, `rect` and `xref`. Rectangles are four-number JSON arrays. Choice fields also include `choices`; checkbox/radio fields include `button_states` when available. Values such as `Yes` and `Off` remain their stored states rather than being guessed from page text. Signature fields include the SDK's `signed` status, which does not verify signature validity or trust.

Logical fields can appear in several places or pages, so appearances remain separate records with their page placement. Dotted hierarchical names, Unicode values, blank fields and multiple-choice metadata are retained. Pages without widgets use `form_fields: []`. Page selection and document metadata work as usual; default text/JSON output is unchanged when this flag is omitted.

The reader copies values before closing the document and leaves the PDF bytes unchanged. It does not fill/reset fields, execute embedded JavaScript or infer form values from appearance text. Normal annotation extraction is a separate view because widgets represent form fields rather than ordinary annotations. This mode requires JSON and a PyMuPDF backend with widget/button-state support.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

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

## PDF embedded attachments

Discover supporting files embedded in a PDF and selectively recover one without modifying the document. Both attachment modes require JSON:

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f portfolio.pdf --format json --attachments
python3 SKILL_DIR/scripts/read_pdf.py -f portfolio.pdf --format json --extract-attachment 0 --output recovered.bin
```

The document-level `attachments` list includes a stable zero-based `index` and the SDK's metadata: embedded name, filenames/description, uncompressed `size`, stored `length`, dates and available portfolio/checksum information. Indices follow physical attachment order and distinguish duplicate names. Strings and PDF dates retain the SDK's representation; a stored checksum is metadata rather than a verification result. Page selection affects text pages, while attachments remain document-wide. Metadata discovery does not load attachment payloads.

Select the index from that list and provide `--output` explicitly. The reader extracts exact bytes, including compressed binary, UTF-8 and empty contents; filenames inside the PDF are never used to choose a filesystem destination. JSON also reports `extracted_attachment` with the chosen index, destination and byte size. Selected payloads are loaded into memory. No attached program or action is executed.

The output parent directory must exist. Extraction publishes a complete new file and refuses every existing output path, including the source PDF and concurrently created files. Publication requires same-filesystem hard-link support; failures leave no partial new output and clean private staging. Unknown indices, missing output paths and incompatible text-mode options fail explicitly. Omit the attachment options for the existing text/JSON schema; original PDF bytes remain unchanged.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

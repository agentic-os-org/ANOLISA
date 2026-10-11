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

## Bounded spreadsheet analysis

Inspect a large table with a predictable number of analyzed records using `--max-rows N`. The positive limit applies independently to each selected Excel worksheet or to the single CSV/TSV table:

```bash
python3 SKILL_DIR/scripts/xlsx_reader.py export.csv --max-rows 1000 --json
python3 SKILL_DIR/scripts/xlsx_reader.py workbook.xlsx --sheet Sales --max-rows 1000 --quality
```

The reader requests at most `N+1` data records from pandas, uses the extra record to detect truncation, then analyzes the first N. Truncated tables are reread with N records so the lookahead cannot alter analyzed values or column types. Headers are excluded from the budget; a quoted CSV field spanning lines still counts as one record. Empty, shorter and exactly N-row tables report no truncation. This bounds loaded dataframe rows, rather than guaranteeing a byte-memory limit for workbook parsing.

When a limit is supplied, JSON gains `sampling` with `scope: analyzed_rows_only`, `max_rows_per_sheet`, and each sheet's `rows_analyzed`/`truncated` values. The human report labels analyzed rows and worksheets with additional data. Structure, null percentages, quality findings and statistics all describe the analyzed prefix; the reader does not infer full-file row totals or quality from that prefix. Errors or encoding problems beyond the records read may remain unobserved.

Omit the flag for the existing full-data behavior and unchanged report schema. `--sheet`, `--quality`, Unicode/quoted records and automatic text decoding remain compatible; original file bytes stay unchanged. This mode selects the first rows and is not a random or representative sample.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

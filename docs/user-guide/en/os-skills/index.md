# OS Skills

OS Skills is a skill library for AI Agents that operate the system. It ships pre-built skills that let an Agent carry out system administration, kernel development, cloud operations, and document processing tasks.

---

## Overview

Each skill lives in its own directory under `src/os-skills/`, grouped by category:

| Category | Directory | Covers |
|----------|-----------|--------|
| **AI Tools** | `ai/` | Installing and configuring AI coding tools, MCP server setup |
| **Alibaba Cloud** | `aliyun/` | ECS lifecycle management through the Alibaba Cloud CLI |
| **DevOps** | `devops/` | GitHub workflows, kernel development, SysOM and AgentSight diagnostics |
| **Monitoring & Perf** | `monitor-perf/` | Reserved for monitoring and performance skills; ships no skills yet |
| **Others** | `others/` | Product guides, skill management, image, PDF, and spreadsheet tools |
| **Security** | `security/` | Alibaba Cloud Linux CVE queries |
| **System Admin** | `system-admin/` | ALinux administration, backup and restore, kernel tuning and upgrade, storage resize, shell scripting |

---

## Installation

```bash
anolisa install os-skills
```

Skills are installed to `/usr/share/anolisa/skills/` and discovered automatically by Copilot Shell.

---

## Quick Start

Once installed, OS Skills are available to any ANOLISA-compatible Agent runtime. The Agent can invoke skills via natural language:

```
> "Grow the partition and the XFS filesystem after the cloud disk expansion"
> "Check whether this CVE affects my Alibaba Cloud Linux host"
> "Extract the tables from report.pdf"
```

---

## Included Skills

The names below are the skill directory names. Each directory holds a `SKILL.md` that defines the skill.

### AI Tools (`ai/`)

| Skill | Description |
|-------|-------------|
| `install-claude-code` | Install and configure Claude Code on RPM-based Linux systems |
| `install-hermes` | One-click installation and configuration of the Hermes Agent |
| `install-openclaw` | Install and configure OpenClaw non-interactively with Alibaba Cloud Model Studio |
| `install-qwenpaw` | Deploy the QwenPaw AI assistant on Alibaba Cloud Linux 4, including the model API key and the DingTalk channel |
| `install-tokenless` | Install and configure Tokenless, the LLM token optimization toolkit |
| `qwenpaw-usage` | QwenPaw CLI usage: heartbeat configuration, scheduled tasks, Docker packaging, workspace migration |
| `setup-mcp` | Add and configure MCP servers in Copilot Shell, converting formats and keeping the JSON valid |

### Alibaba Cloud (`aliyun/`)

| Skill | Description |
|-------|-------------|
| `aliyun-ecs` | Manage Elastic Compute Service through the `aliyun` CLI: instances, disks, snapshots, images, security groups, key pairs, and elastic network interfaces |

### DevOps (`devops/`)

| Skill | Description |
|-------|-------------|
| `github` | Interact with GitHub through the `gh` CLI: issues, pull requests, CI runs, and API calls |
| `kernel-dev` | Linux kernel development automation: SRPM and upstream builds, module development environment, dependency setup |
| `sysom-agentsight` | Query AgentSight token consumption, token savings, audit events, and session interruptions |
| `sysom-diagnosis` | Diagnose Linux server performance and stability problems and surface recommendations |

### Others (`others/`)

| Skill | Description |
|-------|-------------|
| `anolisa-guide` | Answer questions about ANOLISA, Alibaba Cloud Linux 4 Agentic Edition, and Copilot Shell |
| `anolisa-register` | Manage Agentic OS registration, including joining and leaving the co-build program |
| `clawhub-skill-mng` | Search, install, uninstall, update, and manage agent skills |
| `cosh-guide` | Copilot Shell (cosh) CLI user guide and help |
| `humanizer` | Remove signs of AI-generated writing so text reads more naturally |
| `image-gen` | Generate images from text prompts through DashScope/Qwen |
| `pdf-reader` | Extract text and tables from PDF files |
| `xlsx` | Open, create, read, analyze, edit, and validate Excel and CSV/TSV files |

### Security (`security/`)

| Skill | Description |
|-------|-------------|
| `alinux-cve-query` | Query Alibaba Cloud Linux CVE information and the matching security advisories |

### System Admin (`system-admin/`)

| Skill | Description |
|-------|-------------|
| `alinux-admin` | Administer Alibaba Cloud Linux 4: shell scripts, systemd services, and networking |
| `backup-restore` | Back up and restore workspaces, user data, the operating system, and Alibaba Cloud snapshots |
| `ktuner` | Tune kernel parameters automatically, with recommendations, one-click apply, and rollback |
| `regex-mastery` | Write, debug, and optimize regular expressions |
| `shell-scripting` | Write bash or zsh scripts: argument parsing, error handling, and CLI automation |
| `storage-resize` | Grow partitions and XFS/EXT4/Btrfs filesystems after a cloud disk expansion |
| `upgrade-alinux-kernel` | Upgrade the Linux kernel on Alibaba Cloud Linux instances |

---

## Usage with Agent Runtimes

OS Skills integrates with cosh and other ANOLISA-compatible runtimes automatically. Skills are discovered at startup and made available to the Agent's tool inventory.

```bash
# Verify skills are loaded
anolisa status os-skills
```

---

## See Also

- [anolisa CLI](../getting-started/anolisa-cli.md)

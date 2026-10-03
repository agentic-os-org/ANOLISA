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

The bundled skills, grouped by the category directories under `src/os-skills/`:

### AI Tools

| Skill | Description |
|-------|-------------|
| `install-claude-code` | Install and configure Claude Code IDE |
| `install-hermes` | Install and configure Hermes Agent |
| `install-openclaw` | Install and configure OpenClaw |
| `install-qwenpaw` | Deploy the QwenPaw AI assistant with DingTalk integration |
| `install-tokenless` | Install and configure Tokenless (LLM token optimization) |
| `qwenpaw-usage` | QwenPaw usage guide (heartbeat, scheduled tasks, packaging) |
| `setup-mcp` | Configure MCP servers in Copilot Shell |

### System Administration

| Skill | Description |
|-------|-------------|
| `alinux-admin` | ALinux 4 system management (systemd, SSH, firewalld, NetworkManager) |
| `backup-restore` | System backup and restore |
| `ktuner` | Kernel parameter auto-tuning with recommendations and rollback |
| `regex-mastery` | Regular expression guide |
| `shell-scripting` | Bash/Zsh scripting and automation |
| `storage-resize` | Alibaba Cloud disk expansion (XFS/EXT4/Btrfs) |
| `upgrade-alinux-kernel` | ALinux kernel upgrade |

### DevOps

| Skill | Description |
|-------|-------------|
| `github` | GitHub workflows and integration via the `gh` CLI |
| `kernel-dev` | ALinux 4 kernel development automation (SRPM and upstream) |
| `sysom-agentsight` | AgentSight token, audit, and session diagnostics queries |
| `sysom-diagnosis` | SysOM diagnostics and tuning |

### Alibaba Cloud

| Skill | Description |
|-------|-------------|
| `aliyun-ecs` | ECS instance lifecycle management via Alibaba Cloud CLI |

### Security

| Skill | Description |
|-------|-------------|
| `alinux-cve-query` | Query Alibaba Cloud Linux CVE vulnerability info |

### Others

| Skill | Description |
|-------|-------------|
| `anolisa-guide` | Answers about ANOLISA, Agentic OS, cosh, and Copilot Shell |
| `anolisa-register` | Manage ANOLISA registration (join or leave the co-build program) |
| `clawhub-skill-mng` | Search, install, and manage agent skills from ClawHub |
| `cosh-guide` | Copilot Shell user guide and help |
| `humanizer` | Remove signs of AI-generated writing from text |
| `image-gen` | Generate images from text prompts via DashScope/Qwen |
| `pdf-reader` | Extract text from PDF files |
| `xlsx` | Open, create, edit, and validate Excel/spreadsheet files |

---

## Usage with Agent Runtimes

OS Skills integrates with cosh and other ANOLISA-compatible runtimes automatically. Skills are discovered at startup and made available to the Agent's tool inventory.

```bash
# Verify skills are loaded
anolisa status os-skills
```

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)

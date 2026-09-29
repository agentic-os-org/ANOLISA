# OS Skills

[中文版](README_zh.md)

A curated collection of operational skills for AI Agents, covering system administration, monitoring, security, DevOps, and cloud integration. OS Skills is the skill library of [ANOLISA](../../README.md), installed to `/usr/share/anolisa/skills/` and discovered automatically by Copilot Shell.

## Skill Categories

| Category | Directory | Description |
|----------|-----------|-------------|
| **AI Tools** | `ai/` | AI programming tool integration (Claude Code, OpenClaw, QwenPaw, MCP setup) |
| **System Admin** | `system-admin/` | Package management, storage, networking, kernel, shell scripting |
| **DevOps** | `devops/` | Git workflows, CI/CD, kernel development, diagnostics |
| **Alibaba Cloud** | `aliyun/` | ECS instance management, cloud networking, GPU/AI deployment |
| **Security** | `security/` | CVE queries, compliance checks, system hardening |
| **Monitoring & Perf** | `monitor-perf/` | sysAK diagnostics, keentune tuning, sysctl management |
| **Others** | `others/` | Experimental and third-party extensions |

## Included Skills

### AI Tools
- **install-claude-code** — Install and configure Claude Code IDE
- **install-qwenpaw** — Deploy QwenPaw AI assistant with DingTalk integration
- **install-openclaw** — Install and configure OpenClaw
- **install-tokenless** — Install and configure Tokenless (LLM token optimization)
- **qwenpaw-usage** — QwenPaw usage guide
- **setup-mcp** — Configure MCP servers in Copilot Shell

### System Admin
- **alinux-admin** — ALinux 4 system management (systemd, SSH, firewalld, NetworkManager)
- **backup-restore** — System backup and restore
- **regex-mastery** — Regular expression guide
- **shell-scripting** — Bash/Zsh scripting and automation
- **storage-resize** — Alibaba Cloud disk expansion (XFS/EXT4/Btrfs)
- **upgrade-alinux-kernel** — ALinux kernel upgrade

### DevOps
- **github** — GitHub workflows and integration
- **kernel-dev** — ALinux 4 kernel development automation (SRPM & upstream)
- **sysom-agentsight** — System diagnostics tool
- **sysom-diagnosis** — SysOM diagnostics and tuning

### Alibaba Cloud
- **aliyun-ecs** — ECS instance lifecycle management via Alibaba Cloud CLI

### Security
- **alinux-cve-query** — Query Alibaba Cloud Linux CVE vulnerability info

### Other
- **cosh-guide** - Copilot Shell user guide

## Skill Format

Each skill lives in its own directory with at least a `SKILL.md` file:

```
skill-name/
├── SKILL.md              # Required: skill definition (YAML frontmatter + Markdown)
├── scripts/              # Optional: executable scripts (.sh, .py)
├── reference/            # Optional: config templates and reference files
└── docs/                 # Optional: detailed documentation
```

### SKILL.md Structure

```yaml
---
name: my-skill
version: 1.0.0
description: What this skill does
layer: application          # application | system | core
lifecycle: production       # production | operations | usage | maintenance
tags: [example, demo]
platforms: [cosh, claude-code, gemini-cli]
---

# My Skill

Markdown content with usage instructions, prerequisites, and examples.
```

## Installation

### Extension (Recommended)

Install all skills as a single extension:

```
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills
```

Or install a specific category only:

```
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/ai
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/system-admin
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/devops
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/security
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/aliyun
```

Or install from a local path:

```
/extensions install /path/to/os-skills
```

> **Note:** Extension-based installation is currently only supported in Copilot Shell (cosh).

### RPM

```bash
sudo yum install os-skills
```

Skills are installed to `/usr/share/anolisa/skills/` and auto-discovered by Copilot Shell.

### Manual

Copy any skill directory to one of the skill search paths:
1. Project: `.copilot-shell/skills/`
2. User: `~/.copilot-shell/skills/`
3. System: `/usr/share/anolisa/skills/`

## Writing a New Skill

1. Create a directory under the appropriate category (e.g. `system-admin/my-skill/`).
2. Add a `SKILL.md` with YAML frontmatter and Markdown content.
3. Optionally add `scripts/` and `reference/` directories.
4. Test the skill in Copilot Shell.
5. Submit a pull request.

## License

Apache License 2.0 — see [LICENSE](../../LICENSE) for details.

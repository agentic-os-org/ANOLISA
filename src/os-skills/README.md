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

## Planned Skill Retirement

OS Skills plans to remove the nine skills below from its default bundle and wind
down routine maintenance of the bundled copies. They remain in the current bundle.
The plan keeps OS Skills focused on OS operations and directs users to the tools,
upstream skills, and distribution guidance maintained for each workflow.

| Skill | Migration path |
|-------|----------------|
| `github` | Use `gh` directly for issues, pull requests, and CI. See the [GitHub CLI manual](https://cli.github.com/manual/). |
| `clawhub-skill-mng` | Use the official CLI to search, install, and update skills. Check the registry and installation path if you relied on the bundled cosh settings. See the [ClawHub CLI guide](https://github.com/openclaw/clawhub/blob/main/docs/cli.md). |
| `humanizer` | Install the upstream skill separately when needed. See [Humanizer](https://github.com/blader/humanizer). |
| `image-gen` | We plan to maintain Bailian integrations with the products that use them. For direct API use, see the [Alibaba Cloud Model Studio text-to-image guide](https://www.alibabacloud.com/help/en/model-studio/text-to-image). |
| `pdf-reader` | Use your Agent's PDF tools or a PyMuPDF integration. Verify page selection, table extraction, and JSON output before switching an automated workflow. See the [PyMuPDF documentation](https://pymupdf.readthedocs.io/en/latest/). |
| `xlsx` | Use the related upstream skill for general spreadsheet workflows. Test that edited workbooks remain intact and compatible with the applications you use before switching. See [MiniMax XLSX](https://github.com/MiniMax-AI/skills/blob/main/skills/minimax-xlsx/SKILL.md). |
| `regex-mastery` | Use the regular expression reference for your tool or language. For `grep`, see the [GNU grep reference](https://www.gnu.org/s/grep/manual/html_node/Regular-Expressions.html). |
| `shell-scripting` | For general scripting, see the [Bash manual](https://www.gnu.org/s/bash/manual/bash.html) or [Zsh manual](https://zsh.sourceforge.io/Doc/Release/index.html). For ALinux 4 operations, see the [Bash scripting reference](system-admin/alinux-admin/references/shell-scripting.md). |
| `upgrade-alinux-kernel` | Follow the distribution's upgrade and recovery guidance for your installed release. See the [Alibaba Cloud Linux documentation](https://www.alibabacloud.com/help/en/alinux/). |

If a bundled skill causes data loss or blocks a critical workflow, include the
OS Skills version and the observed impact in your [GitHub issue](https://github.com/agentic-os-org/ANOLISA/issues).
Report vulnerabilities through the [security policy](../../SECURITY.md).

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

### ANOLISA

```bash
anolisa install os-skills --backend raw
```

The GitHub Release and Preview workflows build Linux x86_64 and ARM64 raw
archives from the same source commit. Each target provides a `.tar.gz` archive,
its SHA256 checksum, a CycloneDX SBOM, and the SBOM checksum. The package
installs the tracked skill directories under `{datadir}/skills`; no executable
is compiled. CI checks the tag and source version for official releases.
The local release pipeline still uses its existing configuration.

### Extension

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

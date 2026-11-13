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
- **storage-resize** — Alibaba Cloud disk expansion (XFS/EXT4/Btrfs)

### DevOps
- **kernel-dev** — ALinux 4 kernel development automation (SRPM & upstream)
- **sysom-agentsight** — System diagnostics tool
- **sysom-diagnosis** — SysOM diagnostics and tuning

### Alibaba Cloud
- **aliyun-ecs** — ECS instance lifecycle management via Alibaba Cloud CLI

### Security
- **alinux-cve-query** — Query Alibaba Cloud Linux CVE vulnerability info

### Other
- **cosh-guide** - Copilot Shell user guide

## Removed Skills

To focus on OS operations, OS Skills removed the following eight skills from its
default bundle and wound down routine maintenance of the bundled copies.
Users who need these capabilities can use upstream skills, tools, or official
documentation.

- `github` — [GitHub CLI](https://github.com/cli/cli/blob/trunk/skills/gh/SKILL.md)
- `humanizer` — [Humanizer](https://github.com/blader/humanizer)
- `image-gen`
- `pdf-reader`
- `xlsx` — [MiniMax XLSX](https://github.com/MiniMax-AI/skills/blob/main/skills/minimax-xlsx/SKILL.md)
- `regex-mastery`
- `shell-scripting`
- `upgrade-alinux-kernel`

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
installs the skill directories listed in [the release catalog](packaging/released-skills.txt)
under `{datadir}/skills`. CI checks the tag and source version for official releases.
The local release pipeline still uses its existing configuration.

### Extension

Install all released skills as a single extension:

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

Keep script regression tests in `tests/` within this component. See the
[contribution guide](../../CONTRIBUTING.md#skill-regression-tests-and-path-ownership)
for dependencies, test commands, and component ownership.

### Incubation and default delivery

A reviewed skill can enter incubation in its normal category directory. The
release catalog controls default source installs, packages, and COSH extensions.
Trial users can copy the chosen skill from a reviewed source revision using the
manual installation instructions above.

Source installation and release-catalog maintenance require Python 3.6 or newer
with its standard library. See the [source installation guide](../../docs/user-guide/en/os-skills/index.md#installation)
for selecting the installer interpreter.

When maintainers approve default delivery, add the directory to
[`packaging/released-skills.txt`](packaging/released-skills.txt) and run
`python3 src/os-skills/packaging/cosh_manifests.py --write` from the repository
root. Include any applicable adapter registrations in `component.toml`.
CI verifies that the generated COSH manifests match the catalog.

## License

Apache License 2.0 — see [LICENSE](../../LICENSE) for details.

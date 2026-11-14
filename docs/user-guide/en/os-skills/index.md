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

Skills installed with `anolisa install` are placed in `/usr/share/anolisa/skills/` and discovered automatically by Copilot Shell.

From a source checkout, developers can install the collection for cosh:

```bash
make -C src/os-skills install
```

For a regular user, this keeps the existing `~/.copilot-shell/skills/`
directory; root installs to `/usr/local/share/anolisa/skills/`.
`AWPRJ` defaults to `anolisa` for system resources and can be overridden.
Use `SKILLS_DIR=/absolute/path` to select another collection directory;
use the same value when uninstalling. Start a new cosh session and run
`/skills` to check the installed collection.

`INSTALL_PROFILE=user` derives its default paths from `HOME`; when `HOME` is
unset, empty, relative or `/`, the install fails instead of writing under `/`.
Pass `PREFIX`, `DATADIR` and `SKILLS_DIR` explicitly to install without a home
directory.

Local system copies take precedence over RPM copies in
`/usr/share/anolisa/skills/` when their skill names match. Use one delivery
method for the collection; when managing an older source installation,
set `SKILLS_DIR` to its original directory.

`make uninstall` removes only the skill directories recorded by
`make install` in `<datadir>/anolisa/components/os-skills/install-manifest`.
A collection without that manifest is left untouched and the target exits
non-zero, so it never removes a same-name skill directory that os-skills did
not install. `make install` follows the same rule in the other direction: a
same-name directory it did not create is refused unless you pass
`REPLACE_UNOWNED=1`, which replaces it and records it for later removal. Only
`1` enables this; `0` or an unset value keeps the refusal, and any other value
is rejected before anything changes. An
installation made before the manifest existed therefore needs one
`REPLACE_UNOWNED=1 make install` before it can be removed. Recorded directories
are managed as a whole and replaced on every install, so keep personal files out
of them and give your own skills a different name. If an install fails or is
interrupted, it puts the previous skills back and leaves the manifest unchanged.
An interruption after the manifest has recorded the new skills keeps them
installed instead and asks you to run `make install` again to finish.
When it cannot undo a step, for example because the collection directory
became read-only, the error names what is left: a new copy it could not remove,
or the `.os-skills.*/.old` directory that still holds a previous skill. Names
recorded by earlier installs stay in the manifest, so a skill dropped from a
newer release is removed as well. RPM installations are removed by the package
manager and raw
`anolisa install` deployments by `anolisa uninstall os-skills`, not by
`make uninstall`.

A receipt belongs to a single collection: `make install` refuses a directory
that the receipt does not record, and `make uninstall` removes the receipt
together with the skills, so pass a separate `MANIFEST=` for each collection
you keep. Uninstalling one collection keeps the receipts and shared component
metadata of the others.

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

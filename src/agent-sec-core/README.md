# Agent Sec Core

[中文版](README_zh.md)

**OS-level security kernel for AI Agents.** Provides defense in depth for Agent
workloads: prompt injection detection, code scanning, PII detection, skill
integrity tracking, system baseline hardening, sandbox isolation, and a local
security event store. Everything runs locally with no Token cost. Applicable to
Agent OS platforms such as [ANOLISA](../../README.md) and to the six Agent hosts
listed below.

[![AARM Aligned](https://img.shields.io/badge/AARM-Aligned-blue.svg)](https://aarm.dev/builders/agentseccore-anolisa)

**AgentSecCore (ANOLISA)** is listed in the [AARM Builder Registry](https://aarm.dev/builders/agentseccore-anolisa) with **Aligned** status.

AgentSecCore contributes controls to the [ANOLISA OWASP Agentic Top 10 mapping](../../docs/user-guide/en/agent-security/owasp-agentic-top10.md).

## Background

As AI Agents gradually gain OS-level execution capabilities (file I/O, network access, process management, etc.), traditional application security boundaries no longer apply. Agent Sec Core builds a **defense-in-depth** system at the OS layer, ensuring Agents run in a controlled, auditable, least-privilege environment.

## Core Principles

1. **Least Privilege** — Agents receive only the minimum system permissions required to complete a task.
2. **Explicit Authorization** — Sensitive operations require explicit user confirmation; silent privilege escalation is forbidden.
3. **Zero Trust** — Skills are mutually untrusted; each operation is independently authenticated.
4. **Defense in Depth** — Pre-execution prevention → runtime detection → kernel-level containment. Compromise of any single layer does not affect the others.
5. **Security Over Execution** — When security and functionality conflict, security wins. When in doubt, treat as high risk.

## Capabilities

| Module | Description | CLI entry |
|--------|-------------|-----------|
| **Prompt Scanner** | Prompt injection / jailbreak detection: rule engine (L1) + ML classifier (L2), plus multi-turn intent detection (L4) | `agent-sec-cli scan-prompt` |
| **Code Scanner** | Static analysis of bash / python code for dangerous operations | `agent-sec-cli scan-code` |
| **PII Checker** | Personal data and credential detection with redaction | `agent-sec-cli scan-pii` |
| **Skill Ledger** | Ed25519-signed skill integrity ledger with an append-only version chain | `agent-sec-cli skill-ledger` |
| **Security Baseline** | System hardening scan and remediation (wraps `loongshield seharden`) | `agent-sec-cli harden` |
| **Observability** | Agent lifecycle event recording, session debrief, and an interactive review TUI | `agent-sec-cli observability` |
| **Security Events** | Local JSONL + SQLite event store with query and aggregation | `agent-sec-cli events` |
| **Sandbox** | Syscall-level command isolation (bubblewrap + seccomp), used as an architecture layer | `linux-sandbox` |

The background daemon (`agent-sec-daemon`, shipped as the `agent-sec-core.service`
systemd **user** unit) provides health, SkillFS notification, and security-query
RPCs. Prompt scanning runs in-process through the Rust extension; the daemon does
not preload Prompt Scanner models or serve scan RPCs.

## V2 Policy CLI

The source-built Rust `agent-sec-cli` provides all 15 Policy, Scope and Binding CRUD
commands through `asc-daemon`. See the [command reference](../../docs/user-guide/en/agent-security/agent-sec-core/policy-cli.md)
and [V2 workspace](v2/README.md). PAP state is currently process-local; Binding
acceptance does not imply enforcement. For non-root development and E2E, configure
the daemon with `--policy-admin-uid <UID>`; default authorization remains root-only.

## Security Architecture

```
┌─────────────────────────────────────────────────────────┐
│   Agent hosts: cosh · OpenClaw · Hermes ·               │
│                Qwen Code · Qoder · Codex                │
├─────────────────────────────────────────────────────────┤
│   Hooks (per host): code-scanner · prompt-scanner ·     │
│           pii-checker · skill-ledger · observability    │
├──────────────────────────┬──────────────────────────────┤
│  agent-sec-cli           │  agent-sec-daemon            │
│  scan-prompt / scan-code │  health + SkillFS notify     │
│  scan-pii / skill-ledger │  security query RPC          │
│  harden / verify         │                              │
│  events / observability  │                              │
├──────────────────────────┴──────────────────────────────┤
│  Security Events (JSONL + SQLite)                       │
├─────────────────────────────────────────────────────────┤
│  linux-sandbox (bubblewrap + seccomp)                   │
├─────────────────────────────────────────────────────────┤
│  Linux Kernel · loongshield baseline                    │
└─────────────────────────────────────────────────────────┘
```

## Project Structure

```
agent-sec-core/
├── linux-sandbox/             # Rust sandbox executor (bubblewrap + seccomp)
│   ├── src/                   # Rust source (cli, policy, seccomp, bwrap_args, …)
│   ├── tests/                 # Rust integration tests
│   └── docs/                  # dev-guide, user-guide
├── agent-sec-cli/             # Unified CLI + security middleware (Python + Rust ext)
│   ├── src/agent_sec_cli/     # Main Python package
│   │   ├── cli.py             # CLI entry point (Typer)
│   │   ├── asset_verify/      # Skill GPG signature + hash verification
│   │   ├── code_scanner/      # Code scanning engine (regex + llm) and rules
│   │   ├── prompt_scanner/    # Prompt injection / jailbreak scanner
│   │   ├── pii_checker/       # PII and credential detection
│   │   ├── skill_ledger/      # Ed25519 integrity ledger and built-in scanners
│   │   ├── sandbox/           # Command classification + sandbox policy generation
│   │   ├── observability/     # Observability records, report, review TUI
│   │   ├── security_events/   # JSONL + SQLite event store
│   │   ├── security_middleware/ # Middleware layer + backends
│   │   ├── daemon/            # agent-sec-daemon server and client
│   │   ├── model_service/     # Local model backends (e.g. Ollama)
│   │   └── telemetry/         # Telemetry schema + writer
│   ├── dev-tools/             # Developer guides for extending backends
│   └── pyproject.toml         # Build configuration
├── cosh-extension/            # Copilot Shell hooks + sandbox guard
├── openclaw-plugin/           # OpenClaw plugin (TypeScript)
├── hermes-plugin/             # Hermes plugin (Python capabilities)
├── qwen-code-extension/       # Qwen Code hooks
├── qoder-plugin/              # Qoder CLI hooks
├── codex-plugin/              # Codex hooks
├── skills/                    # Bundled security scanning and audit skills
├── tools/                     # sign-skill.sh — PGP skill signing utility
├── packaging/                 # raw package build + systemd unit template
├── scripts/                   # CLI/daemon wrappers and CI helpers
├── docs/design/               # Design documents
├── tests/                     # Unit, integration, packaging, and e2e tests
├── .anolisa/component.toml    # ANOLISA component contract
├── LICENSE
├── Makefile
├── agent-sec-core.spec.in     # RPM packaging spec template
├── README.md
└── README_zh.md
```

Each of the six Agent hosts ships all five hook types (code-scanner,
prompt-scanner, pii-checker, skill-ledger, observability); the enforcement modes
each host supports differ — see [Agent Hook Environment Variables](#agent-hook-environment-variables).

Adapter-specific notes:
[OpenClaw](openclaw-plugin/README.md) ·
[Hermes](hermes-plugin/README.md) ·
[Codex](codex-plugin/README.md) ·
[Qwen Code](qwen-code-extension/README.md)

## Observability Hook Configuration

The OpenClaw, Hermes, cosh, Qwen Code, Qoder, and Codex integrations enable
their observability hooks by default. To disable them, set this variable before
starting the host:

```bash
export OBSERVABILITY_HOOK_ENABLED=false
```

The variable accepts only `true` / `false` (ignoring case and surrounding
whitespace). An unset or invalid value keeps the hook enabled. Restart the host
after changing it.

`OBSERVABILITY_TIMEOUT` sets the timeout in seconds for each local PII
redaction and observability record CLI call. The five non-Hermes integrations
default to `5` and use `5` for an unset, empty, invalid, or non-positive value.
Hermes instead falls back to its observability capability `timeout`, bounded to
at most `5`. Every integration caps a valid environment value above `5` at `5`.

For OpenClaw and Hermes, the existing observability capability `enabled` setting
remains an independent gate. Either switch can disable recording; setting this
variable to `true` does not re-enable a capability disabled in plugin configuration.

## Quick Start

### Prerequisites

| Component | Requirement |
|-----------|-------------|
| **OS** | Alibaba Cloud Linux / Anolis / RHEL family |
| **Permissions** | root or sudo (system-mode install) |
| **loongshield** | >= 1.2.0 (Security Baseline backend) |
| **gpg / gnupg2** | >= 2.0 (asset signature verification) |
| **Python** | 3.11.6 (pinned; the RPM requires `>= 3.11, < 3.12`) |
| **Rust** | >= 1.93 (for building `linux-sandbox` and the CLI native extension) |
| **bubblewrap** | required by `linux-sandbox` |
| **ANOLISA CLI** | >= 0.2.17 |

### Install AgentSecCore

Source and RPM installations support Linux x86_64 and aarch64. The published
ANOLISA raw package is limited to Linux x86_64 in system mode and requires CLI
version 0.2.17 or later. Update the CLI through its installation owner:

```bash
# CLI installed by get.agentic-os.sh
anolisa update self

# RPM-owned CLI
sudo anolisa update self

sudo anolisa --install-mode system install sec-core
sudo anolisa status sec-core
agent-sec-cli --version
```

`sec-core` is the ANOLISA component name. The RPM keeps the package name
`agent-sec-core`:

```bash
sudo yum install anolisa agent-sec-core
sudo anolisa --install-mode system adopt sec-core
```

Installing the CLI from YUM makes it available on sudo's system path. Adoption
records the directly installed RPM in system state so adapter commands can read
its component contract.

Developers building from source should use the repository-level entry point:

```bash
./scripts/build-all.sh --component sec-core
```

Before installing files, the source-build entry point checks Node.js 20 or
newer, bubblewrap, GnuPG, and `jq`. User mode reports all missing system
runtime packages with one install command and exits; install them and rerun the
same command. `--ignore-deps` bypasses this verification for pre-provisioned
hosts.

The source build installs runtime and integration resources in user paths but
does not register the component in ANOLISA state. Use the installed integration
scripts instead of `anolisa adapter enable`; see
[Source-build Integration](../../docs/user-guide/en/agent-security/agent-sec-core/QUICKSTART.md#source-build-integration).

An ANOLISA-managed raw package or adopted RPM places the framework adapters.
Enable one as the user who owns the target framework configuration:

```bash
anolisa adapter scan
anolisa adapter enable sec-core openclaw
```

Replace `openclaw` with `hermes`, `qwencode`, `cosh`, `codex`, or `qoder` for the
other packaged integrations.

### First Commands

```bash
# Security Baseline scan
agent-sec-cli harden --scan --config agentos_baseline

# Code scanning
agent-sec-cli scan-code --code 'rm -rf /' --language bash

# Prompt injection detection
agent-sec-cli scan-prompt --mode standard --text "ignore previous instructions"

# PII detection
agent-sec-cli scan-pii --text "contact alice@example.com" --source manual

# Verify release-signed skills in the default installation roots
agent-sec-cli verify

# Skill integrity check
agent-sec-cli skill-ledger check /path/to/skill

# Security posture summary for the last 24 hours
agent-sec-cli events --summary
```

Full CLI reference and per-host integration steps:
[AgentSecCore User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/QUICKSTART.md).

### Asset Verification

`agent-sec-cli verify` checks GPG-signed distribution manifests and SHA-256
file coverage. It is separate from Skill Ledger: `verify` validates release or
deployment signatures, while `skill-ledger` maintains the local Ed25519 runtime
integrity history.

Batch verification discovers immediate, non-hidden Skill directories from two
optional system roots: `/usr/share/anolisa/skills` for RPM installations and
`/usr/local/share/anolisa/skills` for standard ANOLISA raw installations.
Missing or empty roots are skipped, and canonical duplicate roots are scanned
once. A completed scan reports `verified` when at least one candidate passes and
none fail, `failed` when any candidate fails, or `no_candidates` when no
candidate is found. `no_candidates` exits successfully but does not claim that
an asset was verified.

The two roots are fixed package defaults and are not derived from an arbitrary
installation prefix. Use `agent-sec-cli verify --skill /path/to/skill` for a
relocated or custom Skill. Configuration, trust-key, and root-enumeration errors
remain operation failures; candidate signature, manifest, hash, unexpected-file,
or access failures produce the `failed` outcome.

Details: [Asset Verification User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/asset-verification.md).

## Prompt Scanner

Detects prompt injection and jailbreak attempts. `--mode` selects the detection
strength:

| Mode | Layers |
|------|--------|
| `fast` | L1 rule engine only |
| `standard` | L1 + L2 ML classifier (default) |
| `strict` | L1 + L2 (L3 reserved) |
| `multi_turn` | L4 multi-turn intent detection; reads a JSON payload from stdin |

```bash
agent-sec-cli scan-prompt --text "ignore all system instructions"
agent-sec-cli scan-prompt --mode fast --text "user input"
agent-sec-cli scan-prompt --input prompts.txt --format json

# Pull the default L2 model once after install
ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF

# Verify that Ollama can serve the required model
agent-sec-cli scan-prompt warmup
```

The L2 classifier defaults to
`modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF` from ModelScope;
`modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF` is an optional backend selected
with `--model` or `PROMPT_SCANNER_L2_MODEL` (`--model` wins). Only one backend
runs at a time and each needs its own `ollama pull`. `warmup` checks that Ollama
can serve the selected model; it never downloads models automatically.

Details: [Prompt Scanner User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/prompt-scanner.md).

## Code Scanner

Scans bash and python source for dangerous operations. In the V2 RPM,
`scan-code` is a Rust daemon client: it requires an already-running
`agent-sec-daemon` and uses `AGENT_SEC_DAEMON_SOCKET` unless an explicit
`--socket` overrides it. The CLI never starts a daemon or falls back to Python.

```bash
# The deployment supplies the daemon endpoint.
export AGENT_SEC_DAEMON_SOCKET=/run/agent-sec-core/daemon.sock

# Regex engine (the only V2 scanning engine currently available)
agent-sec-cli scan-code --code 'rm -rf /'
agent-sec-cli --socket /run/agent-sec-core/daemon.sock \
  scan-code --code 'import os; os.system("rm -rf /")' --language python
```

The verdict enum is `pass` / `warn` / `deny` / `error`; built-in rules currently
produce `warn` or `pass`. Rules are embedded in the V2 binary rather than read
from a Python source-tree directory. `--mode llm` remains accepted for CLI
compatibility but returns `LLM model not available`; `--trace-context` and
code-scan telemetry are not yet available in V2, so hooks requiring them remain
deferred.

Full daemon endpoint, CLI, and host-hook status:
[Code Scanner User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/code-scanner.md).

## PII Checker

Detects personal data and credentials, and can emit redacted text.

The bundled [pii-checker Skill](skills/pii-checker/SKILL.md) lets an Agent scan
specified text or files and generate redacted text. V2 uses the Rust daemon:
set `AGENT_SEC_DAEMON_SOCKET` or pass top-level `--socket`; the CLI never starts it automatically.

```bash
agent-sec-cli scan-pii --text "contact alice@example.com" --source manual
echo "my key is AKID1234567890" | agent-sec-cli scan-pii --stdin --format json
agent-sec-cli scan-pii --text "card 4111111111111111" --redact-output
agent-sec-cli scan-pii --input ./sample.log --include-low-confidence
```

V2 loads custom business rules from `/etc/agent-sec/pii-checker/rules.yaml` at daemon startup;
`agent-sec-daemon --pii-rules /absolute/path/rules.yaml` selects an alternative. Restart to update.
V1 retains `~/.config/agent-sec/pii-checker/rules.yaml`. V2 reports coverage, rule identity and `summary.scanner_version: "2.0.0"`;
a `pass` result with partial coverage does not establish complete detection. This phase preserves
Hook contracts without switching live hosts or adding PDP enforcement.

Details: [PII Checker User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/pii-checker.md).

## Skill Ledger

Ed25519-based integrity ledger for skill directories. Tracks file hashes, version chains, and scan results in `.skill-meta/` manifests — all managed via the `agent-sec-cli skill-ledger` subcommand.
For an existing manifest, authenticity is verified before file drift; an unsigned existing manifest is reported as `tampered`.

The six integrity states are `pass` / `none` / `drifted` / `warn` / `deny` /
`tampered`.

`scan` and the default `init` baseline are signed-ledger write paths; use
`analyze` for read-only content findings. In batch mode, a host-backed packaged
Skill under `/usr/share/anolisa/skills/` or `/usr/local/share/anolisa/skills/`
whose ledger state is read-only is reported as `status=skipped`,
`reasonCode=readonly_system_skill`, `persisted=false`. Read-only host Skills directly
under `$XDG_DATA_HOME/anolisa/skills/` (default `~/.local/share/anolisa/skills/`)
are also skipped with `reasonCode=readonly_default_skill` unless covered by
`managedSkillDirs`; managed user Skills retain write errors. This operational skip is
not a `pass` result or an attestation. An explicit `scan <dir>` remains an error.
When a skipped Skill has no prior ledger artifacts, `check` and `status`
continue to report `none` / `unscanned`; neither value means `pass`.

### Key Commands

| Command | Description |
|---------|-------------|
| `init` | Initialize keys and quick-scan covered skills |
| `analyze <dir> --format json` | Read-only content analysis without creating or updating ledger state |
| `scan <dir>` | Run built-in quick scanners and sign the manifest |
| `check <dir>` | Detect drift / tampering against the manifest |
| `show <dir>` | Show latest/active exposure summary, user decision, warnings, and findings |
| `export <dir> --version latest --output <path>` | Export a signed snapshot, manifest, and findings for review |
| `decide <dir> --action allow\|always_allow\|block\|rollback` | Record a user decision and refresh activation |
| `certify <dir> --findings <file>` | Import external scanner findings and sign the manifest |
| `list-scanners` | List registered built-in scanners |
| `status` | System-wide health overview (keys, config, aggregate integrity) |
| `audit <dir>` | Show version history and signature chain |
| `check --all` / `scan --all` | Batch mode across all registered skill dirs |

`init --no-baseline` initializes keys without scanning Skills. `rotate-keys` is
visible in help and reports that it is not implemented (exit 1, no key changes).

### Quick Example

```bash
# Initialize keys and baseline covered skills
agent-sec-cli skill-ledger init

# Check integrity without modifying ledger metadata
agent-sec-cli skill-ledger check /path/to/skill

# Analyze current content without keys, manifests, signatures, or events
agent-sec-cli skill-ledger analyze /path/to/skill --format json

# Inspect runtime exposure and user-decision state
agent-sec-cli skill-ledger show /path/to/skill

# Export a hidden latest version for review, then decide
agent-sec-cli skill-ledger export /path/to/skill --version latest --output /tmp/skill-review
agent-sec-cli skill-ledger decide /path/to/skill --action allow --reason "reviewed manually"

# Quick scan, create/update a signed version, and snapshot
agent-sec-cli skill-ledger scan /path/to/skill

# System health overview
agent-sec-cli skill-ledger status
```

### SkillFS Peer Authentication

Skill Ledger can authenticate both directions of its SkillFS integration with
HMAC-SHA256. The agent-sec-core side uses these environment variables:

| Variable | Purpose |
|----------|---------|
| `AGENT_SEC_SKILLFS_CONTROL_SOCKET` | Override the SkillFS control socket queried by the Ledger resolver |
| `AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE` | Authenticate resolver requests and responses on the control socket |
| `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` | Authenticate SkillFS change notifications received by the daemon |

Without a control authentication key, a missing control socket (`ENOENT`) keeps
the legacy host-path fallback. Once a control key is configured, a missing
socket, connection failure, or authentication failure is fail-closed and never
falls back to the host path or plaintext. Configuring the notify key similarly
requires HMAC for `skill_ledger.skillfs_notify_change`; other daemon methods
remain compatible with their existing plaintext protocol.

Authentication key paths must be absolute and refer to regular, non-symlink
files owned by the effective user, with no group or other permission bits. The
raw key file must contain 32–4096 bytes. See the Skill Ledger user guide for the
full two-key deployment and container volume requirements.

The bundled Qoder CLI plugin registers a `PreToolUse` hook for the `Skill`
tool. It resolves user Skills from `~/.qoder/skills/` before project Skills
from `<cwd>/.qoder/skills/`, runs a read-only `skill-ledger check`, and applies the
`SKILL_LEDGER_MODE=observe|warn|ask|block` policy (default: `ask`). Set
`SKILL_LEDGER_HOOK_ENABLED=false` to bypass the hook. The legacy `debug` value is an
alias for `observe`, while `deny` is an alias for `block`. Each
check carries Qoder trace identifiers into the security audit log.

Design doc: [`docs/design/SKILL_LEDGER_zh.md`](docs/design/SKILL_LEDGER_zh.md) · User guide: [Skill Ledger User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/skill-ledger.md)

## Agent Capability View

`agent-sec-cli capabilities` shows the hook capability view derived from environment variables visible to the current CLI process across Qoder, Qwen Code, Codex, Cosh, OpenClaw, and Hermes.

The command does not read OpenClaw, Hermes, or other Agent configuration files, and it does not resolve Agent home directories. Run it from the same shell/container/service environment that starts the target Agent when you want the closest approximation, but treat the output as an environment-variable view only: it does not prove that hooks are loaded, registered, or currently effective in the target Agent process. Agent config values such as enabled flags, policies, and timeouts can still make runtime behavior differ from this view.

```bash
# All agents and all hook capabilities
agent-sec-cli capabilities

# Filter by agent
agent-sec-cli capabilities --agent openclaw

# Filter by capability
agent-sec-cli capabilities --capability code-scan

# Filter by agent and capability, with machine-readable output
agent-sec-cli capabilities --agent hermes --capability pii-check --output json
```

Supported capability names are fixed: `code-scan`, `prompt-scan`, `pii-check`, `skill-ledger`, and `observability`. Plugin-specific IDs such as `scan-code` or `pii-scan-user-input` are not accepted as CLI filters.

For `observability`, the view applies `OBSERVABILITY_TIMEOUT` consistently across all six integrations: it defaults to `5` seconds, falls back to `5` for invalid or non-positive values, and caps larger values at `5`. A lower Hermes timeout from plugin configuration remains outside this environment-only view.

Table output is limited to `CAPABILITY`, `ENABLED`, `MODE`, `SCAN_MODE`, `TIMEOUT(s)`, and `DIAGNOSTICS`. JSON output keeps the same user-facing fields and sanitized `env` entries containing only `effective` and `default` values. Neither output format exposes hook matcher lists, source labels, Agent config contents, config paths, or raw environment variable values. Diagnostics identify the invalid setting and fallback behavior without echoing the original value.

For `prompt-scan`, the `env` entries also report `PROMPT_SCANNER_L2_MODEL`: no hook reads it itself, but each one shells out to `scan-prompt`, which resolves the L2 backend, so all six integrations inherit it. It is reported case-preserved (escaped and length-capped) because a model name is only meaningful verbatim, and both the reported `default` and the unsupported-backend check come from the native scanner engine instead of a second copy of the backend list. An unsupported name is reported as configured plus a diagnostic, since the engine rejects it at construction and the scan fails. There is no table column for it; read it with `--capability prompt-scan --output json`.

## Security Baseline

`agent-sec-cli harden` wraps `loongshield seharden` and defaults to
`--scan --config agentos_baseline` when no action or profile is given.

```bash
# Compliance scan
agent-sec-cli harden --scan --config agentos_baseline

# Preview remediation
agent-sec-cli harden --reinforce --dry-run --config agentos_baseline

# Execute remediation (requires root)
sudo agent-sec-cli harden --reinforce --config agentos_baseline

# Full downstream loongshield help
agent-sec-cli harden --downstream-help
```

## Observability

```bash
# Interactive drill-down TUI (requires an interactive terminal)
agent-sec-cli observability review

# Per-session debrief report
agent-sec-cli observability report --last
agent-sec-cli observability report --session-id <id> --format json

# Public observability record JSON Schema
agent-sec-cli observability schema
```

Details: [Observability User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/QUICKSTART.md#observability).

## Security Events

Security events are written both as JSONL and into a SQLite store. Query the store
with `agent-sec-cli events`:

```bash
agent-sec-cli events --last-hours 24
agent-sec-cli events --category prompt_scan --output json
agent-sec-cli events --count-by category --last-hours 24
agent-sec-cli events --summary
```

Details: [Security Events User Guide](../../docs/user-guide/en/agent-security/agent-sec-core/QUICKSTART.md#security-events).

## Agent Hook Environment Variables

The host hook matrix is maintained in the user guide to keep one authoritative
source for environment variables and host-specific mode semantics:
[Agent Hook Environment Variables](../../docs/user-guide/en/agent-security/agent-sec-core/QUICKSTART.md#agent-hook-environment-variables).

## Development

```bash
# Build everything (sandbox, CLI wheel, all adapters, skills, component manifest)
make build-all

# Individual targets
make build-sandbox
make build-cli

# Tests
make test               # Python + Rust sandbox + OpenClaw plugin
make test-python
make test-rust
make test-openclaw-plugin

# Lint and formatting
make python-lint
make python-code-pretty

# List all targets
make help
```

## License

Apache License 2.0 — see [LICENSE](../../LICENSE) for details.

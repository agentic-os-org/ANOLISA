# Agent Security Core CLI

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Python](https://img.shields.io/badge/Python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Version](https://img.shields.io/badge/version-0.3.0-green.svg)](../CHANGELOG.md)

**Agent Security Core CLI** is a comprehensive security toolkit for AI Agents, providing system hardening, sandbox isolation, asset integrity verification, and security event tracking.

---

## Features

### 🔒 System Hardening
- Security baseline scanning and assessment
- Automated reinforcement with configurable baselines
- Dry-run mode for safe testing
- Integration with LoongShield security framework

### 🏖️ Sandbox Isolation
- Command classification and risk assessment
- Dynamic sandbox policy generation
- Integration with bubblewrap for process isolation
- Fine-grained resource and access control

### ✅ Asset Integrity Verification
- GPG-signed skill manifests
- SHA-256 hash verification for all files
- Trusted key management
- Batch verification for multiple skills

### 📊 Security Event Tracking
- JSONL-based event logging
- Thread-safe logging with rotation detection
- Time-range based event aggregation
- Multiple output formats (text, JSON)

### 📈 Observability Record Ingestion
- Typed agent hook record validation
- Independent `observability.jsonl` stream
- Forward-compatible unknown field and metric filtering
- JSON Schema output for producers

---

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/alibaba/anolisa.git
cd anolisa/src/agent-sec-core/agent-sec-cli

# Install in development mode (uv manages .venv automatically)
uv sync

# Or build the wheel
uv run maturin build --release
```

### From RPM Package

```bash
# Build RPM (from parent directory)
cd ..
make rpm

# Install RPM
sudo rpm -i agent-sec-core-0.3.0-1.el8.x86_64.rpm
```

### Dependencies

**Required:**
- Python 3.10+
- [uv](https://docs.astral.sh/uv/) (Python package manager)
- GnuPG 2.0+

**Optional:**
- `pgpy >= 0.5` - Pure Python PGP implementation (faster verification)
- `bubblewrap` - Sandbox isolation backend

---

## Usage

### Command-Line Interface

After installation, use the `agent-sec-cli` command:

```bash
# System hardening
agent-sec-cli harden --scan --config agentos_baseline
agent-sec-cli harden --reinforce --config agentos_baseline
agent-sec-cli harden --reinforce --dry-run --config agentos_baseline

# Skill integrity verification
agent-sec-cli verify
agent-sec-cli verify --skill /path/to/skill

# Security event summary
agent-sec-cli summary --hours 24 --format text
agent-sec-cli summary --hours 72 --format json

# Observability record ingestion
agent-sec-cli observability record --format json --stdin < record.json
agent-sec-cli observability schema

# Agent plugin capability configuration view
agent-sec-cli capabilities
agent-sec-cli capabilities --agent openclaw --capability code-scan --output json
```

### Agent Plugin Capability View

`agent-sec-cli capabilities` prints the hook capability view derived from environment variables visible to the current CLI process. It does not read OpenClaw, Hermes, or other Agent configuration files, and it does not resolve Agent home directories.

Run it from the same shell/container/service environment that starts the target Agent when you want the closest approximation. Even then, the output is not proof that hooks are loaded, registered, or currently effective in the target Agent process; Agent config values such as enabled flags, policies, and timeouts can still make runtime behavior differ from this view.

Supported capability filters are fixed to `code-scan`, `prompt-scan`, `pii-check`, `skill-ledger`, and `observability`; plugin-internal IDs are not accepted as aliases.

For `observability`, the view applies the shared `OBSERVABILITY_TIMEOUT` environment semantics used by all six integrations: the default is `5` seconds, invalid or non-positive values fall back to `5`, and larger values are capped at `5`. Hermes configuration can still select a lower runtime timeout when the environment variable is absent, which remains outside this environment-only view.

Table output is limited to the stable user-facing columns `CAPABILITY`, `ENABLED`, `MODE`, `SCAN_MODE`, `TIMEOUT(s)`, and `DIAGNOSTICS`. JSON output uses the same user-facing fields plus sanitized `env` entries with `effective` and `default` values. Neither format exposes hook matcher lists, source labels, Agent config contents, config paths, or raw environment variable values. Diagnostics name the invalid setting and fallback behavior without echoing the original value.

For `prompt-scan`, the `env` entries also carry `PROMPT_SCANNER_L2_MODEL`, the L2 backend shared by all six integrations: no hook reads it itself, but each one shells out to `scan-prompt`, which resolves it. Because a model name is only meaningful verbatim, it is the one entry reported case-preserved (escaped and length-capped) instead of as a normalized keyword. The reported `default` comes from the native scanner engine (`scanner_engine_info`), so the view never carries a second copy of the backend list; before the extension is built it degrades to an empty default. A backend the engine does not support is reported as configured plus a diagnostic rather than replaced by the default, because the engine rejects it at construction and the scan then fails. It has no table column, so use `--capability prompt-scan --output json` to read it.

### Observability Records

`agent-sec-cli observability record` accepts one JSON object from stdin and writes
validated hook telemetry to the independent `observability.jsonl` stream plus an
internal `observability.db` SQLite index. The SQLite index is an implementation
detail for retention and future local reads; this command does not expose a
public query API.

Required wire fields:

- `hook`
- `observedAt` as a timezone-aware timestamp
- `metadata.sessionId`
- `metadata.runId`
- `metrics`

Hook-specific metadata:

- `metadata.callId` is optional on model and tool call records.
- `metadata.toolCallId` is required on `before_tool_call` and `after_tool_call`.

Unknown top-level fields, metadata fields, and metric keys are ignored for
forward compatibility. A record is rejected when no supported metric remains
after filtering. The command is silent on success and exits non-zero if parsing,
validation, or persistence fails.

Current supported hooks:

- `before_agent_run`
- `before_llm_call`
- `after_llm_call`
- `before_tool_call`
- `after_tool_call`
- `after_agent_run`

### Python API

```python
from agent_sec_cli.security_middleware import invoke

# System hardening
result = invoke("harden", args=["--scan", "--config", "agentos_baseline"])
print(result.success)

# Verify a specific skill
result = invoke("verify", skill="/path/to/skill")
if result.success:
    print("Verification passed!")
else:
    print(f"Verification failed: {result.error}")

# Get security event summary
result = invoke("summary", hours=24, format="json")
print(result.stdout)
```

---

## Architecture

```
agent_sec_cli/
├── cli.py                      # Unified CLI entry point
├── asset_verify/               # Integrity verification
│   ├── verifier.py            # Main verification logic
│   ├── errors.py              # Custom exception types
│   ├── config.conf            # Configuration file
│   └── trusted-keys/          # Trusted GPG public keys
├── sandbox/                    # Sandbox policy generation
│   ├── sandbox_policy.py      # Policy generation
│   ├── classify_command.py    # Command classification
│   └── rules.py               # Security rules
├── security_events/            # Event logging
│   ├── writer.py              # JSONL event writer
│   ├── schema.py              # Event schema definitions
│   └── config.py              # Logging configuration
└── security_middleware/        # Unified middleware layer
    ├── __init__.py            # Main entry point (invoke)
    ├── router.py              # Action routing
    ├── lifecycle.py           # Pre/post hooks
    ├── context.py             # Request context
    ├── result.py              # Result wrapper
    └── backends/              # Backend implementations
        ├── hardening.py       # System hardening backend
        ├── sandbox.py         # Sandbox backend
        ├── asset_verify.py    # Verification backend
        ├── summary.py         # Event summary backend
        ├── code_scan.py       # Code scanner backend
        ├── prompt_scan.py     # Rust-native prompt scanner adapter
        ├── pii_scan.py        # PII scanner backend
        └── skill_ledger.py    # Skill Ledger command backend
```

---

## Development

### Setup Development Environment

```bash
# Clone and install all dependencies (dev included by default)
cd agent-sec-cli && uv sync

# Run tests (from agent-sec-core directory)
make test-python

# Format code
uv run black src/
uv run isort src/
```

### Running Tests

```bash
# Unit tests
uv run --project agent-sec-cli pytest tests/unit-test/

# Integration tests
uv run --project agent-sec-cli pytest tests/integration-test/

# All tests with coverage
uv run --project agent-sec-cli pytest --cov=agent_sec_cli tests/
```

### Building from Source

```bash
# Build wheel (maturin + Rust extension)
uv run maturin build --release

# Output:
# target/wheels/
#   └── agent_sec_cli-0.3.0-cp312-cp312-linux_x86_64.whl
```

---

## Configuration

### Asset Verification

The packaged `asset_verify/config.conf` contains both supported system discovery roots:

```ini
skills_dir = [
    /usr/share/anolisa/skills
    /usr/local/share/anolisa/skills
]
```

The first path is used by RPM installations; the second is used by standard ANOLISA raw
installations. Both are optional. Missing or empty roots are skipped, and roots that resolve to the
same canonical path are scanned once. The defaults are not derived from a custom installation
prefix; use `agent-sec-cli verify --skill /path/to/skill` for a relocated Skill.

Normal runs report `verified` when at least one candidate passes and none fail, `failed` when any
candidate fails, or `no_candidates` when discovery completes without finding a candidate.
`no_candidates` exits `0` but does not claim that an asset was verified. Configuration, trusted-key,
canonicalization, and root-enumeration errors exit `1` as operation failures and may omit the
outcome; the CLI writes those operation errors to standard error. Completed runs print
`CHECKED`/`PASSED`/`FAILED` counts and finish with `VERIFICATION PASSED`, `VERIFICATION FAILED`, or
`VERIFICATION SKIPPED: NO CANDIDATE SKILLS`.

Full behavior and topology reference:
[Asset Verification User Guide](../../../docs/user-guide/en/agent-security/agent-sec-core/asset-verification.md).

### Security Events

Event logging configuration is managed in `security_events/config.py`:

```python
LOG_FILE = "/var/log/agent-sec/security-events.jsonl"
MAX_FILE_SIZE = 10 * 1024 * 1024  # 10 MB
ROTATION_COUNT = 5
```

### Observability

Observability records use the same data directory resolver as security events,
but write to separate files:

- default system path: `/var/log/agent-sec/observability.jsonl`
- default SQLite index: `/var/log/agent-sec/observability.db`
- user fallback: `~/.agent-sec-core/observability.jsonl`
- user SQLite fallback: `~/.agent-sec-core/observability.db`
- test/dev override: `AGENT_SEC_DATA_DIR=/path/to/dir`

The observability stream uses its own JSONL file, lock file, SQLite database,
rotation limit, backup count, and 7-day SQLite retention policy; it does not
write to `security-events.jsonl` or `security-events.db`.

### Local JSONL File Permissions

The local `security-events.jsonl`, `observability.jsonl`, and `cli.jsonl`
writers create active data files and their advisory lock files with mode
`0600`, independently of the process umask. Existing active data and lock
files are tightened to `0600` when a writer opens them. On the first write,
recognized timestamped backups retained from older releases are also tightened
to `0600`; backups created by the current writer inherit the tightened mode.

Unprivileged direct readers must run as the file-owning user. Operators with
existing group/other read workflows should move those readers to the owning
user or a controlled export path instead of broadening the source log
permissions.

---

## Security

### Signing Skills

```bash
# Sign a single skill
sign-skill.sh /path/to/skill

# Sign all skills in batch
sign-skill.sh --batch /usr/share/anolisa/skills --force

# Standard ANOLISA raw installation root
sign-skill.sh --batch /usr/local/share/anolisa/skills --force
```

### Verifying Skills

```bash
# Verify all configured skills
agent-sec-cli verify

# Verify one Skill without default-root discovery
agent-sec-cli verify --skill /path/to/skill
```

Batch discovery treats each immediate, non-hidden child directory as a candidate. Candidate
manifest, signature, hash, unexpected-file, and access failures produce the `failed` outcome and
exit `1`. An existing discovery root that is not a directory or cannot be enumerated is an operation
error instead. An explicit `--skill` always represents one candidate, so a nonexistent, non-directory,
unreadable, or invalid path is `failed` with exit `1`, never `no_candidates`. Completed runs expose
`seccore.asset_outcome = verified|failed|no_candidates` to the sanitized telemetry projection;
paths are not uploaded.

---

## Troubleshooting

### Common Issues

**Issue:** `Verification failed: No trusted keys found`
- **Solution:** Add trusted GPG keys to `asset_verify/trusted-keys/`

**Issue:** `Permission denied` errors during hardening
- **Solution:** Run with sudo: `sudo agent-sec-cli harden --reinforce --config agentos_baseline`

### Debug Mode

Enable verbose output:

```bash
python -m agent_sec_cli.cli harden --scan --config agentos_baseline 2>&1 | tee debug.log
```

---

## Extending with dev-tools

The `dev-tools/` directory contains developer guides and skills for adding new security capabilities to agent-sec-cli.

### Quick Start: Add a New Security Command

Follow the step-by-step guide in
[dev-tools/backend-skill/SKILL.md](dev-tools/backend-skill/SKILL.md) to:

1. **Add a CLI subcommand** - Define new command-line interface
2. **Register a router** - Map action names to backend modules
3. **Create a backend** - Implement security logic in Python or Rust
4. **Integrate event logging** - Automatic security event tracking

### Architecture Overview

```
New Security Capability
├── CLI Layer (cli.py)
│   └── Add subcommand with argparse
├── Router Layer (router.py)
│   └── Register action → backend mapping
├── Backend Layer (backends/)
│   ├── Python backend (.py) — delegates to Python module
│   └── Rust backend (.py) — delegates to Rust PyO3 extension
└── Event Logging (security_events/)
    └── Automatic JSONL event recording
```

### Example: Adding a New Backend

**Python Backend:**
```
Use backend-skill in folder dev-tools to create a new python backend called my_scanner with module_path agent_sec_cli.my_scanner.analyzer
```

**Rust Backend:**
```
Use backend-skill in folder dev-tools to create a new rust backend called crypto_verify
```

### Development Resources

| Resource | Location | Purpose |
|----------|----------|---------|
| Extension Guide | `dev-tools/backend-skill/SKILL.md` | Step-by-step tutorial for Rust and Python backends |
| Backend Templates | `dev-tools/backend-skill/templates/` | Python and Rust backend templates |
| Backend Examples | `src/agent_sec_cli/security_middleware/backends/` | Reference implementations |
| CLI Structure | `src/agent_sec_cli/cli.py` | Subcommand patterns |
| Event Schema | `src/agent_sec_cli/security_events/schema.py` | Logging format |

---

## Contributing

We welcome contributions! Please see our [Contributing Guide](https://github.com/alibaba/anolisa/blob/main/CONTRIBUTING.md) for details.

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

---

## License

This project is licensed under the Apache License 2.0 - see the [LICENSE](../LICENSE) file for details.

---

## Acknowledgments

- Part of the [ANOLISA](https://github.com/alibaba/anolisa) project
- Developed by Alibaba Cloud and the open-source community
- Inspired by security best practices for AI Agent platforms

---

## Support

- **Issues:** [GitHub Issues](https://github.com/alibaba/anolisa/issues)
- **Discussions:** [GitHub Discussions](https://github.com/alibaba/anolisa/discussions)
- **Email:** [anolisa@lists.openanolis.cn](mailto:anolisa@lists.openanolis.cn)

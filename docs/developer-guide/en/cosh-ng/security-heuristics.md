# Security Heuristics

[中文版](../../zh/cosh-ng/security-heuristics.md)

## Overview

The cosh-ng audit subsystem implements a PEP→PDP→Log three-stage security decision pipeline. Each command undergoes structured parsing, policy matching, and logging before execution, resulting in one of three dispositions: Allow / Deny / RequireApproval.

## Architecture

```
Raw command string
     │
     ▼
┌─────────────────┐
│ action parser   │  Rejects shell metacharacters, control bytes
│ (PEP boundary)  │  Structures into Action{subsystem,operation,target,args}
└────────┬────────┘
         │ Parse failure → immediate Deny
         ▼
┌─────────────────┐
│ evaluate (PDP)  │  Iterates policy.rules[], first match wins
│                 │  No match → policy.default
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ audit log       │  Redacts then writes to JSONL log
│ (redact + log)  │  CallerInfo: session/user/uid/pid
└─────────────────┘
```

Code located in `crates/cosh-platform/src/audit/`.

## Command Parsing (action parser)

Source file: `audit/action.rs`

The parser rejects dangerous input before PDP:

| Check | Rejection Condition | Reason |
|-------|-------------------|--------|
| Empty string | Empty after `trim()` | No valid operation |
| Control bytes | Contains `\n` or `\r` | Prevents command injection |
| Shell metacharacters | Contains any of `;|&><$\`(){}` | Prevents command chaining/redirection/subshell |

On parse failure, callers should map to `Outcome::Deny` (never auto-allow).

After successful parsing, structure is determined by the first token:
- `pkg` / `svc` / `checkpoint` / `cosh` → Structured subsystem (operation=tokens[1], target=tokens[2])
- Others → Shell subsystem (operation=first token, target=second token, args=tokens[1..])

## Policy System

Source files: `audit/policy.rs`, `audit/builtin.rs`

### Policy Loading Priority

1. File specified by `$COSH_AUDIT_POLICY` environment variable
2. `~/.copilot-shell/cosh/audit.toml` (user-level)
3. `/etc/cosh/audit.toml` (system-level)
4. Built-in `balanced` preset (factory default)

Only the first existing source is used; no cross-file merging.

### Built-in Presets

| Preset | Default Outcome | Use Case |
|--------|----------------|----------|
| `permissive` | Allow | Sandbox / CI environments |
| `balanced` | RequireApproval | Daily development (default) |
| `strict` | Deny | Production / untrusted agents |

### Policy File Format (TOML)

```toml
version = "v1"
default = "RequireApproval"   # Allow / Deny / RequireApproval

[[rules]]
name = "allow-readonly"
matches.subsystem = "shell"
matches.operation = { one_of = ["ls", "cat", "ps", "df", "echo", "uptime"] }
outcome = "Allow"

[[rules]]
name = "deny-destructive"
matches.subsystem = "shell"
matches.operation = { one_of = ["rm", "sudo", "shutdown", "dd", "mkfs", "tee"] }
outcome = "Deny"
reason = "destructive command blocked by policy"
```

### Match Syntax (StringMatch)

| Form | Example | Description |
|------|---------|-------------|
| Exact match | `"install"` | String equality |
| Enum match | `{ one_of = ["start", "restart", "stop"] }` | Any one matches |
| Glob match | `{ glob = "-i*" }` | Supports `*` and `?` |

Match block supports fields: `subsystem`, `operation`, `target`, `arg[].key`, `arg[].value`

## Decision Engine (evaluate)

Source file: `audit/evaluate.rs`

- Iterates `policy.rules[]`; the first matching rule determines the outcome
- Falls back to `policy.default` when no rules match
- Returns `Decision { outcome, reason, matched_rule, policy_version }`
- `policy_version` includes source identifier + SHA256 hash for audit traceability

## Balanced Preset Core Rules

### Allow

| Category | Example Commands |
|----------|-----------------|
| Read-only atomic commands | `uptime`, `ls -la`, `cat`, `ps aux`, `df -h`, `echo` |
| Git read-only | `git status`, `git log`, `git diff`, `git show`, `git blame` |
| Git branch viewing | `git branch`, `git branch -v` |
| Git stash viewing | `git stash`, `git stash list`, `git stash show` |
| Safe tool pairs | `systemctl status`, `apt list`, `dnf list`, `docker ps` |
| pkg/svc read-only | `pkg search`, `pkg list`, `svc status`, `svc list` |
| checkpoint read-only | `checkpoint list`, `checkpoint status` |

### Deny

| Category | Example Commands |
|----------|-----------------|
| Destructive commands | `rm -rf /`, `sudo`, `shutdown`, `dd`, `mkfs`, `tee` |
| Git mutations | `git push`, `git reset --hard`, `git clean`, `git rebase` |
| Git branch mutations | `git branch -D`, `git branch -m`, `git branch --delete` |
| Git stash mutations | `git stash drop`, `git stash clear`, `git stash pop/apply` |
| sed in-place edits | `sed -i`, `sed --in-place` |
| find destructive | `find . -delete`, `find . -fprint` |

### RequireApproval

| Category | Example Commands |
|----------|-----------------|
| Package management writes | `pkg install`, `pkg remove` |
| Service management writes | `svc start`, `svc restart` |
| Checkpoint writes | `checkpoint create`, `checkpoint restore` |
| Unknown commands | Commands not matching any allow/deny rule |

## Logging and Redaction

Source files: `audit/log.rs`, `audit/redact.rs`

### Redaction Rules

Automatic redaction before writing to log:

| Detection Method | Trigger Condition | Replacement |
|-----------------|-------------------|-------------|
| Sensitive key | args key contains `password/secret/token/api_key/apikey` | `<redacted>` |
| PEM content | raw field contains PEM header like `BEGIN PRIVATE KEY` | `<redacted-pem>` |

Redaction occurs at log-write time (not during PDP), ensuring PDP can make decisions based on original values.

### Log Entry Fields

```json
{
  "timestamp": "2025-01-01T00:00:00Z",
  "session_id": "p1234-t1704067200",
  "user": "admin",
  "uid": 1000,
  "euid": 1000,
  "sudo_user": null,
  "pid": 1234,
  "action": { "subsystem": "pkg", "operation": "install", ... },
  "decision": { "outcome": "RequireApproval", "reason": "...", ... },
  "source": "Cli",
  "redacted": false
}
```

Log path is overridable via `$COSH_AUDIT_LOG` environment variable (for testing).

## Public API

| Function | Purpose |
|----------|---------|
| `audit::check(action, source, &loaded)` | Full PEP→PDP→Log pipeline |
| `audit::classify(action, &loaded)` | PDP only, no logging (for TUI real-time classification) |
| `audit::record_decision(action, &decision, source)` | Record an already-made decision (e.g., Deny from parse failure) |
| `audit::evaluate(action, &loaded)` | Pure PDP function |
| `parse_action_string(raw)` | Raw string → Action |
| `LoadedPolicy::load()` | Load the active policy |

## Test Verification

```bash
cd src/cosh-ng

# Audit policy matching tests (balanced preset allow/deny/approve coverage)
cargo test --locked -p cosh-platform -- audit

# Action parser tests
cargo test --locked -p cosh-platform -- action

# Policy loading and validation tests
cargo test --locked -p cosh-platform -- policy

# Redaction tests
cargo test --locked -p cosh-platform -- redact
```

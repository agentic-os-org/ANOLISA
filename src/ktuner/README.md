# ktuner — deterministic kernel-tuning engine

[中文版](README_zh.md)

Agent-facing kernel parameter tuning engine for ANOLISA. Evaluates 207 rules against the running system and outputs structured JSON recommendations. Designed to be called by cosh/agent via `ktuner <command> [options]`.

## Usage

```bash
# Diagnose — output score + recommendations
ktuner check
ktuner check --category net
ktuner check --conservative    # high-confidence only

# Apply recommendations (requires root)
sudo ktuner tune --dry-run     # preview, no changes
sudo ktuner tune               # apply all
sudo ktuner tune --conservative

# Fix a single parameter (requires root)
sudo ktuner fix <param>        # e.g. sudo ktuner fix vm.swappiness

# Explain why a parameter should change
ktuner why <param>             # e.g. ktuner why net.core.somaxconn

# Undo all changes (requires root)
sudo ktuner rollback
```

## JSON output

All output goes to **stdout as JSON**. Errors go to **stderr as JSON**. No ANSI colors, no progress bars, no human-formatted text on stdout.

### Exit codes

| Code | Meaning |
|------|---------|
| 0    | Success (check: system already optimal; tune/fix/rollback: applied OK) |
| 1    | check: has recommendations (not an error, system can be improved) |
| 2    | Error (details in stderr JSON) |

### check output

```json
{
  "score": 30,
  "predicted_score": 100,
  "total_checked": 196,
  "recommendations": [
    {
      "param": "net.ipv4.tcp_rfc1337",
      "current": "0",
      "recommended": "1",
      "reason": "防止 TIME_WAIT 状态下的 RST 攻击",
      "confidence": "high",
      "category": "security",
      "subcategory": "network",
      "writable": true
    }
  ],
  "counts": { "performance": 34, "security": 6, "high_confidence": 5, "writable": 40 },
  "system": { "kernel": "6.6.102+", "cpu_cores": 2, "memory_gb": 8, "numa_nodes": 1 },
  "environment": "物理机/虚拟机",
  "workload": "mixed",
  "services": ["Nginx", "PostgreSQL"]
}
```

### tune output

```json
{ "applied": 5, "score_before": 30, "score_after": 35 }
```

### rollback output

```json
{ "restored": 5, "failed": 0, "skipped": 0, "status": "Full" }
```

### error output (stderr)

```json
{ "error": "tune requires root (sudo ktuner tune)" }
```

## Security

- **Code-execution deny-list**: `kernel.core_pattern`, `kernel.modprobe`, `kernel.hotplug`, `kernel.poweroff_cmd`, `kernel.modules_disabled`, `kernel.kexec_load_disabled`, `kernel.usermodehelper.*`, `fs.binfmt_misc.*` are unconditionally blocked from any write path (tune/fix/rollback). Matching is done on the resolved filesystem path, not the parameter spelling, so slash/dot/traversal variants are all caught.
- **Rollback safety**: Partial failures preserve the rollback ledger; originals are never lost.
- **No autonomous root**: ktuner checks `euid == 0` and errors out if not root. cosh's sandbox-guard + permission prompt ensure the human approves before any `sudo ktuner tune` executes.

## Installation

Install ktuner via the ANOLISA component manager (RPM backend):

```bash
sudo anolisa install ktuner --backend rpm
```

ktuner ships as an RPM only. Pass `--backend rpm` explicitly: the default
backend resolves raw artifacts and has no ktuner release, and there is no
cross-backend fallback.

Install via yum/dnf:

```bash
sudo yum install ktuner
```

Installs:
- `/usr/local/bin/ktuner` — CLI binary
- `/usr/share/anolisa/components/ktuner/component.toml` — component contract

Or build from source:

```bash
cd src/ktuner
cargo build --release
sudo install -m 0755 target/release/ktuner /usr/local/bin/ktuner
```

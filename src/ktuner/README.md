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
sudo ktuner rollback          # destructive + terminal (deletes the ledger)
sudo ktuner rollback --list   # read-only preview of what rollback would restore
```

## JSON output

All output goes to **stdout as JSON**. Errors go to **stderr as JSON**. No ANSI colors, no progress bars, no human-formatted text on stdout.

Object keys are emitted in alphabetical order. Read fields by name rather than relying on their order.

### Exit codes

| Code | Meaning |
|------|---------|
| 0    | Success (check: system already optimal; tune/fix/rollback: applied OK) |
| 1    | check: has recommendations (not an error, system can be improved); tune: recommendations exist but none are applicable here (status "blocked", e.g. read-only /proc/sys in a container); rollback: restoration incomplete |
| 2    | Error (details in stderr JSON) |

`rollback` returns `0` when all recorded values are restored (an empty ledger
is a successful no-op), `1` when any value failed or its path was missing,
and `2` for a command error such as an unreadable ledger. Incomplete restoration
keeps its JSON counts on stdout and preserves the ledger for retry.

### check output

```json
{
  "counts": {
    "high_confidence": 5,
    "performance": 34,
    "security": 6,
    "writable": 40
  },
  "environment": "物理机/虚拟机",
  "predicted_score": 100,
  "recommendations": [
    {
      "category": "security",
      "confidence": "high",
      "current": "0",
      "param": "net.ipv4.tcp_rfc1337",
      "reason": "防止 TIME_WAIT 状态下的 RST 攻击",
      "recommended": "1",
      "subcategory": "network",
      "writable": true
    }
  ],
  "score": 30,
  "services": [
    "Nginx",
    "PostgreSQL"
  ],
  "system": {
    "cpu_cores": 2,
    "kernel": "6.6.102+",
    "memory_gb": 8,
    "numa_nodes": 1
  },
  "total_checked": 196,
  "workload": "mixed"
}
```

### tune output

```json
{"applied": 5, "score_after": 35, "score_before": 30}
```

`tune --dry-run` previews the plan instead; `status` uses the same
vocabulary as the short-circuit path (`planned` here; `optimal`/`blocked`
when there is nothing to apply). `would_apply` lists the entries a real run
would write, `would_skip` names the ones this environment filters out (with
the reason: `unwritable` or `runtime_dangerous`), and `blocked` stays their
count:

```json
{"blocked": 1, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_skip": [{"param": "vm.nr_hugepages", "reason": "runtime_dangerous"}]}
```

### rollback output

```json
{"failed": 0, "restored": 5, "skipped": 0, "status": "Full"}
```

### rollback --list output

`sudo ktuner rollback --list` previews what a rollback would restore — read-only, nothing is
written or deleted (the ledger is 0600 under a 0700 root-owned dir, so it shares rollback's
root gate; a corrupt ledger surfaces as an error rather than an empty list):

```json
{"count": 2, "pending": [{"applied": "1", "param": "vm.swappiness", "previous": "60"}]}
```

Plain `ktuner rollback` is unchanged: it restores, finalizes the ledger, and cleans up.

### error output (stderr)

```json
{"error": "tune requires root (sudo ktuner tune)"}
```

Network `net.ipv4.conf` and `net.ipv6.conf` names preserve interface case and literal dots: `ktuner why net/ipv4/conf/Br0.100/forwarding` addresses `Br0.100`. Dotted aliases are also accepted. Persistence retains that path with a slash-first key when an interface contains dots. Built-in rules do not currently generate per-VLAN recommendations.

## Security

- **Code-execution deny-list**: `kernel.core_pattern`, `kernel.modprobe`, `kernel.hotplug`, `kernel.poweroff_cmd`, `kernel.modules_disabled`, `kernel.kexec_load_disabled`, `kernel.usermodehelper.*`, `fs.binfmt_misc.*` are unconditionally blocked from any write path (tune/fix/rollback). Matching is done on the resolved filesystem path, not the parameter spelling, so slash/dot/traversal variants are all caught.
- **Concurrent operations**: tune, fix, library imports, and rollback share a lock through original-value reads, writes, recording, and persistence. Originals are read after locking; an unreadable ledger blocks new writes. This coordinates KTuner operations, not external sysctl writers or crash recovery.
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

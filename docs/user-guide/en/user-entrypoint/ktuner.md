# ktuner

ktuner is a deterministic kernel-tuning engine for AI agents. It evaluates 207 rules against the running system and outputs structured JSON recommendations, so an agent (or a human) can diagnose, apply, and roll back kernel parameter changes safely.

---

## Overview

ktuner is a rule engine, not an LLM: every recommendation comes from a hard-coded rule reading `/proc/sys` and `/sys`, so results are reproducible and explainable. It covers network, memory, I/O, CPU, and security parameters, scores the current system, and predicts the score after tuning.

It is designed to be driven by cosh and other ANOLISA-compatible agents as a tool, but the CLI is equally usable by hand.

---

## Installation

ktuner ships as an RPM for Linux x86_64 (system mode only). Install it with
the ANOLISA component manager, selecting the RPM backend — ktuner has no raw
release and there is no cross-backend fallback:

```bash
sudo anolisa install ktuner --backend rpm
```

Or install the RPM directly and let ANOLISA track it:

```bash
sudo yum install ktuner
sudo anolisa --install-mode system adopt ktuner
```

To build from source instead:

```bash
cd src/ktuner
cargo build --release
# binary at target/release/ktuner
```

For read-only use you can run the binary directly (`./target/release/ktuner check`). To make `ktuner` available system-wide — required for `ktuner tune` (needs root) and for the cosh first-run integration (which only runs a root-owned binary from a trusted path) — install it to a system path:

```bash
sudo install -o root -g root -m 755 target/release/ktuner /usr/local/bin/ktuner
```

The examples below assume `ktuner` is on your `PATH`.

---

## Quick Start

```bash
# Diagnose — read-only, no root required
ktuner check                   # score + all recommendations
ktuner check --category net    # limit to one category
ktuner check --conservative    # high-confidence recommendations only

# Preview changes without applying (dry-run)
sudo ktuner tune --dry-run

# Apply recommendations (requires root)
sudo ktuner tune               # apply all
sudo ktuner tune --conservative
sudo ktuner tune --exclude vm.dirty_ratio   # apply all but this one

# Fix a single parameter
sudo ktuner fix vm.swappiness

# Explain why a parameter should change
ktuner why net.core.somaxconn

# Undo all changes ktuner made, or one recorded parameter
sudo ktuner rollback          # destructive + terminal: restores and deletes the ledger
sudo ktuner rollback --list   # read-only preview of what rollback would restore
sudo ktuner rollback vm.swappiness   # restore one recorded parameter
```

All output is JSON on stdout; errors are JSON on stderr. Exit codes: `0` success,
`1` check found recommendations or rollback left values unrestored, `2` command error.
Rollback returns `1` for failed writes or missing paths, including partial restoration;
its JSON counts remain on stdout and the ledger is kept for retry. An empty ledger
is a successful no-op (`0`); an unreadable or missing ledger is a command error (`2`).
`sudo ktuner rollback <param>` follows the same contract for the entry it names —
plus the mutually exclusive twin recorded with it, restored together because writing
either knob zeroes the other — and reports that entry as `param` in its JSON; a
parameter the ledger does not record is a command error (`2`).

`tune --exclude <param>` (repeatable) leaves a named recommendation out of the plan:
nothing is written for it, nothing is recorded in the rollback ledger, and nothing is
persisted. It applies after the `--category` and `--conservative` filters, and the
excluded entry is listed in the output (`would_skip`, reason `excluded`); a name that
matches no recommendation in scope is reported in `unmatched_exclude` instead of
failing the run (on an empty plan, every given name). Excluding every recommendation
is a `blocked` run with exit code `1`, like any other plan with nothing applicable. Excluding one half of a mutually exclusive
sysctl pair does not stop the kernel from zeroing it when the other half is written;
ktuner records the cleared original so `rollback` can restore it.

---

For network conf parameters, interface identity is case-sensitive. Both `net/ipv4/conf/Br0.100/forwarding` and `net.ipv4.conf.Br0.100.forwarding` address the same interface; `br0.100` is a different identity. IPv6 follows the same rule. Persisted records for interfaces with literal dots use slash-first sysctl.d keys so systemd preserves those dots. This supports existing valid records or custom library recommendations; built-in rules do not currently generate per-VLAN recommendations.

## Permission Boundary

| Command | Root | Effect |
|---------|------|--------|
| `check`, `why` | No | Read-only diagnosis; never writes the kernel |
| `tune --dry-run` | No | Previews changes, writes nothing |
| `tune`, `fix`, `rollback` | Yes (`sudo`) | Writes `/proc/sys`; refuses to run if not root. `rollback --list` is read-only but shares the root gate (the ledger is 0600 under a 0700 root-owned dir) |

Safety guarantees:

- **Code-execution deny-list**: parameters that can lead to code execution (`kernel.core_pattern`, `kernel.modprobe`, `kernel.hotplug`, and similar) are unconditionally blocked from every write path. Matching is on the resolved filesystem path, so spelling variants cannot bypass it.
- **Concurrent operations**: tune, fix, library imports, and rollback serialize original-value reads, writes, ledger updates, and persistence using one lock. A stale diagnosis is never used as a new rollback original; an unreadable ledger blocks new writes. External sysctl writers and crash recovery are outside this guarantee.
- **Rollback safety**: applied changes are recorded; a partial rollback failure never discards the remaining original values.
- **No autonomous root**: ktuner errors out unless run as root. When invoked through cosh, the sandbox guard and permission prompt ensure a human approves before any `sudo ktuner tune` runs.

---

## Usage with cosh

cosh discovers ktuner automatically via its skill definition (`src/os-skills/system-admin/ktuner/`), so no wiring is needed — ask in natural language:

```
> "Check whether this machine's kernel parameters can be improved"
> "Optimize the kernel for a database workload"
```

On first Linux auth, if a trusted ktuner is installed at a system path, cosh shows a one-line non-blocking hint. Run `/ktuner enable` to view a read-only `ktuner check` report, or `/ktuner disable` to stop asking. You can also change this via `general.ktunerCheck` in `/settings`. cosh never applies changes on its own.

---

## See Also

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [OS Skills](os-skills.md)
- Full reference: `src/ktuner/README.md`

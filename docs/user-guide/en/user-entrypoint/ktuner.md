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

# Fix a single parameter
sudo ktuner fix vm.swappiness

# Explain why a parameter should change
ktuner why net.core.somaxconn

# Undo all changes ktuner made
sudo ktuner rollback
```

All output is JSON on stdout; errors are JSON on stderr. Exit codes: `0` success, `1` check found recommendations (not an error), `2` error.

---

## Permission Boundary

| Command | Root | Effect |
|---------|------|--------|
| `check`, `why` | No | Read-only diagnosis; never writes the kernel |
| `tune --dry-run` | No | Previews changes, writes nothing |
| `tune`, `fix`, `rollback` | Yes (`sudo`) | Writes `/proc/sys`; refuses to run if not root |

Safety guarantees:

- **Code-execution deny-list**: parameters that can lead to code execution (`kernel.core_pattern`, `kernel.modprobe`, `kernel.hotplug`, and similar) are unconditionally blocked from every write path. Matching is on the resolved filesystem path, so spelling variants cannot bypass it.
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

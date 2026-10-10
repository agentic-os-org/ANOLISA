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
sudo ktuner tune --exclude vm.dirty_ratio   # apply all but this one

# Fix a single parameter (requires root)
sudo ktuner fix <param>        # e.g. sudo ktuner fix vm.swappiness
sudo ktuner fix <param> --dry-run   # preview one parameter, no changes

# Explain why a parameter should change
ktuner why <param>             # e.g. ktuner why net.core.somaxconn

# Undo changes (requires root)
sudo ktuner rollback          # destructive + terminal (deletes the ledger)
sudo ktuner rollback --list   # read-only preview of what rollback would restore
sudo ktuner rollback <param> [<param>…]  # restore the recorded parameters named, e.g. vm.dirty_bytes
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
is a successful no-op), `1` when any value failed, its path was missing, or a
cleanup could not be removed — a persisted config file or the ledger itself —
and `2` for a command error such as an unreadable ledger. Incomplete restoration
keeps its JSON counts on stdout and preserves the ledger for retry; a persisted
file that survived the cleanup counts as a failure there, because it re-applies
the tuned values on the next boot, and a ledger that survived keeps
`rollback --list` reporting the entries of a restore that already ran.

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

When this environment filters recommendations out (unwritable, or
runtime-dangerous), a real `tune` names them in `would_skip` with the same
shape as the dry-run preview, so the output reconciles with `check` — which
keeps reporting those parameters (exit 1) after a successful partial tune:

```json
{"applied": 4, "failed": [], "score_after": 35, "score_before": 30, "would_skip": [{"param": "vm.nr_hugepages", "reason": "runtime_dangerous"}]}
```

The fully-blocked short-circuit body carries the same `would_skip` list
alongside its counts.

`tune --exclude <param>` (repeatable) leaves the named recommendation out of
the plan: nothing is written for it, nothing enters the rollback ledger, and
nothing is persisted. Exclusions apply after the `--category`/`--conservative`
filters, the excluded entry is named in `would_skip` with the reason
`excluded` — the operator's instruction outranks `unwritable` and
`runtime_dangerous` — and a name that matches no recommendation in scope is
not an error: it is echoed in `unmatched_exclude`, in the spelling given, so
an inert exclusion is visible instead of silent — an empty plan reports every
given name.

```json
{"blocked": 1, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_skip": [{"param": "kernel.dmesg_restrict", "reason": "excluded"}]}
```

With everything excluded the run answers `status: "blocked"` and exits 1,
like any other plan with nothing applicable (`check` still reports those
parameters); `blocked_excluded` joins the short-circuit counts and the three
add up to `recommendations`:

```json
{"applied": 0, "blocked": 55, "blocked_excluded": 55, "blocked_runtime_dangerous": 0, "blocked_unwritable": 0, "dry_run": true, "recommendations": 55, "status": "blocked", "would_apply": [], "would_skip": [ ... ]}
```

`--exclude` does not reach the kernel's own side effects: writing one half of
a mutually exclusive sysctl pair — `vm.dirty_bytes`/`vm.dirty_ratio`,
`vm.dirty_background_bytes`/`vm.dirty_background_ratio`, and
`vm.overcommit_kbytes`/`vm.overcommit_ratio` — zeroes the other half
(`mm/page-writeback.c`, `mm/util.c`), so a parameter excluded from the plan
is still cleared in the kernel when its counterpart is written. ktuner keeps
recording that cleared original in the ledger (so `rollback` restores it) and
persists only the written half, which reproduces the same cleared state at
boot; the built-in rules never plan both halves of a pair at once.

`tune --dry-run` previews the plan instead; `status` uses the same
vocabulary as the short-circuit path (`planned` here; `optimal`/`blocked`
when there is nothing to apply). `would_apply` lists the entries a real run
would write, `would_skip` names the ones this run leaves out (with the
reason: `unwritable`, `runtime_dangerous`, or `excluded` for an `--exclude`
name), and `blocked` stays their count:

```json
{"blocked": 1, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_skip": [{"param": "vm.nr_hugepages", "reason": "runtime_dangerous"}]}
```

`ktuner why` carries the same reason on a recommendation no write path will
take (`skip_reason`: `unwritable` or `runtime_dangerous`; absent when the
plan would write it), so the explanation never contradicts the plan. `check`
publishes the same classification on its recommendations, so the diagnosis
carries the reason without a dry run.

### fix --dry-run output

`ktuner fix <param> --dry-run` previews the one-parameter write in the same
shape as `tune --dry-run`, scoped to that parameter: `dry_run`, `status`
(`"planned"` here), `blocked`, `would_apply` (the one recommendation this
write would land, in `check`'s per-entry shape) and `would_skip`. It writes
nothing — no kernel write, no rollback record, no persistence, no ledger
lock — and exits `0`:

```json
{
  "blocked": 0,
  "dry_run": true,
  "status": "planned",
  "would_apply": [
    {
      "category": "performance",
      "confidence": "high",
      "current": "1000",
      "param": "net.core.netdev_max_backlog",
      "reason": "万兆网卡场景下增大网卡收包队列深度，避免高流量时软中断处理不及导致丢包",
      "recommended": "65536",
      "subcategory": "network",
      "writable": true
    }
  ],
  "would_skip": []
}
```

`would_skip` is always empty here: a single-parameter command answers every
refusal through its own error channel — a parameter outside the plan
(`parameter not found or already optimal: <param>`), an unwritable one, a
runtime-dangerous one, or a non-root plain run — with exactly the stderr JSON
and exit code `ktuner fix <param>` produces, so
`ktuner fix <param> --dry-run && ktuner fix <param>` cannot be misled by a
preview that disagrees with the command it previews. Like `tune --dry-run`,
the preview needs no root; a plain `fix` still does.

Writing one half of a mutually exclusive sysctl pair zeroes the other half in
the kernel (`mm/page-writeback.c`, `mm/util.c`), and the preview names that
twin in `would_clear`, taken from the same table the write path records from
(the key is absent when the parameter has no twin):

```json
{
  "blocked": 0,
  "dry_run": true,
  "status": "planned",
  "would_apply": [
    {
      "category": "performance",
      "confidence": "medium",
      "current": "0",
      "param": "vm.dirty_bytes",
      "reason": "大内存服务器 (125 GB) 使用 dirty_ratio 百分比会导致脏页过多、IO 突刺，改用固定字节限制更平稳",
      "recommended": "268435456",
      "subcategory": "memory",
      "writable": true
    }
  ],
  "would_clear": [
    "vm.dirty_ratio"
  ],
  "would_skip": []
}
```

A real `fix` records the cleared twin's original in the ledger — so
`rollback` restores it — only when the twin holds a configured (non-zero)
value; persistence reproduces the cleared state.

### rollback output

```json
{"failed": 0, "restored": 5, "skipped": 0, "status": "Full"}
```

### rollback <param>… output

`sudo ktuner rollback <param>` restores just the recorded entry the parameter
names and leaves the rest of the ledger in place. It accepts the same spellings
`fix` and `why` do (slash/dot aliases, and the literal-dot interface names):

```json
{"failed": 0, "param": "vm.dirty_bytes", "restored": 2, "skipped": 0, "status": "Full"}
```

`param` is the ledger entry that was restored, spelled the way `rollback --list`
publishes it. The persisted file is regenerated from the entries that remain;
when the ledger empties, the same terminal cleanup as a full rollback runs
(persisted files, then the ledger). A parameter the kernel keeps mutually
exclusive with a twin (`vm.dirty_bytes` / `vm.dirty_ratio`,
`vm.overcommit_kbytes` / `vm.overcommit_ratio`, and the `dirty_background_`
pair) is restored together with the twin the ledger recorded: writing either
knob zeroes the other, so a half restore could not leave the ledger describing
the live kernel, and `restored` counts both entries. An entry whose write failed
or whose path is gone keeps its record (and its twin's), exits `1`, and can be
retried; a parameter the ledger does not record is a command error (`2`, stderr
JSON), never a silent success. `status` classifies this attempt
(`Full` / `Partial` / `Nothing`), not whether the ledger is now empty. Plain
`ktuner rollback` and `ktuner rollback --list` are unchanged.

Two or more parameters undo a multi-parameter tuning in one run: the whole batch
happens under one ledger lock, and the persisted file is regenerated once from
the entries that remain, not once per parameter. The body keeps the aggregate
counters and replaces `param` (a string) with `params` (an array) — the ledger
key each positional resolved to, in the order given and deduplicated, so
`vm/swappiness` and `VM.SWAPPINESS` both report `vm.swappiness`:

```json
{"failed": 0, "params": ["vm.swappiness", "net.core.somaxconn"], "restored": 2, "skipped": 0, "status": "Full"}
```

The twin restored with an entry is counted in `restored` but not named, exactly
as the single-parameter body does not name it, and naming both halves of a pair
restores it once. A parameter whose write failed or whose path is gone is still
named — the counters and `status` say whether the batch restored. A batch that
restores only part of what it named retires the entries that landed, keeps the
records of the ones that did not (with their twins), exits `1` and reports
`Partial`; a batch where nothing lands touches neither the ledger nor the
persisted file. One name the ledger does not record refuses the whole command
before anything is written (`2`, stderr JSON), the same command error a single
parameter gets: skipping the miss would let a typo drop the rest of the batch
while the run still exited `0`. `rollback` with no parameter, `rollback --list`,
and `rollback --list` combined with any parameter (still a usage error) are
unchanged.

### rollback --list output

`sudo ktuner rollback --list` previews what a rollback would restore — read-only, nothing is
written or deleted (the ledger is 0600 under a 0700 root-owned dir, so it shares rollback's
root gate; a corrupt ledger surfaces as an error rather than an empty list). Each entry keeps
its recorded `param`/`applied`/`previous` and adds what the kernel holds right now: `live`, read
from the entry's own path through the same reader every other surface uses (the bracketed option
of a sysfs list, the single-space form of a multi-value sysctl), and `drifted`, whether `live`
still matches `applied` — compared the way a write is verified, so a value the kernel renders in
its own shape (a bracketed option list, TAB-separated multi-value, a leading-token echo) is not
drift:

```json
{"count": 3, "pending": [
  {"applied": "none", "drifted": null, "live": null, "param": "block/sda/scheduler", "previous": "mq-deadline"},
  {"applied": "0", "drifted": true, "live": "20", "param": "vm.dirty_ratio", "previous": "20"},
  {"applied": "1", "drifted": false, "live": "1", "param": "vm.swappiness", "previous": "60"}
]}
```

An entry the kernel zeroed as a side effect of its mutually exclusive twin records
`applied = "0"` — the value the kernel put live — so it is drifted once it is no longer 0. A path
that cannot be read (a device that is gone, a module that is not loaded, a write-only tunable)
reports `live: null` and `drifted: null`: an unreadable value is not an error and drift never
changes the exit code. The listing is a snapshot — the kernel can change between the preview and
the rollback. Plain `ktuner rollback` is unchanged: it restores, finalizes the ledger, and
cleans up.

### error output (stderr)

```json
{"error": "tune requires root (sudo ktuner tune)"}
```

Network `net.ipv4.conf` and `net.ipv6.conf` names preserve interface case and literal dots: `ktuner why net/ipv4/conf/Br0.100/forwarding` addresses `Br0.100`. Dotted aliases are also accepted. Persistence retains that path with a slash-first key when an interface contains dots. Built-in rules do not currently generate per-VLAN recommendations.

## Security

- **Code-execution deny-list**: `kernel.core_pattern`, `kernel.modprobe`, `kernel.hotplug`, `kernel.poweroff_cmd`, `kernel.modules_disabled`, `kernel.kexec_load_disabled`, `kernel.usermodehelper.*`, `fs.binfmt_misc.*` are unconditionally blocked from any write path (tune/fix/rollback). Matching is done on the resolved filesystem path, not the parameter spelling, so slash/dot/traversal variants are all caught.
- **Runtime-dangerous knobs**: knobs unsafe to change on a live host (`vm.nr_hugepages`) are refused at the same write choke point, for every caller — tune leaves them out of the plan (named in `would_skip` as `runtime_dangerous`), fix refuses them with the advice to persist, and a library import cannot apply them either. Slash/dot spellings of the same knob are both caught.
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

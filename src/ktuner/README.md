# ktuner — deterministic kernel-tuning engine

[中文版](README_zh.md)

Agent-facing kernel parameter tuning engine for ANOLISA. Evaluates 207 rules against the running system and outputs structured JSON recommendations. Designed to be called by cosh/agent via `ktuner <command> [options]`.

Disk `rq_affinity` advice respects `threadirqs` boot parameters and PREEMPT_RT. Quoted parameter values do not introduce flags or a `--` separator; parameters after the actual separator are arguments to init.

## Usage

```bash
# Diagnose — output score + recommendations, with the host details
ktuner check
ktuner check --category net
ktuner check --category net --category mem   # repeatable: keep the union
ktuner check --conservative    # high-confidence only
ktuner check --only vm.swappiness   # report only the named parameters
ktuner check --exclude vm.dirty_ratio   # report everything but this one

# Apply recommendations (requires root)
sudo ktuner tune --dry-run     # preview, no changes
sudo ktuner tune               # apply all
sudo ktuner tune --conservative
sudo ktuner tune --category net --category mem   # apply only these categories
sudo ktuner tune --exclude vm.dirty_ratio   # apply all but this one
sudo ktuner tune --only vm.swappiness   # apply only the named parameters

# Fix a single parameter (requires root)
sudo ktuner fix <param>        # e.g. sudo ktuner fix vm.swappiness
sudo ktuner fix <param> --dry-run   # preview one parameter, no changes

# Explain why one or more parameters should change (one diagnosis per run)
ktuner why <param> [<param>…]  # e.g. ktuner why net.core.somaxconn vm.swappiness

# Undo changes (requires root)
sudo ktuner rollback          # destructive + terminal (deletes the ledger)
sudo ktuner rollback --list   # read-only preview of what rollback would restore
sudo ktuner rollback --list <param> [<param>…]  # preview only what rolling back these would restore
sudo ktuner rollback <param> [<param>…]  # restore the recorded parameters named, e.g. vm.dirty_bytes
sudo ktuner rollback --exclude vm.swappiness   # restore all recorded entries but this one
sudo ktuner rollback --list --exclude vm.swappiness   # preview that same set
sudo ktuner rollback --category net   # restore only the recorded entries of this category
sudo ktuner rollback --list --category net   # preview that same set
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
    "cpu": 1,
    "high_confidence": 11,
    "io": 3,
    "memory": 8,
    "network": 35,
    "performance": 47,
    "security": 8,
    "writable": 55
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
    "cpu_cores": 12,
    "cpu_cores_host": 12,
    "cpu_model": "AMD Ryzen 9 7945HX with Radeon Graphics",
    "disks": [
      {
        "available_schedulers": [
          "none",
          "mq-deadline",
          "kyber"
        ],
        "hidden": false,
        "holders": [],
        "name": "sdd",
        "nr_requests": 950,
        "read_ahead_kb": 128,
        "rq_affinity": 1,
        "scheduler": "none",
        "type": "HDD"
      },
      {
        "available_schedulers": [
          "none",
          "mq-deadline",
          "kyber"
        ],
        "hidden": false,
        "holders": [],
        "name": "sdb",
        "nr_requests": 950,
        "read_ahead_kb": 128,
        "rq_affinity": 1,
        "scheduler": "none",
        "type": "HDD"
      }
    ],
    "kernel": "6.6.87.2-microsoft-standard-WSL2",
    "memory_gb": 11,
    "memory_gb_host": 11,
    "network": [
      {
        "name": "eth0",
        "speed_mbps": 10000
      }
    ],
    "numa_nodes": 1,
    "os_distro": "Ubuntu 24.04.3 LTS"
  },
  "total_checked": 196,
  "workload": "mixed"
}
```

`counts` describes the recommendations in the report: `performance` and
`security` by kind, `high_confidence` and `writable`, and one count per
category — `network`, `memory`, `io` and `cpu` — each the number of
recommendations `check --category` with that name keeps. Security findings
are counted once, under `security`, so the four category counts add up to
`performance`.

`system` is the machine the engine diagnosed. Alongside the four keys it
always carried it names the distro (`os_distro`), the CPU (`cpu_model`), and
the two inventories the rules read: `disks`, one entry per disk with all
nine collected fields — including the three the disk rules gate on
(`hidden`, `holders`, `available_schedulers`), so a suggestion those rules
did not make can be explained from the report alone — and `network`, one
`name`/`speed_mbps` pair per interface, where `speed_mbps: 0` means the
speed is unknown, not zero. `cpu_cores` and `memory_gb` are the effective
values the rules scale by, clamped to the cgroup limits that bind the
process; `cpu_cores_host` and `memory_gb_host` are the host's own readings —
equal unless a limit binds, and the larger number when one does. This page's
examples truncate `disks` and `network`; a real report lists every disk and
interface it read.

`--category` is repeatable: every value keeps its own category (the same
names as before), and the entries shown are the union of the named
categories, so `check --category net --category mem` is one run over both —
and `tune --category net --category mem` plans the same union. Repeating the
aliases of one category (`net`, `network`, `网络`) keeps each entry once, and
a value that names no category is rejected (exit 2) instead of being ignored.

`check --only <param>` (repeatable) reports only the named recommendations and
`check --exclude <param>` (repeatable) reports everything but them. Both
accept the same spellings `fix`/`why` accept (slash/dot aliases, and the
literal-dot interface names) and apply after the
`--category`/`--conservative` filters, narrowing what those filters left. A
name that matches no recommendation in scope is not an error: it is echoed in
the `unmatched_only` (or `unmatched_exclude`) key, in the spelling given, in
the order given — an empty report echoes every given name. The key appears
only when that flag was given and at least one name addressed nothing, so a
run without the flags keeps its exact key set. The two flags are mutually
exclusive: a run carrying both is a usage error (exit 2, stderr JSON) decided
before the system is read.

`score` and `total_checked` describe the system and do not move;
`predicted_score`, `counts` and the exit code follow the report that is left —
the same rule `--category` has always followed — so a selection that leaves
nothing to report answers with an empty `recommendations` list and exit 0
(the diagnosis has no blocked state, unlike `tune`). Narrowing to one
recommendation reports that recommendation alone:

```json
{"counts": {"cpu": 0, "high_confidence": 1, "io": 0, "memory": 0, "network": 1, "performance": 1, "security": 0, "writable": 1}, "environment": "物理机/虚拟机", "predicted_score": 30, "recommendations": [ ... ], "score": 30, "services": [], "system": {"cpu_cores": 12, "cpu_cores_host": 12, "cpu_model": "AMD Ryzen 9 7945HX with Radeon Graphics", "disks": [{"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdd", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}, {"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdb", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}], "kernel": "6.6.87.2-microsoft-standard-WSL2", "memory_gb": 11, "memory_gb_host": 11, "network": [{"name": "eth0", "speed_mbps": 10000}], "numa_nodes": 1, "os_distro": "Ubuntu 24.04.3 LTS"}, "total_checked": 208, "workload": "mixed"}
```

An inert name is echoed instead of failing the run:

```json
{"counts": {"cpu": 0, "high_confidence": 0, "io": 0, "memory": 0, "network": 0, "performance": 0, "security": 0, "writable": 0}, "environment": "物理机/虚拟机", "predicted_score": 30, "recommendations": [], "score": 30, "services": [], "system": {"cpu_cores": 12, "cpu_cores_host": 12, "cpu_model": "AMD Ryzen 9 7945HX with Radeon Graphics", "disks": [{"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdd", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}, {"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdb", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}], "kernel": "6.6.87.2-microsoft-standard-WSL2", "memory_gb": 11, "memory_gb_host": 11, "network": [{"name": "eth0", "speed_mbps": 10000}], "numa_nodes": 1, "os_distro": "Ubuntu 24.04.3 LTS"}, "total_checked": 208, "unmatched_only": ["no_such_ktuner_param"], "workload": "mixed"}
```

### why output

`ktuner why <param>` explains one parameter. A parameter the rules recommend
carries the same fields `check` publishes for it, plus `skip_reason` when no
write path would take it; one already at its recommended value — or outside the
rules, read from its own sysfs/sysctl file — answers with its current value and
exits `0`:

```json
{
  "current": "Linux",
  "param": "kernel.ostype",
  "status": "optimal"
}
```

Two or more parameters are answered in one run, from one diagnosis: running the
command once per parameter gathered the whole system every time, and two
diagnoses could disagree if the kernel moved between them. The body carries
`count` and `params` — one entry per parameter, in the order given and
deduplicated by the same alias policy `fix` and `tune --exclude/--only` use
(`vm/swappiness` and `vm.swappiness` answer once; sysfs identities such as
`block/sda/scheduler` stay verbatim). Each entry is exactly the object that
parameter answers alone:

```json
{
  "count": 2,
  "params": [
    {
      "current": "60",
      "param": "vm.swappiness",
      "status": "optimal"
    },
    {
      "category": "performance",
      "confidence": "high",
      "current": "4096 131072 6291456",
      "param": "net.ipv4.tcp_rmem",
      "reason": "万兆网卡场景下增大 TCP 接收缓冲区上限，充分利用带宽-延迟积",
      "recommended": "4096 131072 16777216",
      "subcategory": "network",
      "writable": true
    }
  ]
}
```

`why` exits `1` when at least one entry is a recommendation and `0` when every
entry is optimal — the 0/1 vocabulary `check` uses on the same parameters. A
parameter that cannot be found, or whose value cannot be read, refuses the
whole command with the error a single parameter gets (`2`, stderr JSON naming
the parameter, nothing on stdout): a batch never answers half a question, so a
typo cannot drop the rest of the batch behind a successful exit.
### tune output

```json
{"applied": 5, "boot_unit_enabled": true, "score_after": 35, "score_before": 30}
```

`boot_unit_enabled` reports whether the non-sysctl boot replay is armed. A run
that persisted block-device or THP values but could not `systemctl enable
ktuner-nonsysctl.service` answers `false`: the values are live now but will not
be replayed after a reboot, and `applied` on its own would not say so. It is
`true` when the unit is enabled, and when the run had nothing outside
`/proc/sys` to persist. `fix`'s body carries the same key.

Writing one half of a mutually exclusive sysctl pair zeroes the other half in
the kernel, and the body names the twins this run zeroed in `cleared` (for
example `"cleared": ["vm.dirty_ratio"]`). The list is exactly the twins the
ledger recorded, so `rollback` restores them; a twin that already read 0, or
one a write left alone because the value did not change, is not named. The key
is absent when nothing was cleared.

A real run also names what it wrote: `written` lists one entry per write, each
`{param, previous, applied}`, where `previous` is the original the ledger
recorded — the value `rollback` restores — and `applied` is the value the
kernel took, the clamped one when the kernel adjusted the request. It is the
pair a single `fix` reports, so a batch no longer needs a second root read of
the ledger to say which parameters changed and from what. The key is always
present, the way `failed` is: a run that wrote nothing answers an empty list.
A write-only tunable, whose original cannot be read, reports `previous: null` —
the same answer `fix` gives. A write that zeroed a twin is still one entry: the
twin is named by `cleared`, and the list's length is `applied`.

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

`tune --only <param>` (repeatable) keeps only the named recommendations in the
plan: it accepts the same spellings as `fix`/`why`, applies after the
`--category`/`--conservative` filters, and is the mirror of `--exclude`, with
which it is mutually exclusive — a run that carries both is a usage error
(exit 2, stderr JSON). Every in-scope entry the selection does not name is
left out exactly like an exclusion (not written, not recorded in the ledger,
not persisted) and is reported in `would_skip` with the reason `not_selected`,
which outranks `unwritable` and `runtime_dangerous` so the counts do not drift
with the environment. A name that matches no recommendation in scope is not
an error: it is echoed in `unmatched_only`, in the spelling given — an empty
plan reports every given name. With nothing selected the run answers `status:
"blocked"` and exits 1 like any other plan with nothing applicable;
`blocked_not_selected` joins the short-circuit counts and the four add up to
`recommendations`.

```json
{"blocked": 11, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_skip": [{"param": "net.core.netdev_max_backlog", "reason": "not_selected"}, ...]}
```

An unmatched name cannot select anything, so a plan selected only by unmatched
names has nothing applicable and reports its own count key:

```json
{"applied": 0, "blocked": 53, "blocked_not_selected": 53, "blocked_runtime_dangerous": 0, "blocked_unwritable": 0, "dry_run": true, "recommendations": 53, "status": "blocked", "unmatched_only": ["vm.swappiness", "no_such_ktuner_param"], "would_apply": [], "would_skip": [ ... ]}
```

`--only` narrows the plan like `--exclude` and, like it, does not reach the
kernel's own side effects: writing one half of a mutually exclusive pair still
zeroes the other half (`mm/page-writeback.c`, `mm/util.c`), so a parameter the
selection left out is cleared in the kernel when its counterpart is written.
The ledger still records the cleared original and `rollback` restores it.

`tune --dry-run` previews the plan instead; `status` uses the same
vocabulary as the short-circuit path (`planned` here; `optimal`/`blocked`
when there is nothing to apply). `would_apply` lists the entries a real run
would write, `would_skip` names the ones this run leaves out (with the
reason: `unwritable`, `runtime_dangerous`, or `excluded` / `not_selected` for
a name a selection left out), and `blocked` stays their count. `would_clear`
names the mutually exclusive twins those writes zero — `fix --dry-run`'s key
and meaning, for the whole plan, from the same table the write path records
from. A plan that writes one half of a pair clears the other half in the
kernel whether or not the selection named it, so the key is what makes the
paragraph above visible in the preview; it is absent when no applied
parameter has a twin, and each twin is named once, in plan order:

```json
{"blocked": 1, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_clear": ["vm.dirty_ratio"], "would_skip": [{"param": "vm.nr_hugepages", "reason": "runtime_dangerous"}]}
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

A real `fix` names the twin its write zeroed in `cleared`, in the same shape
as `would_clear`; the key is absent when the write cleared nothing.

A real `fix` answers with `fixed`, the parameter exactly as it was typed, and
`param`, the ledger key the write was recorded under: the recommendation's own
spelling, which `rollback --list` lists, `rollback <param>` answers with and
`tune`'s `written` entries carry. They differ when the parameter was named by an
alias (`VM/SWAPPINESS`, `vm/swappiness`), and `param` is the one to reconcile
against the ledger.

A real `fix` records the cleared twin's original in the ledger — so
`rollback` restores it — only when the twin holds a configured (non-zero)
value; persistence reproduces the cleared state.

### rollback output

```json
{"failed": 0, "restored": 5, "restored_params": [ ... ], "skipped": 0, "status": "Full"}
```

A rollback names what it did, in both directions. `restored_params` lists the
ledger keys the attempt put back, in the order the batch landed them. The set
is not the one the caller asked for: a mutually exclusive twin is restored with
the entry that clears it, and the selection of `--exclude` / `--category` is
expanded inside the engine, so the run has to answer it — a write the kernel
refused but the twin's restore brought back is named here like any other.

A rollback that left something behind names that too, so a caller reading
`failed` or `skipped` does not have to run `rollback --list` as root to find
out which parameters are still tuned: `failures` carries one
`{"param", "error"}` per entry whose restore did not land (a write the kernel
refused, a read-back that stayed clamped, a value a later write clobbered), and
`missing` names the entries whose recorded path is gone. All three keys appear
only when they have something to say, so a rollback that restored and left
nothing answers with the four counters alone, byte for byte. The counters keep
their meaning: `failed` still counts everything that did not land, including
the persisted files and the ledger a cleanup could not remove, and `skipped`
still counts the absent paths. Every invocation shape reports them — the plain
rollback, a single positional, a batch, and the bodies `--exclude` and
`--category` wrap.

```json
{"failed": 1, "failures": [{"error": "Is a directory (os error 21)", "param": "vm.dirty_ratio"}], "missing": ["net.core.somaxconn"], "restored": 1, "skipped": 1, "status": "Partial"}
```

### rollback <param>… output

`sudo ktuner rollback <param>` restores just the recorded entry the parameter
names and leaves the rest of the ledger in place. It accepts the same spellings
`fix` and `why` do (slash/dot aliases, and the literal-dot interface names):

```json
{"failed": 0, "param": "vm.dirty_bytes", "restored": 2, "restored_params": ["vm.dirty_bytes", "vm.dirty_ratio"], "skipped": 0, "status": "Full"}
```

`param` is the ledger entry that was restored, spelled the way `rollback --list`
publishes it. The persisted file is regenerated from the entries that remain;
when the ledger empties, the same terminal cleanup as a full rollback runs
(persisted files, then the ledger). A parameter the kernel keeps mutually
exclusive with a twin (`vm.dirty_bytes` / `vm.dirty_ratio`,
`vm.overcommit_kbytes` / `vm.overcommit_ratio`, and the `dirty_background_`
pair) is restored together with the twin the ledger recorded: writing either
knob zeroes the other, so a half restore could not leave the ledger describing
the live kernel, and `restored` counts both entries — `restored_params` names
both, the twin by its own ledger key. An entry whose write failed
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
{"failed": 0, "params": ["vm.swappiness", "net.core.somaxconn"], "restored": 2, "restored_params": ["vm.swappiness", "net.core.somaxconn"], "skipped": 0, "status": "Full"}
```

A twin an entry brought along is named by `restored_params`, not by `params`:
the array answers what the batch asked for, the report answers what it retired,
and naming both halves of a pair restores it once. A parameter whose write failed or whose path is gone is still
named in `failures` / `missing` — the counters and `status` say whether the
batch restored. A batch that
restores only part of what it named retires the entries that landed, keeps the
records of the ones that did not (with their twins), exits `1` and reports
`Partial`; a batch where nothing lands touches neither the ledger nor the
persisted file. One name the ledger does not record refuses the whole command
before anything is written (`2`, stderr JSON), the same command error a single
parameter gets: skipping the miss would let a typo drop the rest of the batch
while the run still exited `0`. `rollback` with no parameter and `rollback --list`
are unchanged; `rollback --list <param>…` previews a batch (see below).

Every rollback that keeps part of the ledger (named parameters, `--exclude`,
`--category`) regenerates the persisted files from the entries that remain and
re-runs `systemctl enable` for the non-sysctl boot replay. When that enable is
refused, the body adds `"boot_unit_enabled": false`, the key `tune` and `fix`
publish: block-device or THP values still recorded will not be replayed after a
reboot. The key is absent whenever the replay is armed or nothing is
regenerated, so those bodies are unchanged.

### rollback --list output

`sudo ktuner rollback --list` previews what a rollback would restore — read-only, nothing is
written or deleted (the ledger is 0600 under a 0700 root-owned dir, so it shares rollback's
root gate; a corrupt ledger surfaces as an error rather than an empty list). Each entry keeps
its recorded `param`/`applied`/`previous`, carries the `category` the recording rule classified
it as (the same lowercase spelling `check` reports; `null` for a record written before the field
existed or one a library import supplied, which no category selects), and adds what the kernel
holds right now: `live`, read from the entry's own path through the same reader every other
surface uses (the bracketed option
of a sysfs list, the single-space form of a multi-value sysctl), and `drifted`, whether `live`
still matches `applied` — compared the way a write is verified, so a value the kernel renders in
its own shape (a bracketed option list, TAB-separated multi-value, a leading-token echo) is not
drift, and `cleared_by`:

```json
{"count": 3, "pending": [
  {"applied": "none", "category": null, "cleared_by": null, "drifted": null, "live": null, "param": "block/sda/scheduler", "previous": "mq-deadline"},
  {"applied": "0", "category": "performance", "cleared_by": null, "drifted": true, "live": "20", "param": "vm.dirty_ratio", "previous": "20"},
  {"applied": "1", "category": "performance", "cleared_by": null, "drifted": false, "live": "1", "param": "vm.swappiness", "previous": "60"}
]}
```

An entry the kernel zeroed as a side effect of its mutually exclusive twin records
`applied = "0"` — the value the kernel put live — so it is drifted once it is no longer 0, and
`cleared_by` names the parameter whose write put that 0 there, in the spelling the clearer's own
row publishes as `param`, so the two rows reconcile without knowing the pair table. Without it
the row reads as though ktuner had chosen to disable a knob nobody mentioned. It is `null` when
the ledger records no clearer for the zero: a 0 the caller asked for, and a twin whose clearer
this listing does not carry — the ledger above has no `vm.dirty_bytes`, so its `vm.dirty_ratio`
names nobody. The key is present on every entry, like `live`, `drifted` and `category`. The
`category` is never re-derived from the parameter name (a security finding can sit on a
`net.*` knob), so a record written before the field existed, or imported, reports `null` rather
than a guess, exactly as a path that cannot be read (a device that is gone, a module that is not
loaded, a write-only tunable) reports `live: null` and `drifted: null`: an unreadable value is
not an error and drift never changes the exit code. The listing is a snapshot — the kernel can
change between the preview and the rollback. Plain `ktuner rollback` is unchanged: it restores,
finalizes the ledger, and cleans up.

`sudo ktuner rollback --list <param> [<param>…]` narrows the preview to what
`rollback <param> [<param>…]` would restore. The names resolve exactly as the restore resolves
them (the same spellings, the same lookup), so the listing is each named entry plus the mutually
exclusive twin the ledger recorded for it, which the restore takes along; an entry named twice,
or under two spellings, is listed once. The body has the same keys and entry fields as the
unfiltered listing, in the same param order — `rollback --list vm/swappiness vm.dirty_bytes`:

```json
{"count": 3, "pending": [
  {"applied": "1073741824", "category": "performance", "cleared_by": null, "drifted": false, "live": "1073741824", "param": "vm.dirty_bytes", "previous": "0"},
  {"applied": "0", "category": "performance", "cleared_by": "vm.dirty_bytes", "drifted": false, "live": "0", "param": "vm.dirty_ratio", "previous": "20"},
  {"applied": "1", "category": "performance", "cleared_by": null, "drifted": false, "live": "1", "param": "vm.swappiness", "previous": "60"}
]}
```

Both halves of the pair come along, so the zeroed half's `cleared_by` names a row this listing
carries — naming either half previews the same pair, the one a rollback of it would restore.

A name the ledger does not record is the same command error the restore reports (`2`, stderr
JSON, nothing on stdout), so a preview never promises a rollback that would refuse; with no
ledger every name is unrecorded. Nothing is written in either case. Before this, `--list` with
any parameter was a usage error.

`sudo ktuner rollback --list --drifted` narrows that listing to the entries whose `drifted` is
`true` — the changes something else undid. The shape is unchanged and `count` is the filtered count,
so the command answers "what did I lose?" directly. An entry whose path could not be read (`drifted:
null`) is **not** listed: an unreadable value is not evidence of drift, and the plain `--list` still
shows those entries. With names, the names narrow the preview first and the drift filter applies to
what remains. `--drifted` without `--list` is a usage error (stderr JSON, exit 2), refused before
the root gate like the other argument errors.

### rollback --exclude output

`sudo ktuner rollback --exclude <param> [<param>…]` (repeatable) restores every recorded entry except the
named ones, in one transaction: one ledger lock, one regeneration of the persisted file from the entries
that remain. The body keeps the full-rollback keys, and an `--exclude` name that addresses no ledger entry
is not an error — it is echoed in `unmatched_exclude`, in the order given, so an inert exclusion is visible
instead of silent (the same rule `tune --exclude` applies):

```json
{"failed": 0, "restored": 3, "skipped": 0, "status": "Full"}
```

An excluded entry stays in the ledger, exactly as the entries a `rollback <param>…` does not name stay in
place. The mutually exclusive twin is the same restore unit it is everywhere else: an exclusion on either
half keeps the whole unit — the excluded entry and the half the ledger recorded with it both stay in place —
because writing the free half would zero the excluded one in the kernel (`v6.6 mm/page-writeback.c:518`,
`v6.6 mm/util.c:810`) and leave the ledger describing a kernel it does not match. When nothing is left to
restore — every entry excluded, or every remaining unit kept by an exclusion — the body is the one an empty
ledger answers (`status: "Nothing"`, exit `0`), and the ledger, which still holds the excluded records, is
not deleted.

`sudo ktuner rollback --list --exclude <param>` previews that same set read-only in the `--list` shape,
and `--drifted` still narrows what remains. A name that addresses no ledger entry is echoed there too; with
no ledger every name is unrecorded, so the preview is the empty listing. Restoring named parameters and
excluding named parameters are opposite instructions, so `rollback <param>…` with `--exclude` is a usage
error (`2`, stderr JSON), refused before the root gate like the other argument errors. Plain
`ktuner rollback`, `ktuner rollback --list` and `ktuner rollback <param>…` are unchanged.

### rollback --category output

`sudo ktuner rollback --category <cat> [--category <cat>…]` restores only the recorded entries whose
category matches, keeping every other record in the ledger, and `sudo ktuner rollback --list --category
<cat>` previews that same set in the `--list` shape — each entry carrying the recorded `category`
(`--drifted` narrows what remains). The values are the ones `check` and
`tune` accept — repeatable, the union is kept — and the selection is the one `tune --category <cat>`
would have written: the category is recorded with every entry a rule wrote, because a parameter name
does not say which rule wrote the value. `net.core.bpf_jit_harden` is a security recommendation on a
`net.*` knob, so `rollback --category net` must not touch it. `--exclude` stacks on the selection (the
categories select first, then the exclusions drop, as in `tune --category … --exclude …`), and its
inert-name report is the one `rollback --exclude` already answers:

```json
{"failed": 0, "restored": 1, "skipped": 0, "status": "Full", "unclassified": ["vm.vfs_cache_pressure"]}
```

A record with no category — a ledger written before categories existed, or one a library import wrote —
is selected by no value, and the classification is never guessed from the parameter name. Those records
are echoed by name in `unclassified`, in param order, and only when the list is not empty:

```json
{
  "count": 1,
  "pending": [
    {
      "applied": "1",
      "category": "performance",
      "drifted": false,
      "live": "1",
      "param": "vm.swappiness",
      "previous": "60"
    }
  ],
  "unclassified": [
    "vm.vfs_cache_pressure"
  ],
  "unmatched_exclude": [
    "vm.typo"
  ]
}
```

When the selection is empty — no recorded entry of the named categories, or every selected unit kept by
an exclusion — the body is the one an empty ledger answers (`status: "Nothing"`, exit `0`), and the
ledger, which still holds the other records, is not deleted. Selecting named parameters and selecting by
category are opposite instructions, so `rollback <param>… --category <cat>` is a usage error (`2`, stderr
JSON), refused before the root gate like the other argument errors; an unknown category is refused there
too, with the message `check` and `tune` answer. Plain `ktuner rollback`, `ktuner rollback --list`,
`ktuner rollback <param>…` and `ktuner rollback --exclude …` are unchanged, and a ledger read by this
version keeps the bytes it always had: the category field is only ever written for entries a rule
recorded.

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

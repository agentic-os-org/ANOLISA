# ktuner

ktuner is a deterministic kernel-tuning engine for AI agents. It evaluates 208 rules against the running system and outputs structured JSON recommendations, so an agent (or a human) can diagnose, apply, and roll back kernel parameter changes safely.

---

## Overview

ktuner is a rule engine, not an LLM: every recommendation comes from a hard-coded rule reading `/proc/sys` and `/sys`, so results are reproducible and explainable. It covers network, memory, I/O, CPU, and security parameters, scores the current system, and predicts the score after tuning.

It is designed to be driven by cosh and other ANOLISA-compatible agents as a tool, but the CLI is equally usable by hand.

Disk `rq_affinity` advice respects `threadirqs` boot parameters and PREEMPT_RT. Quoted parameter values do not introduce flags or a `--` separator; parameters after the actual separator are arguments to init.

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
ktuner check --category net --category mem   # categories may be repeated; the union is kept
ktuner check --conservative    # high-confidence recommendations only
ktuner check --only vm.swappiness   # report only the named parameters
ktuner check --exclude vm.dirty_ratio   # report everything but this one

# Preview changes without applying (dry-run)
sudo ktuner tune --dry-run

# Apply recommendations (requires root)
sudo ktuner tune               # apply all
sudo ktuner tune --conservative
sudo ktuner tune --exclude vm.dirty_ratio   # apply all but this one
sudo ktuner tune --only vm.swappiness       # apply only the named parameters

# Fix a single parameter
sudo ktuner fix vm.swappiness
sudo ktuner fix vm.swappiness --dry-run   # preview one parameter, no changes

# Explain why parameters should change — several share one diagnosis
ktuner why net.core.somaxconn vm.swappiness

# Undo all changes ktuner made, or the recorded parameters named
sudo ktuner rollback          # destructive + terminal: restores and deletes the ledger
sudo ktuner rollback --list   # read-only preview of what rollback would restore
sudo ktuner rollback --list vm.swappiness   # preview only what rolling back vm.swappiness would restore
sudo ktuner rollback vm.swappiness net.core.somaxconn   # restore the recorded parameters named
sudo ktuner rollback --exclude vm.swappiness   # restore everything but this recorded parameter
sudo ktuner rollback --list --exclude vm.swappiness   # preview that same set
sudo ktuner rollback --category net   # restore only the recorded entries of this category
sudo ktuner rollback --list --category net   # preview that same set
```

`sudo ktuner fix <param> --dry-run` previews that one write in the same shape
as `tune --dry-run` (`dry_run`, `status`, `blocked`, `would_apply`,
`would_skip`, plus `would_clear` on either preview when a write zeroes a
mutually exclusive twin) and exits `0` without writing anything, so a
parameter can be inspected before it is changed. It needs no root, and refuses
exactly like `sudo ktuner fix <param>` — same stderr JSON, same exit code —
when the parameter is outside the plan, unwritable, or runtime-dangerous, so
the preview cannot disagree with the command it previews.

A real `fix` answers with `fixed`, the name as typed, and `param`, the ledger
key the write was recorded under — the key `rollback --list` lists and
`rollback <param>` answers with. The two differ when the parameter was named by
an alias such as `vm/swappiness`.

A real `tune` or `fix` names the mutually exclusive twins its writes zeroed in
`cleared`, in the shape of `would_clear`: exactly the twins the ledger recorded
for `rollback` to restore. The key is absent when nothing was cleared.

A real `tune` also names what it wrote: `written` lists one entry per write
with the parameter, `previous` (the original the ledger recorded, which
`rollback` restores) and `applied` (the value the kernel took), the pair a
single `fix` already reports. The key is always present; a run that wrote
nothing answers an empty list, and a write-only tunable reports a null
`previous` because its write cannot be recorded.

All output is JSON on stdout; errors are JSON on stderr. Exit codes: `0` success,
`1` check found recommendations or rollback left values unrestored, `2` command error.
Rollback returns `1` for failed writes or missing paths, including partial restoration;
its JSON counts remain on stdout and the ledger is kept for retry. The body also names
what it restored — `restored_params` lists the ledger keys that landed, which is not
the set the caller named: a mutually exclusive twin comes with the entry that clears
it, and `--exclude`/`--category` expand their selection inside the engine — and what it
left: `failures` carries one `{"param", "error"}` per entry whose restore did not land,
and `missing` lists the entries whose recorded path is gone. All three keys appear only
when they have something to say, so a rollback that restored and left nothing keeps the
four counters it always had, and every invocation shape reports them the same way. An empty ledger
is a successful no-op (`0`); an unreadable or missing ledger is a command error (`2`).
`sudo ktuner rollback <param>` follows the same contract for the entry it names —
plus the mutually exclusive twin recorded with it, restored together because writing
either knob zeroes the other — and reports that entry as `param` in its JSON; a
parameter the ledger does not record is a command error (`2`).

Several parameters are accepted and undone in one run: the whole batch happens under
one ledger lock and the persisted file is regenerated once from the entries that
remain, so two parameters cannot leave a half-written state between them. The body
keeps the aggregate counters and replaces `param` with `params`, the ledger key each
parameter resolved to in the order given and deduplicated. Each name is answered by
the same contract as a single one; a batch that restores only part of what it named
retires the entries that landed, keeps the rest for a retry and exits `1`, and one
name the ledger does not record refuses the whole command before anything is written
(`2`), the same command error a single parameter gets.

`check --only <param>` (repeatable) reports only the named recommendations and
`check --exclude <param>` (repeatable) reports everything but them. The names
resolve exactly as `fix`/`why` resolve them, and the selection applies after
the `--category`/`--conservative` filters. A name that matches no
recommendation in scope is not an error: it is echoed in `unmatched_only`
(or `unmatched_exclude`) in the spelling given — an empty report echoes every
given name. `score` and `total_checked` describe the system and do not move;
`predicted_score`, `counts` and the exit code follow the report that is left,
exactly as `--category` has always done, so a selection that leaves nothing to
report answers with an empty list and exit `0` (the diagnosis has no blocked
state). The two flags are mutually exclusive, and a run carrying both is a
command error (`2`, stderr JSON), decided before the system is read.
The report's `counts` object counts the recommendations it carries:
`performance` and `security` by kind, `high_confidence`, `writable`, and one
count per category — `network`, `memory`, `io` and `cpu` — each the number of
recommendations `check --category` with that name keeps (security findings
are counted once, under `security`). Like the other counts they follow the
report that is left after `--category`, `--only` and `--exclude`.
The report's `system` object describes the machine itself: `kernel`,
`cpu_cores`, `memory_gb` and `numa_nodes` as before, plus `os_distro`,
`cpu_model`, the `disks` inventory (one entry per disk, carrying the fields
the disk rules gate on — `hidden`, `holders`, `available_schedulers`) and
the `network` inventory (`name`/`speed_mbps` per interface, where
`speed_mbps: 0` means the speed is unknown, not zero). `cpu_cores` and
`memory_gb` are the effective values the rules scale by, clamped to the
cgroup limits binding the process; `cpu_cores_host` and `memory_gb_host` are
the host's own readings — equal unless a limit binds, and larger when one
does.
`ktuner why <param>…` explains one or several parameters. A single parameter keeps
its flat body: a recommendation with the fields `check` publishes for it (`skip_reason`
when no write path would take it), or `{"param": …, "current": …, "status": "optimal"}`
for a parameter already at its recommended value. Two or more parameters are answered
in one run, from one diagnosis — running the command once per parameter gathered the
whole system each time. The body carries `count` and `params`: one entry per parameter,
in the order given and deduplicated by the same alias policy `fix` uses
(`vm/swappiness` and `vm.swappiness` are one parameter; sysfs identities such as
`block/sda/scheduler` stay verbatim), and each entry is exactly the object that
parameter answers alone. `why` exits `1` when at least one entry is a recommendation
and `0` when every entry is optimal; a parameter that cannot be found or read refuses
the whole command with its own error — exit `2`, stderr JSON naming the parameter,
nothing on stdout — so a batch never answers half a question.
`tune --exclude <param>` (repeatable) leaves a named recommendation out of the plan:
nothing is written for it, nothing is recorded in the rollback ledger, and nothing is
persisted. It applies after the `--category` and `--conservative` filters, and the
excluded entry is listed in the output (`would_skip`, reason `excluded`); a name that
matches no recommendation in scope is reported in `unmatched_exclude` instead of
failing the run (on an empty plan, every given name). Excluding every recommendation
is a `blocked` run with exit code `1`, like any other plan with nothing applicable. Excluding one half of a mutually exclusive
sysctl pair does not stop the kernel from zeroing it when the other half is written;
ktuner records the cleared original so `rollback` can restore it.

`tune --only <param>` (repeatable) is the mirror of `--exclude` — the two are
mutually exclusive, and a run carrying both is a command error (`2`). Only the named
recommendations stay in the plan; every other in-scope entry is listed in `would_skip`
with reason `not_selected`, and a name that matches nothing in scope is echoed in
`unmatched_only` instead of failing the run (selecting nothing is a `blocked` run,
exit code `1`). Like `--exclude`, it filters the plan only: it does not stop the
kernel from zeroing a mutually exclusive twin when the other half is written.

---

For network conf parameters, interface identity is case-sensitive. Both `net/ipv4/conf/Br0.100/forwarding` and `net.ipv4.conf.Br0.100.forwarding` address the same interface; `br0.100` is a different identity. IPv6 follows the same rule. Persisted records for interfaces with literal dots use slash-first sysctl.d keys so systemd preserves those dots. This supports existing valid records or custom library recommendations; built-in rules do not currently generate per-VLAN recommendations.

`sudo ktuner rollback --list` reports the live state as well: each pending entry adds `live` (the value read from the entry's path right now, or `null` when the path cannot be read — a device that is gone, a module that is not loaded), `drifted` (whether `live` still matches the recorded `applied`; `null` when there is no live value to compare), `category` (the rule-layer category the entry was recorded with — the same lowercase spelling `check` uses; `null` when the record carries none, e.g. a record written before the field existed or one an import supplied), and `cleared_by` (the ledger key of the entry whose write zeroed this one when the kernel did that as the side effect of a mutually exclusive pair — spelled the way that entry's own row publishes its `param`, so the two reconcile; `null` for an entry no recorded twin cleared, which is a zero the caller asked for, and for a twin whose clearer this listing does not carry). A row with an `applied` of `0` that nothing asked for would otherwise read as though ktuner had chosen to disable the knob. These fields are additions to the existing shape — `count`, `pending` and the recorded `param`/`applied`/`previous` values are unchanged — and an unreadable value is not an error: the command still exits `0`. Values are rendered and compared the same way as everywhere else in ktuner, so a sysfs option list or a multi-value sysctl is not mistaken for drift. `sudo ktuner rollback --list --drifted` narrows that listing to the entries whose recorded value no longer matches the kernel — the changes something else undid; entries whose path cannot be read are not listed (the plain `--list` still shows them), and `--drifted` without `--list` is a usage error.

`sudo ktuner rollback --list <param>…` narrows that preview to what `rollback <param>…` would restore: each named entry plus the mutually exclusive twin recorded with it, in the same shape and order as the full listing — both halves of a pair come along, so a zeroed half's `cleared_by` names a row this listing carries. A name the ledger does not record is the same command error (`2`) the restore reports.

`sudo ktuner rollback --exclude <param>…` restores every recorded entry except the named ones in one transaction, and `sudo ktuner rollback --list --exclude <param>…` previews that same set (with `--drifted` narrowing what remains). An excluded entry stays in the ledger, and so does the half of a mutually exclusive pair recorded with it — writing either knob zeroes the other in the kernel (`v6.6 mm/page-writeback.c:518`, `v6.6 mm/util.c:810`), so the pair is one restore unit and an exclusion on either half keeps it whole. The body keeps the full-rollback keys; an `--exclude` name that addresses no ledger entry is echoed in `unmatched_exclude` instead of failing the run, exactly as `tune --exclude` reports an inert exclusion. With every entry excluded nothing is restored, the body reports the empty-ledger answer (`status "Nothing"`, exit `0`), and the ledger keeps the excluded records. `rollback <param>…` and `--exclude` are opposite instructions, so carrying both is a usage error (`2`, stderr JSON).

`sudo ktuner rollback --category <cat>…` restores only the recorded entries whose category matches
(the values `check` and `tune` accept, repeatable as a union), and `sudo ktuner rollback --list
--category <cat>` previews that same set with `--drifted` narrowing what remains. The selection matches
what `tune --category <cat>` would have written, because the category is recorded with each entry a rule
wrote — a name cannot say whether the rule behind it was a security one (`net.core.bpf_jit_harden` is a
security finding on a `net.*` knob), so the selection never guesses from the parameter. `--exclude`
stacks on the selection, selecting by category first and dropping the exclusions after, and reports an
inert name the way `rollback --exclude` does. A record with no category (a ledger written before the
field existed, or a library import) is selected by no value at all and is echoed by name in
`unclassified`. An empty selection restores nothing, answers the empty-ledger body (`status "Nothing"`,
exit `0`), and leaves the ledger in place; `rollback <param>…` with `--category` is a usage error (`2`,
stderr JSON), and an unknown category is refused with the message `check` and `tune` answer.

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

- [OS Skills](../os-skills/index.md)
- Full reference: `src/ktuner/README.md`

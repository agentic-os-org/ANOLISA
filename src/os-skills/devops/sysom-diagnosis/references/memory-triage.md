# Memory Triage

Use this reference after `sysom-osops memory classify` or when the user provided
fresh SysOM memory output.

## Agent-Visible Evidence

The answer must come from `agent.summary`, `agent.findings[].detail`,
`agent.findings[].category`, and `agent.next_steps[]`. If a required entity is
only present in backend structured evidence, the command output should be treated
as incomplete for Agent diagnosis and the next focused SysOM action should fill
the missing entity.

Use the visible envelope to check entity completeness, not to run a fixed memory
playbook. A memory answer is complete when the current envelope names the
material memory object, the owner or scope, the mechanism reported by SysOM, and
the safe action target.

| Entity gap in visible output | Completion signal |
|------------------------------|-------------------|
| Event or pressure source | selected event, victim or affected scope, and limit/current or equivalent pressure source |
| Holder attribution | PID or command, real executable when available, and service or cgroup when SysOM can resolve it |
| Object attribution | file path, shared-memory object, socket, cgroup, or kernel memory class with magnitude |
| Ownership transition | original owner, current holder when relevant, residue or refcount, and cleanup order |
| Mechanism and action target | memory mechanism, currentness qualifier, and cleanup, throttling, or configuration target |

## Decision Discipline

Use this reference to check whether visible SysOM output is complete enough to
answer. It should not override `agent.next_steps[]` or invent a fixed command
sequence.

- Prefer a visible `category=root_cause` finding, then highest severity, then the
  finding that best matches the user's symptom.
- Run another SysOM command only when the current `detail` is missing a required
  Agent-visible entity and the command can fill that named gap.
- Preserve ownership qualifiers from the envelope. Original owner, current
  holder, residue type, and cleanup target are complementary when SysOM ties them
  to the same symptom.
- Do not run broad overview commands after holder, magnitude, mechanism,
  ownership, and safe next action are already visible.
- Do not perform cleanup while diagnosing. Commands that kill a holder, remove a
  file, change VM tunables, or drop caches are remediation steps; recommend them
  in the answer only when the user has not explicitly requested execution.
- Do not add raw Linux verification commands for an already-closed holder,
  module, file, cgroup, event, or limit. If remediation needs ownership or
  dependency review, state that as a review gate, not as a default command
  checklist.
- In the final answer, avoid executable shell snippets for cleanup or
  verification. State the object, owner/scope, dependency gate, and SysOM
  metric/action to re-check instead.
- Do not write default final-answer steps as module inspection/removal commands,
  memory summary commands, cgroup file writes, cache-drop controls, sysctl
  changes, or process-kill actions.

## Focused Action Map

Only choose a deep memory action from a missing Agent-visible entity already
named by SysOM output. Do not infer the route from the user's symptom wording
alone.

| Missing entity named by SysOM | Public action that can fill it |
|-------------------------------|--------------------------------|
| event, victim, scope, limit/current, dominant charge, or remediation level | `sysom-osops memory oom` |
| process identity, real executable, cgroup, service, RSS, or swap holder attribution | `sysom-osops memory process` |
| memory composition, kernel/userspace split, reclaim pressure, slab, socket, or broad subsystem attribution | `sysom-osops memory memgraph` |
| socket holder, state, queue, PID, cgroup, or service attribution | `sysom-osops memory memgraph --enable-socket` |
| cached file, cache owner, magnitude, holder, or cleanup target | `sysom-osops memory filecache` |
| shared-memory object, holder, cgroup, service, or cleanup target | `sysom-osops memory shmem` |
| cgroup ownership transition, original owner, refcount or residue, current holder, or cleanup order | `sysom-osops memory memcgoffline` |
| kernel-hidden growth (vmalloc/page-allocator/slab/percpu) where memgraph only closes to a candidate and the leaking call point/function/module is missing | `sysom-osops memory memleak` |
| Java heap or GC memory entity | `sysom-osops memory javamem` |

Preserve the cleanup order reported by SysOM. If SysOM separates original owner,
current holder, affected resource, and cgroup reference release, keep those
qualifiers distinct. Once the visible output contains the missing entity, answer
without re-checking the same PID, cgroup, service, file, event, or memory totals
with raw Linux commands.

## Other Memory Follow-Ups

Use the focused SysOM action that fills the missing Agent-visible entity named by
the current envelope. Typical gaps are holder identity, executable, cgroup or
service attribution, memory composition, socket queue ownership, currentness, or
a durable cleanup or throttling target.

Stay in memory only while visible SysOM output names a memory-domain missing
entity that one focused memory action can close. Pivot to IO, load, network, or
Java when the current memory envelope is healthy or inconclusive and another
domain names a stronger concrete root cause.

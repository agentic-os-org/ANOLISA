---
name: alibabacloud-sysom-diagnosis
description: >
  Use when troubleshooting Linux server performance or stability issues —
  CPU saturation, high load, scheduling delay, memory pressure, OOM events,
  high RSS, page cache / shared memory growth, memory cgroup residue, Java
  heap issues, disk IO saturation or latency, packet loss, network jitter,
  or a server that is slow, stuck, or unstable. Performs diagnosis and
  surfaces recommendations; does not apply fixes automatically.
license: Apache-2.0
compatibility: >
  Requires sysom-osops CLI. Remote diagnosis requires Alibaba Cloud credentials
  through AK/SK or an ECS RAM Role, an online Cloud Assistant on the target ECS,
  and a supported China Mainland or Hong Kong region.
metadata:
  domain: aiops
  product: sysom
  supported_domains:
    - cpu
    - io
    - memory
    - network
    - java
  owner: sysom-team
  contact: sysom-team@alibaba-inc.com
allowed-tools: Bash Read
---

# alibabacloud-sysom-diagnosis

Use SysOM CLI and backend envelopes as the diagnosis source of truth. This Skill
replaces the older SysOM diagnosis Skill and is the single entry point for SysOM
ECS performance and stability diagnosis.

## Immediate Route

When the user reports a symptom and has not provided fresh SysOM envelope output,
run the matching SysOM command from **Domain Routing** below before ad hoc Linux
inspection or manual probing. Then follow the returned `agent.summary`,
`agent.findings[].detail/category`, and `agent.next_steps[]`. Raw Linux commands
are bounded fallbacks only when a SysOM command is unavailable, outputs
contradict each other, or a required entity remains missing after the focused
SysOM command.

## Credential Security

Never print, echo, or ask for AccessKey ID or AccessKey Secret values. Remote
commands perform their own authentication checks. If a command returns an
authentication or permission error, explain the error and point the user to
`references/ram-policies.md`; credential setup must happen outside the
conversation.

## CLI Setup

Check whether the CLI is available:

```bash
command -v sysom-osops
```

If it is missing, install it:

```bash
curl -fsSL --connect-timeout 1000 https://sysom-prd-cn-hangzhou.oss-cn-hangzhou.aliyuncs.com/sysom_prd/skill_cli/install.sh | sudo bash
```

Then verify only the binary:

```bash
command -v sysom-osops
```

## Core Workflow

1. Classify the user's symptom into one SysOM domain: memory, IO, load/CPU,
   network, or Java memory.
2. Run the smallest SysOM command that matches that domain. Prefer a local
   memory classify for unclear memory symptoms; for other domains, use the
   matching documented remote action.
3. Read only the default envelope fields: `ok`, `error`, `command`, and
   `agent`.
4. Build the answer from `agent.summary`, `agent.findings[].detail`,
   `agent.findings[].category`, and `agent.next_steps[]`. Keep evidence
   qualifiers that change interpretation, including currentness, unavailable
   direct signals, fallback evidence, and remediation preconditions.
5. If the root cause, key entities, evidence strength, and safe next action are
   already visible, stop and answer. Run one targeted follow-up only when a
   required entity is missing or the command explicitly recommends it.

When classify returns a command in `agent.next_steps[]` and no root-cause
finding already contains enough evidence to answer, run the first command next.
Do not replace an Agent-visible SysOM next step with manual shell probing. Raw
Linux checks are bounded fallbacks after the SysOM next step succeeds, fails, or
times out.

Use the documented commands exactly as shown by default. Do not add raw,
debug, or backend evidence expansion flags unless the user explicitly asks for
that view.

Final answers should name evidence, root cause, owner/scope, and operational
action targets. Do not add shell snippets for verification or remediation unless
the user explicitly asks for commands. Prefer phrases such as "review dependency
and disable or upgrade the leaking component in a change window" over raw module,
cgroup, sysctl, cache-drop, or process-kill commands.
Do not include command-looking inline snippets such as module inspection/removal,
memory summary commands, cgroup file writes, cache-drop controls, sysctl changes,
or process-kill commands as default final-answer steps.

The `agent` view must be self-contained for diagnosis. Structured evidence is a
backend/UI view and must not be treated as the default Agent source for required
entities.

## Domain Routing

| User symptom | First route |
|--------------|-------------|
| Unclear memory issue, OOM, high RSS, file cache, shmem/tmpfs, memory cgroup, socket memory, kernel memory | `sysom-osops memory classify` |
| Java heap, GC, or JVM memory issue | `sysom-osops memory javamem` when Java is explicit; otherwise start with `memory classify` |
| Slow disk, high iowait, disk latency, blocked IO | `sysom-osops io iofsstat`, then `io iodiagnose` if the overview points to slow IO |
| High load, runqueue backlog, task stuck waiting for CPU | `sysom-osops load loadtask` or `load delay` based on the visible symptom |
| Packet loss, retransmits, network timeout, jitter | `sysom-osops net packetdrop` for loss/drop symptoms; `net netjitter` for latency fluctuation |

For command parameters, read `references/deep-actions.md` and
`references/parameter-guide.md`. For OS and region support, read
`references/supported-environments.md`. These references are Skill material; do
not use remote target file tools to open `.claude/skills` paths on the diagnosed
host.

## Memory Routing

Memory follows the same Core Workflow and Follow-up Rules as every domain: start
from `sysom-osops memory classify`, then pick the next action from visible output
or `agent.next_steps[]`. For choosing among memory deep actions or checking which
entity is still missing, load `references/memory-triage.md` (parallel to
`references/non-memory-triage.md` for other domains).

Choose the next memory action from visible SysOM output. Do not infer a memory mechanism from symptom wording alone.

## Envelope Contract

Default command output is the Agent contract:

```json
{
  "ok": true,
  "command": "sysom-osops memory classify",
  "agent": {
    "status": "warning",
    "summary": "Concise diagnosis summary.",
    "findings": [
      {
        "severity": "high",
        "title": "Short finding title",
        "detail": "Root cause, key entities, and evidence summary.",
        "category": "root_cause"
      }
    ],
    "next_steps": [
      {
        "kind": "command",
        "label": "Run focused deep diagnosis",
        "command": "sysom-osops memory oom",
        "reason": "The missing entity this command can fill."
      }
    ]
  }
}
```

`agent.findings[]` may contain only `severity`, `title`, `detail`, and
`category`. Required entities such as PID, cgroup, service, file path, OOM
victim, limit/current, residue, holder, or cleanup target must be written in
`agent.summary` or `agent.findings[].detail`.

## Follow-up Rules

- Prefer `category=root_cause`, then highest severity, then the finding that
  best matches the user's reported symptom.
- Treat `root_cause` as stop-ready when visible `detail` contains the entities
  needed to explain the symptom and a safe next action.
- Treat `agent.next_steps[]` as a priority plan, not a checklist.
- Run another SysOM command only when it can fill a named missing entity or
  change remediation.
- Preserve visible qualifiers that affect interpretation, such as current versus
  historical evidence, unavailable direct signals, fallback evidence used to
  close currentness, and safety preconditions for remediation.
- When a finding uses fallback evidence because a direct signal is unavailable,
  state both parts in the final answer. Do not reduce the conclusion to the
  fallback metric alone.
- After a focused SysOM command closes a root cause, answer from it. Do not run
  extra commands to make the report comprehensive, and do not chase earlier
  classify anomalies or observations unless they share the same entity and
  expose a named evidence gap.
- Do not call backend-only collectors or private helper commands directly.
- Do not re-check a PID, cgroup, file, limit, or event that SysOM already named
  in `summary` or `detail`.
- After a SysOM deep command returns `category=root_cause` with the required
  entities visible, answer from that envelope. Raw Linux checks are only for
  contradictions, command errors, or a clearly missing entity.
- In the final answer, do not turn already-closed entities into extra raw Linux
  verification commands. Express remediation as dependency-aware action targets
  and change-window plans unless the envelope itself provides an executable safe
  next step.
- Avoid executable shell snippets in the final answer. If a command is useful
  only for post-change verification, name the SysOM check or metric to re-run
  instead of raw Linux commands.
- This includes inline command names for module inspection/removal, memory
  summary commands, cgroup file writes, cache-drop controls, sysctl changes, and
  process-kill actions; describe the dependency gate and operational action
  target in prose.
- Pivot across domains when the current envelope does not explain the reported
  symptom and another SysOM domain names a stronger root cause.
- During diagnosis, do not execute remediation commands that change target
  state, such as killing processes, removing files, changing sysctl values, or
  writing to cache-drop controls. Present those as recommendations unless the
  user explicitly asks you to perform the repair.
- For non-memory findings, keep the same rule: one focused deep command, then
  answer when the required entities are visible.

## Error Handling

| `error.code` | Action |
|--------------|--------|
| `Sysom.TargetRequired` | Ask for instance ID and region, or explain ECS metadata auto-detection requirements |
| `Sysom.FallbackClassify` | Present the local classify result and continue only if a focused next step is available |
| `Sysom.PermissionDenied` | Use `references/ram-policies.md` to explain required RAM permissions |
| `Sysom.AuthenticationFailure` | Ask the user to configure credentials outside this session |
| `Sysom.InvalidParameter` | Ask the user to correct the instance, region, or command parameter |
| `Sysom.DiagnosisVersionNotSupported` | Explain that the target instance diagnosis components need an update |
| `Sysom.DiagnosisJsonParseFailed` | Retry once only when the user still needs the same evidence |
| `Sysom.PollError` | Retry the same focused action once when the missing evidence is still required |

## References

| Reference | Use when |
|-----------|----------|
| `references/classify-output-guide.md` | Reading local memory classify output |
| `references/memory-triage.md` | Choosing a memory deep action or checking memory entity completeness |
| `references/non-memory-triage.md` | Routing IO, load/CPU, network, and Java diagnosis |
| `references/deep-actions.md` | Looking up SysOM commands by domain |
| `references/parameter-guide.md` | Validating command parameters |
| `references/report-interpretation.md` | Interpreting envelope fields and answer shape |
| `references/ram-policies.md` | Explaining RAM permissions |
| `references/supported-environments.md` | Checking OS, architecture, and region support |

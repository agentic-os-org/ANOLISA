# Deep Actions Reference

This file lists public SysOM commands that this Skill may route to. On an ECS
instance, sysom-osops can auto-detect `--instance` and `--region`; add them only
for cross-instance diagnosis or when auto-detection fails.

## Memory

| Command | Mode | Use when |
|---------|------|----------|
| `sysom-osops memory classify` | Local | First route for unclear memory symptoms, OOM hints, high RSS, cache growth, shmem/tmpfs, memcg residue, or kernel memory suspicion |
| `sysom-osops memory memgraph` | Remote | Full memory landscape is missing after classify, or the issue is broad kernel/userspace memory composition |
| `sysom-osops memory memgraph --enable-socket` | Remote | Socket buffer pressure is visible and socket holder, state, PID, cgroup, or service attribution is missing |
| `sysom-osops memory process` | Remote | A process is the suspected holder but identity, real executable, cgroup, or service attribution is missing |
| `sysom-osops memory oom` | Remote | OOM killer event or memcg/host OOM evidence is visible |
| `sysom-osops memory filecache` | Remote | File/page cache is the dominant unresolved entity and file/holder attribution is missing |
| `sysom-osops memory shmem` | Remote | shmem, tmpfs, memfd, or SysV shared memory holder attribution is missing |
| `sysom-osops memory memleak` | Remote | Kernel-hidden growth (vmalloc/page-allocator/slab/percpu) is the dominant unresolved entity and the leaking call point/function/module is missing after memgraph closed only to a candidate (`--type slab\|page\|vmalloc\|percpu`, default vmalloc) |
| `sysom-osops memory javamem` | Remote | Java heap or GC memory issue is explicit |
| `sysom-osops memory memcgoffline` | Remote | Cgroup ownership-transition evidence is visible in SysOM output and original ownership, residue/refcount, or cleanup order needs attribution |

## IO

| Command | Mode | Use when |
|---------|------|----------|
| `sysom-osops io iofsstat` | Remote | Disk IO overview is needed for high iowait, slow disk, or IO saturation |
| `sysom-osops io iodiagnose` | Remote | Slow IO root-cause attribution is needed after the overview or when latency is the primary symptom |

## Load and CPU Scheduling

| Command | Mode | Use when |
|---------|------|----------|
| `sysom-osops load loadtask` | Remote | Load average, runqueue, or task composition is the primary symptom |
| `sysom-osops load delay` | Remote | Runnable tasks are not getting CPU time, scheduling delay is reported, or processes appear stuck without IO evidence |

## Network

| Command | Mode | Use when |
|---------|------|----------|
| `sysom-osops net packetdrop` | Remote | Packet loss, retransmits, drops, connection resets, or timeout symptoms are primary |
| `sysom-osops net netjitter` | Remote | Latency fluctuation, jitter, or intermittent connectivity degradation is primary |

## Common Notes

- All public commands return the same envelope shape: `ok`, `error`,
  `command`, and `agent`.
- Continue from `agent.next_steps[]` only when a required entity is missing or
  the next action changes remediation.
- Do not call backend-only collectors from the Skill. Public commands above are
  the supported interface.

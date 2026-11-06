# Go / No-Go

**Default observation is denied; production observation and rewrite remain No-Go.**
An ordinary build returns `observation_disabled` before reading a permit, creating
an event file or attaching. Only an explicit `unsafe-lab` build admits disposable
fixtures. Tracer SIGKILL reproducibly terminates synthetic targets with SIGTRAP,
including after arming but before the first TLS write. P0 remains incomplete and
P1 remains disabled. A metadata-only tracer can still disrupt its
target; payload immutability does not establish fault isolation.

| Dimension | Decision | Evidence / remaining condition |
| --- | --- | --- |
| Default admission | Go | Rejected before target access; live fixture completes with verified server bytes |
| Disposable advise experiments | Conditional Go | Real OpenSSL receiver confirms all successful fixture writes; keep bounded isolated targets |
| Single and multithread collection | Go within tested envelope | Existing and post-attach 4/8/16-thread fixtures; exact write/hit counts; capped sustained run |
| Controlled stop and exit | Go within tested envelope | SIGINT/SIGTERM, normal detach, target kill, rejected debugger and unsupported exec/fork cleanup |
| Forced tracer death | **No-Go** | Five deterministic armed/hit phases fail under SIGKILL; no target-side handler masks it |
| Handled-error recovery | Go within tested envelope | Partial installation, inherited thread state and exiting-thread ESRCH cleanup; pending hardware traps consumed before detach |
| Thread churn / group stop | Go within tested envelope | Four thread waves, busy attach, SIGSTOP/SIGCONT and detach preserving group stop |
| Performance | Conditional | Measured p99 is below the initial 5 ms goal; throughput drops, idle polling CPU and worst-case pauses remain material |
| Process ownership | Go for private fixture import | Real AW instance/scope, stale identity rejection, ambiguity preserved; production revocation/export remains absent |
| Semantic coverage | Transport only | Unknown coverage; no model/tool boundary, request completeness or send-result claim |
| Privilege / integration | Conditional | Same-user experimental permit and pidfd; existing kernel policy unchanged; sibling demo requires policy permitting attachment |
| Existing uProbe / other ABI | Unverified | Existing int3 is rejected; no live AgentSight probe environment exercised; static/other TLS unsupported |

## Read-only uProbe follow-up

**Conditional Go for further BPF Source development.** The count-only
`BPF_TRACE_UPROBE_MULTI` experiment passes 112 functional/fault cases, 24
performance cases and four independent-probe coexistence cases. All 28 collector
SIGKILL cases preserve target completion and receiver bytes. Existing and future
threads count exactly in the tested fixtures; another unauthorized same-UID
fixture contributes zero hits. Details and performance costs are in the
[report](POC_REPORT.md#read-only-uprobe-comparison).

Production deployment remains No-Go: bounded payload events, drops, concurrent
buffer consistency, busy-process coverage, actual PID reuse, live AgentSight
coexistence and AW correlation for this Source remain unverified. The privileged
helper is an operator-run lab harness, not a production broker. Count-only
performance does not predict full capture costs. The original ptrace forced-death
failure remains unchanged; this read-only mechanism cannot justify enabling P1.

Minimum next work: implement bounded transport metadata/capture with explicit
loss and unknown coverage; repeat fault isolation and shared-buffer tests;
reuse the real AW fixture importer; then review the read-only identity/revocation
export and query/projection Provider. Keep Native Hook/SDK preferred for semantic
boundaries. No production-enable or serviceization step follows from these results.

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

The next decision should compare a read-only eBPF Source, an explicit TLS/proxy
endpoint and Native Hook/SDK on the same fixture. Native Hook/SDK offers the
clearest application semantics. eBPF avoids this ptrace-stop failure mode but
requires separate privilege and coverage validation; a proxy requires explicit
network/TLS integration. A supervisor alone does not prove that killing the
actual ptrace owner is safe.

Minimum follow-up: choose the fault-isolated Source mechanism and repeat forced
death, existing-probe tests and broaden busy-thread coverage; define the read-only AW
identity/revocation export; add the bounded query/projection Provider. Only after
P0 safety passes should equal-length rewrite be implemented and checked against
receiver-confirmed bytes. There is no serviceization or production-enable step
authorized by these results.

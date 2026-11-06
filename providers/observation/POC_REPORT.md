# AW ptrace Runtime Hook POC — measured results

[中文版](POC_REPORT_zh.md)

On 2026-11-06, the independent advise Sidecar and real AW lifecycle demo ran on
Linux x86-64. Default builds refuse observation because forced-death isolation fails. Native acceptance finished with **55 PASS, 5 FAIL, 4 SKIP**. All five
failures were active tracer SIGKILL cases: the target terminated with SIGTRAP.
**P0 is not accepted; P1 was not implemented or run. Production observation and
rewrite are No-Go.** [GO_NO_GO.md](GO_NO_GO.md) records the decision and next work.

## Tested objects and environment

The base and reference commits are fixed in [SOURCE.md](SOURCE.md). AW CLI was
built from that main checkout and used as an ordinary same-user process. No AW
framework, wire contract, configuration schema, capability matrix or installer
was changed. The independently locked Rust package lives entirely under
`providers/observation/`.

Native environment: x86-64, kernel `6.6.102-5.3.1.alnx4.x86_64`, ordinary UID 1000,
pre-existing Yama `ptrace_scope=0`, OpenSSL 3.0.12, GCC 12.3.0, Python 3.11.6 and
Rust 1.97.1. Local development host: AArch64. Its CLI rejection was exercised and
returned `unsupported_architecture` before opening event output. Native ptrace
acceptance ran on the x86-64 lab only. No global ptrace/security setting changed.

The C client sends synthetic repeated bytes to a loopback TLS receiver. Each
worker owns a connection and buffer. The receiver verifies total bytes and
SHA-256 after a complete bidirectional TLS close. These are synthetic OpenSSL
results; real Agent traffic, model accounts and Native SDK adoption were unused.

## Admission and race recovery

The default `observe` command now returns `observation_disabled` before reading
a permit, creating output or attaching. This is the current fail-closed boundary.
Collection requires an explicit `unsafe-lab` build, which labels its hazardous
experimental status. P1 remains disabled.

Cleanup ownership is recorded before the first debug-register write and inherited
by tracked clone/fork children. ESRCH from an exiting thread no longer aborts
cleanup of other threads. Initial readiness compares actual TID sets. Group stops
use LISTEN until SIGCONT; detach preserves the existing group stop. Register-read
errors retain cleanup responsibility instead of forwarding an unclassified trap.

Stress diagnostics captured an interrupt stop with a hardware SIGTRAP still in
the per-thread pending queue. Clearing DR7 and immediately detaching let that
signal reach the target. Cleanup now uses PTRACE_PEEKSIGINFO after disabling
breakpoints, consumes the pending signal's delivery stop under ptrace, then
suppresses the owned signal and detaches. The final 100-run regression exercised
three actual pending hardware traps.

| Added acceptance | Final result |
| --- | --- |
| Default refusal, three runs | 3 PASS; no event file or tracer ownership; receiver verifies bytes |
| Nine handled-error phases × 1/4/16 threads × three runs | 81 PASS; target continues, receiver verifies bytes, resources reaped |
| Actual SIGKILL at those phases | 36 PASS, 45 FAIL; every failure is fatal target SIGTRAP |
| After-continue error, 16 threads, 100 runs | 100 PASS, including pending hardware signal consumption |
| Thread churn, busy attach, group resume and group-stop detach | 16 PASS across 1/4/8/16 threads; exact hits during fully observed four-wave churn |
| Original matrix rerun | 55 PASS, 5 FAIL, 4 SKIP; failures remain tracer SIGKILL |
| Real AW demo rerun | 400 associated and 100 unregistered events; bindings zero after owner exit |

The 45 deterministic deaths cover five phases, nine runs each: all threads armed,
trap stop, RF set, before CONT and after CONT. In the armed-before-first-write
case, TracerPid was zero after tracer death but the next write still killed the
target. Automatic ptrace unlink therefore does not restore this implementation's
debug state. Partial-installation checkpoints touch only the main thread; their
passing outcomes do not establish safety for active worker threads.

**Handled-error recovery passes within the stated experiment; actual tracer
SIGKILL remains unsafe.** Default refusal does not count as P0 acceptance. No
EXITKILL policy, target SIGTRAP handler or global security change is used. Original
failures and corrected runs remain in task evidence; current numerical results
are in [measurements/20261106.json](measurements/20261106.json).

## Acceptance matrix

| Case | Actual result |
| --- | --- |
| 1/4/8/16 existing threads, 3 repetitions | Exact 300/1200/2400/4800 write hits per run; server digests verified; clean detach |
| Threads created after attach | 4/8/16 threads, 400/800/1600 hits; TRACECLONE initialization and cleanup pass |
| SSL_write_ex | 100 entries; receiver confirms bytes |
| 0 B / 4096 B / 4097 B | Correct lengths; 4097 B has null digest and explicit coverage gap |
| Fragmented writes | 500 writes of 7 bytes; reported as writes, no request reconstruction |
| Event/rate caps | Drops counted; `unknown` coverage and explicit limit reason |
| Sustained bounded run | 65,536 hits; 10 retained events, 65,526 drops; receiver confirms 8,388,608 bytes |
| Normal detach / SIGINT / SIGTERM | Target continues; TracerPid becomes zero; receiver verifies payload after early detach |
| Foreign signal | SIGUSR1 reaches the fixture handler exactly once |
| Target SIGKILL / owner exit | Observer terminates; AW bindings reach zero after owner exit |
| Tracer SIGKILL during writes | **5/5 FAIL**: target exits with signal 5 (SIGTRAP); no target-side trap handler hides the failure |
| Stale start/namespace/boot/exe/fingerprint | Rejected before attachment; UID and executable inode mutation also covered by unit tests |
| No debug permission / second debugger | Explicit rejection; target and first debugger remain under their original ownership |
| Unsupported TLS / offsets / enforce | Explicit unsupported diagnostics; no fallback offsets or payload edits |
| Exec / fork | Observation rejects unsupported transition and detaches; exec target remains alive; fork fixture completes normally |
| Real AW instance and Scope | CLI register → pending_ready → ready → open_scope → acknowledge; imported live ownership matches |
| Same PID, two AW instances | Association remains ambiguous; instance/scope IDs stay null |
| Unregistered process | Observed/unassociated; no Ready, tool ID or Scope authority invented |
| Existing live uProbe | **SKIP**: no existing authorized AgentSight/uProbe environment available in this lab; entry int3 is rejected by code |
| Actual kernel PID wraparound | **SKIP**: stale identity mutation is tested; PID reuse was not forced |
| Other TLS implementations / static ABI | **SKIP**: unsupported; no compatibility claim |
| P1 equal-length rewrite | **SKIP**: failed crash-safety prerequisite; enforce mode is rejected |

Normal fixture write failures were zero and uncapped observation hit rate was
100%. Every transport event still has unknown semantic coverage and unknown
send adoption; the independent synthetic receiver supplies the separate byte
confirmation. Interrupted observers intentionally cover only their active window.

## Performance

128-byte writes, 300 writes per thread, three baseline and three traced runs for
each concurrency. Values below are medians of per-run quantiles; maximum is the
largest maximum across those runs. Throughput includes loopback TLS setup/close,
so it is a comparative fixture result rather than a general OpenSSL capacity test.
Raw numerical values and source digests are in [measurements/20261106.json](measurements/20261106.json).

| Threads | Baseline write p99 µs | Traced write p99 µs | Tracer pause p50/p95/p99/max µs | Baseline → traced writes/s | Peak RSS KiB | Tracer CPU seconds |
| --- | ---: | ---: | --- | --- | ---: | ---: |
| 1 | 6.21 | 250.63 | 53 / 64 / 75 / 126 | 6582 → 4174 | 3244 | 0.02 |
| 4 | 8.12 | 426.13 | 97 / 185 / 221 / 466 | 19346 → 13997 | 3244 | 0.07 |
| 8 | 12.43 | 993.38 | 206 / 417 / 487 / 752 | 31320 → 15454 | 3256 | 0.14 |
| 16 | 9.48 | 1891.33 | 433 / 860 / 965 / 1353 | 51436 → 15821 | 3256 | 0.29 |

Tracer pause measures receipt of a wait status through PTRACE_CONT, including
the pending-stop queue. It excludes the initial kernel-to-tracer scheduling
delay. Client write latency includes the complete synchronous SSL call. The
rough p99 difference is 0.24–1.88 ms in this fixture, below the proposed 5 ms
target; percentile differences are not paired latency samples. At 16 threads,
throughput falls about 69%, which matters for frequent small writes.

The dedicated 2.000-second idle sample consumed 0.06 CPU seconds (about **3% of
one core**); the current 100 µs polling loop accounts for that cost. CPU counters
have clock-tick granularity. The sustained 65,536-write run took 3.425 seconds:
client write p99 **1.644 ms**, maximum **3.818 ms**, tracer pause p99 **821 µs**,
maximum **1240 µs**. Samples and retained event storage remained bounded.

A preliminary immediate-resume loop produced a 2.46-second client maximum due
to low-TID starvation. The delivered implementation batches available stops
before resume. The table and sustained results above are from that corrected
implementation; the original failed run is retained only as diagnostic evidence.

## Reproduction and validation

Use the build, demo and acceptance commands in [README.md](README.md).
`scripts/acceptance.py` returns **1** for the five real crash failures; these are
not expected-pass assertions. Native Rust tests: 6 default and 5 unsafe-lab passed. Correlation Python
tests: 4 passed. Formatting, Clippy with warnings denied (AArch64 and x86-64
targets), and metadata identity checks passed. The one-command demo produced
400 registered events, 100 unregistered events and zero remaining AW bindings.

Per-case `result.json`, private JSONL, receiver confirmations, stderr and
`results.json` are generated in the chosen output directory. The task's local
handoff retains the actual native output and command logs. The formal AW gate
was not run: no existing AW framework/test or declared Provider workspace was
changed, as scoped by the task book. The new experimental workspace is validated
independently. Documentation lint/link checks are recorded with the handoff.

## Remaining limits and cleanup

SIGKILL safety is unresolved; removing payload mutation and shared text patches
does not prevent a pending debug trap from killing a target when the tracer dies.
There is no watchdog or invented successful recovery claim. The POC does not
test concurrent mutation of shared payload buffers, unconstrained thread churn
beyond the finite fixture, dynamic library replacement,
true PID/namespace recreation, or production workloads. Hashes are metadata,
not anonymization against guessing known short messages.

All launched tracer/client/server processes were reaped, the private AW daemon
was stopped, and AW owner cleanup left zero bindings. No persistent service,
Agent configuration, system ptrace setting or pre-existing AgentSight process
was modified. Reverting the experiment consists of removing this package/local
commit and its explicitly chosen test output directories. No default runtime
entrypoint or package registration requires rollback. The separate
[architecture notes](ARCHITECTURE_NOTES.md) describe the minimum future identity
export and query bridge; they are proposals, not implemented service integration.

# Transport observation validation

[中文版](POC_REPORT_zh.md)

The read-only BPF Source captures synthetic OpenSSL writes and correlates real
AW fixture identities. Paced fixtures pass, supporting further low-rate pilot
validation. Actual Qoder/SDK compatibility has not been tested. Pilot scope and
remaining integration work are in [Hook integration](ARCHITECTURE_NOTES.md).

## Tested scope

Tests ran on 2026-11-07 on Linux x86-64, kernel 6.6.102, OpenSSL 3.0.12,
Python 3.11.6, Clang 17, GCC 12.3 and libbpf 1.2.2. AW was built from the baseline
in [SOURCE.md](SOURCE.md). The fixture sends synthetic bytes to a loopback TLS
receiver and verifies its byte count and SHA-256. No model account or real Agent
traffic was involved. Existing Yama=0, perf_event_paranoid=2 and
unprivileged_bpf_disabled=2 remained unchanged.

| Coverage | Result |
| --- | --- |
| Bounded event matrix | 69/69 pass: two SSL APIs, 1/4/8/16 threads, new threads, four creation waves, size boundaries, count/rate caps and backpressure |
| Lifecycle | 21/21 pass: same-PID executable change and 20 repeated target exits |
| Strengthened kill/exit regression | 12/12 pass |
| Unit checks | 13/13 pass on native x86-64 and local AArch64 |
| Real AW registration | 200 uniquely associated events; 200 events retain same-PID multi-instance ambiguity; owner exit clears bindings |
| Collector death | All eight main-matrix SIGKILL targets complete with receiver-confirmed bytes; final loss summary is absent |
| Buffer handling | Per-thread alternating reuse and shared immutable buffers pass; concurrent mutation remains non-atomic |
| Comparison backend | Earlier count-only matrix: 112 functional, 24 performance and four coexistence cases pass; shared-broker regressions remain passing |

The AW fixture uses the existing empty-route Qoder SDK lifecycle profile and
same-PID exec. It establishes registration/correlation behavior, without real SDK
Hooks or child-Worker adoption. Kernel payload-read failure has unit projection
coverage only. Live AgentSight coexistence and actual PID reuse remain untested.

## Workload envelope

Paced cases write 50 times per thread with 5 ms sleeps, retaining exactly
50/200/400/800 events for 1/4/8/16 threads. These short synthetic runs do not set a
sustainable production rate. A stopped collector retains 252 records in the
1 MiB ring and accounts for subsequent loss while the target completes.

Unpaced load uses 1024 writes per thread, 128 bytes each, with one baseline and
one observed run per concurrency:

| Threads | Baseline p99 µs | Observed p99 / max µs | Emitted / hits | Ring loss |
| --- | --- | --- | --- | --- |
| 1 | 4.650 | 24.754 / 78.483 | 293 / 1024 | 71.4% |
| 4 | 9.013 | 23.030 / 53.402 | 300 / 4096 | 92.7% |
| 8 | 14.860 | 38.118 / 1222.998 | 321 / 8192 | 96.1% |
| 16 | 12.960 | 34.067 / 3958.296 | 380 / 16384 | 97.7% |

These are whole SSL_write latencies, not isolated probe pauses. Collector RSS
is 19,956–19,972 KiB and active CPU increments are 0.04–0.09 seconds, measured at
10 ms granularity. Payload-Source idle CPU was not measured. Even a single-thread
burst loses records, so a small pilot must measure its actual bursts as well as
concurrency. Current loss accounting reconciles hits, ring drops, emitted events,
output caps and identity discards; passing that check does not imply full capture.

## Retained failure and next validation

The ptrace comparison remains disabled by default: tracer SIGKILL can terminate
the target with SIGTRAP. The original native acceptance had five such failures.
This failure applies even to small deployments, and payload rewrite remains
unavailable. BPF collector-death results apply only to the read-only Source.

Next validate pinned Qoder/SDK versions, one managed Worker and one active Query
at the chosen task rate. Confirm the actual TLS sender, process ownership,
collection lifecycle and fixed privilege assets before a restricted read-only
pilot. Extreme-concurrency optimization can follow separately.

Reproduction commands are in [README](README.md). Native C/BPF warning checks,
Black, Python tests and documentation/link checks pass; unchanged Rust sources
retain their prior fmt/test/clippy results. Detailed JSONL and measurement files
are generated in the selected private output directory. They are runtime evidence
and are not checked into this source package.

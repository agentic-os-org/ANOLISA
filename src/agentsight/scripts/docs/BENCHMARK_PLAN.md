# AgentSight Benchmark and Stability Validation Plan

[中文版](BENCHMARK_PLAN_zh.md)

This document defines how to validate AgentSight memory governance, sustained
high-load stability, malformed-input tolerance, and resource cleanup. The final
report must answer how much resource use changed at the same QPS, how much the
maximum sustainable QPS changed, whether capture completeness regressed, and
whether long-running and malformed-input workloads remain stable.

The harness implementation covers local orchestration and report generation;
formal before/after measurements still require one frozen Linux environment,
two selected AgentSight builds, and the multi-hour execution described below.
The report records the actual architecture, and conclusions apply only to that
environment. The campaign runner treats both binaries and their source configs
as immutable inputs and verifies their SHA-256 values before and after execution.

## review summary

### Project focus

Memory governance is the primary goal. Throughput, latency, drop rate, and
trace completeness are constraints, with stability, malformed-input, and
resource-leak evidence alongside them. The goal is not merely higher QPS: it
is to prove that memory has an explicit upper bound under sustained load
without sacrificing capture completeness through throttling, backpressure, or
eviction.

The final report must answer five questions directly:

1. How much lower is optimized RSS than baseline RSS at the same QPS?
2. What maximum sustainable QPS can each variant process?
3. Do RSS, FDs, threads, or sockets continue growing after four hours?
4. Does memory optimization regress CPU, P99 latency, drops, or completeness?
5. Can overload or malformed input cause OOM, panic, exit, or failed recovery?

### Core experiments and evidence

| Question | Core test | Primary metrics | Comparison |
| --- | --- | --- | --- |
| Memory reduction | 20/40/60/80/100% of the common capacity | RSS Avg/P99/Max and slope | three before/after repetitions |
| Traffic capacity | doubling, interval refinement, formal confirmation | maximum sustainable and effective QPS, P99 | three before/after repetitions |
| Memory bound | four-hour run at the same high QPS | RSS peak/slope; queue/cache bytes when exported | same-QPS before/after |
| Resource leaks | four-hour run plus connection churn | FD/thread/socket start-to-end and slope | same-QPS before/after |
| Overload recovery | steady 10m, overload 5m, recover 15m | RSS/P99 recovery; queue recovery when exported | three before/after repetitions |
| Malformed-input safety | inject malformed, truncated, and oversized input into valid traffic | panic/OOM, valid success rate, resource recovery | baseline evidence and optimized result |
| Capture quality | every performance and stability test | drops, trace completeness, token accuracy | before/after gates |

Measure baseline and optimized capacities independently, then use the lower
value as the common maximum. The primary matrix is two variants by five QPS
levels by three repetitions. Each run has a three-minute warm-up and a
15-minute measurement, for 30 formal runs and at least nine hours. Then run
one four-hour soak per variant and three overload-recovery trials per variant.

### Primary tools

- k6 for constant-QPS HTTP/1.1 SSE/JSON traffic and load metrics;
- h2load for supplemental HTTP/2 throughput and latency;
- a deterministic HTTPS/SSE/JSON mock LLM server;
- `single_run/collect_metrics.py` and Linux `/proc` for CPU, RSS, threads, FDs, and sockets;
- SQLite request-ID reconciliation for completeness, end-to-end capture loss,
  and token accuracy;
- AgentSight runtime queue, cache-byte, completion, eviction, and drop counters
  exported as an atomic snapshot;
- Cargo test, pytest, coverage.py, and diff-cover for regression gates;
- the campaign runner, aggregate reporter, and fault injector for automation.

### Constraints and gate calibration

1. Formal tests run on a Linux host with readable kernel BTF, and
   conclusions apply only to that frozen environment.
2. Capacity uses a short automatic pretest, starts formal search at 80% of its
   estimate, and binary-searches to the declared resolution. 1000 QPS is not a
   ceiling.
3. Run one four-hour soak for each variant to establish bounded behavior within
   that observation window.
4. Trace completeness has a fixed minimum of 99.9%.
5. Calibrate every other numeric gate in an independent baseline pre-test,
   freeze it in `campaign.json`, and only then run formal baseline/optimized data.

Throughout this plan, the formal `drop_rate` gate uses the worst available
observation: `max(internal_drop_rate, 1 - trace_completeness)`. If the unmodified
binary has no internal counters, exact request-ID reconciliation still provides
the end-to-end capture-loss value; missing counters are not replaced with zero.

## 1. Validation goals

| ID | Required claim | Primary evidence |
| --- | --- | --- |
| G1 | Memory overhead is lower at the same load | RSS before/after at five QPS levels |
| G2 | Memory stays bounded under sustained high load | Four-hour RSS maximum, slope, and rolling-window trend |
| G3 | Load capacity improves or does not regress | Maximum sustainable QPS, effective QPS, and P99 latency |
| G4 | Memory controls do not sacrifice capture quality | Drop rate, trace completeness, and token accuracy |
| G5 | The pipeline recovers after overload | RSS and latency recovery time; queue/connection diagnostics when available |
| G6 | Malformed input cannot take down the pipeline | No panic/OOM while valid traffic continues |
| G7 | Long runs do not leak resources | FD, thread, and socket counts and slopes |

## 2. Test tools

### 2.1 Existing tools

| Tool | Purpose | Artifact |
| --- | --- | --- |
| `single_run/load/k6.js` with k6 | Constant-QPS HTTP/1.1 SSE/JSON load, request IDs, latency, and success rate | `k6.jsonl.gz`, `k6.log` |
| `single_run/load/h2load.sh` with h2load | Supplemental HTTP/2 throughput and latency test | `h2load.txt` |
| `single_run/mock_llm_server.py` | Deterministic HTTPS SSE/JSON/HTTP/2 LLM responses | `mock-server.log` |
| `single_run/collect_metrics.py` with Linux `/proc` | AgentSight CPU, RSS, threads, FDs, and sockets every second | `metrics.csv` |
| AgentSight Prometheus metrics snapshot | Real connection-cache, queue, stage-duration, completion, eviction, and drop metrics; current binaries export it when `AGENTSIGHT_METRICS_FILE` is set | merged into `metrics.csv`; the raw snapshot retains labeled stage metrics |
| `single_run/validate_results.py` with Python `sqlite3` | Reconcile SQLite by request ID and calculate completeness/token accuracy | `report.json` |
| `single_run/render_report.py` | Convert one run's JSONL/CSV into human-readable Markdown | `benchmark-report.md` |
| `single_run/run.sh` | Execute one load, resource collection, validation, and report | single-run directory |
| Cargo test | Rust regressions for memory budgets, parser tolerance, and cleanup | test log |
| pytest | Behavioral and statistical tests for benchmark scripts | test log |
| coverage.py and diff-cover | Enforce at least 85% incremental coverage for new Python lines | coverage XML and diff result |

`openssl` creates temporary test certificates and `curl` checks mock health.
`git`, `sha256sum`, `uname`, and `lscpu` record versions, binary checksums, and
host metadata. Formal CPU/RSS data comes from `single_run/collect_metrics.py`; `pidstat`
may only be used as a manual cross-check and must not be mixed into the primary
statistics.

### 2.2 Campaign automation

| Tool | Implemented capability |
| --- | --- |
| `campaign/reproduce_campaign.sh` | Creates temporary worktrees from two Git refs, builds and freezes both inputs with one toolchain, and runs the campaign in one command |
| `campaign/run_campaign.py` | Preflights inputs, creates mock-server-only runtime config copies, starts one immutable binary at a time, resumes interrupted stages, and collects all outputs under one directory |
| `campaign/campaign.py` | Orchestrates variants, adaptive QPS, warm-up, repetitions, soak, recovery, and fault phases |
| `campaign/aggregate_report.py` | Produces per-QPS median/min/max, before/after deltas, CSV, Markdown, and a strict evidence verdict |
| `single_run/fault_injector.py` | Uses Python `ssl`/`socket` to send malformed, truncated, binary, oversized, and TLS churn input |
| `campaign/campaign_manifest.py` | Freezes gates, configuration, binaries, log/metric paths, and host metadata before execution |

Prefer extending `single_run/run.sh`, `single_run/render_report.py`, and existing test modules. Add a
new script only when those responsibilities cannot reasonably contain the
behavior. Malformed HTTP/2 frames remain Rust fixture regressions initially;
ordinary h2load traffic must not be described as malformed HTTP/2 testing.

## 3. Before/after scope

| Test | Comparison | Reason |
| --- | --- | --- |
| Fixed-QPS performance matrix | Full before/after with three repetitions | Quantifies resource and performance changes at the same load |
| Maximum sustainable QPS | Full before/after | Quantifies stable capacity change |
| Four-hour soak | Same-QPS before/after | Detects continuing memory and resource growth |
| Overload recovery | Full before/after with three repetitions | Compares peaks and recovery speed |
| Runtime malformed input | Record both outcomes without requiring a performance percentage | The key result is safe degradation instead of a crash or leak |
| Rust/Python regressions | Optimized must pass; retain baseline failure evidence when useful | Proves specific bugs do not recur, not a performance result |

An optional AgentSight-off control can estimate observability overhead. It does
not replace the baseline and is excluded from the primary optimization delta.

## 4. Controlled conditions

- Run primary results on the same frozen Linux host and record its architecture.
- Keep CPU/memory limits, kernel, AgentSight configuration, database settings,
  payload, SSE chunks, and mock delay identical between variants.
- Alternate variants when practical and cool down between runs to reduce
  thermal and time-of-day drift.
- Reboot a physical host after suspend and disable sleep for the campaign so
  BPF and userspace timestamp domains remain aligned.
- Record git commit, binary and configuration checksums, CPU, memory, kernel,
  cgroup, CPU governor, tool versions, and UTC start/end times.
- Use a distinct warm-up before every measured phase and exclude warm-up data.
- Apply capture-quality and internal-metric gates only to the measured phase, so
  an overloaded warm-up cannot abort capacity boundary discovery; safety guards
  remain active during both phases.
- Preserve partial artifacts when the baseline crashes; never report only the
  successful samples.
- Freeze every numerical gate in `campaign.json` before collecting results.

## 5. Test scenarios

### 5.1 Smoke test

Run a short test once for each variant to prove that the mock, AgentSight, load
generator, collector, SQLite validation, and report pipeline start and stop.
The default is 10 QPS for 30 seconds. Smoke validates the harness and does not
contribute to performance conclusions. The one-command runner gives each
variant an empty SQLite directory in a private mount namespace so existing host
data and the other variant cannot affect this check.

### 5.2 Maximum sustainable QPS

Run an independent adaptive capacity search for baseline and optimized; do not
use 1000 QPS as a hard ceiling:

1. **Estimate automatically:** use short probes from `qps_start` (10 QPS by
   default), doubling after a pass or halving after a failure until a rough
   pass/fail interval is available. The estimate is the interval midpoint.
2. **Start at 80%:** round 80% of the estimate down to `qps_resolution` (1 QPS
   by default), then establish a formal passing lower bound and failing upper
   bound. Expand upward when the pretest underestimated the host. If the first
   formal point fails, verify the last pretest pass before stepping farther
   down.
3. **Search by binary subdivision:** halve the formal pass/fail interval until
   the two bounds are one `qps_resolution` step apart.
4. **Confirm formally:** measure both the candidate highest pass and its
   adjacent first failure for 15 minutes, three times each. If the candidate
   fails confirmation, use previously passing search points and adaptive binary
   subdivision to locate a confirmed lower bound; do not scan downward one QPS
   step at a time or confirm a higher point after its lower bound has failed.
5. **Decide:** a level passes only if at least two repetitions pass every gate.

The runner restarts AgentSight before every capacity probe while preserving the
isolated per-version SQLite directory. This clears in-memory backlog from a
failed higher-QPS probe before the search steps down. If the formal probe at the
minimum configured resolution still fails, the stage records an unconfirmed
capacity and stops without running redundant confirmation repetitions.
   The highest passing level with an adjacent confirmed failure is the maximum
   sustainable QPS. Non-monotonic results require host-interference analysis
   and new run IDs.

The automatic pretest uses a 30-second warm-up plus a 60-second measurement.
Formal binary-search probes use a three-minute warm-up plus five-minute
measurement. RSS slope is not a monotonic function of QPS and neither short
window can distinguish allocator noise from a long-term leak, so every
capacity probe and confirmation defers that gate to the four-hour soak. Every
other frozen capacity gate still applies. Neither kind of search probe
establishes the final capacity claim. Each formal confirmation must satisfy all
of these conditions:

- `effective_qps / input_qps` meets the declared minimum;
- HTTP success, trace completeness, and token accuracy meet their declared minimums;
- P99 latency, drop rate, and RSS maximum stay within their gates;
- no OOM, panic, unexpected exit, or database write failure occurs.

The four-hour same-QPS soak evaluates the frozen RSS-slope gate for both
versions and remains mandatory for the overall formal verdict.

The trace-completeness gate is 99.9%, and `min_token_accuracy` is 1.0.
`min_throughput_ratio`, `max_p99_ms`, `max_drop_rate`, `max_rss_mb`, and
`max_rss_slope_mb_per_hour` come from the independent pre-test and must be
frozen before formal execution. Without every gate, or when the safety ceiling
is reached without an adjacent failure, the report may show a confirmed lower
bound but must not claim a maximum sustainable QPS. This independent threshold
calibration is separate from the automatic capacity pretest described above.

### 5.3 Same-QPS performance matrix

After confirming both capacities, calculate:

```text
common_max_qps = min(baseline_max_qps, optimized_max_qps)
```

Use the same five absolute QPS values for both variants: 20%, 40%, 60%, 80%,
and 100% of `common_max_qps`, rounded down by `qps_resolution`. The five values
must remain unique. The runner records them in `campaign-resolution.json` so
the frozen `campaign.json` itself remains immutable.

| Dimension | Requirement |
| --- | --- |
| Variants | baseline and optimized |
| QPS | 20/40/60/80/100% of `common_max_qps` |
| Repetitions | three per variant/QPS |
| Warm-up | three minutes, excluded |
| Measurement | 15 minutes |
| Resource sample interval | one second |
| Primary protocol | HTTP/1.1 SSE |
| Supplemental protocols | JSON and HTTP/2, reported separately |

The matrix contains 30 measured runs and takes at least nine hours including
warm-up. Compare the following at every QPS:

- input, effective, and successfully completed QPS;
- CPU average/P95/P99/maximum;
- RSS average/P95/P99/maximum, first/last sample, and slope;
- latency P50/P95/P99/maximum;
- HTTP error/timeout rates and ring-buffer/channel drop rates;
- trace match rate, trace completeness, and token accuracy.

### 5.4 Four-hour soak

After capacity testing, select one high QPS that both variants pass. Prefer 80%
of the lower maximum sustainable QPS, rounded down to a measured level. Run one
ten-minute warm-up plus one four-hour measurement for each variant; rerun under
a new ID if the host is disturbed or the result is anomalous.

Record RSS first/last/average/P99/maximum, hourly linear RSS slope, maximum
five-minute rolling increase, FD/thread/socket first-to-last deltas and hourly
slopes, end-to-end capture loss, completeness, and OOM/panic/exit counts.
Record queue/cache bytes, evictions, and kernel/userspace drops from the atomic
runtime snapshot. A historical baseline that predates the exporter keeps these
fields unavailable rather than substituting zero.

Passing requires a live process, RSS below its gate, RSS/FD/thread/socket slopes
within campaign gates, and no completeness regression. This supports a bounded
claim over the four-hour observation window, not a mathematical claim about
unlimited runtime.

### 5.5 Overload and recovery

Run these phases three times per variant:

1. Ten-minute steady phase at the soak-test QPS.
2. Five-minute overload phase at the first failing capacity level.
3. Fifteen-minute recovery phase back at steady QPS.

Compare RSS peaks, effective-QPS recovery, P99 recovery, RSS recovery, and
recovery-phase trace completeness. When the tested binary exports real
queue/connection metrics, also report their peaks and drain time. Declare the
recovery window, tolerance, and deadline in the campaign. The recommended
default is for recovery metrics to remain within 10% of the steady phase for
five consecutive minutes.

### 5.6 Runtime malformed input

While valid traffic continues, the fault injector sends each of these classes:

- malformed JSON, invalid chunk size, and conflicting Content-Length;
- truncated HTTP body/SSE, invalid UTF-8, and binary body;
- input over the HTTP body or SSE continuation limit;
- TLS connect-and-close and repeated connection churn.

Record injected, rejected, and safely degraded counts; panic/OOM/exit counts;
valid-request success and trace completeness before/after injection; and
whether RSS/FD/socket counts recover. Optimized must stay alive and continue
processing valid traffic. Baseline preserves failure evidence; a percentage
performance improvement is not required.

### 5.7 Code-level regressions

| Goal | Required tests |
| --- | --- |
| Bounded memory | Event-channel byte budget, oversized-event rejection, pending-GenAI count/byte eviction, and HTTP body/SSE continuation caps |
| Malformed input | Invalid JSON/chunked body, binary/invalid UTF-8, and truncated SSE/HTTP/2 fixtures never panic |
| Cleanup | Reservation release/refund, concurrent drain without phantom reservation, idle eviction clears side maps, and child-process/FD cleanup |
| Reporting | P99, QPS grouping, counter resets, drop denominator, slope, recovery time, missing values, and deltas |

Every code change requires a new or updated test. Run applicable Cargo/pytest
checks and enforce incremental coverage with `diff-cover --fail-under=85`.

## 6. Metric definitions

```text
effective_qps = completed_load_requests / measured_seconds
http_success_rate = successful_http_requests / sent_requests
trace_match_rate = matched_request_ids / sent_request_ids
trace_completeness = complete_captured_calls / sent_request_ids
drop_rate = (ring_buffer_delta + channel_delta)
            / (completed_delta + ring_buffer_delta + channel_delta)
delta_pct = (optimized_median - baseline_median)
            / baseline_median * 100
```

Calculate RSS, FD, thread, and socket slopes by ordinary linear regression over
measured samples and time. Calculate cumulative counters as window deltas while
tolerating resets. A zero denominator or missing field is `—`, never zero or a
pass. Use nearest-rank percentiles consistently. Aggregate the three fixed-QPS
repetitions as median/minimum/maximum.

Lower CPU, RSS, latency, error, and drop values are better, so a negative delta
is an improvement. Higher throughput and completeness are better. If the
baseline median is zero, report only the absolute difference.

## 7. Optimization-to-evidence mapping

| Optimization | Primary metrics | Required side-effect check |
| --- | --- | --- |
| Event-channel byte budget and backpressure | RSS maximum/slope and queued bytes | Drop rate, P99, effective QPS |
| Pending-connection count/byte/TTL limits | RSS, cache bytes, eviction count | Trace match/completeness |
| HTTP body/SSE continuation caps | Oversized-input memory peak and result | Completeness for valid large requests |
| Cleanup and idle eviction | FD/socket/thread slopes and recovery time | False eviction and association failures |
| Safe malformed-input degradation | Panic/OOM count and valid-request success | CPU and error-log volume |

The final report must state which mechanism changed which metric, by how much,
and with which trade-off. A single total memory percentage is insufficient.

## 8. Reports and artifacts

Keep a manifest, configuration, logs, `metrics.csv`, raw load output, SQLite
validation, and one Markdown report for every run. A campaign produces at least:

- `performance-comparison.md/.csv` for five-QPS before/after results;
- `capacity-report.md` for maximum sustainable QPS and failed gates;
- `soak-report.md` for four-hour memory and leak trends;
- `recovery-report.md` for overload peaks and recovery time;
- `fault-report.md` for every malformed-input class and valid-traffic impact;
- `regression-report.md` for Cargo, pytest, and diff-cover results;
- `final-summary.json` for the machine-readable campaign verdict and issues;
- `final-report.md` for mechanisms, gains, completeness, limitations, and conclusions.

The primary performance table is:

| QPS | Variant | Effective QPS | CPU Avg/P95 | RSS Avg/P99/Max | RSS slope | Latency P99 | Drop rate | Trace completeness |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 500 | baseline/optimized | … | … | … | … | … | … | … |

Raw JSONL/CSV files are immutable inputs; Markdown and aggregate CSV are
rebuildable outputs. Show every failed run and missing-metric reason instead of
overwriting or silently excluding it.

## 9. Implementation and execution order

1. Complete matrix orchestration, measured windows, manifests, and phase control.
2. Complete procfs collector fields needed for process-resource slopes and
   collection of real AgentSight internal counters.
3. Complete aggregate reporting, P99, deltas, recovery-time calculations, and reports.
4. Complete the fault injector and its automated tests.
5. Run Rust/Python regressions and smoke both variants.
6. Freeze the host, configuration, gates, and tool versions.
7. Locate and formally confirm maximum sustainable QPS for both variants.
8. Run the 30 same-QPS measurements and generate the performance comparison.
9. Run the four-hour soak, overload recovery, and runtime malformed-input tests.
10. Generate final Markdown/CSV and manually review outliers and raw artifacts.

## 10. Definition of done

The harness implementation covers steps 1–4. The test deliverable is complete
only after the Linux campaign also satisfies all of the following:

- both capacity boundaries have doubling, refinement, and three formal confirmations;
- all three repetitions for five common QPS levels and both variants have traceable artifacts;
- every QPS reports CPU, RSS, effective QPS, P99, drop rate, and trace completeness;
- maximum sustainable QPS and every failed gate are reported for both variants;
- both variants complete the same-QPS four-hour soak and three recovery repetitions;
- malformed input runs alongside valid traffic and optimized has no OOM, panic, or unexpected exit;
- RSS, FD, thread, and socket results include start/end, maximum, slope, and gate outcome;
- before/after memory, throughput, latency, drop, and completeness deltas are reproducible;
- every optimization maps to a quantified gain and side-effect check;
- all regressions pass and new-code diff coverage is at least 85%;
- the report records environment, raw artifacts, failed runs, limitations, and reproduction details.

`reproduce_campaign.sh` stops an active `agentsight.service` before quick or
formal runs and does not restart it; restart the service manually after the
campaign if needed.

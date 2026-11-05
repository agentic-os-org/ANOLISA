# AgentSight benchmark usage

[中文版](BENCHMARK_USAGE_zh.md)

Generate a repeatable load of independent simulated Agents, each receiving a
specified number of output tokens per second. The entry point is
[`single_run/run.sh`](../benchmark/single_run/run.sh), which starts a local
HTTPS/SSE mock server, client processes, and report generation.

For automatic collector startup, RSS/CPU reporting and Git A/B comparison, use
[`single_run/resource_run.sh`](../benchmark/single_run/resource_run.sh) as described in §7.

## 1. Prepare the environment

Use Linux with Python 3.10 or later, `openssl`, and `curl`. Run these commands
from the repository root:

```bash
cd src/agentsight
python3 -m venv /tmp/agentsight-bench-venv
source /tmp/agentsight-bench-venv/bin/activate
python -m pip install -r scripts/benchmark/requirements.txt
```

Agent mode does not require k6. `tiktoken` verifies that repeated ` x` text
contains one `cl100k_base` token per fragment. Its encoding data is downloaded
and cached on first use, before the measurement window. The mock reports a
synthetic input usage of 12 tokens per request.

## 2. Start 32 Agents at 100 tok/s each

In the same terminal, from `src/agentsight`:

```bash
bash scripts/benchmark/single_run/run.sh \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60
```

This creates 32 separate client processes, targeting 3,200 output tok/s in
total. Each client keeps one SSE request active and starts the next request
immediately after completion. The default response contains 1,000 output
tokens, taking approximately 10 seconds at 100 tok/s.

The 60-second measurement excludes preparation and the discovery delay.
In-flight responses drain before exit, so total wall time is longer. Pass
`--agents` or `--tokens-per-second` to select Agent mode; otherwise the runner
uses its existing QPS mode.

## 3. Change the load and output directory

For example, run 8 Agents at 50 tok/s each, with 250 output tokens per response:

```bash
bench_output_dir="/tmp/agentsight-bench-$(date -u +%Y%m%dT%H%M%SZ)"
bash scripts/benchmark/single_run/run.sh \
  --agents 8 \
  --tokens-per-second 50 \
  --tokens-per-response 250 \
  --duration 10 \
  --port 8444 \
  --output-dir "$bench_output_dir"
```

Without `--output-dir`, artifacts go to
`src/agentsight/scripts/benchmark/results/<UTC timestamp>/`, with a timestamp
formatted as `YYYYMMDDTHHMMSSZ`. The runner prints the final directory. Use a
fresh directory for each run: Agent mode rejects an existing `agents/` path.
The `OUTPUT_DIR` environment variable also sets the destination; an explicit
`--output-dir` takes precedence.

## 4. Common options

| Option | Default | Purpose |
| --- | --- | --- |
| `--agents N` | 32 in Agent mode | Number of independent client processes; positive integer. |
| `--tokens-per-second RATE` | 100 in Agent mode | Target output tok/s per Agent; positive finite number. |
| `--duration SEC` | 900 | Measurement duration in seconds. Set it explicitly for a shorter run. |
| `--tokens-per-response N` | 1000 | Output tokens per request; changes stream length and request turnover. |
| `--payload-kb N` | 4 | Approximate request padding size in KiB; JSON overhead also contributes. |
| `--startup-delay SEC` | 5 | Delay after all clients are ready, allowing probe discovery; zero is allowed. |
| `--rate-tolerance FRACTION` | 0.05 | Allowed relative rate deviation; 0.05 means ±5%, with `0 < value < 1`. |
| `--output-dir DIR` | UTC timestamp directory | Artifact location; use a fresh path for each run. |
| `--host HOST` | `127.0.0.1` | Mock server bind address and client target. |
| `--port PORT` | 8443 | Mock HTTPS port; select an unused port if the default is occupied. |
| `--agentsight-pid PID` | Unset | Collect resources of an already-running collector process. |
| `--metrics-file PATH` | Unset | Read an existing internal Prometheus metrics snapshot. |
| `--db PATH` | Unset | Reconcile request IDs with an existing standalone AgentSight SQLite database. |
| `--ffi-events PATH` | Unset | Reconcile FFI callback receipts; mutually exclusive with `--db`. |
| `--validation-wait-seconds SEC` | 30 | Additional wait for asynchronous database writes after the load finishes. |
| `--help` | — | Print the runner's options and Agent mode defaults. |

When either load option selects Agent mode, the other defaults to 32 Agents or
100 tok/s. This mode supports HTTP/1.1 SSE. `--qps` is not its rate control:
the runner derives nominal request throughput from Agent count, output rate,
and tokens per response.

Keep payload size and tokens per response identical when comparing memory
before and after an optimization. Both affect retained data and stream lifetime.

## 5. Read the results

| Artifact | Contents |
| --- | --- |
| `benchmark-report.md` | Human-readable load, resource, and optional capture report. |
| `agent-summary.json` | Distinct PIDs, per-Agent received rates, stream overlap, and `passed`. |
| `run-summary.json` | Combined machine-readable summary, including `agent_load`. |
| `agents.jsonl.gz` | Request IDs, success counters, and request latency records. |
| `agents/` | Per-client results, request logs, and the shared start barrier. |
| `agents.log` | Client coordinator output and failure details. |
| `mock-server.log` | Local mock server startup and errors. |
| `request-logs/` | Mock server's accepted-request ledgers. |
| `metrics.csv` | Process resources, when `--agentsight-pid` enables collection. |
| `report.json` | Request-ID and Token reconciliation from SQLite or FFI callback receipts. |

In `agent-summary.json`, check `distinct_agent_pids`, `peak_active_streams`,
`average_active_streams`, `agents[].tokens_per_second`, and `passed`. Every
Agent must receive output within the configured tolerance; peak overlap must
reach the Agent count and average overlap must reach `N × (1 - tolerance)`.
Rates count received output only within the measurement window, excluding
synthetic input usage and tokens received during the final drain.

Successful execution returns 0. Failed load verification or optional collection
or validation returns nonzero. When a resource safety guard stops the load,
the runner returns 75 and writes `safety-stop.json`. A workload PASS alone does
not establish capture completeness or the 275 MB memory target.

## 6. Measure AgentSight resources

Start the collector beforehand as described in the
[CLI reference](../../../../docs/user-guide/en/agentsight/cli-reference.md).
eBPF tracing requires root or the required capabilities. In an isolated copy
of the production configuration, match the clients with this `cmdline.allow`
entry and exclude mock-server discovery rules:

```json
{
  "rule": ["*python*", "*agents.py", "--worker"],
  "agent_name": "BenchmarkAgent"
}
```

Preserve the feature, resource-limit, and storage settings being evaluated.
Add `--agentsight-pid <COLLECTOR_PID>` to the load command, using the actual
collector PID. Add `--metrics-file <METRICS_SNAPSHOT_PATH>` when its metrics
exporter is configured, and `--db <GENAI_DATABASE_PATH>` when standalone SQLite
storage is available. These placeholders must be replaced with existing values.
The runner does not start AgentSight or configure its metrics exporter.

Resource collection starts at the shared measurement barrier and continues
through response drain. `metrics.csv` measures RSS in MiB; the existing report
may label it MB. For an FFI integration, supplying the host process PID measures
the host's RSS. LoongCollector WSS requires a separate measurement with the
same scope as the original experiment.

## 7. One-command resource measurement and A/B comparison

From `src/agentsight`, run:

```bash
bash scripts/benchmark/single_run/resource_run.sh \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60 \
  --output-dir /tmp/agentsight-resource-single
```

The script prepares missing Python dependencies in an isolated cached venv,
freezes the current working tree, and builds a release binary with Cargo's lock
file. Git/source mode needs the project's Rust and Linux eBPF build prerequisites;
use `--agentsight-bin PATH` to supply a prebuilt executable instead. Builds and
dependencies are cached under `${XDG_CACHE_HOME:-$HOME/.cache}/agentsight-benchmark/`.
It requests `sudo` for tracing and database validation, so a password prompt may
appear. Use `--no-sudo` only when the current user already has eBPF permissions.

The script creates an isolated configuration, database and log files. It prepares
all client TLS connections before starting AgentSight, so startup discovery sees
the loaded TLS libraries. Every client sends a small preflight call; the wrapper
requires a complete SQLite record (CLI) or FFI callback receipt with correct token usage from every client
before releasing the measurement barrier. Preflight requests use a separate ID
prefix and are excluded from load totals, capture totals and RSS samples.

`--ready-timeout` defaults to 30 seconds and separately bounds collector startup
and capture readiness. A failed preflight stops before measurement and records
the per-client result in `capture-readiness.json`; inspect it alongside `trace.log`
and `agents.log`. Normal exit, failure, Ctrl+C and SIGTERM reclaim the processes
started by that invocation. A/B uses the same preparation and readiness checks
for both binaries.

The isolated configuration sets `runtime_limits.connection_capacity` to the Agent
count by default, so 32 Agents get 32 connection slots. Use `--connection-capacity N`
to override it; the report records the chosen value. A/B uses the same capacity
for both runs. Compared binaries must support this configuration key; older
binaries that ignore it retain their own limit and a capture failure keeps the
comparison INVALID.

`--ring-buffer-mb N` overrides the configured eBPF ring buffer size in MiB.
`N` must be a positive power of two, such as 32 or 64. Without this option,
the value from `--config` is preserved (32 MiB in the default configuration).
Keep this size identical in both A/B runs; enlarging the buffer changes the
memory baseline. For simultaneous large requests, a 64 MiB run can check whether
the default buffer is too small. The report prints the chosen size.

Compare the last commit with the current working tree, including staged,
unstaged and non-ignored untracked files:

```bash
bash scripts/benchmark/single_run/resource_run.sh \
  --ab \
  --baseline HEAD \
  --candidate working-tree \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60 \
  --output-dir /tmp/agentsight-resource-ab
```

`--baseline` and `--candidate` also accept commit hashes, local branches and tags.
`working-tree` is the candidate default; `HEAD` excludes uncommitted changes.
Sources build in temporary detached worktrees using a private snapshot index;
the caller's branch and staging area are preserved. Both binaries build before
measurement. Runs execute sequentially with the same frozen harness, configuration
and load parameters, fresh collector processes, and separate databases.
For existing binaries, use `--ab --baseline-bin PATH --candidate-bin PATH`.

The terminal and `resource-report.txt` show average/peak RSS, their percentages of
host RAM, average/P95/peak CPU, Load/Capture PASS or FAIL, and run status.
CPU includes all threads of the standalone AgentSight process: 100% means one
logical CPU, and values can exceed 100%. The report records the host logical CPU
count; mock-server and client processes are outside this scope.
A/B shows RSS reductions in MiB and relative percent. CPU reductions use percentage
points (`before - after`) and relative percent; negative values mean increased
resource usage. A zero CPU baseline has no relative percentage. Host RAM share
changes use percentage points. `resource-summary.json` records the measurements,
version/tree IDs and binary hashes. Per-run CSV, capture reports and logs are under
`run/`, or `before/` and `after/` in A/B mode.

The result is VALID only if the load passes, capture completeness and token
accuracy are each at least 99.9%, the collector survives with valid resource
samples, and available probe counters report no dropped events. Ring and
userspace-channel drops are printed separately. These counters cover all probe
types, so they do not identify which events were lost; any observed drops keep
the result INVALID even if all requests have correct usage. Missing counters
are shown as N/A; older binaries continue to use the request-level checks.
Missing or invalid CPU samples, or CPU coverage below 95% of the load window,
also make the result INVALID. An INVALID result suppresses all improvement calculations.
Success exits 0; invalid/failed runs exit nonzero; Ctrl+C exits 130 and preserves
partial artifacts. Use a fresh output directory for each invocation. The default
is `scripts/benchmark/results/resource-<UTC timestamp>-<PID>/`.

This wrapper defaults to 32 Agents, 100 tok/s each and 60 seconds; the load-only
`run.sh` still defaults to 900 seconds. It accepts the load, discovery, port and
safety controls listed above; run `resource_run.sh --help` for all supported flags.
RSS and CPU are sampled about once per second through response drain. Main
statistics use the monotonic start/end window in `agents/start.json`; response
drain is summarized separately in each run's `tail` object and the text report.
Average RSS is the arithmetic mean of in-window samples; peak RSS is the sampled
maximum, not the process-lifetime `VmHWM`. RSS is reported in MiB, not decimal MB.
The first sample establishes a CPU baseline and leaves CPU fields empty. Average
CPU is weighted by the actual measured interval, with CPU seconds reported too.
P95 uses nearest-rank interval samples; peak CPU is the maximum interval average.
Intervals crossing a window boundary contribute proportionally to their overlap.
`metrics.csv` retains wall-clock timestamps and adds `monotonic_timestamp` and
`cpu_interval_seconds` for these calculations. The original LoongCollector WSS
target needs a separate measurement using the same original scope.

### 7.1 Measure the FFI library with a minimal C host

Add `--ffi` to the resource command to build `libagentsight.so` and a minimal
[C consumer](../benchmark/single_run/ffi_runner.c). It accepts the same load,
configuration and Git A/B options. A C compiler (`cc`, or `CC`) is required.
eBPF still requires root or the appropriate capabilities; sudo is requested by default.

```bash
bash scripts/benchmark/single_run/resource_run.sh --ffi \
  --agents 32 --tokens-per-second 100 --duration 60
```

The host waits on eventfd with epoll and drains events through non-NULL callbacks.
`--ffi-api typed` (default) measures the typed
`agentsight_read` interface; `--ffi-api v2` measures `agentsight_read_v2` with only
the JSON envelope callback. Select the interface used by the real integration.
No per-event printing or application processing is included. CPU/RSS cover all
AgentSight threads, FFI conversion, receipt forwarding and the small C host baseline;
they are not an exact allocation attribution inside a production host.
FFI validation uses callbacks and does not require AgentSight SQLite storage.
Storage feature settings from `--config` are preserved, including disabled SQLite.

Requests include a benchmark ID marker in user content so compact FFI events retain
their identity even when the library generates its own response ID. Typed callbacks
write only the ID, HTTP status and Token usage to `ffi-events.log`. V2 callbacks forward
their existing JSON payload without parsing or serializing it again; this can produce
large receipt files for large requests. A separate validator process reads these receipts
incrementally and reuses the normal readiness, completeness and Token accuracy checks.
The measured host includes receipt writes; reconciliation and JSON parsing are outside it.

Supply an existing library with `--ffi --candidate-lib /path/to/libagentsight.so`.
For two prebuilt libraries, use `--ffi --ab --baseline-lib PATH --candidate-lib PATH`;
for Git versions, use `--ffi --ab --baseline REF --candidate REF`.
Do not combine `--ffi` with executable input options (`--agentsight-bin`,
`--candidate-bin`, `--baseline-bin`). Prebuilt libraries must be compatible with
the checkout's `include/agentsight.h`; their runtime dependencies must be installed.

Each run retains its library beside the runner, with a relative library search
path. `inputs.json` and `resource-summary.json` record both library and runner
SHA-256 hashes. `ffi-consumer.json` records callback counts, including preflight;
missing or incorrect callback receipts, an unsuccessful consumer exit, missing final
pipeline metrics or any FFI queue drops invalidate the result.
The report labels the measurement as a minimal C FFI host.

See the [benchmark plan](BENCHMARK_PLAN.md) for the complete before/after design.


`--cpu-budget-percent 25` sets `runtime_limits.cpu_budget_percent` for the captured process.
Omitting the flag uses the configured target or 25% when unset; `0` disables pacing. This cooperative target
paces capture persistence and primary/GenAI cleanup, increasing delivery latency when needed.
It leaves workload generation, CPU sampling and validity checks unchanged. Inspect
`completion_queue_count` / `completion_queue_bytes` in `metrics.csv`, probe drops and response drain
alongside CPU/RSS; sustained queue growth is not an acceptable CPU improvement.
